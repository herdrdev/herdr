//! Shared wire protocol and presentation encoding code.

pub mod endpoint;
pub(crate) mod render_ansi;
mod wire;

pub use wire::*;
