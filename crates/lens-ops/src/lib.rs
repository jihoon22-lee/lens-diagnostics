//! Shared operations layer: collection, validation, and judgement for
//! every `lens` command, consumed by `lens-cli` (rendering) and
//! `lens-mcp` (tool results). No clap, no printing — each entry point
//! returns a [`Report`] carrying the data, structured [`Finding`]s, and
//! a `complete` flag that together decide the exit code.

pub mod abi;
pub mod build;
pub mod bundle;
pub mod detect;
pub mod disk;
pub mod doctor;
pub mod env;
mod finding;
pub mod json;
pub mod log;
pub mod net;
pub mod sys;
pub mod test;
pub mod trace;

pub use finding::{Finding, Report, Severity};
pub use json::read_json;
