//! Coordination for native file workers.

pub mod control;
pub mod locking;
pub mod registry;
pub mod worker;

/// XFS's filesystem magic number.
pub(crate) const XFS_SUPER_MAGIC: libc::c_long = 0x5846_5342;
