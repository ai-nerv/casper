#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use std::fs as disk;
use std::os::unix::fs::symlink;

const HEADER: &str = "---\nname: review\ndescription: Review changes\n---\n";
const SOURCE: &str = "---\nname: review\ndescription: Review changes\n---\nRead only.\n";

fn fixture() -> crate::scratch::Scratch {
    crate::scratch::Scratch::new("workflow", "acquisition")
}

fn put(root: &Path, name: &str, text: impl AsRef<[u8]>) {
    disk::create_dir_all(root.join(name)).unwrap();
    disk::write(root.join(name).join("SKILL.md"), text).unwrap();
}

#[test]
fn missing_and_empty_roots_have_empty_catalogs() {
    let root = fixture();
    for path in [root.to_path_buf(), root.join("missing")] {
        assert_eq!(
            catalog(&path).unwrap(),
            Catalog {
                entries: Vec::new(),
                invalid_names: 0
            }
        );
    }
    assert_eq!(
        load(&root.join("missing"), "review"),
        Err(Failure::NotFound)
    );
}

#[test]
fn sorted_catalog_contains_metadata_not_bodies() {
    let root = fixture();
    for name in ["z", "a", "review"] {
        put(&root, name, SOURCE);
    }
    let result = catalog(&root).unwrap();
    assert_eq!(
        result
            .entries
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        ["a", "review", "z"]
    );
    assert!(
        result
            .entries
            .iter()
            .all(|c| c.metadata.as_deref() == Ok(HEADER))
    );
    assert_eq!(result.invalid_names, 0);
}

#[test]
fn full_source_is_byte_exact_with_independent_digest() {
    let root = fixture();
    put(&root, "review", SOURCE);
    let source = load(&root, "review").unwrap();
    assert_eq!(source.text, SOURCE);
    assert_eq!(
        source.sha256,
        "8707b745571ed633664cb84525f173b2e82138dde583814eb8cbca02f26f60b5"
    );
}

#[test]
fn entry_ceiling_counts_invalid_and_unrelated_entries() {
    let root = fixture();
    for n in 0..ENTRIES_MOST {
        disk::write(root.join(format!("invalid-{n}.txt")), "").unwrap();
    }
    let listing = catalog(&root).unwrap();
    assert_eq!(listing.invalid_names, ENTRIES_MOST);
    assert!(listing.entries.is_empty());
    disk::write(root.join("another.txt"), "").unwrap();
    assert_eq!(catalog(&root), Err(Failure::TooManyEntries));
}

#[test]
fn metadata_and_source_byte_limits_are_inclusive() {
    let root = fixture();
    let header = format!(
        "---\ndescription: {}\n---\n",
        "x".repeat(METADATA_MOST - 22)
    );
    assert_eq!(header.len(), METADATA_MOST);
    let source = format!("{header}{}", "x".repeat(FILE_MOST - header.len()));
    put(&root, "review", &source);
    assert_eq!(
        catalog(&root)
            .unwrap()
            .entries
            .first()
            .unwrap()
            .metadata
            .as_deref(),
        Ok(header.as_str())
    );
    assert_eq!(load(&root, "review").unwrap().text.len(), FILE_MOST);
    put(&root, "review", format!("{source}x"));
    assert_eq!(load(&root, "review"), Err(Failure::Oversized));
    assert_eq!(
        catalog(&root).unwrap().entries.first().unwrap().metadata,
        Err(Failure::Oversized)
    );
}

#[test]
fn no_delimiter_or_oversized_header_has_visible_failure() {
    let root = fixture();
    for text in [
        "plain body".to_owned(),
        format!("---\n{}\n---\nbody", "x".repeat(METADATA_MOST)),
    ] {
        put(&root, "review", text);
        assert_eq!(
            catalog(&root).unwrap().entries.first().unwrap().metadata,
            Err(Failure::Metadata)
        );
    }
}

#[test]
fn metadata_is_lazy_even_with_non_utf8_body_and_split_unicode() {
    let root = fixture();
    let mut bytes = HEADER.as_bytes().to_vec();
    bytes.extend_from_slice(&vec![b'x'; METADATA_MOST - HEADER.len()]);
    bytes.extend_from_slice("🦀".as_bytes());
    put(&root, "review", &bytes);
    assert_eq!(
        catalog(&root)
            .unwrap()
            .entries
            .first()
            .unwrap()
            .metadata
            .as_deref(),
        Ok(HEADER)
    );
    bytes.push(0xff);
    put(&root, "review", bytes);
    assert!(
        catalog(&root)
            .unwrap()
            .entries
            .first()
            .unwrap()
            .metadata
            .is_ok()
    );
    assert_eq!(load(&root, "review"), Err(Failure::Encoding));
}

#[test]
fn invalid_header_encoding_and_nul_are_not_replaced() {
    let root = fixture();
    for bad in [0, 255] {
        let mut bytes = b"---\ndescription: ".to_vec();
        bytes.push(bad);
        bytes.extend_from_slice(b"\n---\nbody");
        put(&root, "review", &bytes);
        assert_eq!(
            catalog(&root).unwrap().entries.first().unwrap().metadata,
            Err(Failure::Encoding)
        );
        assert_eq!(load(&root, "review"), Err(Failure::Encoding));
    }
}

#[test]
fn symlinks_at_root_ancestor_directory_or_leaf_are_refused() {
    let root = fixture();
    let real = root.join("real");
    put(&real, "review", SOURCE);
    symlink(&real, root.join("linked")).unwrap();
    for path in [root.join("linked"), root.join("linked/review")] {
        assert_eq!(catalog(&path), Err(Failure::UnsafePath));
    }
    symlink(real.join("review"), real.join("alias")).unwrap();
    assert_eq!(load(&real, "alias"), Err(Failure::UnsafePath));
    disk::remove_file(real.join("review/SKILL.md")).unwrap();
    disk::write(root.join("outside"), SOURCE).unwrap();
    symlink(root.join("outside"), real.join("review/SKILL.md")).unwrap();
    assert_eq!(load(&real, "review"), Err(Failure::UnsafePath));
}

#[test]
fn hardlinks_fifo_and_directory_sources_are_refused() {
    let root = fixture();
    put(&root, "review", SOURCE);
    let path = root.join("review/SKILL.md");
    disk::hard_link(&path, root.join("alias")).unwrap();
    assert_eq!(load(&root, "review"), Err(Failure::UnsafePath));
    disk::remove_file(&path).unwrap();
    rustix::fs::mknodat(
        fs::CWD,
        &path,
        fs::FileType::Fifo,
        Mode::RUSR | Mode::WUSR,
        0,
    )
    .unwrap();
    assert_eq!(load(&root, "review"), Err(Failure::UnsafePath));
    disk::remove_file(&path).unwrap();
    disk::create_dir(&path).unwrap();
    assert_eq!(load(&root, "review"), Err(Failure::UnsafePath));
}

#[test]
fn absolute_bounded_roots_and_simple_names_are_required() {
    assert_eq!(catalog(Path::new("relative")), Err(Failure::InvalidPath));
    assert_eq!(catalog(Path::new("/tmp/../")), Err(Failure::InvalidPath));
    assert_eq!(
        catalog(Path::new(&format!("/{}", "x".repeat(4096)))),
        Err(Failure::InvalidPath)
    );
    let root = fixture();
    for name in [
        "",
        "..",
        "../outside",
        "a/b",
        "a\\b",
        "Review",
        "réview",
        "-a",
        "a-",
        &"a".repeat(65),
    ] {
        assert_eq!(load(&root, name), Err(Failure::InvalidPath), "{name:?}");
    }
}

#[test]
fn missing_and_non_directory_entries_remain_visible() {
    let root = fixture();
    disk::write(root.join("plain"), "not a directory").unwrap();
    disk::create_dir(root.join("empty")).unwrap();
    let listing = catalog(&root).unwrap();
    assert_eq!(listing.entries.len(), 2);
    assert_eq!(
        listing.entries.first().unwrap().metadata,
        Err(Failure::NotFound)
    );
    assert_eq!(
        listing.entries.last().unwrap().metadata,
        Err(Failure::UnsafePath)
    );
}

#[test]
fn nested_directories_are_not_scanned() {
    let root = fixture();
    put(&root.join("outer"), "review", SOURCE);
    let listing = catalog(&root).unwrap();
    assert_eq!(listing.entries.len(), 1);
    assert_eq!(listing.entries.first().unwrap().name, "outer");
    assert_eq!(
        listing.entries.first().unwrap().metadata,
        Err(Failure::NotFound)
    );
}

#[test]
fn expired_budget_refuses_before_acquisition() {
    let root = fixture();
    let expired = Budget(Instant::now());
    assert!(matches!(Root::open(&root, &expired), Err(Failure::Timeout)));
    let budget = Budget::new();
    let opened = Root::open(&root, &budget).unwrap();
    assert_eq!(
        reading::read(&opened, "review", false, &expired),
        Err(Failure::Timeout)
    );
}

#[test]
fn replacing_root_after_binding_is_detected() {
    let parent = fixture();
    let path = parent.join("root");
    put(&path, "review", SOURCE);
    let budget = Budget::new();
    let root = Root::open(&path, &budget).unwrap();
    disk::rename(&path, parent.join("old")).unwrap();
    put(&path, "review", SOURCE);
    assert_eq!(
        reading::read(&root, "review", false, &budget),
        Err(Failure::Changed)
    );
}

#[test]
fn source_and_directory_replacement_during_read_are_detected() {
    for directory in [false, true] {
        let root = fixture();
        put(&root, "review", SOURCE);
        let budget = Budget::new();
        let opened = Root::open(&root, &budget).unwrap();
        let result = reading::capture(&opened, "review", false, &budget, || {
            if directory {
                disk::rename(root.join("review"), root.join("old")).unwrap();
                put(&root, "review", SOURCE);
            } else {
                disk::rename(root.join("review/SKILL.md"), root.join("old")).unwrap();
                put(&root, "review", SOURCE);
            }
        });
        assert_eq!(result, Err(Failure::Changed));
    }
}

#[test]
fn in_place_mutation_and_deadline_after_open_are_detected() {
    let root = fixture();
    put(&root, "review", SOURCE);
    let budget = Budget::new();
    let opened = Root::open(&root, &budget).unwrap();
    let result = reading::capture(&opened, "review", false, &budget, || {
        put(&root, "review", "changed")
    });
    assert_eq!(result, Err(Failure::Changed));
    let budget = Budget(Instant::now() + Duration::from_millis(20));
    let result = reading::capture(&opened, "review", false, &budget, || {
        std::thread::sleep(Duration::from_millis(30))
    });
    assert_eq!(result, Err(Failure::Timeout));
}
