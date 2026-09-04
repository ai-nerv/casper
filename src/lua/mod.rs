//! The VM tools are declared in.
//!
//! A declaration is data and a function. The data — a name, what the tool does, its schema, the
//! verb it acts under — comes out into Rust; the function stays, because a function cannot be
//! described as data. See [`engine`].
//!
//! The standard library is trimmed the way the siblings trim theirs, and for a reason that is
//! sharper here than anywhere: casper's job is running programs, so it offers exactly one way to
//! run one. See [`exec`].

pub mod ask;
pub mod convert;
pub mod engine;
pub mod exec;
pub mod fs;
pub mod json;
pub mod paint;
pub mod sandbox;
pub mod stream;
