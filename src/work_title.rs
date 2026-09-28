use std::fs::OpenOptions;
use std::io::{self, Read, Seek, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;
use serde::Deserialize;

use crate::api::schema::PaneReportMetadataParams;

pub(crate) const WORK_TITLE_SOURCE: &str = "herdr:work-title";
/// Metadata source for an agent-reported session rename. It is separate from
/// `WORK_TITLE_SOURCE` because a rename patches only the session name and must
/// not replace the work context the last turn established.
pub(crate) const SESSION_NAME_SOURCE: &str = "herdr:session-name";
/// Session names are authored for humans, so they are kept whole rather than
/// reduced to the derived work title's word budget.
pub(crate) const SESSION_NAME_MAX_CHARS: usize = 80;
pub(crate) const WORK_TITLE_MAX_CHARS: usize = 48;
const WORK_TITLE_MIN_WORDS: usize = 1;
const WORK_TITLE_MAX_WORDS: usize = 7;
const WORK_TITLE_LONG_BRIEF_MIN_WORDS: usize = 40;
const SYSTEM_NOTIFICATION_MARKER: &str = "[SYSTEM NOTIFICATION - NOT USER INPUT]";

#[derive(Debug, Deserialize)]
struct TurnStartHookInput {
    hook_event_name: Option<String>,
    session_id: Option<String>,
    prompt: Option<String>,
    #[serde(default)]
    agent_id: Option<String>,
    #[serde(default)]
    transcript_path: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkTitleProvider {
    Claude,
    Codex,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionNameWriteTarget {
    provider: WorkTitleProvider,
    session_id: String,
    path: PathBuf,
}

impl SessionNameWriteTarget {
    pub(crate) fn new(provider: WorkTitleProvider, session_id: String, path: PathBuf) -> Self {
        Self {
            provider,
            session_id,
            path,
        }
    }

    pub(crate) fn matches_session(
        &self,
        source: &str,
        agent: &str,
        kind: crate::agent_resume::AgentSessionRefKind,
        value: &str,
    ) -> bool {
        kind == crate::agent_resume::AgentSessionRefKind::Id
            && source == self.provider.lifecycle_source()
            && agent == self.provider.agent()
            && value == self.session_id
    }

    pub(crate) fn matches_binding(
        &self,
        provider: WorkTitleProvider,
        session_id: &str,
        path: &Path,
    ) -> bool {
        self.matches_provider_session(provider, session_id) && self.path == path
    }

    pub(crate) fn matches_provider_session(
        &self,
        provider: WorkTitleProvider,
        session_id: &str,
    ) -> bool {
        self.provider == provider && self.session_id == session_id
    }
}

impl WorkTitleProvider {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            _ => None,
        }
    }

    pub(crate) fn agent(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    pub(crate) fn lifecycle_source(self) -> &'static str {
        match self {
            Self::Claude => "herdr:claude",
            Self::Codex => "herdr:codex",
        }
    }
}

pub(crate) fn session_id_from_hook_input(
    provider: WorkTitleProvider,
    input: &str,
) -> Option<String> {
    let input: TurnStartHookInput = serde_json::from_str(input).ok()?;
    input.hook_event_name.as_deref()?;
    if provider == WorkTitleProvider::Claude
        && input
            .agent_id
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
    {
        return None;
    }
    input
        .session_id
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub(crate) fn request_from_turn_start(
    provider: WorkTitleProvider,
    pane_id: Option<&str>,
    input: &str,
    seq: u64,
) -> Option<PaneReportMetadataParams> {
    let pane_id = pane_id.map(str::trim).filter(|value| !value.is_empty())?;
    let input: TurnStartHookInput = serde_json::from_str(input).ok()?;
    if input.hook_event_name.as_deref() != Some("UserPromptSubmit") {
        return None;
    }
    // Claude exposes subagent identity explicitly. A native subagent has no
    // independent Herdr pane/title surface, so never let it rename its parent.
    if provider == WorkTitleProvider::Claude
        && input
            .agent_id
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
    {
        return None;
    }
    let session_id = input
        .session_id
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())?;
    let prompt = sanitize_prompt(input.prompt.as_deref()?);
    if prompt.trim().is_empty() {
        return None;
    }
    let title = calculate_work_title(&prompt);
    // `repo` is intentionally absent: this context is mined from the prompt
    // text, and a repository named in prose is ambient noise rather than
    // evidence of the work. Repositories are declared or observed, never read
    // out of what the human happened to type.
    let mut work_context = crate::work_context::PaneWorkContext {
        ticket_ids: crate::work_context::extract_ticket_ids(&prompt),
        pr_urls: crate::work_context::extract_pr_urls(&prompt),
        preview_urls: crate::work_context::extract_preview_urls(&prompt),
        missive_urls: crate::work_context::extract_missive_urls(&prompt),
        branch: None,
        repo: None,
        work_title: title.clone(),
        // The turn-title path never names the session; only the guarded
        // session-name report may.
        session_name: None,
        role: None,
        active_owner: false,
    };
    work_context.set_latest_work_items();

    Some(PaneReportMetadataParams {
        pane_id: pane_id.to_string(),
        source: WORK_TITLE_SOURCE.to_string(),
        agent: Some(provider.agent().to_string()),
        applies_to_source: Some(provider.lifecycle_source().to_string()),
        agent_session_id: Some(session_id),
        title,
        work_context: Some(work_context),
        work_title_long_brief: is_long_work_brief(&prompt),
        display_agent: None,
        state_labels: std::collections::HashMap::new(),
        tokens: std::collections::HashMap::new(),
        clear_title: false,
        clear_display_agent: false,
        clear_state_labels: false,
        seq: Some(seq),
        ttl_ms: None,
    })
}

pub(crate) fn calculate_work_title(prompt: &str) -> Option<String> {
    let sanitized = sanitize_prompt(prompt);
    let words = meaningful_objective_words(&sanitized)?;
    let mut title_words = Vec::new();
    for word in words.into_iter().take(WORK_TITLE_MAX_WORDS) {
        let word = title_case_word(&word);
        let candidate = if title_words.is_empty() {
            word.clone()
        } else {
            format!("{} {word}", title_words.join(" "))
        };
        if candidate.chars().count() > WORK_TITLE_MAX_CHARS {
            break;
        }
        title_words.push(word);
    }
    if title_words.len() < WORK_TITLE_MIN_WORDS {
        return None;
    }
    Some(title_words.join(" "))
}

fn sanitize_prompt(prompt: &str) -> String {
    let without_injected = strip_machine_injected_text(prompt);
    let without_escapes = ansi_regex().replace_all(&without_injected, " ");
    let without_secrets = secret_regex().replace_all(&without_escapes, " ");
    without_secrets
        .chars()
        .map(|character| {
            if character == '\n' {
                character
            } else if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect()
}

fn strip_machine_injected_text(prompt: &str) -> String {
    let without_task_notifications = task_notification_regex().replace_all(prompt, " ");
    let without_system_reminders =
        system_reminder_regex().replace_all(&without_task_notifications, " ");

    let mut paragraphs = Vec::new();
    let mut current = Vec::new();
    for line in without_system_reminders.lines() {
        if line.trim().is_empty() {
            if !current.is_empty() {
                paragraphs.push(current.join("\n"));
                current.clear();
            }
        } else {
            current.push(line);
        }
    }
    if !current.is_empty() {
        paragraphs.push(current.join("\n"));
    }

    paragraphs
        .into_iter()
        .filter(|paragraph| {
            !paragraph.contains(SYSTEM_NOTIFICATION_MARKER)
                && !paragraph
                    .trim_start()
                    .to_ascii_lowercase()
                    .starts_with("stop hook feedback:")
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn task_notification_regex() -> &'static Regex {
    static TASK_NOTIFICATION: OnceLock<Regex> = OnceLock::new();
    TASK_NOTIFICATION.get_or_init(|| {
        Regex::new(r"(?is)<task-notification\b[^>]*>.*?(?:</task-notification\s*>|\z)")
            .expect("static task-notification regex")
    })
}

fn system_reminder_regex() -> &'static Regex {
    static SYSTEM_REMINDER: OnceLock<Regex> = OnceLock::new();
    SYSTEM_REMINDER.get_or_init(|| {
        Regex::new(r"(?is)<system-reminder\b[^>]*>.*?(?:</system-reminder\s*>|\z)")
            .expect("static system-reminder regex")
    })
}

pub(crate) fn work_title_has_new_reference(
    current: &crate::work_context::PaneWorkContext,
    requested: &crate::work_context::PaneWorkContext,
) -> bool {
    requested
        .ticket_ids
        .iter()
        .any(|ticket| !current.ticket_ids.contains(ticket))
        || requested
            .pr_urls
            .iter()
            .any(|pr_url| !current.pr_urls.contains(pr_url))
}

fn is_long_work_brief(prompt: &str) -> bool {
    prompt
        .split_whitespace()
        .filter(|word| word.chars().any(char::is_alphanumeric))
        .count()
        >= WORK_TITLE_LONG_BRIEF_MIN_WORDS
}

fn meaningful_objective_words(prompt: &str) -> Option<Vec<String>> {
    let mut paragraphs = Vec::new();
    let mut current = Vec::new();
    for line in prompt.lines() {
        let line = line.trim();
        if line.is_empty() {
            if !current.is_empty() {
                paragraphs.push(current.join(" "));
                current.clear();
            }
        } else {
            current.push(line);
        }
    }
    if !current.is_empty() {
        paragraphs.push(current.join(" "));
    }
    paragraphs.into_iter().rev().find_map(|paragraph| {
        let relevant = relevant_objective_clause(&paragraph);
        let words: Vec<String> = objective_words(relevant)
            .into_iter()
            .filter(|word| !is_stopword(word))
            .take(WORK_TITLE_MAX_WORDS)
            .collect();
        (!words.is_empty()).then_some(words)
    })
}

fn relevant_objective_clause(prompt: &str) -> &str {
    let lower = prompt.to_ascii_lowercase();
    let mut offset = 0;
    let mut sentence_start = 0;
    for (index, character) in lower.char_indices() {
        if !matches!(character, '.' | '!' | '?' | ';') {
            continue;
        }
        let sentence = &lower[sentence_start..index];
        let dismissed = [
            " is unrelated",
            " are unrelated",
            " not the task",
            " not the objective",
            " not requested",
        ]
        .iter()
        .any(|marker| sentence.contains(marker));
        let candidate = index + character.len_utf8();
        if dismissed && objective_words(&prompt[candidate..]).len() >= 2 {
            offset = candidate;
        }
        sentence_start = candidate;
    }
    for marker in [" instead ", " actually ", " just ", " only "] {
        if let Some(index) = lower.rfind(marker) {
            let candidate = index + marker.len();
            if objective_words(&prompt[candidate..]).len() >= 2 && candidate > offset {
                offset = candidate;
            }
        }
    }
    &prompt[offset..]
}

fn objective_words(value: &str) -> Vec<String> {
    value
        .split(|character: char| !character.is_alphanumeric() && character != '-')
        .map(|word| word.trim_matches('-').to_lowercase())
        .filter(|word| {
            !word.is_empty()
                && !looks_sensitive(word)
                && !looks_like_identifier(word)
                && word != "redacted"
        })
        .collect()
}

fn title_case_word(word: &str) -> String {
    if word.eq_ignore_ascii_case("pr")
        || word.eq_ignore_ascii_case("ci")
        || word.chars().any(|character| character.is_ascii_digit())
            && word
                .chars()
                .any(|character| character.is_ascii_alphabetic())
    {
        return word.to_ascii_uppercase();
    }
    let mut characters = word.chars();
    let Some(first) = characters.next() else {
        return String::new();
    };
    first.to_uppercase().collect::<String>() + characters.as_str()
}

fn looks_sensitive(word: &str) -> bool {
    let lower = word.to_ascii_lowercase();
    [
        "password",
        "passwd",
        "secret",
        "credential",
        "authorization",
        "api-key",
        "apikey",
        "access-token",
        "customer",
        "username",
    ]
    .contains(&lower.as_str())
}

fn looks_like_identifier(word: &str) -> bool {
    let ascii_alnum = word
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || character == '-');
    let hex = word.len() >= 16 && word.chars().all(|character| character.is_ascii_hexdigit());
    let opaque = word.len() >= 24
        && ascii_alnum
        && word.chars().any(|character| character.is_ascii_digit())
        && word
            .chars()
            .any(|character| character.is_ascii_alphabetic());
    hex || opaque
}

fn is_stopword(word: &str) -> bool {
    matches!(
        word,
        "a" | "about"
            | "an"
            | "and"
            | "answering"
            | "are"
            | "as"
            | "at"
            | "be"
            | "been"
            | "begin"
            | "by"
            | "can"
            | "carry"
            | "claude"
            | "codex"
            | "continue"
            | "could"
            | "do"
            | "does"
            | "for"
            | "from"
            | "get"
            | "go"
            | "ahead"
            | "her"
            | "here"
            | "him"
            | "how"
            | "i"
            | "in"
            | "into"
            | "is"
            | "it"
            | "its"
            | "just"
            | "let"
            | "like"
            | "me"
            | "my"
            | "need"
            | "now"
            | "ok"
            | "of"
            | "on"
            | "onto"
            | "or"
            | "our"
            | "please"
            | "proceed"
            | "research"
            | "scope"
            | "should"
            | "start"
            | "that"
            | "thanks"
            | "the"
            | "their"
            | "them"
            | "these"
            | "this"
            | "those"
            | "to"
            | "under"
            | "us"
            | "using"
            | "very"
            | "want"
            | "was"
            | "we"
            | "what"
            | "when"
            | "which"
            | "why"
            | "with"
            | "working"
            | "would"
            | "you"
            | "your"
    )
}

fn ansi_regex() -> &'static Regex {
    static ANSI: OnceLock<Regex> = OnceLock::new();
    ANSI.get_or_init(|| {
        Regex::new(r"(?:\x1B\][^\x07]*(?:\x07|\x1B\\))|(?:\x1B\[[0-?]*[ -/]*[@-~])")
            .expect("static ANSI regex")
    })
}

fn secret_regex() -> &'static Regex {
    static SECRET: OnceLock<Regex> = OnceLock::new();
    SECRET.get_or_init(|| {
        Regex::new(
            r#"(?ix)
            \b(?:bearer)\s+\S+
            |\b(?:api[\s_-]*key|access[\s_-]*token|password|passwd|secret|authorization)\s*[:=]\s*\S+
            |\b(?:sk|ghp|github_pat|xox[baprs]|sb_secret|akia)[-_][a-z0-9_-]{4,}\b
            |\b[a-z0-9._%+-]+@[a-z0-9.-]+\.[a-z]{2,}\b
            |(?:^|\s)(?:/|~/|[a-z]:\\)\S+
            |@[a-z0-9_-]{2,}
            "#,
        )
        .expect("static secret regex")
    })
}

/// Claude records the name it gave a session as an `ai-title` entry in the
/// session transcript, and appends a fresh entry every time it renames. The
/// last entry for the reported session is therefore the current name.
pub(crate) fn latest_session_name(transcript: &str, session_id: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct TitleRecord {
        #[serde(rename = "type")]
        record_type: Option<String>,
        #[serde(rename = "aiTitle")]
        ai_title: Option<String>,
        #[serde(rename = "customTitle")]
        custom_title: Option<String>,
        #[serde(rename = "sessionId")]
        session_id: Option<String>,
    }

    let mut latest = None;
    for line in transcript.lines() {
        let line = line.trim();
        if line.is_empty() || !line.contains("-title") {
            continue;
        }
        let Ok(record) = serde_json::from_str::<TitleRecord>(line) else {
            continue;
        };
        let name = match record.record_type.as_deref() {
            Some("ai-title") => record.ai_title.as_deref(),
            Some("custom-title") => record.custom_title.as_deref(),
            _ => None,
        };
        let Some(name) = name else {
            continue;
        };
        // Transcript files are per session, but a resumed session can carry
        // entries for the session it forked from. Only the reported session
        // may rename this pane.
        if record
            .session_id
            .as_deref()
            .is_some_and(|value| value.trim() != session_id)
        {
            continue;
        }
        if let Some(title) = normalize_session_name_for_read(name) {
            latest = Some(title);
        }
    }
    latest
}

/// Codex records the name it gave a thread as a `thread_name` entry in the
/// session index, and appends a fresh entry every time it renames. The last
/// entry for the reported thread is therefore the current name.
pub(crate) fn latest_codex_thread_name(index: &str, thread_id: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct ThreadNameRecord {
        id: Option<String>,
        thread_name: Option<String>,
    }

    let mut latest = None;
    for line in index.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(record) = serde_json::from_str::<ThreadNameRecord>(line) else {
            continue;
        };
        // The index is global across every Codex thread on the machine, so the
        // reported thread is the only one that may rename this pane.
        if record.id.as_deref().map(str::trim) != Some(thread_id) {
            continue;
        }
        if let Some(name) = record
            .thread_name
            .as_deref()
            .and_then(normalize_session_name_for_read)
        {
            latest = Some(name);
        }
    }
    latest
}

fn normalize_session_name_for_read(name: &str) -> Option<String> {
    let trimmed = name.trim();
    if trimmed.is_empty()
        || trimmed.chars().any(char::is_control)
        || trimmed.split_whitespace().count() < 2
    {
        return None;
    }
    if trimmed.chars().count() <= SESSION_NAME_MAX_CHARS {
        return Some(trimmed.to_string());
    }

    let mut collapsed = String::new();
    for word in trimmed.split_whitespace() {
        if collapsed.chars().count() + 1 + word.chars().count() > SESSION_NAME_MAX_CHARS {
            break;
        }
        if !collapsed.is_empty() {
            collapsed.push(' ');
        }
        collapsed.push_str(word);
    }
    (collapsed.split_whitespace().count() >= 2).then_some(collapsed)
}

pub(crate) fn normalize_session_name_for_write(name: &str) -> Option<String> {
    let trimmed = name.trim();
    if trimmed.is_empty()
        || trimmed.chars().count() > SESSION_NAME_MAX_CHARS
        || trimmed.chars().any(char::is_control)
        || trimmed.split_whitespace().count() < 2
    {
        return None;
    }
    Some(trimmed.to_string())
}

pub(crate) fn validated_codex_session_index_path(
    raw_path: Option<&str>,
    thread_id: &str,
) -> Option<PathBuf> {
    let raw_path = raw_path?;
    if raw_path.is_empty() || raw_path.len() > 4096 || raw_path.chars().any(char::is_control) {
        return None;
    }
    let path = Path::new(raw_path);
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
        || path.file_name()?.to_str()? != "session_index.jsonl"
    {
        return None;
    }
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if !metadata.file_type().is_file() {
        return None;
    }
    let index = std::fs::read_to_string(path).ok()?;
    codex_index_contains_thread(&index, thread_id).then(|| path.to_path_buf())
}

fn codex_index_contains_thread(index: &str, thread_id: &str) -> bool {
    #[derive(Deserialize)]
    struct ThreadRecord {
        id: Option<String>,
    }

    index.lines().any(|line| {
        serde_json::from_str::<ThreadRecord>(line.trim())
            .ok()
            .and_then(|record| record.id)
            .is_some_and(|id| id.trim() == thread_id)
    })
}

fn write_target_path_matches(target: &SessionNameWriteTarget) -> bool {
    match target.provider {
        WorkTitleProvider::Claude => {
            let expected_file = format!("{}.jsonl", target.session_id);
            let mut components = target.path.components().rev();
            let Some(file_component) = components.next() else {
                return false;
            };
            let file = file_component.as_os_str();
            let Some(project_slug) = components.next() else {
                return false;
            };
            let Some(projects_component) = components.next() else {
                return false;
            };
            let projects = projects_component.as_os_str();
            file == expected_file.as_str()
                && projects == "projects"
                && matches!(project_slug, Component::Normal(value) if !value.is_empty())
        }
        WorkTitleProvider::Codex => {
            target.path.file_name().and_then(|name| name.to_str()) == Some("session_index.jsonl")
        }
    }
}

pub(crate) fn append_session_name(target: &SessionNameWriteTarget, name: &str) -> io::Result<bool> {
    let Some(name) = normalize_session_name_for_write(name) else {
        return Ok(false);
    };
    if !write_target_path_matches(target) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "session name target does not match the bound provider and session",
        ));
    }
    let metadata = std::fs::symlink_metadata(&target.path)?;
    if !metadata.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "session name target must be an existing non-symlink regular file",
        ));
    }
    if target.provider == WorkTitleProvider::Codex {
        let index = std::fs::read_to_string(&target.path)?;
        if !codex_index_contains_thread(&index, &target.session_id) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Codex session index does not contain the bound thread",
            ));
        }
    }
    let record = match target.provider {
        WorkTitleProvider::Claude => serde_json::json!({
            "type": "custom-title",
            "customTitle": name,
            "sessionId": target.session_id,
        }),
        WorkTitleProvider::Codex => serde_json::json!({
            "id": target.session_id,
            "thread_name": name,
            "updated_at": time::OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)
                .map_err(io::Error::other)?,
        }),
    };
    let mut file = OpenOptions::new()
        .read(true)
        .append(true)
        .open(&target.path)?;
    let needs_separator = if file.metadata()?.len() == 0 {
        false
    } else {
        file.seek(io::SeekFrom::End(-1))?;
        let mut last = [0_u8; 1];
        file.read_exact(&mut last)?;
        last[0] != b'\n'
    };
    let mut line = serde_json::to_vec(&record).map_err(io::Error::other)?;
    line.push(b'\n');
    if needs_separator {
        line.insert(0, b'\n');
    }
    file.write_all(&line)?;
    Ok(true)
}

/// Build the rename report for a hook payload and the name source it points at
/// -- Claude's session transcript, or Codex's session index. Unlike the
/// turn-title path this accepts every hook event: both agents name and rename a
/// session outside prompt submission, so binding renames to `UserPromptSubmit`
/// is exactly what made the label go stale.
pub(crate) fn request_from_session_name(
    provider: WorkTitleProvider,
    pane_id: Option<&str>,
    input: &str,
    name_source: &str,
    seq: u64,
) -> Option<PaneReportMetadataParams> {
    let pane_id = pane_id.map(str::trim).filter(|value| !value.is_empty())?;
    // A native Claude subagent shares its parent pane and must never rename or
    // bind a write target for it. A Codex subagent has its own thread id.
    let session_id = session_id_from_hook_input(provider, input)?;
    let session_name = match provider {
        WorkTitleProvider::Claude => latest_session_name(name_source, &session_id),
        WorkTitleProvider::Codex => latest_codex_thread_name(name_source, &session_id),
    }?;

    Some(PaneReportMetadataParams {
        pane_id: pane_id.to_string(),
        source: SESSION_NAME_SOURCE.to_string(),
        agent: Some(provider.agent().to_string()),
        applies_to_source: Some(provider.lifecycle_source().to_string()),
        agent_session_id: Some(session_id),
        title: None,
        work_context: Some(crate::work_context::PaneWorkContext {
            session_name: Some(session_name),
            ..Default::default()
        }),
        work_title_long_brief: false,
        display_agent: None,
        state_labels: std::collections::HashMap::new(),
        tokens: std::collections::HashMap::new(),
        clear_title: false,
        clear_display_agent: false,
        clear_state_labels: false,
        seq: Some(seq),
        ttl_ms: None,
    })
}

/// Read the transcript a hook payload points at, if any.
pub(crate) fn transcript_path_from_hook_input(input: &str) -> Option<String> {
    let input: TurnStartHookInput = serde_json::from_str(input).ok()?;
    input
        .transcript_path
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_dir(label: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let path = std::env::temp_dir().join(format!(
            "herdr-work-title-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn calculates_concrete_titles_for_representative_turns() {
        for (prompt, expected) in [
            (
                "Please audit the local CI evidence",
                "Audit Local CI Evidence",
            ),
            (
                "Merge PR 672 skill metadata safely",
                "Merge PR 672 Skill Metadata Safely",
            ),
            (
                "Measure fleet token savings over twenty turns",
                "Measure Fleet Token Savings Over Twenty Turns",
            ),
            (
                "Fix the billing retry regression",
                "Fix Billing Retry Regression",
            ),
            (
                "Review auth migration safety",
                "Review Auth Migration Safety",
            ),
        ] {
            assert_eq!(calculate_work_title(prompt).as_deref(), Some(expected));
        }
    }

    #[test]
    fn short_first_turn_uses_the_prompt_without_filler() {
        assert_eq!(
            calculate_work_title("write a poem").as_deref(),
            Some("Write Poem")
        );
    }

    #[test]
    fn correction_uses_the_latest_objective_clause() {
        assert_eq!(
            calculate_work_title(
                "wezterm doesn't need to get updated, just the agent title in herdr"
            )
            .as_deref(),
            Some("Agent Title Herdr")
        );
    }

    #[test]
    fn long_title_is_compacted_to_the_contract() {
        let title = calculate_work_title(
            "Implement automatic recalculation of descriptive work session titles for every managed agent across all providers",
        )
        .unwrap();
        assert!((WORK_TITLE_MIN_WORDS..=WORK_TITLE_MAX_WORDS)
            .contains(&title.split_whitespace().count()));
        assert!(title.chars().count() <= WORK_TITLE_MAX_CHARS);
    }

    #[test]
    fn secrets_paths_users_and_control_sequences_never_reach_titles() {
        let title = calculate_work_title(
            "Fix \u{1b}[31mbilling\u{1b}[0m for jane@example.com using api_key=sk-live-abcdef /Users/jane/private",
        )
        .unwrap();
        assert_eq!(title, "Fix Billing");
        for forbidden in ["jane", "example", "sk", "users", "\u{1b}"] {
            assert!(!title.to_ascii_lowercase().contains(forbidden));
        }
    }

    #[test]
    fn terse_continuation_has_no_standalone_title() {
        assert_eq!(calculate_work_title("please do it now"), None);
    }

    #[test]
    fn task_notification_prompt_does_not_set_a_title() {
        let input = serde_json::json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": "session-notification",
            "prompt": "<task-notification>\nClose PR 1549 and remove the work title.\n</task-notification>"
        })
        .to_string();

        assert!(
            request_from_turn_start(WorkTitleProvider::Codex, Some("w1:p1"), &input, 1,).is_none()
        );
    }

    #[test]
    fn system_reminder_only_prompt_does_not_set_a_title() {
        let input = serde_json::json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": "session-reminder",
            "prompt": "<system-reminder>\nFix SCA-42 and rename this tab.\n</system-reminder>"
        })
        .to_string();

        assert!(
            request_from_turn_start(WorkTitleProvider::Codex, Some("w1:p1"), &input, 1,).is_none()
        );
    }

    #[test]
    fn injected_paragraphs_do_not_contribute_titles_or_references() {
        let input = serde_json::json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": "session-injected-paragraph",
            "prompt": "Keep the work title stable.\n\n[SYSTEM NOTIFICATION - NOT USER INPUT]\nClose https://github.com/o/r/pull/1549\n\nStop hook feedback: rename the tab to billing."
        })
        .to_string();
        let request =
            request_from_turn_start(WorkTitleProvider::Codex, Some("w1:p1"), &input, 1).unwrap();

        assert_eq!(request.title.as_deref(), Some("Keep Work Title Stable"));
        let context = request.work_context.unwrap();
        assert!(context.pr_urls.is_empty());
        assert!(context.ticket_ids.is_empty());
    }

    #[test]
    fn long_brief_threshold_counts_words_across_paragraphs() {
        let twenty_words = "keep this work title stable across every short follow up turn and retain the task topic while ordinary questions arrive";
        let prompt = format!("{}\n\n{}", twenty_words, twenty_words);
        assert!(is_long_work_brief(&prompt));
        assert!(!is_long_work_brief(twenty_words));

        let input = serde_json::json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": "session-long-brief",
            "prompt": prompt
        })
        .to_string();
        let request =
            request_from_turn_start(WorkTitleProvider::Codex, Some("w1:p1"), &input, 1).unwrap();
        assert!(request.work_title_long_brief);
    }

    #[test]
    fn new_work_reference_means_a_ticket_or_pull_request_not_in_context() {
        let current = crate::work_context::PaneWorkContext {
            ticket_ids: vec!["SCA-9".into()],
            pr_urls: vec!["https://github.com/o/r/pull/9".into()],
            ..Default::default()
        };
        let repeated = crate::work_context::PaneWorkContext {
            ticket_ids: vec!["SCA-9".into()],
            pr_urls: vec!["https://github.com/o/r/pull/9".into()],
            ..Default::default()
        };
        let new_ticket = crate::work_context::PaneWorkContext {
            ticket_ids: vec!["SCA-10".into()],
            ..Default::default()
        };
        let new_pr = crate::work_context::PaneWorkContext {
            pr_urls: vec!["https://github.com/o/r/pull/10".into()],
            ..Default::default()
        };

        assert!(!work_title_has_new_reference(&current, &repeated));
        assert!(work_title_has_new_reference(&current, &new_ticket));
        assert!(work_title_has_new_reference(&current, &new_pr));
    }

    #[test]
    fn one_a_is_a_short_non_brief_prompt() {
        let input = serde_json::json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": "session-one-a",
            "prompt": "1a"
        })
        .to_string();
        let request =
            request_from_turn_start(WorkTitleProvider::Codex, Some("w1:p1"), &input, 1).unwrap();

        assert_eq!(request.title.as_deref(), Some("1A"));
        assert!(!request.work_title_long_brief);
        assert!(request
            .work_context
            .is_some_and(|context| context.ticket_ids.is_empty() && context.pr_urls.is_empty()));
    }

    #[test]
    fn latest_non_empty_paragraph_owns_the_title_subject() {
        let prompt = "Real-time policy management is not the task.\n\n\
            Implement briefing-derived tab titles for every window.";
        assert_eq!(
            calculate_work_title(prompt).as_deref(),
            Some("Implement Briefing-derived Tab Titles Every")
        );
        assert!(!calculate_work_title(prompt)
            .unwrap()
            .to_ascii_lowercase()
            .contains("real-time policy management"));
    }

    #[test]
    fn initial_briefing_ignores_a_trailing_meta_paragraph() {
        assert_eq!(
            calculate_work_title("Implement sidebar lifecycle assertions\n\nplease start")
                .as_deref(),
            Some("Implement Sidebar Lifecycle Assertions")
        );
    }

    #[test]
    fn dismissed_objective_in_the_same_paragraph_is_not_selected() {
        assert_eq!(
            calculate_work_title(
                "Real-time policy management is unrelated. Implement sidebar lifecycle assertions"
            )
            .as_deref(),
            Some("Implement Sidebar Lifecycle Assertions")
        );
    }

    #[test]
    fn provider_fixtures_build_session_guarded_requests() {
        let payload = include_str!("../tests/fixtures/work-titles/codex-user-prompt-submit.json");
        let request =
            request_from_turn_start(WorkTitleProvider::Codex, Some("w1:p2"), payload, 42).unwrap();
        assert_eq!(request.pane_id, "w1:p2");
        assert_eq!(request.agent.as_deref(), Some("codex"));
        assert_eq!(request.applies_to_source.as_deref(), Some("herdr:codex"));
        assert_eq!(
            request.agent_session_id.as_deref(),
            Some("fixture-codex-session")
        );
        assert_eq!(
            request.title.as_deref(),
            Some("Fix Billing Retry Regression")
        );
        assert_eq!(request.seq, Some(42));

        let claude = request_from_turn_start(
            WorkTitleProvider::Claude,
            Some("w1:p3"),
            include_str!("../tests/fixtures/work-titles/claude-user-prompt-submit.json"),
            43,
        )
        .unwrap();
        assert_eq!(claude.agent.as_deref(), Some("claude"));
        assert_eq!(
            claude.agent_session_id.as_deref(),
            Some("fixture-claude-session")
        );
        assert_eq!(
            claude.title.as_deref(),
            Some("Review Auth Migration Safety")
        );
    }

    #[test]
    fn ac1_ac2_ac3_provider_fixtures_carry_prompt_derived_work_context() {
        let codex = request_from_turn_start(
            WorkTitleProvider::Codex,
            Some("w1:p2"),
            include_str!(
                "../tests/fixtures/work-titles/codex-work-context-user-prompt-submit.json"
            ),
            44,
        )
        .unwrap();
        let codex_context = codex.work_context.expect("derived Codex work context");
        assert_eq!(codex_context.ticket_ids, vec!["SCA-9"]);
        assert_eq!(
            codex_context.pr_urls,
            vec!["https://github.com/scalable-so/herdr/pull/21"]
        );
        assert_eq!(
            codex_context.preview_urls,
            vec!["https://codex-preview.vercel.app"]
        );

        let claude = request_from_turn_start(
            WorkTitleProvider::Claude,
            Some("w1:p3"),
            include_str!(
                "../tests/fixtures/work-titles/claude-work-context-user-prompt-submit.json"
            ),
            45,
        )
        .unwrap();
        let claude_context = claude.work_context.expect("derived Claude work context");
        assert_eq!(claude_context.ticket_ids, vec!["SCA-42"]);
        assert_eq!(
            claude_context.pr_urls,
            vec!["https://github.com/scalable-so/herdr/pull/17"]
        );
        assert_eq!(
            claude_context.preview_urls,
            vec!["https://claude-preview.vercel.app"]
        );
    }

    #[test]
    fn turn_hook_assigns_the_last_mentioned_ticket_and_pull_request() {
        let request = request_from_turn_start(
            WorkTitleProvider::Codex,
            Some("w1:p1"),
            r#"{
                "hook_event_name":"UserPromptSubmit",
                "session_id":"session-last-work-item",
                "prompt":"SCA-1 SCA-2 SCA-1 https://github.com/o/r/pull/1 https://github.com/o/r/pull/2 https://github.com/o/r/pull/1"
            }"#,
            46,
        )
        .expect("guarded turn hook should produce metadata");
        let context = request.work_context.expect("work context");

        assert_eq!(context.ticket_ids, ["SCA-1"]);
        assert_eq!(context.pr_urls, ["https://github.com/o/r/pull/1"]);
    }

    #[test]
    fn ac25_turn_hook_keeps_the_bare_preview_root_and_the_full_url() {
        let request = request_from_turn_start(
            WorkTitleProvider::Codex,
            Some("w1:p1"),
            r#"{
                "hook_event_name":"UserPromptSubmit",
                "session_id":"session-preview",
                "prompt":"Ship https://Demo-Preview.Vercel.App and https://evil.example.test, then https://second.vercel.app?token=secret"
            }"#,
            46,
        )
        .expect("guarded turn hook should produce metadata");

        // A non-Vercel host is still rejected outright. A recognised host keeps
        // both forms: the bare root as the stable address, and the URL as
        // written, whose query carries the bypass token the preview needs.
        assert_eq!(
            request.work_context.expect("work context").preview_urls,
            vec![
                "https://demo-preview.vercel.app",
                "https://second.vercel.app",
                "https://second.vercel.app?token=secret"
            ]
        );
    }

    #[test]
    fn turn_hook_extracts_missive_conversation_links_from_the_prompt() {
        let request = request_from_turn_start(
            WorkTitleProvider::Codex,
            Some("w1:p1"),
            r#"{
                "hook_event_name":"UserPromptSubmit",
                "session_id":"session-missive",
                "prompt":"Reply in https://mail.missiveapp.com/#inbox/conversations/abc123 and ignore https://mail.missiveapp.com"
            }"#,
            47,
        )
        .expect("guarded turn hook should produce metadata");

        assert_eq!(
            request.work_context.expect("work context").missive_urls,
            vec!["https://mail.missiveapp.com/#inbox/conversations/abc123"]
        );
    }

    #[test]
    fn missing_support_and_claude_subagents_are_safe_noops() {
        let root_payload = r#"{
            "hook_event_name":"UserPromptSubmit",
            "session_id":"session-1",
            "prompt":"Review auth migration safety"
        }"#;
        assert!(
            request_from_turn_start(WorkTitleProvider::Claude, None, root_payload, 1).is_none()
        );

        let subagent_payload = r#"{
            "hook_event_name":"UserPromptSubmit",
            "session_id":"subagent-1",
            "agent_id":"worker-1",
            "prompt":"Review auth migration safety"
        }"#;
        assert!(request_from_turn_start(
            WorkTitleProvider::Claude,
            Some("w1:p1"),
            subagent_payload,
            2
        )
        .is_none());
    }

    #[test]
    fn latest_session_name_takes_the_most_recent_rename() {
        let transcript = concat!(
            r#"{"type":"user","sessionId":"s1"}"#,
            "\n",
            r#"{"type":"ai-title","aiTitle":"First name","sessionId":"s1"}"#,
            "\n",
            r#"{"type":"assistant","sessionId":"s1"}"#,
            "\n",
            r#"{"type":"ai-title","aiTitle":"Renamed after the rename","sessionId":"s1"}"#,
            "\n",
        );
        assert_eq!(
            latest_session_name(transcript, "s1").as_deref(),
            Some("Renamed after the rename")
        );
    }

    #[test]
    fn latest_session_name_orders_ai_and_custom_titles_together() {
        let transcript = concat!(
            r#"{"type":"ai-title","aiTitle":"First generated name","sessionId":"s1"}"#,
            "\n",
            r#"{"type":"custom-title","customTitle":"Human chosen name","sessionId":"s1"}"#,
            "\n",
            r#"{"type":"ai-title","aiTitle":"Latest generated name","sessionId":"s1"}"#,
            "\n",
        );
        assert_eq!(
            latest_session_name(transcript, "s1").as_deref(),
            Some("Latest generated name")
        );
    }

    #[test]
    fn latest_session_name_ignores_other_sessions_and_blank_names() {
        let transcript = concat!(
            r#"{"type":"ai-title","aiTitle":"My name","sessionId":"s1"}"#,
            "\n",
            r#"{"type":"ai-title","aiTitle":"Someone else","sessionId":"s2"}"#,
            "\n",
            r#"{"type":"ai-title","aiTitle":"   ","sessionId":"s1"}"#,
            "\n",
            "not json at all",
            "\n",
        );
        assert_eq!(
            latest_session_name(transcript, "s1").as_deref(),
            Some("My name")
        );
        assert_eq!(latest_session_name("", "s1"), None);
    }

    #[test]
    fn latest_session_name_caps_and_normalizes_whitespace() {
        let long = "word ".repeat(40);
        let transcript = format!(r#"{{"type":"ai-title","aiTitle":"{long}","sessionId":"s1"}}"#);
        let name = latest_session_name(&transcript, "s1").expect("session name");
        assert!(name.chars().count() <= SESSION_NAME_MAX_CHARS, "{name:?}");
        assert!(!name.contains("  "), "{name:?}");
    }

    #[test]
    fn inbound_session_names_reject_single_tokens() {
        let claude = r#"{"type":"custom-title","customTitle":"random-id","sessionId":"s1"}"#;
        let codex = r#"{"id":"t1","thread_name":"random-id"}"#;
        assert_eq!(latest_session_name(claude, "s1"), None);
        assert_eq!(latest_codex_thread_name(codex, "t1"), None);
    }

    #[test]
    fn inbound_session_names_preserve_written_paths_emails_and_handles() {
        let name = "Review /tmp/report for dev@example.com with @ops";
        let claude = serde_json::json!({
            "type": "custom-title",
            "customTitle": name,
            "sessionId": "s1",
        })
        .to_string();
        let codex = serde_json::json!({
            "id": "t1",
            "thread_name": name,
        })
        .to_string();
        assert_eq!(latest_session_name(&claude, "s1").as_deref(), Some(name));
        assert_eq!(
            latest_codex_thread_name(&codex, "t1").as_deref(),
            Some(name)
        );
    }

    #[test]
    fn session_name_request_is_guarded_and_carries_only_the_name() {
        let input =
            r#"{"hook_event_name":"Stop","session_id":"s1","transcript_path":"/tmp/s1.jsonl"}"#;
        let transcript = r#"{"type":"ai-title","aiTitle":"Live name","sessionId":"s1"}"#;
        let request = request_from_session_name(
            WorkTitleProvider::Claude,
            Some("w1:pA"),
            input,
            transcript,
            7,
        )
        .expect("session name request");
        assert_eq!(request.source, SESSION_NAME_SOURCE);
        assert_eq!(request.agent.as_deref(), Some("claude"));
        assert_eq!(request.applies_to_source.as_deref(), Some("herdr:claude"));
        assert_eq!(request.agent_session_id.as_deref(), Some("s1"));
        assert_eq!(request.title, None);
        let context = request.work_context.expect("work context");
        assert_eq!(context.session_name.as_deref(), Some("Live name"));
        assert_eq!(context.work_title, None);
        assert!(context.ticket_ids.is_empty());
    }

    #[test]
    fn session_name_request_rejects_subagents_and_missing_evidence() {
        let transcript = r#"{"type":"ai-title","aiTitle":"Live name","sessionId":"s1"}"#;
        let subagent = r#"{"hook_event_name":"Stop","session_id":"s1","agent_id":"sub"}"#;
        assert!(request_from_session_name(
            WorkTitleProvider::Claude,
            Some("w1:pA"),
            subagent,
            transcript,
            7
        )
        .is_none());

        let ok = r#"{"hook_event_name":"Stop","session_id":"s1"}"#;
        // Codex reads its own index, never a Claude transcript.
        assert!(request_from_session_name(
            WorkTitleProvider::Codex,
            Some("w1:pA"),
            ok,
            transcript,
            7
        )
        .is_none());
        // No pane, no report.
        assert!(
            request_from_session_name(WorkTitleProvider::Claude, None, ok, transcript, 7).is_none()
        );
        // No named session yet, no report.
        assert!(
            request_from_session_name(WorkTitleProvider::Claude, Some("w1:pA"), ok, "", 7)
                .is_none()
        );
    }

    #[test]
    fn transcript_path_is_read_from_the_hook_payload() {
        assert_eq!(
            transcript_path_from_hook_input(
                r#"{"hook_event_name":"Stop","transcript_path":" /tmp/a.jsonl "}"#
            )
            .as_deref(),
            Some("/tmp/a.jsonl")
        );
        assert_eq!(
            transcript_path_from_hook_input(r#"{"hook_event_name":"Stop"}"#),
            None
        );
    }

    #[test]
    fn latest_codex_thread_name_takes_the_most_recent_rename() {
        let index = concat!(
            r#"{"id":"t1","thread_name":"help me to reduce disk storage on th","updated_at":"2026-09-05T10:00:37Z"}"#,
            "\n",
            r#"{"id":"t1","thread_name":"Reduce machine disk usage","updated_at":"2026-09-05T10:00:43Z"}"#,
            "\n",
        );
        assert_eq!(
            latest_codex_thread_name(index, "t1").as_deref(),
            Some("Reduce machine disk usage")
        );
    }

    #[test]
    fn latest_codex_thread_name_ignores_other_threads_and_blank_names() {
        let index = concat!(
            r#"{"id":"t1","thread_name":"My thread"}"#,
            "\n",
            r#"{"id":"t2","thread_name":"Someone else"}"#,
            "\n",
            r#"{"id":"t1","thread_name":"   "}"#,
            "\n",
            "not json at all",
            "\n",
        );
        assert_eq!(
            latest_codex_thread_name(index, "t1").as_deref(),
            Some("My thread")
        );
        assert_eq!(latest_codex_thread_name("", "t1"), None);
    }

    #[test]
    fn latest_codex_thread_name_caps_and_normalizes_whitespace() {
        let long = "word ".repeat(40);
        let index = format!(r#"{{"id":"t1","thread_name":"{long}"}}"#);
        let name = latest_codex_thread_name(&index, "t1").expect("thread name");
        assert!(name.chars().count() <= SESSION_NAME_MAX_CHARS, "{name:?}");
        assert!(!name.contains("  "), "{name:?}");
    }

    #[test]
    fn codex_session_name_request_is_guarded_and_carries_only_the_name() {
        let input =
            r#"{"hook_event_name":"Stop","session_id":"01a07102-f2ad-7660-9d01-7d33518b5dc4"}"#;
        let index = r#"{"id":"01a07102-f2ad-7660-9d01-7d33518b5dc4","thread_name":"Reduce machine disk usage"}"#;
        let request =
            request_from_session_name(WorkTitleProvider::Codex, Some("w1:pA"), input, index, 9)
                .expect("session name request");
        assert_eq!(request.source, SESSION_NAME_SOURCE);
        assert_eq!(request.agent.as_deref(), Some("codex"));
        assert_eq!(request.applies_to_source.as_deref(), Some("herdr:codex"));
        assert_eq!(
            request.agent_session_id.as_deref(),
            Some("01a07102-f2ad-7660-9d01-7d33518b5dc4")
        );
        assert_eq!(request.title, None);
        let context = request.work_context.expect("work context");
        assert_eq!(
            context.session_name.as_deref(),
            Some("Reduce machine disk usage")
        );
        assert_eq!(context.work_title, None);
        assert!(context.ticket_ids.is_empty());
    }

    #[test]
    fn codex_session_name_needs_a_pane_and_a_named_thread() {
        let input = r#"{"hook_event_name":"Stop","session_id":"t1"}"#;
        let index = r#"{"id":"t1","thread_name":"Named"}"#;
        // No pane, no report.
        assert!(
            request_from_session_name(WorkTitleProvider::Codex, None, input, index, 9).is_none()
        );
        // A thread Codex has not named yet produces no report.
        assert!(request_from_session_name(
            WorkTitleProvider::Codex,
            Some("w1:pA"),
            input,
            r#"{"id":"other","thread_name":"Named"}"#,
            9
        )
        .is_none());
    }

    #[test]
    fn appends_provider_records_and_rejects_single_token_names() {
        let root = temp_dir("append");
        let claude_dir = root.join("projects/-tmp-repro");
        std::fs::create_dir_all(&claude_dir).unwrap();
        let transcript = claude_dir.join("claude-session.jsonl");
        std::fs::write(&transcript, b"{\"type\":\"user\"}").unwrap();
        let claude = SessionNameWriteTarget::new(
            WorkTitleProvider::Claude,
            "claude-session".into(),
            transcript.clone(),
        );
        assert!(append_session_name(&claude, "Review billing retries").unwrap());
        let claude_line = std::fs::read_to_string(&transcript)
            .unwrap()
            .lines()
            .last()
            .unwrap()
            .to_string();
        let claude_record: serde_json::Value = serde_json::from_str(&claude_line).unwrap();
        assert_eq!(
            std::fs::read_to_string(&transcript)
                .unwrap()
                .lines()
                .count(),
            2
        );
        assert_eq!(claude_record["type"], "custom-title");
        assert_eq!(claude_record["customTitle"], "Review billing retries");
        assert_eq!(claude_record["sessionId"], "claude-session");

        let exact_name = "Review /tmp/report for dev@example.com with @ops";
        assert!(append_session_name(&claude, &format!("  {exact_name}  ")).unwrap());
        let claude_contents = std::fs::read_to_string(&transcript).unwrap();
        let exact_line = std::fs::read_to_string(&transcript)
            .unwrap()
            .lines()
            .last()
            .unwrap()
            .to_string();
        let exact_record: serde_json::Value = serde_json::from_str(&exact_line).unwrap();
        assert_eq!(exact_record["customTitle"], exact_name);
        assert_eq!(
            latest_session_name(&claude_contents, "claude-session").as_deref(),
            Some(exact_name)
        );

        let index = root.join("session_index.jsonl");
        std::fs::write(
            &index,
            b"{\"id\":\"codex-thread\",\"thread_name\":\"Original thread\"}",
        )
        .unwrap();
        assert_eq!(
            validated_codex_session_index_path(index.to_str(), "codex-thread"),
            Some(index.clone())
        );
        let codex = SessionNameWriteTarget::new(
            WorkTitleProvider::Codex,
            "codex-thread".into(),
            index.clone(),
        );
        assert!(append_session_name(&codex, "Audit session naming").unwrap());
        let codex_line = std::fs::read_to_string(&index)
            .unwrap()
            .lines()
            .last()
            .unwrap()
            .to_string();
        let codex_record: serde_json::Value = serde_json::from_str(&codex_line).unwrap();
        assert_eq!(std::fs::read_to_string(&index).unwrap().lines().count(), 2);
        assert_eq!(codex_record["id"], "codex-thread");
        assert_eq!(codex_record["thread_name"], "Audit session naming");
        let timestamp = codex_record["updated_at"].as_str().unwrap();
        assert!(timestamp.contains('T') && timestamp.ends_with('Z'));

        assert!(append_session_name(&codex, exact_name).unwrap());
        let codex_contents = std::fs::read_to_string(&index).unwrap();
        assert_eq!(
            latest_codex_thread_name(&codex_contents, "codex-thread").as_deref(),
            Some(exact_name)
        );

        let before = std::fs::read(&index).unwrap();
        assert!(!append_session_name(&codex, "random-id").unwrap());
        assert_eq!(std::fs::read(&index).unwrap(), before);
        assert!(!append_session_name(&codex, &format!("Review {}", "x".repeat(80))).unwrap());
        assert_eq!(std::fs::read(&index).unwrap(), before);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn write_back_requires_the_bound_existing_regular_file() {
        let root = temp_dir("invalid-target");
        let missing = root.join("projects/-tmp/missing.jsonl");
        let target =
            SessionNameWriteTarget::new(WorkTitleProvider::Claude, "missing".into(), missing);
        assert!(append_session_name(&target, "Valid session name").is_err());

        let wrong_index = root.join("session_index.jsonl");
        std::fs::write(&wrong_index, b"{\"id\":\"other-thread\"}\n").unwrap();
        assert!(validated_codex_session_index_path(wrong_index.to_str(), "bound-thread").is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn write_back_rejects_symlink_targets() {
        use std::os::unix::fs::symlink;

        let root = temp_dir("symlink-target");
        let claude_dir = root.join("projects/-tmp");
        std::fs::create_dir_all(&claude_dir).unwrap();
        let real = root.join("real.jsonl");
        std::fs::write(&real, b"\n").unwrap();
        let link = claude_dir.join("session.jsonl");
        symlink(&real, &link).unwrap();
        let target = SessionNameWriteTarget::new(WorkTitleProvider::Claude, "session".into(), link);
        assert!(append_session_name(&target, "Valid session name").is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
