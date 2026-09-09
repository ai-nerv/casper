//! The shipped examples, run.
//!
//! Each file is loaded over the shipped declarations, exactly as a plugin directory would, and
//! checked for declaring what it says it declares. Whether `jq` is installed is not checked.

use casper::lua::engine::Engine;

/// One example, read from the tree at run time, the way a plugin directory reads one.
fn example(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("examples/plugin")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|why| panic!("{}: {why}", path.display()))
}

/// The shipped declarations, with an example layered over them.
fn layered(name: &str) -> Engine {
    let mut engine = Engine::new();
    engine
        .run(include_str!("../config/tools.lua"), "tools.lua")
        .expect("the shipped declarations load");
    engine
        .run(&example(name), name)
        .unwrap_or_else(|why| panic!("{name}: {why}"));
    engine
}

#[test]
fn an_example_adds_tools_without_taking_the_shipped_ones_with_it() {
    // The registry replaces by name, so a file declaring `cat` would replace the shipped one.
    let engine = layered("jq.lua");
    let cards = engine.tools();
    let named: Vec<&str> = cards.iter().map(|card| card.name.as_str()).collect();

    assert!(named.contains(&"jq"), "{named:?}");
    assert!(named.contains(&"json-keys"), "{named:?}");
    assert!(
        named.contains(&"cat"),
        "and the shipped tools are untouched, which is the whole point: {named:?}"
    );
}

#[test]
fn every_example_tool_says_what_it_is_and_what_it_needs() {
    let engine = layered("jq.lua");
    for card in engine.tools() {
        if card.name != "jq" && card.name != "json-keys" {
            continue;
        }
        assert!(
            !card.description.is_empty(),
            "{} says nothing about itself",
            card.name
        );
        assert!(
            card.parameters.get("properties").is_some(),
            "{} takes arguments and does not say which",
            card.name
        );
        assert_eq!(
            card.needs.as_deref(),
            Some("read"),
            "{} does not say which permission it acts under",
            card.name
        );
    }
}
