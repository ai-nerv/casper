use super::*;

/// A node as the driver writes it: kind, field, flags, bytes, rows and columns, child count.
type Written = (u16, u16, u16, [u32; 6], u32);

/// The driver's bytes for `nodes` under symbols `a b c` and fields `left right`.
fn written(rev: &str, nodes: &[Written]) -> Vec<u8> {
    let mut out = b"CSTR".to_vec();
    out.extend(VERSION.to_le_bytes());
    let name = |out: &mut Vec<u8>, text: &str| {
        out.extend(u16::try_from(text.len()).expect("short").to_le_bytes());
        out.extend(text.as_bytes());
    };
    name(&mut out, rev);
    out.extend(3_u32.to_le_bytes());
    for symbol in ["a", "b", "c", "ERROR"] {
        name(&mut out, symbol);
    }
    out.extend(2_u32.to_le_bytes());
    for field in ["left", "right"] {
        name(&mut out, field);
    }
    out.extend(u32::try_from(nodes.len()).expect("few").to_le_bytes());
    for (kind, field, flags, [start, end, srow, scol, erow, ecol], count) in nodes {
        for half in [kind, field, flags, &0] {
            out.extend(half.to_le_bytes());
        }
        for word in [start, end, srow, scol, erow, ecol, count] {
            out.extend(word.to_le_bytes());
        }
    }
    out
}

/// `a(b c)` over `xy`, `c` an extra under `right`, and the root holding an error.
fn small() -> Vec<Written> {
    vec![
        (0, 0, NAMED | HAS_ERROR, [0, 2, 0, 0, 0, 2], 2),
        (1, 1, NAMED, [0, 1, 0, 0, 0, 1], 0),
        (3, 2, EXTRA, [1, 2, 0, 1, 0, 2], 0),
    ]
}

#[test]
fn a_tree_reads_back_with_its_families_fields_and_flags() {
    let tree = decoded(&written("r1", &small()), 2, "r1").expect("a tree");
    let root = tree.node(0).expect("root");
    assert_eq!(tree.kind(root), "a");
    assert_eq!(tree.children(root), &[1, 2]);
    assert!(root.has_error() && root.named() && !root.extra());
    let (b, c) = (tree.node(1).expect("b"), tree.node(2).expect("c"));
    assert_eq!(tree.kind(b), "b");
    assert_eq!(tree.kind(c), "ERROR", "the error symbol is the last name");
    assert!(c.extra() && !c.named());
    assert_eq!(tree.field(root, "left").collect::<Vec<_>>(), [1]);
    assert_eq!(tree.field(root, "right").collect::<Vec<_>>(), [2]);
    assert_eq!(tree.field(root, "nowhere").count(), 0);
    assert_eq!((tree.parent(c), tree.parent(root)), (Some(0), None));
    assert_eq!(
        (tree.prev_sibling(c), tree.prev_sibling(b)),
        (Some(1), None)
    );
    assert_eq!(
        c.start,
        Point {
            row: 0,
            column: 1,
            byte: 1
        }
    );
}

#[test]
fn a_driver_built_from_another_revision_is_named_as_stale() {
    let why = decoded(&written("old", &small()), 2, "new").expect_err("stale");
    assert_eq!(why, "was built from old, not the pinned new");
}

#[test]
fn bytes_that_do_not_hold_a_tree_are_an_error_and_never_a_panic() {
    let whole = written("r", &small());
    for cut in 0..whole.len() {
        assert!(decoded(&whole[..cut], 2, "r").is_err(), "cut at {cut}");
    }
    let mut longer = whole.clone();
    longer.push(0);
    assert!(decoded(&longer, 2, "r").is_err(), "a byte past the tree");
    assert!(decoded(b"NOPE", 2, "r").is_err());
    let mut wrong = small();
    wrong[1].0 = 9;
    assert!(
        decoded(&written("r", &wrong), 2, "r").is_err(),
        "a kind past the table"
    );
    let mut wrong = small();
    wrong[2].1 = 3;
    assert!(
        decoded(&written("r", &wrong), 2, "r").is_err(),
        "a field past the table"
    );
    assert!(decoded(&whole, 1, "r").is_err(), "a node past its source");
    let mut wrong = small();
    wrong[0].4 = 3;
    assert!(
        decoded(&written("r", &wrong), 2, "r").is_err(),
        "more children than nodes"
    );
    let mut wrong = small();
    wrong[0].4 = 1;
    assert!(
        decoded(&written("r", &wrong), 2, "r").is_err(),
        "a node after the root ended"
    );
    let mut wrong = small();
    wrong[1].3[1] = 0;
    wrong[1].3[0] = 1;
    assert!(
        decoded(&written("r", &wrong), 2, "r").is_err(),
        "a node ending before it starts"
    );
}

#[test]
fn the_rust_driver_parses_a_source_into_the_tree_tree_sitter_would() {
    let shelf = crate::testing::grammars();
    let driver = shelf.find("rust").expect("rust");
    let rev = &crate::grammars::pinned("rust").expect("pinned").rev;
    let source = b"struct A { b: u8 }\nfn c() {}\n";
    let until = Instant::now() + crate::running::READING;
    let tree = parsed(&driver.display().to_string(), source, rev, until).expect("a tree");
    let root = tree.node(0).expect("root");
    assert_eq!(tree.kind(root), "source_file");
    let item = tree.node(tree.children(root)[0]).expect("struct");
    assert_eq!(tree.kind(item), "struct_item");
    let name = tree.field(item, "name").next().and_then(|at| tree.node(at));
    assert_eq!(name.map(|n| (n.start.byte, n.end.byte)), Some((7, 8)));
    let refused = parsed(
        &driver.display().to_string(),
        &vec![b' '; 2 << 20 | 1],
        rev,
        until,
    );
    assert_eq!(
        refused.err().as_deref(),
        Some("the source is larger than 2 MiB (exit 2)")
    );
}
