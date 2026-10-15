//! The tree-sitter half of the indexer's VM: the part of maki's `maki.treesitter` its Lua reads,
//! installed as the global `treesitter`, under the same names and with the same meanings.
//!
//! ```lua
//! local root = treesitter.get_parser(source, "rust"):parse()[1]:root()
//! local name = treesitter.get_node_text(root:child(0):field("name")[1], source)
//! ```
//!
//! No grammar is linked in. A parse runs the language's driver from the [`Shelf`] and reads its
//! tree back with [`super::tree`]; a node is a position in that tree.

use super::tree::{self, Node, Tree};
use crate::shelf::Shelf;
use luna::{Callback, CallbackReturn, Context, Error, Stack, Table, UserData, Value};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Instant;

/// A source and the driver that parses it, parsed on the first `parse` and kept.
struct Parsing {
    source: Vec<u8>,
    driver: PathBuf,
    rev: String,
    language: String,
    until: Instant,
    tree: RefCell<Option<Rc<Tree>>>,
}

/// A parsed tree.
struct Parsed(Rc<Tree>);

/// One node: its tree, and where in it.
struct Held {
    tree: Rc<Tree>,
    at: u32,
}

/// A node method: the node's metatable, its tree, the node, and the call's stack to answer on.
type Method =
    for<'gc> fn(Context<'gc>, Table<'gc>, &Rc<Tree>, &Node, &mut Stack<'gc, '_>) -> Answer<'gc>;

type Answer<'gc> = Result<(), Error<'gc>>;

/// Every node method the indexer calls, and nothing it does not.
const NODE: &[(&str, Method)] = &[
    ("type", |ctx, _, tree, node, stack| {
        stack.replace(ctx, text(ctx, tree.kind(node).as_bytes()));
        Ok(())
    }),
    ("start", |ctx, _, _, node, stack| {
        let at = node.start;
        stack.replace(ctx, (int(at.row), int(at.column), int(at.byte)));
        Ok(())
    }),
    ("end_", |ctx, _, _, node, stack| {
        let at = node.end;
        stack.replace(ctx, (int(at.row), int(at.column), int(at.byte)));
        Ok(())
    }),
    ("child", |ctx, nodes, tree, node, stack| {
        let kids = tree.children(node);
        let child = position(stack.get(1)).and_then(|at| kids.get(at).copied());
        stack.replace(ctx, child.map(|at| wrap(ctx, nodes, held(tree, at))));
        Ok(())
    }),
    ("child_count", |ctx, _, _, node, stack| {
        stack.replace(ctx, i64::from(node.child_count()));
        Ok(())
    }),
    ("children", |ctx, nodes, tree, node, stack| {
        let kids = tree.children(node).iter().copied();
        stack.replace(ctx, listed(ctx, nodes, tree, kids));
        Ok(())
    }),
    ("named_children", |ctx, nodes, tree, node, stack| {
        let kids = tree.children(node).iter().copied();
        let named = kids.filter(|at| tree.node(*at).is_some_and(Node::named));
        stack.replace(ctx, listed(ctx, nodes, tree, named));
        Ok(())
    }),
    ("field", |ctx, nodes, tree, node, stack| {
        let Value::String(name) = stack.get(1) else {
            return Err(raise(ctx, "field(name): a field name"));
        };
        let name = String::from_utf8_lossy(name.as_bytes()).into_owned();
        stack.replace(ctx, listed(ctx, nodes, tree, tree.field(node, &name)));
        Ok(())
    }),
    ("parent", |ctx, nodes, tree, node, stack| {
        let up = tree.parent(node);
        stack.replace(ctx, up.map(|at| wrap(ctx, nodes, held(tree, at))));
        Ok(())
    }),
    ("prev_sibling", |ctx, nodes, tree, node, stack| {
        let before = tree.prev_sibling(node);
        stack.replace(ctx, before.map(|at| wrap(ctx, nodes, held(tree, at))));
        Ok(())
    }),
    ("extra", |ctx, _, _, node, stack| {
        stack.replace(ctx, node.extra());
        Ok(())
    }),
    ("has_error", |ctx, _, _, node, stack| {
        stack.replace(ctx, node.has_error());
        Ok(())
    }),
];

/// Put `treesitter` in front of the indexer: `get_parser` and `get_node_text`, and behind them
/// the methods of a parser, a tree and a node. A parse finds its driver on `shelf`. Past `until`,
/// every node call raises: the time a wide tree costs is spent in these calls, where no
/// instruction budget sees it.
pub fn install(ctx: Context<'_>, until: Instant, shelf: Shelf) {
    let (nodes, node_methods) = class(ctx);
    for (name, method) in NODE {
        node_methods
            .set(ctx, *name, on_node(ctx, nodes, *method, until))
            .ok();
    }
    let (trees, tree_methods) = class(ctx);
    let root = Callback::from_fn_with(&ctx, nodes, |nodes, ctx, _exec, mut stack| {
        let Parsed(tree) = this::<Parsed>(ctx, &stack, "tree")?;
        stack.replace(ctx, wrap(ctx, *nodes, held(tree, 0)));
        Ok(CallbackReturn::Return)
    });
    tree_methods.set(ctx, "root", root).ok();
    let (parsers, parser_methods) = class(ctx);
    let parse = Callback::from_fn_with(&ctx, trees, |trees, ctx, _exec, mut stack| {
        let tree = this::<Parsing>(ctx, &stack, "parser")?
            .tree()
            .map_err(|why| raise(ctx, &why))?;
        let list = Table::new(&ctx);
        list.set(ctx, 1, wrap(ctx, *trees, Parsed(tree))).ok();
        stack.replace(ctx, list);
        Ok(CallbackReturn::Return)
    });
    parser_methods.set(ctx, "parse", parse).ok();

    let treesitter = Table::new(&ctx);
    treesitter
        .set(ctx, "get_parser", get_parser(ctx, parsers, until, shelf))
        .ok();
    treesitter
        .set(ctx, "get_node_text", get_node_text(ctx, until))
        .ok();
    ctx.set_global("treesitter", treesitter);
}

impl Parsing {
    /// The tree, parsed now if it has not been yet.
    fn tree(&self) -> Result<Rc<Tree>, String> {
        if let Some(tree) = self.tree.borrow().as_ref() {
            return Ok(Rc::clone(tree));
        }
        let stopped = || {
            let most = crate::running::READING.as_secs();
            format!("stopped after {most}s, the most one index may take")
        };
        if Instant::now() >= self.until {
            return Err(stopped());
        }
        let driver = self.driver.display().to_string();
        let tree = tree::parsed(&driver, &self.source, &self.rev, self.until).map_err(|why| {
            if Instant::now() >= self.until {
                return stopped();
            }
            let lang = &self.language;
            if why.starts_with("was built") {
                return format!("the {lang} grammar {why}: run `casper grammar fetch {lang}`");
            }
            format!("the {lang} grammar: {why}")
        })?;
        let tree = Rc::new(tree);
        *self.tree.borrow_mut() = Some(Rc::clone(&tree));
        Ok(tree)
    }
}

/// `treesitter.get_parser(source, lang)`: a parser, or `nil` and why there is none.
fn get_parser<'gc>(
    ctx: Context<'gc>,
    parsers: Table<'gc>,
    until: Instant,
    shelf: Shelf,
) -> Callback<'gc> {
    Callback::from_fn_with(&ctx, parsers, move |parsers, ctx, _exec, mut stack| {
        let (Value::String(source), Value::String(lang)) = (stack.get(0), stack.get(1)) else {
            return Err(raise(ctx, "get_parser(source, lang): two strings"));
        };
        let lang = String::from_utf8_lossy(lang.as_bytes()).into_owned();
        let found = crate::grammars::pinned(&lang)
            .ok_or_else(|| format!("no language registered: {lang}"))
            .and_then(|pin| {
                let driver = shelf.find(&lang).ok_or_else(|| {
                    format!(
                        "no {lang} grammar built: run `casper grammar fetch {lang} && casper \
                         grammar build {lang}`"
                    )
                })?;
                Ok((driver, pin.rev.clone()))
            });
        let (driver, rev) = match found {
            Ok(found) => found,
            Err(why) => {
                stack.replace(ctx, (Value::Nil, text(ctx, why.as_bytes())));
                return Ok(CallbackReturn::Return);
            }
        };
        let parsing = Parsing {
            source: source.as_bytes().to_vec(),
            driver,
            rev,
            language: lang,
            until,
            tree: RefCell::new(None),
        };
        stack.replace(ctx, wrap(ctx, *parsers, parsing));
        Ok(CallbackReturn::Return)
    })
}

/// `treesitter.get_node_text(node, source)`: the bytes of `source` the node covers.
fn get_node_text(ctx: Context<'_>, until: Instant) -> Callback<'_> {
    Callback::from_fn(&ctx, move |ctx, _exec, mut stack| {
        within(ctx, until)?;
        let held = this::<Held>(ctx, &stack, "node")?;
        let Value::String(source) = stack.get(1) else {
            return Err(raise(
                ctx,
                "get_node_text(node, source): the source as a string",
            ));
        };
        let node = held
            .tree
            .node(held.at)
            .ok_or_else(|| raise(ctx, "node not found in tree"))?;
        let covered = source
            .as_bytes()
            .get(node.start.byte as usize..node.end.byte as usize)
            .ok_or_else(|| raise(ctx, "node range exceeds source length"))?;
        stack.replace(ctx, text(ctx, covered));
        Ok(CallbackReturn::Return)
    })
}

/// A node method as a callable, finding its node before it runs.
fn on_node<'gc>(
    ctx: Context<'gc>,
    nodes: Table<'gc>,
    method: Method,
    until: Instant,
) -> Callback<'gc> {
    Callback::from_fn_with(&ctx, nodes, move |nodes, ctx, _exec, mut stack| {
        within(ctx, until)?;
        let held = this::<Held>(ctx, &stack, "node")?;
        let node = held
            .tree
            .node(held.at)
            .ok_or_else(|| raise(ctx, "node not found in tree"))?;
        method(ctx, *nodes, &held.tree, node, &mut stack)?;
        Ok(CallbackReturn::Return)
    })
}

/// A raise once `until` has passed.
fn within(ctx: Context<'_>, until: Instant) -> Answer<'_> {
    if Instant::now() < until {
        return Ok(());
    }
    let most = crate::running::READING.as_secs();
    Err(raise(
        ctx,
        &format!("stopped after {most}s, the most one index may take"),
    ))
}

/// A metatable whose `__index` is the methods table it hands back beside it.
fn class(ctx: Context<'_>) -> (Table<'_>, Table<'_>) {
    let (meta, methods) = (Table::new(&ctx), Table::new(&ctx));
    meta.set(ctx, "__index", methods).ok();
    (meta, methods)
}

fn held(tree: &Rc<Tree>, at: u32) -> Held {
    Held {
        tree: Rc::clone(tree),
        at,
    }
}

/// `held` as a userdata answering to `meta`.
fn wrap<'gc, T: 'static>(ctx: Context<'gc>, meta: Table<'gc>, held: T) -> Value<'gc> {
    let data = UserData::new_static(&ctx, held);
    data.set_metatable(ctx, Some(meta));
    Value::UserData(data)
}

/// The nodes at `found`, as a list.
fn listed<'gc>(
    ctx: Context<'gc>,
    nodes: Table<'gc>,
    tree: &Rc<Tree>,
    found: impl Iterator<Item = u32>,
) -> Table<'gc> {
    let list = Table::new(&ctx);
    for (n, at) in found.enumerate() {
        list.set(ctx, int(n + 1), wrap(ctx, nodes, held(tree, at)))
            .ok();
    }
    list
}

/// The value a method was called on, as a `T`.
fn this<'gc, T: 'static>(
    ctx: Context<'gc>,
    stack: &Stack<'gc, '_>,
    what: &str,
) -> Result<&'gc T, Error<'gc>> {
    match stack.get(0) {
        Value::UserData(data) => data
            .downcast_static::<T>()
            .map_err(|_| raise(ctx, &format!("expected a {what}"))),
        _ => Err(raise(ctx, &format!("expected a {what}"))),
    }
}

/// A child's position, from a Lua integer or a whole float.
fn position(value: Value<'_>) -> Option<usize> {
    match value {
        Value::Integer(at) => usize::try_from(at).ok(),
        Value::Number(at) if at.fract() == 0.0 && at >= 0.0 => usize::try_from(at as u64).ok(),
        _ => None,
    }
}

fn int(n: impl TryInto<i64>) -> i64 {
    n.try_into().unwrap_or(i64::MAX)
}

fn text<'gc>(ctx: Context<'gc>, bytes: &[u8]) -> Value<'gc> {
    Value::String(luna::String::from_slice(&ctx, bytes))
}

fn raise<'gc>(ctx: Context<'gc>, message: &str) -> Error<'gc> {
    Error::from_value(text(ctx, message.as_bytes()))
}
