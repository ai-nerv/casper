use super::snapshot::Resolved;
use crate::files::locking::Key;
use rustix::fs::{self, AtFlags};
use serde::{Deserialize, Serialize};
use std::io::{Error, ErrorKind, Result};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Absent,
    Present,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Presence {
    pub revision_contract: u16,
    pub path_key: String,
    pub state: State,
}

impl Resolved {
    pub(super) fn presence(&self) -> Result<Presence> {
        self.validate_parent()?;
        let state = match fs::statat(&self.parent, self.leaf.as_str(), AtFlags::SYMLINK_NOFOLLOW) {
            Ok(_) => State::Present,
            Err(rustix::io::Errno::NOENT) => State::Absent,
            Err(error) => return Err(error.into()),
        };
        self.validate_parent()?;
        Ok(Presence {
            revision_contract: 1,
            path_key: self.binding.key().into(),
            state,
        })
    }
}

impl Presence {
    pub(super) fn validate(&self, key: &Key) -> Result<()> {
        if self.revision_contract != 1 || self.path_key != key.as_str() {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "native presence reply has an unsupported version or binding",
            ));
        }
        Key::from_wire(&self.path_key)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::Scratch;

    #[test]
    fn presence_revalidates_parent_and_never_opens_existing_or_special_leaf_objects() {
        let root = Scratch::new("native-presence", "bindings");
        std::fs::create_dir(root.join("parent")).expect("parent");
        let path = root.join("parent/leaf");
        let resolved = Resolved::bind(path.to_str().expect("path")).expect("binding");
        let proof = resolved.presence().expect("absent");
        assert_eq!(proof.state, State::Absent);
        proof
            .validate(&Key::from_wire(&proof.path_key).expect("key"))
            .expect("supported");
        std::fs::write(&path, "source").expect("regular leaf");
        assert_eq!(resolved.presence().expect("present").state, State::Present);
        std::fs::remove_file(&path).expect("remove");
        std::os::unix::fs::symlink("missing", &path).expect("dangling symlink");
        assert_eq!(
            resolved.presence().expect("lstat symlink").state,
            State::Present
        );
        std::fs::remove_file(&path).expect("remove link");
        fs::mknodat(
            fs::CWD,
            &path,
            fs::FileType::Fifo,
            fs::Mode::from_raw_mode(0o600),
            0,
        )
        .expect("FIFO");
        assert_eq!(
            resolved.presence().expect("nonblocking FIFO stat").state,
            State::Present
        );
        std::fs::rename(root.join("parent"), root.join("moved")).expect("retarget parent");
        std::fs::create_dir(root.join("parent")).expect("replacement parent");
        assert!(
            resolved.presence().is_err(),
            "renamed parent produced absence at wrong binding"
        );
        let mut bad = proof;
        bad.revision_contract = 2;
        assert!(
            bad.validate(&Key::from_wire(&bad.path_key).expect("key"))
                .is_err()
        );
    }
}
