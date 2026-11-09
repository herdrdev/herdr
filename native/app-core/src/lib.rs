//! Toolkit-free core of the native Shepherd app. Nothing here draws anything.

pub use shepherd_core;

pub mod actions;
pub mod api;
pub mod bus_message;
pub mod config_data;
pub mod data;
pub mod endpoint;
pub mod installed;
pub mod pattern_edit;
pub mod pattern_graph;
pub mod snapshot;
pub mod watch;
pub mod wire;

/// Tests that change process-wide environment variables (`XDG_CONFIG_HOME`) take this lock.
#[cfg(test)]
pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
