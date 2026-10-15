use super::*;

/// Every parser name the indexer's Lua can ask for.
const ASKED: &str = "bash c c_sharp clojure containerfile cpp css dart elixir gleam go hcl html java \
                     json kotlin lua make markdown nix php python ruby rust scala sql starlark swift \
                     toml tsx typescript v yaml zig";

fn hex(text: &str) -> bool {
    text.len() == 40 && text.bytes().all(|b| b.is_ascii_hexdigit())
}

#[test]
fn the_lock_pins_every_parser_the_indexer_asks_for_and_nothing_else() {
    let lock = lock();
    assert!(
        lock.about.contains("never edited by hand"),
        "{}",
        lock.about
    );
    assert!(lock.languages.contains(&lock.helix), "{}", lock.languages);
    let pinned: Vec<&str> = lock.grammars.keys().map(String::as_str).collect();
    assert_eq!(pinned, ASKED.split_whitespace().collect::<Vec<_>>());
    for (language, pin) in lock
        .grammars
        .iter()
        .chain([(&RUNTIME.to_owned(), &lock.runtime)])
    {
        assert!(pin.git.starts_with("https://"), "{language}: {}", pin.git);
        assert!(hex(&pin.rev), "{language}: {}", pin.rev);
        assert!(pin.hash.starts_with("sha256-"), "{language}: {}", pin.hash);
        assert!(
            crate::shelf::plain(&pin.grammar.replace('-', "_")),
            "{language}"
        );
    }
    for (language, pin) in &lock.grammars {
        let symbol = pin.symbol.as_deref().unwrap_or_default();
        assert!(symbol.starts_with("tree_sitter_"), "{language}: {symbol}");
        assert!(pin.sources(Path::new("/r")).is_ok(), "{language}");
    }
    assert_eq!(lock.runtime.grammar, RUNTIME);
    assert!(lock.runtime.tag.is_some());
}

#[test]
fn a_subpath_that_leaves_its_checkout_is_refused() {
    let mut pin = lock().grammars["typescript"].clone();
    assert_eq!(
        pin.sources(Path::new("/r")),
        Ok(PathBuf::from("/r/src/typescript/typescript"))
    );
    for sub in ["../x", "/etc", "a/../../b"] {
        pin.subpath = Some(sub.to_owned());
        assert!(pin.sources(Path::new("/r")).is_err(), "{sub}");
    }
}

fn verbed(args: &[&str]) -> Reply {
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
    verb(&args)
}

#[test]
fn the_verb_says_how_it_is_used_and_names_a_language_it_has_no_grammar_for() {
    for args in [
        &["grammar"][..],
        &["grammar", "--json"],
        &["grammar", "--cbor"],
    ] {
        let reply = verbed(args);
        assert!(!reply.ok);
        assert_eq!(reply.error.as_deref(), Some(USAGE));
    }
    let reply = verbed(&["grammar", "remove"]);
    assert!(
        reply
            .error
            .unwrap_or_default()
            .starts_with("no such grammar action: remove")
    );
    let reply = verbed(&["grammar", "fetch", "cobol", "rust", "--dir", "/nonexistent"]);
    let said = reply.error.unwrap_or_default();
    assert!(
        said.starts_with("no grammar for cobol; there is one for bash c "),
        "{said}"
    );
}

/// `language` and its state, for each row.
fn states(reply: &Reply) -> Vec<(String, String)> {
    let text = |value: &serde_json::Value| value.as_str().unwrap_or_default().to_owned();
    reply
        .result
        .iter()
        .map(|row| (text(&row["language"]), text(&row["state"])))
        .collect()
}

#[test]
fn each_language_lists_as_built_stale_fetched_or_missing() {
    let root = crate::scratch::Scratch::new("grammar", "list");
    let chosen = chosen(&["go".into(), "json".into(), "rust".into(), "zig".into()]).expect("known");
    let built = crate::testing::grammars();
    std::fs::create_dir_all(root.join("bin")).expect("bin");
    std::fs::copy(built.find("rust").expect("rust"), root.join("bin/rust")).expect("rust");
    std::fs::copy(built.find("go").expect("go"), root.join("bin/json")).expect("json");
    std::fs::create_dir_all(root.join("src/zig/.git")).expect("zig");
    std::fs::write(
        root.join("src/zig/.git/HEAD"),
        &pinned("zig").expect("zig").rev,
    )
    .expect("head");
    let reply = listed(&Shelf::at(vec![root.to_path_buf()]), &root, &chosen);
    let expected = [
        ("go", "missing"),
        ("json", "stale"),
        ("rust", "built"),
        ("zig", "fetched"),
    ];
    let expected: Vec<_> = expected.map(|(a, b)| (a.to_owned(), b.to_owned())).into();
    assert_eq!(states(&reply), expected);
}

#[test]
fn building_what_was_never_fetched_says_to_fetch_it() {
    let root = crate::scratch::Scratch::new("grammar", "unfetched");
    let reply = verbed(&[
        "grammar",
        "build",
        "--dir",
        &root.display().to_string(),
        "rust",
    ]);
    assert!(!reply.ok);
    let said = reply.error.unwrap_or_default();
    assert!(said.contains("run `casper grammar fetch`"), "{said}");
}
