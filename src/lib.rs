//! casper — the tooling interface.
//!
//! magi keeps `read`, `write` and `edit` as a floor it can never be without. Everything else is
//! here: the tools, what they say to the model, and what they show the person. `DESIGN.md` is the
//! argument; this is the code.
//!
//! **A tool has two faces.** The model reads text; the person reads a painted view. They are not
//! the same content — a permission question has a view and no result, a `shell` has a result and
//! no view — so they are two fields and either may be absent.

pub mod framing;
pub mod lua;
pub mod paint;
pub mod tools;
pub mod wire;
