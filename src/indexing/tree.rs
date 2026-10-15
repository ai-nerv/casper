//! A syntax tree as a grammar's driver wrote it, read back into an arena: what `treesitter`'s nodes
//! are positions in. See `src/grammars/driver.c` for the format.
//!
//! Every count, offset and index is checked against what came before it, so a driver that wrote
//! nonsense is an error here and never a panic or a node pointing outside the tree.

use std::time::Instant;

/// The format version this reads.
const VERSION: u32 = 1;

/// The most nodes a tree may have, as the driver caps it.
pub const MOST_NODES: usize = 1 << 20;

/// The most bytes a driver may write, as it caps them.
pub const MOST_OUTPUT: usize = 48 * 1024 * 1024;

/// One node's fixed record.
const RECORD: usize = 36;

const NAMED: u16 = 1;
const EXTRA: u16 = 1 << 1;
const HAS_ERROR: u16 = 1 << 2;

/// Where a node starts or ends: row, column and byte, as tree-sitter counts them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Point {
    pub row: u32,
    pub column: u32,
    pub byte: u32,
}

/// One node: what it is, where it is, and where its family is in the arena.
#[derive(Debug, Clone, Default)]
pub struct Node {
    kind: u16,
    field: u16,
    flags: u16,
    pub start: Point,
    pub end: Point,
    parent: Option<u32>,
    slot: u32,
    kids: u32,
    count: u32,
}

/// A parsed source's tree. Node `0` is the root.
#[derive(Debug, Default)]
pub struct Tree {
    kinds: Vec<String>,
    fields: Vec<String>,
    nodes: Vec<Node>,
    kids: Vec<u32>,
}

impl Tree {
    #[must_use]
    pub fn node(&self, at: u32) -> Option<&Node> {
        self.nodes.get(at as usize)
    }

    #[must_use]
    pub fn kind(&self, node: &Node) -> &str {
        self.kinds
            .get(usize::from(node.kind))
            .map_or("", String::as_str)
    }

    /// The children of `node`, in order.
    #[must_use]
    pub fn children(&self, node: &Node) -> &[u32] {
        let from = node.kids as usize;
        self.kids
            .get(from..from + node.count as usize)
            .unwrap_or_default()
    }

    /// The children of `node` under the field called `name`.
    pub fn field<'a>(&'a self, node: &'a Node, name: &str) -> impl Iterator<Item = u32> + 'a {
        let id = self
            .fields
            .iter()
            .position(|field| field == name)
            .and_then(|at| u16::try_from(at + 1).ok());
        self.children(node)
            .iter()
            .copied()
            .filter(move |kid| id.is_some() && self.node(*kid).map(|n| n.field) == id)
    }

    #[must_use]
    pub fn parent(&self, node: &Node) -> Option<u32> {
        node.parent
    }

    /// The child before `node` under the same parent.
    #[must_use]
    pub fn prev_sibling(&self, node: &Node) -> Option<u32> {
        let parent = self.node(node.parent?)?;
        let before = node.slot.checked_sub(1)?;
        self.children(parent).get(before as usize).copied()
    }
}

impl Node {
    #[must_use]
    pub const fn named(&self) -> bool {
        self.flags & NAMED != 0
    }

    #[must_use]
    pub const fn extra(&self) -> bool {
        self.flags & EXTRA != 0
    }

    #[must_use]
    pub const fn has_error(&self) -> bool {
        self.flags & HAS_ERROR != 0
    }

    #[must_use]
    pub const fn child_count(&self) -> u32 {
        self.count
    }
}

/// Run `driver` on `source` and read back its tree, stopping it at `until`. `rev` is the grammar
/// revision it must have been built from.
///
/// # Errors
/// Why there is no tree: the driver refused, overran, or wrote what does not decode.
pub fn parsed(driver: &str, source: &[u8], rev: &str, until: Instant) -> Result<Tree, String> {
    let limit = until.saturating_duration_since(Instant::now());
    let out = crate::running::piped(driver, source.to_vec(), MOST_OUTPUT, limit)?;
    decoded(&out, source.len(), rev)
}

/// The tree `bytes` hold, for a source of `length` bytes.
///
/// # Errors
/// Where the bytes stop making sense.
pub fn decoded(bytes: &[u8], length: usize, rev: &str) -> Result<Tree, String> {
    let mut at = Reader { bytes, at: 0 };
    if at.take(4)? != b"CSTR" || at.u32()? != VERSION {
        return Err("the grammar's driver wrote something other than a tree".to_owned());
    }
    let built = at.name()?;
    if built != rev {
        return Err(format!("was built from {built}, not the pinned {rev}"));
    }
    let symbols = at.u32()? as usize;
    let mut tree = Tree {
        kinds: at.names(symbols + 1)?,
        ..Tree::default()
    };
    let fields = at.u32()? as usize;
    tree.fields = at.names(fields)?;
    let count = at.u32()? as usize;
    if count == 0 || count > MOST_NODES || count * RECORD != at.left() {
        return Err(format!(
            "the tree's {count} nodes do not fit what was written"
        ));
    }
    tree.nodes.reserve_exact(count);
    tree.kids.reserve_exact(count - 1);
    let mut open: Vec<(u32, u32)> = Vec::new();
    for index in 0..count {
        let mut node = at.node()?;
        let fits = usize::from(node.kind) <= symbols
            && usize::from(node.field) <= fields
            && node.start.byte <= node.end.byte
            && node.end.byte as usize <= length;
        if !fits {
            return Err(format!(
                "node {index} points outside its tables or its source"
            ));
        }
        let index = u32::try_from(index).map_err(|_| "too many nodes".to_owned())?;
        if let Some((parent, filled)) = open.last_mut() {
            let slot = tree.nodes.get(*parent as usize).map_or(0, |p| p.kids) + *filled;
            *tree
                .kids
                .get_mut(slot as usize)
                .ok_or("a child outside its parent")? = index;
            node.parent = Some(*parent);
            node.slot = *filled;
            *filled += 1;
        } else if index != 0 {
            return Err(format!("node {index} follows a finished tree"));
        }
        node.kids = u32::try_from(tree.kids.len()).map_err(|_| "too many children")?;
        if tree.kids.len() + node.count as usize > count - 1 {
            return Err(format!(
                "node {index} claims more children than there are nodes"
            ));
        }
        tree.kids
            .resize(tree.kids.len() + node.count as usize, u32::MAX);
        let kids = node.count;
        tree.nodes.push(node);
        if kids > 0 {
            open.push((index, 0));
        }
        while let Some((parent, filled)) = open.last() {
            if tree.nodes.get(*parent as usize).map(|p| p.count) != Some(*filled) {
                break;
            }
            open.pop();
        }
    }
    if !open.is_empty() || tree.kids.len() != count - 1 {
        return Err("the tree ended before its nodes did".to_owned());
    }
    Ok(tree)
}

/// A cursor over the driver's bytes.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn left(&self) -> usize {
        self.bytes.len().saturating_sub(self.at)
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let taken = self
            .bytes
            .get(self.at..self.at.saturating_add(n))
            .ok_or("the driver's tree ends early")?;
        self.at += n;
        Ok(taken)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], String> {
        self.take(N)?
            .try_into()
            .map_err(|_| "the driver's tree ends early".to_owned())
    }

    fn u16(&mut self) -> Result<u16, String> {
        self.array().map(u16::from_le_bytes)
    }

    fn u32(&mut self) -> Result<u32, String> {
        self.array().map(u32::from_le_bytes)
    }

    fn name(&mut self) -> Result<String, String> {
        let n = usize::from(self.u16()?);
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
    }

    fn names(&mut self, n: usize) -> Result<Vec<String>, String> {
        if n > self.left() / 2 {
            return Err(format!("{n} names do not fit what was written"));
        }
        (0..n).map(|_| self.name()).collect()
    }

    fn point(&mut self, byte: u32) -> Result<Point, String> {
        Ok(Point {
            byte,
            row: self.u32()?,
            column: self.u32()?,
        })
    }

    fn node(&mut self) -> Result<Node, String> {
        let (kind, field, flags, _) = (self.u16()?, self.u16()?, self.u16()?, self.u16()?);
        let (start, end) = (self.u32()?, self.u32()?);
        Ok(Node {
            kind,
            field,
            flags,
            start: self.point(start)?,
            end: self.point(end)?,
            count: self.u32()?,
            ..Node::default()
        })
    }
}

#[cfg(test)]
#[path = "tree_tests.rs"]
mod tests;
