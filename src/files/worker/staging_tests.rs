use super::*;
use crate::files::worker::snapshot::Resolved;

#[test]
fn source_metadata_policy_refuses_foreign_owners_invalid_groups_and_privileged_modes() {
    let root = crate::scratch::Scratch::new("stage-policy", "metadata");
    let path = root.join("source");
    std::fs::write(&path, "owned source\n").expect("private source");
    let resolved = Resolved::open(path.to_str().expect("source path")).expect("source binding");
    let mut source = resolved.acquire(true).expect("actual source metadata");
    let original = source.metadata;
    source_metadata(&source).expect("ordinary owner-writable source");
    for case in [
        "foreign-owner",
        "invalid-group",
        "readonly",
        "setuid",
        "setgid",
        "sticky",
    ] {
        source.metadata = original;
        match case {
            "foreign-owner" => source.metadata.st_uid = original.st_uid.saturating_add(1),
            "invalid-group" => source.metadata.st_gid = u32::MAX,
            "readonly" => source.metadata.st_mode &= !0o200,
            "setuid" => source.metadata.st_mode |= 0o4000,
            "setgid" => source.metadata.st_mode |= 0o2000,
            "sticky" => source.metadata.st_mode |= 0o1000,
            _ => unreachable!(),
        }
        let error = source_metadata(&source).expect_err("unsupported source metadata");
        assert_eq!(error.kind(), ErrorKind::InvalidData, "{case}");
        assert!(
            error.to_string().contains("owner-writable"),
            "{case}: {error}"
        );
    }
    assert_eq!(
        std::fs::read(&path).expect("source intact"),
        b"owned source\n"
    );
    let after = fs::fstat(&source.file).expect("actual metadata unchanged");
    assert_eq!(
        (after.st_uid, after.st_gid, after.st_mode),
        (original.st_uid, original.st_gid, original.st_mode)
    );
}

#[test]
fn an_actual_unsupported_filesystem_refuses_before_staging_or_publication() {
    let parent = fs::open(
        "/proc/self/fd",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .expect("actual procfs directory");
    assert_eq!(
        fs::fstatfs(&parent).expect("procfs classification").f_type,
        libc::PROC_SUPER_MAGIC
    );
    let error = Stage::prepare(&parent, None, "never published")
        .err()
        .expect("unsupported filesystem");
    assert_eq!(error.kind(), ErrorKind::InvalidData);
    assert!(
        error.to_string().contains("supported local filesystem"),
        "{error}"
    );
}
