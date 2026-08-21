mod engine;
mod orchestration;
mod policy;
mod readers;

pub(crate) use engine::{OwnershipMutation, ResolvedPane, TitleSyncEngine};
pub(crate) use orchestration::{title_for_pane, ReaderPaths};
pub(crate) use policy::{
    agent_identity, clear_ownership, has_agent_identity, read_ownership, rename_decision,
    sanitize_title, session_key, write_ownership, AgentSession, Ownership, PaneInput, ProcessInput,
    RenameDecision,
};
