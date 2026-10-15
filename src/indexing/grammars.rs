//! The grammar behind each parser name the indexer asks for: the names maki's own registry answers
//! to, less `javascript`, which its indexer reads with the TSX grammar.

use tree_sitter::Language;

/// The grammar called `name`, or `None` for a name no grammar here answers to.
#[must_use]
pub fn named(name: &str) -> Option<Language> {
    let grammar: Language = match name {
        "bash" => tree_sitter_bash::LANGUAGE.into(),
        "c" => tree_sitter_c::LANGUAGE.into(),
        "c_sharp" => tree_sitter_c_sharp::LANGUAGE.into(),
        "clojure" => tree_sitter_clojure_orchard::LANGUAGE.into(),
        "containerfile" | "dockerfile" => tree_sitter_containerfile::LANGUAGE.into(),
        "cpp" => tree_sitter_cpp::LANGUAGE.into(),
        "css" => tree_sitter_css::LANGUAGE.into(),
        "dart" => tree_sitter_dart::LANGUAGE.into(),
        "elixir" => tree_sitter_elixir::LANGUAGE.into(),
        "gleam" => tree_sitter_gleam::LANGUAGE.into(),
        "go" => tree_sitter_go::LANGUAGE.into(),
        "hcl" => tree_sitter_hcl::LANGUAGE.into(),
        "html" => tree_sitter_html::LANGUAGE.into(),
        "java" => tree_sitter_java::LANGUAGE.into(),
        "json" => tree_sitter_json::LANGUAGE.into(),
        "kotlin" => tree_sitter_kotlin_ng::LANGUAGE.into(),
        "lua" => tree_sitter_lua::LANGUAGE.into(),
        "make" => tree_sitter_make::LANGUAGE.into(),
        "markdown" => tree_sitter_md::LANGUAGE.into(),
        "nix" => tree_sitter_nix::LANGUAGE.into(),
        "php" => tree_sitter_php::LANGUAGE_PHP.into(),
        "python" => tree_sitter_python::LANGUAGE.into(),
        "ruby" => tree_sitter_ruby::LANGUAGE.into(),
        "rust" => tree_sitter_rust::LANGUAGE.into(),
        "scala" => tree_sitter_scala::LANGUAGE.into(),
        "sql" => tree_sitter_sequel::LANGUAGE.into(),
        "starlark" => tree_sitter_starlark::LANGUAGE.into(),
        "swift" => tree_sitter_swift::LANGUAGE.into(),
        "toml" => tree_sitter_toml_ng::LANGUAGE.into(),
        "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
        "typescript" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "v" => tree_sitter_vlang::LANGUAGE.into(),
        "yaml" => tree_sitter_yaml::LANGUAGE.into(),
        "zig" => tree_sitter_zig::LANGUAGE.into(),
        _ => return None,
    };
    Some(grammar)
}

#[cfg(test)]
mod tests {
    use super::named;

    /// Every parser name the indexer's Lua can ask for, with a line each grammar reads cleanly.
    const ASKED: &[(&str, &str)] = &[
        ("bash", "f() { echo hi; }\n"),
        ("c", "int main(void) { return 0; }\n"),
        ("c_sharp", "class A { }\n"),
        (
            "clojure",
            "(ns a.b (:require [c.d :as e]))\n(defn f [x] (inc x))\n",
        ),
        ("containerfile", "FROM alpine\n"),
        ("cpp", "namespace a { int f(); }\n"),
        ("css", "a { color: red; }\n"),
        ("dart", "void main() {}\n"),
        ("elixir", "defmodule A do\nend\n"),
        ("gleam", "pub fn main() { 1 }\n"),
        ("go", "package main\n"),
        ("hcl", "a = 1\n"),
        ("html", "<p>hi</p>\n"),
        ("java", "class A {}\n"),
        ("json", "{\"a\": 1}\n"),
        ("kotlin", "fun main() {}\n"),
        ("lua", "local a = 1\n"),
        ("make", "all:\n\techo hi\n"),
        ("markdown", "# Title\n"),
        ("nix", "{ a = 1; }\n"),
        ("php", "<?php function f() {}\n"),
        ("python", "def f():\n    pass\n"),
        ("ruby", "class A\nend\n"),
        ("rust", "fn main() {}\n"),
        ("scala", "object A\n"),
        ("sql", "SELECT 1;\n"),
        ("starlark", "load(\"//a:b.bzl\", \"c\")\n"),
        ("swift", "func f() {}\n"),
        ("toml", "a = 1\n"),
        ("tsx", "export const x = <T a={1} />;\n"),
        ("typescript", "const x = <T>y;\n"),
        ("v", "module main\n"),
        ("yaml", "a: 1\n"),
        ("zig", "const a = 1;\n"),
    ];

    #[test]
    fn every_grammar_the_indexer_asks_for_loads_and_parses() {
        for (name, source) in ASKED {
            let grammar = named(name).unwrap_or_else(|| panic!("{name} has no grammar"));
            let mut parser = tree_sitter::Parser::new();
            parser
                .set_language(&grammar)
                .unwrap_or_else(|why| panic!("{name}: {why}"));
            let tree = parser.parse(source, None).expect("a tree");
            assert!(!tree.root_node().has_error(), "{name}: {source:?}");
        }
        assert!(named("javascript").is_none());
        assert!(named("cobol").is_none());
    }
}
