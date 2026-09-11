//! casper — the tooling interface: the tools, what they say to the model, and what they show the
//! person. A tool's two faces are separate fields and either may be absent — a permission question
//! has a view and no result, a `shell` has a result and no view.

pub mod acknowledged;
pub mod jail;
pub mod lua;
pub mod noted;
pub mod paint;
pub mod plugins;
pub mod pty;
pub mod scratch;
pub mod setup;
pub mod surface;
pub mod tied;
pub mod tools;
pub mod wire;
