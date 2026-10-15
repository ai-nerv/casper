//! The tree-sitter half of the indexer's VM: the part of maki's `maki.treesitter` its Lua reads,
//! installed as the global `treesitter`, under the same names and with the same meanings.
//!
//! ```lua
//! local root = treesitter.get_parser(source, "rust"):parse()[1]:root()
//! local name = treesitter.get_node_text(root:child(0):field("name")[1], source)
//! ```

use luna::{Callback, CallbackReturn, Context, Error, Stack, Table, UserData, Value};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;
use tree_sitter::{Language, Node, Parser, Tree};

/// A source and its grammar, parsed on the first `parse` and kept.
struct Parsing {
    source: Vec<u8>,
    grammar: Language,
    tree: RefCell<Option<Rc<Tree>>>,
}

/// A parsed tree.
struct Parsed(Rc<Tree>);

/// One node, held by where it sits rather than borrowed, and found in its tree again on each call.
struct Held {
    tree: Rc<Tree>,
    start: usize,
    end: usize,
    id: usize,
}

impl Held {
    fn new(node: Node<'_>, tree: &Rc<Tree>) -> Self {
        Self {
            tree: Rc::clone(tree),
            start: node.start_byte(),
            end: node.end_byte(),
            id: node.id(),
        }
    }

    /// The node again: an ancestor of the smallest node over its bytes, or, for a zero-width node
    /// that search cannot see, found by descending only through nodes that contain it.
    fn node(&self) -> Option<Node<'_>> {
        let root = self.tree.root_node();
        let mut climbing = root.descendant_for_byte_range(self.start, self.end);
        while let Some(node) = climbing {
            if node.id() == self.id {
                return Some(node);
            }
            climbing = node.parent();
        }
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if node.id() == self.id {
                return Some(node);
            }
            let mut cursor = node.walk();
            stack.extend(
                node.children(&mut cursor).filter(|child| {
                    child.start_byte() <= self.start && child.end_byte() >= self.end
                }),
            );
        }
        None
    }
}

/// A node method: the node's metatable, its tree, the node, and the call's stack to answer on.
type Method =
    for<'gc> fn(Context<'gc>, Table<'gc>, &Rc<Tree>, Node<'_>, &mut Stack<'gc, '_>) -> Answer<'gc>;

type Answer<'gc> = Result<(), Error<'gc>>;

/// Every node method the indexer calls, and nothing it does not.
const NODE: &[(&str, Method)] = &[
    ("type", |ctx, _, _, node, stack| {
        stack.replace(ctx, text(ctx, node.kind().as_bytes()));
        Ok(())
    }),
    ("start", |ctx, _, _, node, stack| {
        let at = node.start_position();
        stack.replace(ctx, (int(at.row), int(at.column), int(node.start_byte())));
        Ok(())
    }),
    ("end_", |ctx, _, _, node, stack| {
        let at = node.end_position();
        stack.replace(ctx, (int(at.row), int(at.column), int(node.end_byte())));
        Ok(())
    }),
    ("child", |ctx, nodes, tree, node, stack| {
        let child = position(stack.get(1)).and_then(|at| node.child(at));
        stack.replace(
            ctx,
            child.map(|child| wrap(ctx, nodes, Held::new(child, tree))),
        );
        Ok(())
    }),
    ("child_count", |ctx, _, _, node, stack| {
        stack.replace(ctx, i64::from(node.child_count()));
        Ok(())
    }),
    ("children", |ctx, nodes, tree, node, stack| {
        let mut cursor = node.walk();
        stack.replace(ctx, listed(ctx, nodes, tree, node.children(&mut cursor)));
        Ok(())
    }),
    ("named_children", |ctx, nodes, tree, node, stack| {
        let mut cursor = node.walk();
        stack.replace(
            ctx,
            listed(ctx, nodes, tree, node.named_children(&mut cursor)),
        );
        Ok(())
    }),
    ("field", |ctx, nodes, tree, node, stack| {
        let Value::String(name) = stack.get(1) else {
            return Err(raise(ctx, "field(name): a field name"));
        };
        let name = String::from_utf8_lossy(name.as_bytes()).into_owned();
        let mut cursor = node.walk();
        let found = node.children_by_field_name(&name, &mut cursor);
        stack.replace(ctx, listed(ctx, nodes, tree, found));
        Ok(())
    }),
    ("parent", |ctx, nodes, tree, node, stack| {
        stack.replace(
            ctx,
            node.parent()
                .map(|up| wrap(ctx, nodes, Held::new(up, tree))),
        );
        Ok(())
    }),
    ("prev_sibling", |ctx, nodes, tree, node, stack| {
        let before = node.prev_sibling();
        stack.replace(ctx, before.map(|n| wrap(ctx, nodes, Held::new(n, tree))));
        Ok(())
    }),
    ("extra", |ctx, _, _, node, stack| {
        stack.replace(ctx, node.is_extra());
        Ok(())
    }),
    ("has_error", |ctx, _, _, node, stack| {
        stack.replace(ctx, node.has_error());
        Ok(())
    }),
];

/// Put `treesitter` in front of the indexer: `get_parser` and `get_node_text`, and behind them
/// the methods of a parser, a tree and a node. Past `until`, every node call raises: the time a
/// wide tree costs is spent in these calls, where no instruction budget sees it.
pub fn install(ctx: Context<'_>, until: Instant) {
    let (nodes, node_methods) = class(ctx);
    for (name, method) in NODE {
        node_methods
            .set(ctx, *name, on_node(ctx, nodes, *method, until))
            .ok();
    }
    let (trees, tree_methods) = class(ctx);
    let root = Callback::from_fn_with(&ctx, nodes, |nodes, ctx, _exec, mut stack| {
        let Parsed(tree) = this::<Parsed>(ctx, &stack, "tree")?;
        stack.replace(ctx, wrap(ctx, *nodes, Held::new(tree.root_node(), tree)));
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
        .set(ctx, "get_parser", get_parser(ctx, parsers))
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
        let mut parser = Parser::new();
        parser
            .set_language(&self.grammar)
            .map_err(|why| format!("failed to set language: {why}"))?;
        let tree = Rc::new(
            parser
                .parse(&self.source, None)
                .ok_or("parse returned no tree")?,
        );
        *self.tree.borrow_mut() = Some(Rc::clone(&tree));
        Ok(tree)
    }
}

/// `treesitter.get_parser(source, lang)`: a parser, or `nil` and why there is none.
fn get_parser<'gc>(ctx: Context<'gc>, parsers: Table<'gc>) -> Callback<'gc> {
    Callback::from_fn_with(&ctx, parsers, |parsers, ctx, _exec, mut stack| {
        let (Value::String(source), Value::String(lang)) = (stack.get(0), stack.get(1)) else {
            return Err(raise(ctx, "get_parser(source, lang): two strings"));
        };
        let lang = String::from_utf8_lossy(lang.as_bytes()).into_owned();
        let Some(grammar) = super::grammars::named(&lang) else {
            let why = format!("no language registered: {lang}");
            stack.replace(ctx, (Value::Nil, text(ctx, why.as_bytes())));
            return Ok(CallbackReturn::Return);
        };
        let parsing = Parsing {
            source: source.as_bytes().to_vec(),
            grammar,
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
            .node()
            .ok_or_else(|| raise(ctx, "node not found in tree"))?;
        let covered = source
            .as_bytes()
            .get(node.start_byte()..node.end_byte())
            .ok_or_else(|| raise(ctx, "node range exceeds source length"))?;
        stack.replace(ctx, text(ctx, covered));
        Ok(CallbackReturn::Return)
    })
}

/// A node method as a callable, finding its node again before it runs.
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
            .node()
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

/// `held` as a userdata answering to `meta`.
fn wrap<'gc, T: 'static>(ctx: Context<'gc>, meta: Table<'gc>, held: T) -> Value<'gc> {
    let data = UserData::new_static(&ctx, held);
    data.set_metatable(ctx, Some(meta));
    Value::UserData(data)
}

/// The nodes, as a list.
fn listed<'gc, 'tree>(
    ctx: Context<'gc>,
    nodes: Table<'gc>,
    tree: &Rc<Tree>,
    found: impl Iterator<Item = Node<'tree>>,
) -> Table<'gc> {
    let list = Table::new(&ctx);
    for (at, node) in found.enumerate() {
        list.set(ctx, int(at + 1), wrap(ctx, nodes, Held::new(node, tree)))
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
fn position(value: Value<'_>) -> Option<u32> {
    match value {
        Value::Integer(at) => u32::try_from(at).ok(),
        Value::Number(at) if at.fract() == 0.0 && at >= 0.0 => u32::try_from(at as u64).ok(),
        _ => None,
    }
}

fn int(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

fn text<'gc>(ctx: Context<'gc>, bytes: &[u8]) -> Value<'gc> {
    Value::String(luna::String::from_slice(&ctx, bytes))
}

fn raise<'gc>(ctx: Context<'gc>, message: &str) -> Error<'gc> {
    Error::from_value(text(ctx, message.as_bytes()))
}
