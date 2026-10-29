use std::collections::BTreeMap;

const PROGRAMS: &[&str] = &[
    "sh", "bash", "cat", "mktemp", "rm", "mv", "sleep", "stty", "false",
];
const ENVIRONMENT: &str = "CASPER_CONTAINMENT_PROGRAMS";

fn capture() -> BTreeMap<String, String> {
    PROGRAMS
        .iter()
        .map(|name| {
            let found = super::installed(name)
                .unwrap_or_else(|| panic!("required fixture utility unavailable: {name}"));
            let path = found
                .parent()
                .expect("utility directory")
                .canonicalize()
                .expect("canonical fixture utility directory")
                .join(found.file_name().expect("utility filename"));
            let path = path.to_str().expect("UTF-8 fixture utility");
            assert!(
                path.bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"/._+-".contains(&byte)),
                "fixture utility path cannot be embedded in shell or Lua"
            );
            ((*name).to_owned(), path.to_owned())
        })
        .collect()
}

pub(super) fn environment() -> (&'static str, String) {
    (
        ENVIRONMENT,
        serde_json::to_string(&capture()).expect("fixture utility map"),
    )
}

pub(super) fn native(source: &str) -> String {
    let programs: BTreeMap<String, String> =
        if std::env::var_os("CASPER_CONTAINMENT_CHILD").is_some() {
            let encoded = std::env::var(ENVIRONMENT).expect("captured fixture utilities");
            assert!(encoded.len() <= 64 * 1024, "fixture utility map bound");
            serde_json::from_str(&encoded).expect("fixture utility map")
        } else {
            capture()
        };
    let mut pieces = source.split("/bin/");
    let mut script = pieces.next().unwrap_or_default().to_owned();
    for piece in pieces {
        let end = piece
            .find(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '-')
            .unwrap_or(piece.len());
        let (name, rest) = piece.split_at(end);
        let boundary = script
            .chars()
            .last()
            .is_none_or(|c| !c.is_ascii_alphanumeric() && !"/._+-".contains(c));
        if let Some(path) = programs.get(name).filter(|_| boundary) {
            script.push_str(path);
            script.push_str(rest);
        } else {
            script.push_str("/bin/");
            script.push_str(piece);
        }
    }
    script
}

#[test]
fn fixture_paths_expand_complete_tokens_without_expanding_resolved_paths_again() {
    let found = super::installed("sh").expect("host shell");
    let shell = found
        .parent()
        .expect("shell directory")
        .canonicalize()
        .expect("canonical directory")
        .join(found.file_name().expect("shell filename"));
    let shell = shell.to_str().expect("UTF-8 shell");
    assert_eq!(native("/bin/sh"), shell);
    assert_eq!(native(shell), shell);
    assert_eq!(native("/bin/shutdown"), "/bin/shutdown");
    assert_eq!(native("/synthetic/bin/sh"), "/synthetic/bin/sh");
    assert_eq!(native("#!/bin/sh"), format!("#!{shell}"));
}
