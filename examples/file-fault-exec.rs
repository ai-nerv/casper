use seccompiler::{BpfProgram, SeccompAction, SeccompFilter};
use std::os::unix::process::CommandExt as _;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let program = args
        .next()
        .ok_or("expected the selected Casper executable")?;
    let filter = SeccompFilter::new(
        [
            (libc::SYS_fsync, Vec::new()),
            (libc::SYS_fdatasync, Vec::new()),
        ]
        .into_iter()
        .collect(),
        SeccompAction::Allow,
        SeccompAction::Errno(libc::EIO as u32),
        std::env::consts::ARCH.try_into()?,
    )?;
    let program_filter: BpfProgram = filter.try_into()?;
    seccompiler::apply_filter(&program_filter)?;
    Err(std::process::Command::new(program).args(args).exec().into())
}
