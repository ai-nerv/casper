//! Bubblewrap's post-namespace syscall filter, passed through an anonymous descriptor.

use std::io::{Seek, Write};
use std::os::fd::OwnedFd;

pub(super) fn descriptor(filter: &seccompiler::BpfProgram) -> std::io::Result<OwnedFd> {
    let fd = rustix::fs::memfd_create("casper-seccomp", rustix::fs::MemfdFlags::CLOEXEC)?;
    let mut file = std::fs::File::from(fd);
    for instruction in filter {
        file.write_all(&instruction.code.to_ne_bytes())?;
        file.write_all(&[instruction.jt, instruction.jf])?;
        file.write_all(&instruction.k.to_ne_bytes())?;
    }
    file.rewind()?;
    Ok(file.into())
}
