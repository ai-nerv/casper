use super::*;

fn rows(rows: &[&str]) -> String {
    format!("{}\n", rows.join("\n"))
}

#[test]
fn a_walk_is_read_back_as_what_it_found() {
    let walked = sifted(
        &rows(&["d\t4096\tsrc", "f\t1200\tsrc/a.rs", "f\t97\tREADME.md"]),
        false,
    );
    assert_eq!(walked.entries.len(), 3);
    assert_eq!(walked.entries[0].path, "src");
    assert!(walked.entries[0].dir);
    assert_eq!(walked.entries[1].path, "src/a.rs");
    assert_eq!(walked.entries[1].bytes, 1200);
    assert_eq!(walked.entries[1].depth(), 2);
    assert_eq!(walked.entries[1].name(), "a.rs");
}

#[test]
fn a_place_reads_before_its_contents() {
    let walked = sifted(
        &rows(&[
            "f\t10\tREADME.md",
            "d\t4096\tsrc",
            "f\t20\tsrc/b.rs",
            "d\t4096\tsrc/lua",
            "f\t30\tsrc/lua/c.rs",
        ]),
        false,
    );
    let order: Vec<&str> = walked
        .entries
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    assert_eq!(
        order,
        ["src", "src/lua", "src/lua/c.rs", "src/b.rs", "README.md"],
        "directories before files at each level, and a parent before its children"
    );
}

#[test]
fn a_buried_directory_is_named_rather_than_walked() {
    let walked = sifted(&rows(&["d\t4096\tnode_modules", "f\t10\ta.rs"]), false);
    assert_eq!(walked.pruned, ["node_modules"]);
    assert!(walked.entries.iter().all(|entry| entry.path == "a.rs"));
    assert!(tallied(&walked.entries, &walked.pruned).contains("1 not opened"));
}

#[test]
fn a_dotfile_is_shown_only_when_it_was_asked_for() {
    let listing = rows(&["f\t10\t.env", "d\t4096\t.github", "f\t20\ta.rs"]);
    assert_eq!(sifted(&listing, false).entries.len(), 1);
    assert_eq!(sifted(&listing, true).entries.len(), 3);
}

#[test]
fn what_is_under_a_dotted_directory_is_hidden_too() {
    let walked = sifted(&rows(&["f\t10\t.github/workflows/ci.yml"]), false);
    assert!(
        walked.entries.is_empty(),
        "a hidden directory's contents are not visible for being deeper"
    );
}

#[test]
fn the_walker_prunes_before_it_prints() {
    let argv = argv("src", 3);
    let prune = argv.iter().position(|arg| arg == "-prune").expect("prunes");
    let print = argv
        .iter()
        .position(|arg| arg == "-printf")
        .expect("prints");
    assert!(prune < print, "{argv:?}");
    assert_eq!(argv[1], "-maxdepth", "find reads it as a global option");
    assert_eq!(argv[2], "3");
    assert!(argv.iter().any(|arg| arg == "node_modules"));
}

#[test]
fn a_depth_is_held_inside_what_a_walk_can_stand() {
    assert_eq!(argv("src", 0)[2], "1");
    assert_eq!(argv("src", 9_000)[2], "32");
}

#[test]
fn a_size_is_three_figures_and_a_unit() {
    assert_eq!(sized(0), "0B");
    assert_eq!(sized(999), "999B");
    assert_eq!(sized(1024), "1.0K");
    assert_eq!(sized(12_800), "12K");
    assert_eq!(sized(5 * 1024 * 1024), "5.0M");
}

#[test]
fn a_listing_ends_on_what_it_held() {
    let walked = sifted(&rows(&["d\t4096\tsrc", "f\t1024\ta.rs"]), false);
    let shown = list(".", &walked);
    assert!(shown.said.starts_with("src/"), "{}", shown.said);
    assert!(shown.said.contains("a.rs  1.0K"), "{}", shown.said);
    assert!(
        shown.said.lines().all(|row| row.trim_end() == row),
        "nothing is padded past its last character:\n{}",
        shown.said
    );
    assert!(
        shown.said.ends_with("(1 file, 1 directory)\n"),
        "{}",
        shown.said
    );
    assert_eq!(shown.lines.len(), 3);
}

#[test]
fn one_is_never_told_in_the_plural() {
    let one = sifted(&rows(&["d\t4096\tsrc"]), false);
    assert!(tallied(&one.entries, &[]).contains("0 files, 1 directory"));
    let two = sifted(&rows(&["d\t4096\tsrc", "d\t4096\ttests"]), false);
    assert!(tallied(&two.entries, &[]).contains("2 directories"));
}

#[test]
fn a_tree_is_drawn_in_straight_lines() {
    let walked = sifted(
        &rows(&[
            "d\t4096\tsrc",
            "d\t4096\tsrc/lua",
            "f\t100\tsrc/lua/ask.rs",
            "f\t200\tsrc/b.rs",
            "f\t300\tREADME.md",
        ]),
        false,
    );
    let drawn = tree(".", &walked).said;
    assert!(
        !drawn.contains('└') && !drawn.contains('┌') && !drawn.contains('┘'),
        "a tree turns no corners:\n{drawn}"
    );
    assert!(drawn.contains("├─ src/"), "{drawn}");
    assert!(drawn.contains("│  ├─ ask.rs"), "{drawn}");
}

#[test]
fn the_column_stops_under_a_branch_that_has_no_more() {
    let walked = sifted(
        &rows(&[
            "d\t4096\tone",
            "f\t10\tone/a.rs",
            "d\t4096\ttwo",
            "f\t10\ttwo/b.rs",
        ]),
        false,
    );
    let drawn = tree(".", &walked).said;
    assert!(
        drawn.contains("│  ├─ a.rs"),
        "one has two after it:\n{drawn}"
    );
    assert!(
        drawn.contains("\n   ├─ b.rs"),
        "nothing follows two, so nothing is drawn under it:\n{drawn}"
    );
}

#[test]
fn a_tree_says_how_much_it_did_not_draw() {
    let many: Vec<String> = (0..branching::MOST + 5)
        .map(|nth| format!("f\t10\tf{nth}.rs"))
        .collect();
    let listing: Vec<&str> = many.iter().map(String::as_str).collect();
    let drawn = tree(".", &sifted(&rows(&listing), false)).said;
    assert!(
        drawn.contains("(5 more not shown)"),
        "{}",
        &drawn[drawn.len() - 120..]
    );
}

#[test]
fn a_tree_version_control_ignores_is_counted_rather_than_drawn() {
    let mut walked = sifted(
        &rows(&[
            "d\t4096\tsrc",
            "f\t20\tsrc/b.rs",
            "d\t4096\tout",
            "f\t900\tout/big.o",
            "f\t10\tnotes.log",
            "f\t97\tREADME.md",
        ]),
        false,
    );
    Accounted::of(["src/b.rs", "README.md"].into_iter()).restrict(&mut walked);
    let kept: Vec<&str> = walked
        .entries
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    assert_eq!(kept, ["src", "src/b.rs", "README.md"]);
    assert_eq!(walked.pruned, ["out"], "a shut tree went uncounted");
}

#[test]
fn a_shut_tree_is_counted_once_and_not_at_every_depth() {
    let mut walked = sifted(
        &rows(&[
            "d\t4096\tout",
            "d\t4096\tout/deep",
            "f\t9\tout/deep/a.o",
            "f\t97\tREADME.md",
        ]),
        false,
    );
    Accounted::of(["README.md"].into_iter()).restrict(&mut walked);
    assert_eq!(walked.pruned, ["out"]);
}

#[test]
fn a_file_version_control_has_not_seen_yet_is_still_drawn() {
    let mut walked = sifted(&rows(&["f\t10\tnew.rs", "f\t97\tREADME.md"]), false);
    Accounted::of(["new.rs", "README.md"].into_iter()).restrict(&mut walked);
    assert_eq!(walked.entries.len(), 2);
}
