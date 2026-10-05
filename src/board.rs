//! The focus board's durable, human-readable weekly note.
//!
//! Board records live in the Obsidian vault. The Markdown heading and checkbox
//! are authoritative so checking a task in Obsidian is visible on reload.
//! Herdr's extra fields are stored in a single JSON comment on the same line.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use time::{Date, Duration, Weekday};

static NEXT_REMOTE_LINE_REQUEST_ID: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(1);

pub(crate) fn next_remote_line_request_id() -> u64 {
    NEXT_REMOTE_LINE_REQUEST_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Column {
    #[default]
    Draft,
    Todo,
    InProgress,
    Done,
}

impl Column {
    pub(crate) const ALL: [Self; 4] = [Self::Draft, Self::Todo, Self::InProgress, Self::Done];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Draft => "Draft",
            Self::Todo => "To Do",
            Self::InProgress => "In Progress",
            Self::Done => "Done",
        }
    }

    pub(crate) fn move_by(self, delta: i8) -> Self {
        let index = Self::ALL
            .iter()
            .position(|value| *value == self)
            .unwrap_or(0);
        Self::ALL[index.saturating_add_signed(delta as isize).min(3)]
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Area {
    Scalable,
    #[default]
    Harness,
    Personal,
}

impl Area {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Scalable => "scalable",
            Self::Harness => "harness",
            Self::Personal => "personal",
        }
    }

    pub(crate) fn icon(self, nerd_font: bool) -> &'static str {
        match (self, nerd_font) {
            (Self::Scalable, true) => "⚡",
            (Self::Harness, true) => "⚙",
            (Self::Personal, true) => "󰣉",
            (Self::Scalable, false) => "S",
            (Self::Harness, false) => "H",
            (Self::Personal, false) => "P",
        }
    }

    pub(crate) fn next(self) -> Self {
        match self {
            Self::Scalable => Self::Harness,
            Self::Harness => Self::Personal,
            Self::Personal => Self::Scalable,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoalScope {
    #[default]
    Week,
    Today,
    Container,
}

impl GoalScope {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Week => "week",
            Self::Today => "today",
            Self::Container => "container",
        }
    }

    pub(crate) fn next(self) -> Self {
        match self {
            Self::Week => Self::Today,
            Self::Today => Self::Container,
            Self::Container => Self::Week,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) struct AgentLink {
    pub(crate) host: String,
    pub(crate) pane_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Update {
    pub(crate) at: u64,
    pub(crate) text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Card {
    pub(crate) id: String,
    pub(crate) title: String,
    #[serde(default)]
    pub(crate) description: String,
    #[serde(default)]
    pub(crate) area: Area,
    #[serde(default)]
    pub(crate) column: Column,
    #[serde(default)]
    pub(crate) goal_id: Option<String>,
    #[serde(default)]
    pub(crate) agent_summary: String,
    #[serde(default)]
    pub(crate) updates: Vec<Update>,
    #[serde(default)]
    pub(crate) agents: Vec<AgentLink>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Goal {
    pub(crate) id: String,
    pub(crate) title: String,
    #[serde(default)]
    pub(crate) scope: GoalScope,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Board {
    pub(crate) goals: Vec<Goal>,
    pub(crate) cards: Vec<Card>,
}

#[derive(Debug, Clone)]
pub(crate) struct BoardView {
    pub(crate) note: WeekNote,
    pub(crate) board: Board,
    pub(crate) column: Column,
    pub(crate) row: usize,
    pub(crate) goal_offset: usize,
    pub(crate) dialog: Option<Dialog>,
    pub(crate) detail: Option<Detail>,
    pub(crate) editor: Option<Editor>,
    pub(crate) drag_id: Option<String>,
    pub(crate) drag_moved: bool,
    pub(crate) move_mode: bool,
    pub(crate) error: Option<String>,
    baseline: String,
    pub(crate) agent_lines: std::collections::HashMap<AgentLink, (u64, String)>,
    pub(crate) agent_activity: std::collections::HashMap<AgentLink, std::time::Instant>,
    pub(crate) agent_lanes: std::collections::HashMap<AgentLink, Lane>,
    pub(crate) last_agent_refresh_unix_s: u64,
    pub(crate) remote_line_fetch_in_flight: bool,
    pub(crate) remote_line_request_id: u64,
    pub(crate) last_remote_line_fetch_unix_s: u64,
}

impl BoardView {
    pub(crate) fn open() -> Result<Self, String> {
        let note = WeekNote::current()?;
        Self::from_note(note)
    }

    fn from_note(note: WeekNote) -> Result<Self, String> {
        let baseline = match fs::read_to_string(&note.path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(error.to_string()),
        };
        let board = if baseline.is_empty() {
            Board::default()
        } else {
            parse(&baseline)?
        };
        Ok(Self {
            note,
            board,
            column: Column::Draft,
            row: 0,
            goal_offset: 0,
            dialog: None,
            detail: None,
            editor: None,
            drag_id: None,
            drag_moved: false,
            move_mode: false,
            error: None,
            baseline,
            agent_lines: std::collections::HashMap::new(),
            agent_lanes: std::collections::HashMap::new(),
            agent_activity: std::collections::HashMap::new(),
            last_agent_refresh_unix_s: 0,
            remote_line_fetch_in_flight: false,
            remote_line_request_id: 0,
            last_remote_line_fetch_unix_s: 0,
        })
    }

    #[cfg(test)]
    pub(crate) fn test_new(note: WeekNote, board: Board) -> Self {
        Self {
            note,
            board,
            column: Column::Draft,
            row: 0,
            goal_offset: 0,
            dialog: None,
            detail: None,
            editor: None,
            drag_id: None,
            drag_moved: false,
            move_mode: false,
            error: None,
            baseline: String::new(),
            agent_lines: std::collections::HashMap::new(),
            agent_lanes: std::collections::HashMap::new(),
            agent_activity: std::collections::HashMap::new(),
            last_agent_refresh_unix_s: 0,
            remote_line_fetch_in_flight: false,
            remote_line_request_id: 0,
            last_remote_line_fetch_unix_s: 0,
        }
    }

    pub(crate) fn visible_cards(&self, app: &crate::app::state::AppState) -> Vec<&Card> {
        let mut cards: Vec<_> = self
            .board
            .cards
            .iter()
            .filter(|card| card.column == self.column)
            .collect();
        if self.column == Column::InProgress {
            cards.sort_by_key(|card| app.board_lane(card));
        }
        cards
    }

    pub(crate) fn selected_id(&self, app: &crate::app::state::AppState) -> Option<String> {
        self.visible_cards(app)
            .get(self.row)
            .map(|card| card.id.clone())
    }

    pub(crate) fn persist(&mut self) -> bool {
        let current = match fs::read_to_string(&self.note.path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => {
                self.error = Some(error.to_string());
                return false;
            }
        };
        if current != self.baseline {
            self.error =
                Some("weekly note changed in Obsidian; reopen the board before saving".into());
            return false;
        }
        match self.note.save(&self.board) {
            Ok(()) => {
                self.baseline = format_note(&self.board, &self.note);
                self.error = None;
                true
            }
            Err(error) => {
                self.error = Some(error);
                false
            }
        }
    }

    /// A spawned pane already exists. Rebase just its link onto a concurrent
    /// Obsidian edit so the pane remains attached to its card.
    pub(crate) fn persist_spawn_link(&mut self, id: &str, link: AgentLink) -> bool {
        let mut latest = match Self::from_note(self.note.clone()) {
            Ok(view) => view,
            Err(error) => {
                self.error = Some(error);
                return false;
            }
        };
        let Some(card) = latest.board.card_mut(id) else {
            self.error = Some(format!(
                "spawned pane {} has no card in the weekly note",
                link.pane_id
            ));
            return false;
        };
        if !card.agents.is_empty() && !card.agents.contains(&link) {
            self.error = Some(format!(
                "card already has an agent; spawned pane {} needs linking",
                link.pane_id
            ));
            return false;
        }
        if !card.agents.contains(&link) {
            card.agents.push(link);
        }
        card.column = Column::InProgress;
        if !latest.persist() {
            self.error = latest.error;
            return false;
        }
        self.board = latest.board;
        self.baseline = latest.baseline;
        self.error = None;
        true
    }
}

#[derive(Debug, Clone)]
pub(crate) enum Dialog {
    Card {
        title: String,
        description: String,
        area: Area,
        goal: Option<String>,
        new_goal: String,
        field: usize,
    },
    Goal {
        title: String,
        scope: GoalScope,
        todos: String,
        field: usize,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct Detail {
    pub(crate) card_id: String,
    pub(crate) agent_tab: bool,
    pub(crate) agent_row: usize,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum EditField {
    HumanReplace,
    HumanAppend,
    AgentSummary,
    AgentUpdate,
}

#[derive(Debug, Clone)]
pub(crate) struct Editor {
    pub(crate) field: EditField,
    pub(crate) text: String,
}

impl Board {
    pub(crate) fn goal_done(&self, id: &str) -> bool {
        let linked: Vec<_> = self
            .cards
            .iter()
            .filter(|card| card.goal_id.as_deref() == Some(id))
            .collect();
        !linked.is_empty() && linked.iter().all(|card| card.column == Column::Done)
    }

    pub(crate) fn card(&self, id: &str) -> Option<&Card> {
        self.cards.iter().find(|card| card.id == id)
    }

    pub(crate) fn card_mut(&mut self, id: &str) -> Option<&mut Card> {
        self.cards.iter_mut().find(|card| card.id == id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Lane {
    Blocked,
    Working,
    DoneAwaitingYou,
}

impl Lane {
    pub(crate) fn glyph(self) -> &'static str {
        match self {
            Self::Blocked => "●",
            Self::Working => "◐",
            Self::DoneAwaitingYou => "✓",
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Blocked => "blocked",
            Self::Working => "working",
            Self::DoneAwaitingYou => "done · awaiting you",
        }
    }
}

pub(crate) fn worst_lane(lanes: impl IntoIterator<Item = Lane>) -> Lane {
    let mut any = false;
    let mut worst = Lane::DoneAwaitingYou;
    for lane in lanes {
        any = true;
        worst = worst.min(lane);
    }
    if any {
        worst
    } else {
        Lane::Working
    }
}

pub(crate) struct AgentDisplay<'a> {
    pub(crate) pane_id: &'a str,
    pub(crate) host: &'a str,
    pub(crate) lane: Lane,
    pub(crate) last_line: &'a str,
}

impl crate::app::state::AppState {
    /// Refresh only linked terminals whose output changed. Called during view
    /// computation, while the board is visible; render consumes cached text.
    pub(crate) fn refresh_board_agent_lines(
        &mut self,
        runtimes: &crate::terminal::TerminalRuntimeRegistry,
        observed_unix_s: u64,
    ) {
        let Some(view) = self.board_view.as_ref() else {
            return;
        };
        if view.last_agent_refresh_unix_s == observed_unix_s {
            return;
        }
        let links: Vec<_> = view
            .board
            .cards
            .iter()
            .flat_map(|card| card.agents.iter())
            .cloned()
            .collect();
        let mut seen = std::collections::HashSet::new();
        let mut line_changes = Vec::new();
        let mut lane_changes = Vec::new();
        let mut activity_changes = std::collections::HashMap::new();
        let now = self.view_observed_at;
        for link in links {
            if !seen.insert(link.clone()) {
                continue;
            }
            if link.host != self.agent_host_name {
                let remote = self.remote_agent_panel_entries.iter().find(|entry| {
                    entry.agent_ref.host == link.host && entry.agent_ref.agent == link.pane_id
                });
                let lane = remote
                    .map(|entry| lane_from_state(entry.entry.state))
                    .unwrap_or(Lane::Working);
                if let Some(at) = remote.and_then(|entry| entry.entry.activity_at) {
                    activity_changes.insert(link.clone(), at);
                }
                lane_changes.push((link.clone(), lane));
                if !view.agent_lines.contains_key(&link) {
                    line_changes.push((link, (u64::MAX, "remote terminal".into())));
                }
                continue;
            }
            let evidence = link
                .pane_id
                .split_once(":p")
                .and_then(|(workspace_id, number)| {
                    let public_number = crate::workspace::decode_public_number(number)?;
                    let workspace = self
                        .workspaces
                        .iter()
                        .find(|workspace| workspace.id == workspace_id)?;
                    let pane_id =
                        workspace
                            .public_pane_numbers
                            .iter()
                            .find_map(|(pane_id, value)| {
                                (*value == public_number).then_some(*pane_id)
                            })?;
                    let pane = workspace.pane_state(pane_id)?;
                    let terminal = self.terminals.get(&pane.attached_terminal_id)?;
                    let lane = lane_from_state(pane.agent_projection(terminal).state);
                    Some((
                        pane.attached_terminal_id.clone(),
                        lane,
                        now.checked_sub(pane.activity.inactive_for(now)),
                    ))
                });
            let Some((terminal_id, lane, activity_at)) = evidence else {
                lane_changes.push((link.clone(), Lane::Working));
                line_changes.push((link, (u64::MAX, "terminal unavailable".into())));
                continue;
            };
            lane_changes.push((link.clone(), lane));
            if let Some(at) = activity_at {
                activity_changes.insert(link.clone(), at);
            }
            let Some(runtime) = runtimes.get(&terminal_id) else {
                continue;
            };
            let revision = runtime.content_revision();
            if self
                .board_view
                .as_ref()
                .and_then(|view| view.agent_lines.get(&link))
                .is_some_and(|(known, _)| *known == revision)
            {
                continue;
            }
            let snapshot = runtime.recent_text_snapshot(2);
            let line = snapshot
                .text
                .lines()
                .rev()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("terminal quiet")
                .split_whitespace()
                .take(7)
                .collect::<Vec<_>>()
                .join(" ");
            line_changes.push((link, (revision, line)));
        }
        if let Some(view) = self.board_view.as_mut() {
            view.last_agent_refresh_unix_s = observed_unix_s;
            view.agent_activity = activity_changes;
            for (id, lane) in lane_changes {
                view.agent_lanes.insert(id, lane);
            }
            for (id, line) in line_changes {
                view.agent_lines.insert(id, line);
            }
        }
    }

    pub(crate) fn board_agent<'a>(&'a self, link: &'a AgentLink) -> AgentDisplay<'a> {
        AgentDisplay {
            pane_id: &link.pane_id,
            host: &link.host,
            lane: self.board_agent_lane(link),
            last_line: self
                .board_view
                .as_ref()
                .and_then(|view| view.agent_lines.get(link))
                .map(|(_, text)| text.as_str())
                .unwrap_or("terminal quiet"),
        }
    }

    fn board_agent_lane(&self, link: &AgentLink) -> Lane {
        self.board_view
            .as_ref()
            .and_then(|view| view.agent_lanes.get(link))
            .copied()
            .unwrap_or(Lane::Working)
    }

    pub(crate) fn board_lane(&self, card: &Card) -> Lane {
        worst_lane(card.agents.iter().map(|agent| self.board_agent_lane(agent)))
    }
}

fn lane_from_state(state: crate::detect::AgentState) -> Lane {
    match state {
        crate::detect::AgentState::Blocked => Lane::Blocked,
        crate::detect::AgentState::Idle => Lane::DoneAwaitingYou,
        _ => Lane::Working,
    }
}

#[derive(Debug, Clone)]
pub(crate) struct WeekNote {
    pub(crate) path: PathBuf,
    pub(crate) start: Date,
    pub(crate) today: Date,
}

impl WeekNote {
    pub(crate) fn current() -> Result<Self, String> {
        let today = crate::platform::local_datetime()
            .ok_or("cannot determine local date")?
            .date();
        let root = std::env::var_os("OBSIDIAN_VAULT")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join("workspaces/obsidian"))
            })
            .ok_or("cannot locate Obsidian vault")?;
        if !root.join(".obsidian").is_dir() {
            return Err(format!("Obsidian vault not found at {}", root.display()));
        }
        Self::for_date(&root, today)
    }

    pub(crate) fn for_date(root: &Path, today: Date) -> Result<Self, String> {
        let start = today - Duration::days(i64::from(today.weekday().number_days_from_monday()));
        let (year, week, _) = today.to_iso_week_date();
        Ok(Self {
            path: root.join("todos").join(format!("{year}-W{week:02}.md")),
            start,
            today,
        })
    }

    pub(crate) fn header(&self) -> String {
        let (_, week, _) = self.today.to_iso_week_date();
        let end = self.start + Duration::days(6);
        format!(
            "{} {:02}.{:02} · W{week:02} {:02}.{:02}–{:02}.{:02}",
            weekday_de(self.today.weekday()),
            self.today.day(),
            u8::from(self.today.month()),
            self.start.day(),
            u8::from(self.start.month()),
            end.day(),
            u8::from(end.month())
        )
    }

    pub(crate) fn save(&self, board: &Board) -> Result<(), String> {
        let parent = self.path.parent().ok_or("invalid weekly note path")?;
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        let text = format_note(board, self);
        let temp = self
            .path
            .with_extension(format!("md.{}.tmp", std::process::id()));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|error| error.to_string())?;
        let result = (|| {
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
            fs::rename(&temp, &self.path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result.map_err(|error| error.to_string())
    }
}

fn weekday_de(day: Weekday) -> &'static str {
    match day {
        Weekday::Monday => "Mo",
        Weekday::Tuesday => "Di",
        Weekday::Wednesday => "Mi",
        Weekday::Thursday => "Do",
        Weekday::Friday => "Fr",
        Weekday::Saturday => "Sa",
        Weekday::Sunday => "So",
    }
}

fn format_note(board: &Board, week: &WeekNote) -> String {
    let mut out = format!("# Herdr board · {}\n\n", week.header());
    for goal in &board.goals {
        let done = if board.goal_done(&goal.id) { 'x' } else { ' ' };
        let metadata = serde_json::json!({"id": goal.id, "scope": goal.scope});
        out.push_str(&format!(
            "## [{done}] {} <!-- herdr-goal:{metadata} -->\n",
            goal.title
        ));
        for card in board
            .cards
            .iter()
            .filter(|card| card.goal_id.as_deref() == Some(&goal.id))
        {
            push_card(&mut out, card);
        }
        out.push('\n');
    }
    let unlinked: Vec<_> = board
        .cards
        .iter()
        .filter(|card| card.goal_id.is_none())
        .collect();
    if !unlinked.is_empty() {
        out.push_str("## Ungrouped\n");
        for card in unlinked {
            push_card(&mut out, card);
        }
        out.push('\n');
    }
    out
}

fn push_card(out: &mut String, card: &Card) {
    let checked = if card.column == Column::Done {
        'x'
    } else {
        ' '
    };
    let metadata = serde_json::json!({
        "id": card.id, "description": card.description, "area": card.area,
        "column": card.column, "agent_summary": card.agent_summary,
        "updates": card.updates, "agents": card.agents,
    });
    out.push_str(&format!(
        "- [{checked}] {} <!-- herdr-card:{metadata} -->\n",
        card.title
    ));
}

fn parse(text: &str) -> Result<Board, String> {
    let mut board = Board::default();
    let mut goal_id = None;
    for (index, line) in text.lines().enumerate() {
        let line_number = index + 1;
        if let Some(rest) = line.strip_prefix("## ") {
            if rest == "Ungrouped" {
                goal_id = None;
                continue;
            }
            let (checked, rest) = checkbox(rest)
                .ok_or_else(|| format!("line {line_number}: invalid goal checkbox"))?;
            let (title, json) = comment(rest, "herdr-goal:")
                .ok_or_else(|| format!("line {line_number}: invalid goal metadata"))?;
            let meta: GoalMeta = serde_json::from_str(json)
                .map_err(|error| format!("line {line_number}: {error}"))?;
            if board.goals.iter().any(|goal| goal.id == meta.id) {
                return Err(format!("line {line_number}: duplicate goal ID"));
            }
            goal_id = Some(meta.id.clone());
            // Goal completion is derived from the child checkboxes. The heading
            // checkbox is rendered for Obsidian and never becomes state itself.
            let _ = checked;
            board.goals.push(Goal {
                id: meta.id,
                title: title.into(),
                scope: meta.scope,
            });
        } else if let Some(rest) = line.strip_prefix("- ") {
            let (checked, rest) = checkbox(rest)
                .ok_or_else(|| format!("line {line_number}: invalid card checkbox"))?;
            let (title, json) = comment(rest, "herdr-card:")
                .ok_or_else(|| format!("line {line_number}: invalid card metadata"))?;
            let meta: CardMeta = serde_json::from_str(json)
                .map_err(|error| format!("line {line_number}: {error}"))?;
            if board.cards.iter().any(|card: &Card| card.id == meta.id) {
                return Err(format!("line {line_number}: duplicate card ID"));
            }
            board.cards.push(Card {
                id: meta.id,
                title: title.into(),
                description: meta.description,
                area: meta.area,
                column: if checked {
                    Column::Done
                } else if meta.column == Column::Done {
                    Column::Todo
                } else {
                    meta.column
                },
                goal_id: goal_id.clone(),
                agent_summary: meta.agent_summary,
                updates: meta.updates,
                agents: meta.agents,
            });
        } else if !line.trim().is_empty() && !line.starts_with("# Herdr board · ") {
            return Err(format!(
                "line {line_number}: unsupported weekly note content"
            ));
        }
    }
    Ok(board)
}

fn checkbox(line: &str) -> Option<(bool, &str)> {
    if let Some(rest) = line.strip_prefix("[ ] ") {
        Some((false, rest))
    } else {
        line.strip_prefix("[x] ")
            .or_else(|| line.strip_prefix("[X] "))
            .map(|rest| (true, rest))
    }
}

fn comment<'a>(line: &'a str, tag: &str) -> Option<(&'a str, &'a str)> {
    let (title, json) = line.rsplit_once(&format!(" <!-- {tag}"))?;
    Some((title.trim(), json.strip_suffix(" -->")?))
}

#[derive(Deserialize)]
struct GoalMeta {
    id: String,
    #[serde(default)]
    scope: GoalScope,
}

#[derive(Deserialize)]
struct CardMeta {
    id: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    area: Area,
    #[serde(default)]
    column: Column,
    #[serde(default)]
    agent_summary: String,
    #[serde(default)]
    updates: Vec<Update>,
    #[serde(default)]
    agents: Vec<AgentLink>,
}

pub(crate) fn new_id(prefix: &str) -> Result<String, String> {
    crate::day::new_id(crate::day::unix_seconds_now().saturating_mul(1_000))
        .map(|id| format!("{prefix}-{id}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn week_boundary_and_header_use_iso_week() {
        let date = Date::from_calendar_date(2026, time::Month::September, 28).expect("date");
        let note = WeekNote::for_date(Path::new("/vault"), date).expect("note");
        assert_eq!(note.path, PathBuf::from("/vault/todos/2026-W40.md"));
        assert_eq!(note.header(), "Mo 28.09 · W40 28.09–04.10");
    }

    #[test]
    fn obsidian_checkboxes_override_stored_columns_and_goal_completion() {
        let date = Date::from_calendar_date(2026, time::Month::September, 28).expect("date");
        let note = WeekNote::for_date(Path::new("/vault"), date).expect("note");
        let board = Board {
            goals: vec![Goal {
                id: "g1".into(),
                title: "Ship board".into(),
                scope: GoalScope::Week,
            }],
            cards: vec![Card {
                id: "c1".into(),
                title: "Check sync".into(),
                description: "raw\ntext".into(),
                area: Area::Harness,
                column: Column::Todo,
                goal_id: Some("g1".into()),
                agent_summary: String::new(),
                updates: vec![],
                agents: vec![],
            }],
        };
        let text = format_note(&board, &note).replace("- [ ] Check sync", "- [x] Check sync");
        let parsed = parse(&text).expect("parse");
        assert_eq!(parsed.cards[0].column, Column::Done);
        assert!(parsed.goal_done("g1"));
        assert_eq!(parsed.cards[0].description, "raw\ntext");
    }

    #[test]
    fn external_note_edit_blocks_stale_board_write_and_reloads_done() {
        let date = Date::from_calendar_date(2026, time::Month::September, 28).expect("date");
        let root = std::env::temp_dir().join(new_id("herdr-board-test").expect("id"));
        let note = WeekNote::for_date(&root, date).expect("note");
        let mut view = BoardView::from_note(note.clone()).expect("empty note");
        view.board.goals.push(Goal {
            id: "g1".into(),
            title: "Ship".into(),
            scope: GoalScope::Week,
        });
        view.board.cards.push(Card {
            id: "c1".into(),
            title: "Verify".into(),
            description: String::new(),
            area: Area::Harness,
            column: Column::Todo,
            goal_id: Some("g1".into()),
            agent_summary: String::new(),
            updates: Vec::new(),
            agents: Vec::new(),
        });
        assert!(view.persist());
        let original = fs::read_to_string(&note.path).expect("written note");
        let external = original.replace("- [ ] Verify", "- [x] Verify");
        fs::write(&note.path, &external).expect("Obsidian edit");
        view.board.cards[0].description = "stale change".into();
        assert!(!view.persist());
        assert_eq!(fs::read_to_string(&note.path).expect("unchanged"), external);
        let reloaded = BoardView::from_note(note.clone()).expect("reload");
        assert_eq!(reloaded.board.cards[0].column, Column::Done);
        assert!(reloaded.board.goal_done("g1"));
        assert!(!Board {
            goals: reloaded.board.goals.clone(),
            cards: Vec::new()
        }
        .goal_done("g1"));
        fs::remove_dir_all(root).expect("remove own fixture");
    }

    #[test]
    fn spawned_agent_link_rebases_over_external_note_edit() {
        let date = Date::from_calendar_date(2026, time::Month::September, 28).expect("date");
        let root = std::env::temp_dir().join(new_id("herdr-board-spawn-test").expect("id"));
        let note = WeekNote::for_date(&root, date).expect("note");
        let mut view = BoardView::from_note(note.clone()).expect("empty note");
        view.board.cards.push(Card {
            id: "c1".into(),
            title: "Verify".into(),
            description: String::new(),
            area: Area::Harness,
            column: Column::Todo,
            goal_id: None,
            agent_summary: String::new(),
            updates: Vec::new(),
            agents: Vec::new(),
        });
        assert!(view.persist());
        let edited = fs::read_to_string(&note.path)
            .expect("written note")
            .replace("- [ ] Verify", "- [ ] Verify edited");
        fs::write(&note.path, edited).expect("Obsidian edit");
        let link = AgentLink {
            host: "ub2".into(),
            pane_id: "pane-1".into(),
        };
        assert!(view.persist_spawn_link("c1", link.clone()));
        let reloaded = BoardView::from_note(note).expect("reload");
        let card = reloaded.board.card("c1").expect("card");
        assert_eq!(card.agents, vec![link]);
        assert_eq!(card.title, "Verify edited");
        assert_eq!(card.column, Column::InProgress);
        fs::remove_dir_all(root).expect("remove own fixture");
    }

    #[test]
    fn unknown_obsidian_content_is_rejected_instead_of_overwritten() {
        let date = Date::from_calendar_date(2026, time::Month::September, 28).expect("date");
        let note = WeekNote::for_date(Path::new("/vault"), date).expect("note");
        let text = format!("{}My private note\n", format_note(&Board::default(), &note));
        assert!(parse(&text).is_err());
    }

    #[test]
    fn new_ids_are_unique_and_board_moves_stop_at_edges() {
        assert_ne!(
            new_id("goal").expect("first"),
            new_id("goal").expect("second")
        );
        assert_eq!(Column::Draft.move_by(-1), Column::Draft);
        assert_eq!(Column::Draft.move_by(1), Column::Todo);
        assert_eq!(Column::Done.move_by(1), Column::Done);
    }

    #[test]
    fn worst_linked_agent_state_chooses_blocked_before_working_before_done() {
        assert_eq!(worst_lane([]), Lane::Working);
        assert_eq!(
            worst_lane([Lane::DoneAwaitingYou, Lane::DoneAwaitingYou]),
            Lane::DoneAwaitingYou
        );
        assert_eq!(
            worst_lane([Lane::DoneAwaitingYou, Lane::Working]),
            Lane::Working
        );
        assert_eq!(
            worst_lane([Lane::Working, Lane::Blocked, Lane::DoneAwaitingYou]),
            Lane::Blocked
        );
    }
}
