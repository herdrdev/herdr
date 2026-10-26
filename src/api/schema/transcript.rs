//! Wire shapes of `agent.transcript`: an omp agent's conversation read from its
//! session file and normalised into chat entries.

use serde::{Deserialize, Serialize};

use crate::cli::agent::bindings;
use crate::cli::projection::{cli_params, Field};

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct AgentTranscriptParams => AgentTranscriptProjection {
        pub target: String => Field::new(String::new()).input(bindings::target()),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub before: Option<String> => Field::new(None).input(bindings::transcript_before()),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub after: Option<String> => Field::new(None).input(bindings::transcript_after()),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub limit: Option<u32> => Field::new(None).input(bindings::transcript_limit()),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub subagent: Option<String> => Field::new(None).input(bindings::transcript_subagent()),
    }
}

/// Who wrote a `user` entry: the owner at the terminal, or a guest whose prompt
/// arrived through HerdrUp with the `<name> (via HerdrUp): ` prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptAuthor {
    Owner,
    Guest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptStepStatus {
    Running,
    Ok,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptNoteSource {
    Gitmoot,
    AsyncResult,
    Advisor,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptTurnStatus {
    Done,
    Error,
    Aborted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TranscriptModel {
    pub provider: String,
    pub id: String,
    /// The effort omp runs the model at: minimal, low, medium, high, xhigh or
    /// max. Absent when thinking is off or the level is unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<String>,
    /// omp picks the level per turn (`/thinking auto`).
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub thinking_auto: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptSubagentStatus {
    Running,
    Completed,
    Failed,
    Aborted,
    Unknown,
}

/// A subagent the agent spawned with omp's `task` tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TranscriptSubagent {
    /// The omp agent id, also its job id.
    pub id: String,
    /// The agent type, e.g. `reviewer`; may be empty.
    pub agent: String,
    /// The spawn's description, else its name.
    pub label: Option<String>,
    /// The assignment, at most 300 characters; owner only.
    pub task: Option<String>,
    pub status: TranscriptSubagentStatus,
    pub started_ms: u64,
    pub finished_ms: Option<u64>,
    pub duration_ms: Option<u64>,
    /// The newest tool intent in the subagent's own session file.
    pub last_intent: Option<String>,
    pub last_ms: Option<u64>,
    /// `agent.transcript` with `subagent` set to `id` pages its conversation.
    pub has_transcript: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TranscriptRecap {
    pub turn_id: String,
    pub text: String,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptTaskStatus {
    Pending,
    InProgress,
    Completed,
    Blocked,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TranscriptPlanItem {
    pub text: String,
    pub done: bool,
    #[serde(default)]
    pub status: TranscriptTaskStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocker: Option<String>,
}

/// One question of an omp `ask` tool call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AskQuestion {
    pub question: String,
    pub options: Vec<String>,
    pub multi: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommended: Option<u32>,
}

/// The newest `ask` still waiting for an answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PendingAsk {
    /// The ask's tool call id.
    pub id: String,
    pub questions: Vec<AskQuestion>,
    /// One single-select question with at least one option, and a caller that
    /// may answer it (guests never may).
    pub answerable: bool,
}

/// The tool step the agent is running now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TranscriptWorking {
    pub intent: String,
    pub tool: String,
    pub since_ms: u64,
}

/// One chat entry. Every entry has a stable, unique `id` and a `ts_ms`
/// (milliseconds since the Unix epoch); clients ignore kinds they do not know.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TranscriptEntry {
    User {
        id: String,
        ts_ms: u64,
        text: String,
        author: TranscriptAuthor,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        author_name: Option<String>,
    },
    /// Assistant text parts joined with a blank line; `final` marks an answer
    /// that ended the model's turn (stop reason `stop`/`length`).
    Reply {
        id: String,
        ts_ms: u64,
        text: String,
        #[serde(rename = "final")]
        is_final: bool,
    },
    Thinking {
        id: String,
        ts_ms: u64,
        text: String,
    },
    Step {
        id: String,
        ts_ms: u64,
        tool: String,
        intent: String,
        status: TranscriptStepStatus,
        /// The command or path, at most 300 characters; owner only.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// Fleet traffic delivered into the session (gitmoot inbox, async job
    /// results, advisors); owner only.
    Note {
        id: String,
        ts_ms: u64,
        source: TranscriptNoteSource,
        title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<String>,
        body: String,
    },
    /// The turn's todo list; one per turn, re-sent with the same id when it changes.
    Plan {
        id: String,
        ts_ms: u64,
        items: Vec<TranscriptPlanItem>,
    },
    /// An `ask` tool call; `id` is the tool call id, `answer` the selection once made.
    Ask {
        id: String,
        ts_ms: u64,
        questions: Vec<AskQuestion>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        answer: Option<Vec<String>>,
    },
    TurnEnd {
        id: String,
        ts_ms: u64,
        status: TranscriptTurnStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// A compaction: the agent's earlier context was replaced by a summary.
    Summary {
        id: String,
        ts_ms: u64,
        text: String,
    },
}
