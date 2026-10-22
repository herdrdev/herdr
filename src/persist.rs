//! Session persistence — save/restore workspaces, layouts, and working directories.
//!
//! Stored at `~/.config/herdr/session.json`.
//! Optional pane screen history is stored separately at `session-history.json`.
//! Installed plugins are persisted separately at `plugins.json`.

pub mod activities;
pub mod devices;
pub mod gram;
pub mod gram_files;
mod io;
pub mod machine;
pub mod plugin_registry;
mod restore;
mod snapshot;
pub mod staged_build;
mod writer;

pub use self::io::{clear_history, load, load_history};
pub use self::restore::restore;
pub(crate) use self::restore::restored_worktree_space_membership;
#[cfg(unix)]
pub use self::restore::{handoff_pane_aliases, restore_handoff};
pub use self::snapshot::{
    capture, capture_history, ArchivedAgentMeta, ArchivedAgentSnapshot, DirectionSnapshot,
    LayoutSnapshot, PaneAgentSessionSnapshot, SessionHistorySnapshot, SessionSnapshot, TabSnapshot,
    WorkspaceSnapshot,
};
pub(crate) use self::writer::SessionWriter;
