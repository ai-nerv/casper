use super::*;

const HARNESS: &str = include_str!("../../tests/code/test_helpers.lua");
const HELPERS: &str = include_str!("../../tests/code/helpers.lua");

/// Run one of maki's specs through the embedded indexer, and hold it to the number of cases it has.
fn spec(name: &str, source: &str, cases: usize) {
    let module = format!("tests/{name}");
    let modules = [
        ("maki/test_helpers", HARNESS),
        ("tests/helpers", HELPERS),
        (module.as_str(), source),
    ];
    let mut indexer = Indexer::with(&modules, deadline(), crate::testing::grammars())
        .unwrap_or_else(|why| panic!("the indexer loads: {why}"));
    let chunk = format!(
        "require('tests.{}')\n__code_out = tostring(require('maki.test_helpers').report())",
        name.replace('/', ".")
    );
    indexer
        .run(&chunk, name)
        .unwrap_or_else(|why| panic!("{why}"));
    assert_eq!(indexer.text("__code_out"), Some(cases.to_string()));
}

macro_rules! specs {
    ($($lang:ident: $cases:literal),* $(,)?) => {$(
        #[test]
        fn $lang() {
            let source = include_str!(concat!("../../tests/code/lang/", stringify!($lang), ".lua"));
            spec(concat!("lang/", stringify!($lang)), source, $cases);
        }
    )*};
}

specs! {
    bash: 1, bazel: 45, c: 7, c_sharp: 1, clojure: 14, containerfile: 3, cpp: 1, css: 5, dart: 4,
    elixir: 2, gleam: 7, go: 2, hcl: 3, html: 3, java: 1, json: 6, kotlin: 3, lua_lang: 1,
    make: 6, markdown: 3, nix: 23, php: 1, python: 2, ruby: 2, rust: 7, scala: 1, sql: 24,
    swift: 2, toml: 11, tsx: 5, typescript: 2, v: 8, yaml: 25, zig: 8,
}

#[test]
fn indexer_core() {
    spec(
        "indexer_core",
        include_str!("../../tests/code/indexer_core.lua"),
        2,
    );
}

fn deadline() -> Instant {
    Instant::now() + crate::running::READING
}

/// An indexer parsing with the suite's grammars.
fn indexer() -> Indexer {
    Indexer::with(&[], deadline(), crate::testing::grammars()).expect("the indexer loads")
}

/// What one expression evaluates to inside a fresh indexer.
fn probe(expression: &str) -> String {
    let mut indexer = indexer();
    indexer
        .run(&format!("__code_out = tostring({expression})"), "probe.lua")
        .expect("the probe runs");
    indexer.text("__code_out").unwrap_or_default()
}

#[test]
fn the_indexer_reaches_nothing_a_declaration_could_not() {
    for gone in [
        "io",
        "os.execute",
        "os.remove",
        "debug",
        "package",
        "dofile",
        "loadfile",
    ] {
        assert_eq!(probe(gone), "nil", "{gone} is reachable");
    }
    assert_eq!(probe("casper"), "nil");
    let node = "treesitter.get_parser('fn a() {}', 'rust'):parse()[1]:root()";
    assert_eq!(probe(&format!("getmetatable({node})")), "nil");
    let finalized = "select(2, pcall(setmetatable, {}, { __gc = function() end }))";
    assert!(probe(finalized).contains("__gc"));
    let dumped = "select(2, pcall(load, string.dump(load('return 1'))))";
    assert!(probe(dumped).contains("precompiled"));
    assert!(probe("select(2, pcall(require, 'io'))").contains("not found"));
}

#[test]
fn the_tree_sitter_surface_is_the_one_the_indexer_reads() {
    let source = "'struct A { b: u8 }\\nfn c() {}\\n'";
    let root = format!("treesitter.get_parser({source}, 'rust'):parse()[1]:root()");
    assert_eq!(probe(&format!("{root}:type()")), "source_file");
    assert_eq!(probe(&format!("{root}:child_count()")), "2");
    assert_eq!(probe(&format!("#{root}:children()")), "2");
    let item = format!("{root}:child(0)");
    let name = format!("{item}:field('name')[1]");
    assert_eq!(
        probe(&format!("treesitter.get_node_text({name}, {source})")),
        "A"
    );
    assert_eq!(probe(&format!("select(3, {root}:child(1):start())")), "19");
    assert_eq!(probe(&format!("({root}:child(1):end_())")), "1");
    assert_eq!(probe(&format!("{name}:parent():type()")), "struct_item");
    assert_eq!(
        probe(&format!("{root}:child(1):prev_sibling():type()")),
        "struct_item"
    );
    assert_eq!(probe(&format!("{root}:child(5)")), "nil");
    assert_eq!(probe(&format!("{root}:has_error()")), "false");
    assert_eq!(probe(&format!("{item}:extra()")), "false");
    assert_eq!(probe(&format!("#{item}:named_children()")), "2");
    assert_eq!(probe(&format!("#{item}:children()")), "3");
    assert_eq!(
        probe("select(2, treesitter.get_parser('x', 'cobol'))"),
        "no language registered: cobol"
    );
}

#[test]
fn past_its_deadline_the_indexer_stops_rather_than_walks_on() {
    let mut indexer =
        Indexer::with(&[], Instant::now(), crate::testing::grammars()).expect("the indexer loads");
    let why = indexer.index(b"fn a() {}\n", "rust").expect_err("stopped");
    assert!(why.contains("the most one index may take"), "{why}");
}

#[test]
fn a_signature_written_over_several_lines_is_one_tidy_line() {
    let source = b"fn spread(\n    first: u8,\n    rest: &[u8],\n) -> u8 {\n    first\n}\n";
    let skeleton = indexer().index(source, "rust").expect("indexed");
    assert!(
        skeleton.contains("spread(first: u8, rest: &[u8]) -> u8 [1-6]"),
        "{skeleton}"
    );
}

#[test]
fn a_file_type_it_does_not_read_is_named_with_what_to_use_instead() {
    let mut indexer = Indexer::new().expect("the indexer loads");
    for (path, read_as) in [
        ("src/a.rs", "rust"),
        ("x/Makefile", "make"),
        ("BUILD.bazel", "bazel_build"),
        ("app.jsx", "tsx"),
    ] {
        assert_eq!(indexer.language(path).as_deref(), Ok(read_as), "{path}");
    }
    assert_eq!(
        indexer.language("notes.xyz"),
        Err("Unsupported file type: .xyz. Use the read tool instead.".to_owned())
    );
    assert_eq!(
        indexer.language("LICENSE"),
        Err("Unsupported file type: (no extension). Use the read tool instead.".to_owned())
    );
}

#[test]
fn a_skeleton_too_large_for_one_reply_is_cut_at_a_line_and_says_so() {
    let line = format!("{}\n", "x".repeat(99));
    let whole = line.repeat(4000);
    let cut = fitted(whole.clone());
    assert!(serde_json::to_string(&cut).expect("json").len() <= SAID + 64);
    assert!(
        cut.ends_with("more bytes, not shown"),
        "{}",
        &cut[cut.len() - 40..]
    );
    let kept = cut.lines().filter(|l| l.len() == 99).count();
    assert_eq!(cut.lines().next(), Some("x".repeat(99).as_str()));
    assert_eq!(
        cut,
        format!(
            "{}\n… {} more bytes, not shown",
            line.repeat(kept).trim_end(),
            whole.len() - kept * 100
        )
    );
    assert_eq!(fitted("short".to_owned()), "short");
}
