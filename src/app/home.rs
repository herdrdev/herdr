//! Home: the fleet at a glance, and one keystroke to whatever is waiting.
//!
//! The inbox next door answers "deal with these one at a time" and deliberately
//! shows a single agent, because clearing a queue is a loop. Home answers the
//! question you ask *before* that loop — is there anything worth entering it
//! for, and if so, which one — so it shows the whole queue and jumps.
//!
//! Like the inbox, the queue is derived on every read rather than cached, so an
//! agent that answers its own gate simply stops appearing. The only thing held
//! here is the cursor plus the draft dispatch settings, which cannot be
//! re-derived.

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};

use crate::{app::inbox::BlockedAgent, detect::Agent, ui::dropdown::DropdownFilterState};

use super::home_catalog::{
    ClaudeContextWindowForm, HomeCatalog, HomeProviderCatalog, AUTO_EFFORT, DEFAULT_CONTEXT_WINDOW,
    DEFAULT_MODEL, LARGE_CONTEXT_WINDOW,
};
use super::home_refs::HomeRef;

pub(crate) const HOME_COMPOSER_MIN_HEIGHT: u16 = 11;
pub(crate) const HOME_LENS_MIN_HEIGHT: u16 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HomeFocus {
    Reply,
    Prompt,
    Agent,
    Model,
    Effort,
    Access,
    Context,
    Project,
    Repo,
    Directory,
    Machine,
    Workspace,
    Ref,
    Target,
}

/// Which optional composer fields the card is currently showing.
///
/// Traversal has to skip hidden fields, and five positional booleans is how the
/// wrong one gets passed. Naming them costs one struct and removes that class
/// of bug entirely.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct HomeFieldVisibility {
    pub(crate) effort: bool,
    pub(crate) access: bool,
    pub(crate) context: bool,
    pub(crate) project: bool,
    pub(crate) repo: bool,
    pub(crate) machine: bool,
}

/// The card's field order, top row first.
///
/// `Reply` is deliberately absent: it is the blocked-queue row above the card,
/// not a field in it, so it hands off into this ring rather than sitting in it.
const FOCUS_RING: [HomeFocus; 13] = [
    HomeFocus::Prompt,
    HomeFocus::Agent,
    HomeFocus::Model,
    HomeFocus::Effort,
    HomeFocus::Access,
    HomeFocus::Context,
    HomeFocus::Project,
    HomeFocus::Repo,
    HomeFocus::Directory,
    HomeFocus::Machine,
    HomeFocus::Workspace,
    HomeFocus::Ref,
    HomeFocus::Target,
];

impl HomeFocus {
    /// A field the card is not drawing must not be reachable by Tab.
    fn shown(self, visibility: HomeFieldVisibility) -> bool {
        match self {
            Self::Effort => visibility.effort,
            Self::Access => visibility.access,
            Self::Context => visibility.context,
            Self::Project => visibility.project,
            Self::Repo => visibility.repo,
            Self::Machine => visibility.machine,
            _ => true,
        }
    }

    pub(crate) fn next(self, visibility: HomeFieldVisibility) -> Self {
        self.step(visibility, true)
    }

    pub(crate) fn previous(self, visibility: HomeFieldVisibility) -> Self {
        self.step(visibility, false)
    }

    fn step(self, visibility: HomeFieldVisibility, forward: bool) -> Self {
        if self == Self::Reply {
            return if forward { Self::Prompt } else { Self::Target };
        }
        let len = FOCUS_RING.len();
        let index = FOCUS_RING
            .iter()
            .position(|focus| *focus == self)
            .unwrap_or(0);
        (1..=len)
            .map(|offset| {
                let position = if forward {
                    index + offset
                } else {
                    index + len - offset
                };
                FOCUS_RING[position % len]
            })
            .find(|candidate| candidate.shown(visibility))
            .unwrap_or(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HomePicker {
    Agent,
    Model,
    Effort,
    Access,
    Context,
    Project,
    Repo,
    Directory,
    Machine,
    Workspace,
    Ref,
    Target,
}

impl HomePicker {
    pub(crate) fn for_focus(focus: HomeFocus) -> Option<Self> {
        match focus {
            HomeFocus::Reply => None,
            HomeFocus::Prompt => None,
            HomeFocus::Agent => Some(Self::Agent),
            HomeFocus::Model => Some(Self::Model),
            HomeFocus::Effort => Some(Self::Effort),
            HomeFocus::Access => Some(Self::Access),
            HomeFocus::Context => Some(Self::Context),
            HomeFocus::Project => Some(Self::Project),
            HomeFocus::Repo => Some(Self::Repo),
            HomeFocus::Directory => Some(Self::Directory),
            HomeFocus::Machine => Some(Self::Machine),
            HomeFocus::Workspace => Some(Self::Workspace),
            HomeFocus::Ref => Some(Self::Ref),
            HomeFocus::Target => Some(Self::Target),
        }
    }
}

/// Permission choices exposed by the home composer.
///
/// Labels stay provider-neutral in the card while each variant owns the exact
/// CLI spelling its provider accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HomeAccess {
    ClaudeDefault,
    ClaudeAcceptEdits,
    ClaudePlan,
    ClaudeBypass,
    CodexReadOnly,
    CodexWorkspaceWrite,
    CodexFull,
}

impl HomeAccess {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::ClaudeDefault => "default",
            Self::ClaudeAcceptEdits => "accept edits",
            Self::ClaudePlan => "plan",
            Self::ClaudeBypass => "bypass",
            Self::CodexReadOnly => "read-only",
            Self::CodexWorkspaceWrite => "workspace-write",
            Self::CodexFull => "full",
        }
    }

    fn flags(self) -> &'static [&'static str] {
        match self {
            Self::ClaudeDefault => &["--permission-mode", "default"],
            Self::ClaudeAcceptEdits => &["--permission-mode", "acceptEdits"],
            Self::ClaudePlan => &["--permission-mode", "plan"],
            Self::ClaudeBypass => &["--dangerously-skip-permissions"],
            Self::CodexReadOnly => &["-s", "read-only"],
            Self::CodexWorkspaceWrite => &["-s", "workspace-write"],
            Self::CodexFull => &[
                "-s",
                "danger-full-access",
                "--dangerously-bypass-approvals-and-sandbox",
            ],
        }
    }
}

const CLAUDE_ACCESS_OPTIONS: &[HomeAccess] = &[
    HomeAccess::ClaudeDefault,
    HomeAccess::ClaudeAcceptEdits,
    HomeAccess::ClaudePlan,
    HomeAccess::ClaudeBypass,
];

const CODEX_ACCESS_OPTIONS: &[HomeAccess] = &[
    HomeAccess::CodexReadOnly,
    HomeAccess::CodexWorkspaceWrite,
    HomeAccess::CodexFull,
];

/// Access mode a provider starts on before the operator picks one.
///
/// Claude opens on `bypass`: every agent this fork dispatches from a ticket or
/// the composer is meant to run unattended, and the default permission mode
/// stops on the first tool prompt with nobody watching the pane. Codex opens on
/// `full` for the same reason. Every other provider keeps the first (most
/// restricted) option.
pub(crate) fn default_access(agent: Agent) -> Option<HomeAccess> {
    match agent {
        Agent::Claude => Some(HomeAccess::ClaudeBypass),
        // Same reason as Claude's bypass: a sandboxed Codex stops on the first
        // write with nobody watching the pane.
        Agent::Codex => Some(HomeAccess::CodexFull),
        _ => access_options(agent).first().copied(),
    }
}

/// Model the composer opens on for a provider, before the operator picks one.
///
/// The catalog is probed from each CLI, so a preference is only a preference:
/// it applies when the probed catalog still carries that model and otherwise
/// falls back to the catalog's own first entry. Nothing here can name a model
/// the installed CLI would reject.
pub(crate) fn preferred_model(agent: Agent) -> Option<&'static str> {
    match agent {
        // The reasoning lane this fork dispatches unattended work to.
        Agent::Claude => Some("claude-opus-5"),
        Agent::Codex => Some("gpt-5.6-sol"),
        _ => None,
    }
}

/// Reasoning effort to open on, under the same catalog-membership rule as
/// [`preferred_model`].
pub(crate) fn preferred_effort(agent: Agent) -> Option<&'static str> {
    match agent {
        Agent::Codex => Some("medium"),
        _ => None,
    }
}

pub(crate) fn access_options(agent: Agent) -> &'static [HomeAccess] {
    match agent {
        Agent::Claude => CLAUDE_ACCESS_OPTIONS,
        Agent::Codex => CODEX_ACCESS_OPTIONS,
        _ => &[],
    }
}

/// Client-local selections retained per provider between Home openings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HomeAgentChoice {
    pub(crate) agent: Agent,
    pub(crate) model: String,
    pub(crate) effort: Option<String>,
    pub(crate) context_window: Option<String>,
    pub(crate) access: Option<HomeAccess>,
}

fn store_agent_choice(choices: &mut Vec<HomeAgentChoice>, choice: HomeAgentChoice) {
    if let Some(saved) = choices.iter_mut().find(|saved| saved.agent == choice.agent) {
        *saved = choice;
    } else {
        choices.push(choice);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HomeTarget {
    NewSpace,
    Existing(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HomeWorkspace {
    CurrentCheckout,
    NewWorktree,
    PreviousWorktree(PathBuf),
}

impl HomeWorkspace {
    pub(crate) fn label(&self) -> String {
        match self {
            Self::CurrentCheckout => "⌂ Current checkout".into(),
            Self::NewWorktree => "⎇ New worktree".into(),
            Self::PreviousWorktree(path) => {
                let name = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string());
                format!("↺ Previous worktree ({name})")
            }
        }
    }
}

/// The last row of the directory picker: type a path instead of picking one.
pub(crate) const BROWSE_OPTION_LABEL: &str = "Browse…";

/// The project importer follows the direct path input in the headline picker.
pub(crate) const ADD_PROJECT_OPTION_LABEL: &str = "+ Add project…";

/// Shown under the card when the typed path is not a directory.
pub(crate) const NO_SUCH_DIRECTORY: &str = "no such directory";

/// The directory picker lists what has been used, what the repository already
/// checked out, and one way to name anything else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HomeDirectoryOption {
    Recent(PathBuf),
    Worktree(PathBuf),
    Browse,
    AddProject,
}

impl HomeDirectoryOption {
    pub(crate) fn label(&self) -> String {
        match self {
            Self::Recent(path) => directory_label(path),
            Self::Worktree(path) => format!("⎇ {}", directory_label(path)),
            Self::Browse => BROWSE_OPTION_LABEL.to_string(),
            Self::AddProject => ADD_PROJECT_OPTION_LABEL.to_string(),
        }
    }

    pub(crate) fn path(&self) -> Option<&Path> {
        match self {
            Self::Recent(path) | Self::Worktree(path) => Some(path),
            Self::Browse | Self::AddProject => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum AddProjectTab {
    #[default]
    LocalFolder,
    GitUrl,
    GitHub,
}

impl AddProjectTab {
    pub(crate) const ALL: [Self; 3] = [Self::LocalFolder, Self::GitUrl, Self::GitHub];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::LocalFolder => "Local folder",
            Self::GitUrl => "Git URL",
            Self::GitHub => "GitHub",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AddProjectCloneRequest {
    pub(crate) url: String,
    pub(crate) target: PathBuf,
}

/// Client-local state for the centred project importer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AddProjectState {
    pub(crate) tab: AddProjectTab,
    pub(crate) browse: FolderBrowser,
    pub(crate) git_url: String,
    pub(crate) github_query: String,
    pub(crate) github_owner: String,
    pub(crate) github_repos: Vec<String>,
    pub(crate) github_filter: DropdownFilterState,
    pub(crate) github_loading: bool,
    pub(crate) github_refresh_request: Option<String>,
    pub(crate) clone_request: Option<AddProjectCloneRequest>,
    pub(crate) clone_pending: bool,
    pub(crate) error: Option<String>,
}

impl AddProjectState {
    pub(crate) fn starting_at(directory: &Path) -> Self {
        Self {
            tab: AddProjectTab::LocalFolder,
            browse: FolderBrowser::starting_at(directory),
            git_url: String::new(),
            github_query: String::new(),
            github_owner: String::new(),
            github_repos: Vec::new(),
            github_filter: DropdownFilterState::default(),
            github_loading: false,
            github_refresh_request: None,
            clone_request: None,
            clone_pending: false,
            error: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FolderBrowserEntry {
    pub(crate) name: String,
    pub(crate) is_dir: bool,
    pub(crate) branch: Option<String>,
}

/// Filesystem-backed state for the Add project folder dialog.
///
/// Entries are read only when navigation or filtering changes. Rendering stays
/// pure and never touches the filesystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FolderBrowser {
    pub(crate) directory: PathBuf,
    pub(crate) filter: String,
    pub(crate) entries: Vec<FolderBrowserEntry>,
    pub(crate) selected: usize,
    pub(crate) show_hidden: bool,
    pub(crate) error: Option<String>,
}

impl FolderBrowser {
    pub(crate) fn starting_at(directory: &Path) -> Self {
        let directory = crate::worktree::canonical_or_original(directory);
        let mut browser = Self {
            directory,
            filter: String::new(),
            entries: Vec::new(),
            selected: 0,
            show_hidden: false,
            error: None,
        };
        browser.refresh();
        browser
    }

    pub(crate) fn refresh(&mut self) {
        match folder_browser_entries(&self.directory, &self.filter, self.show_hidden) {
            Ok(entries) => {
                self.entries = entries;
                self.selected = self.selected.min(self.entries.len().saturating_sub(1));
                self.error = None;
            }
            Err(error) => {
                self.entries.clear();
                self.selected = 0;
                self.error = Some(error);
            }
        }
    }

    pub(crate) fn push_filter(&mut self, character: char) {
        self.filter.push(character);
        self.selected = 0;
        self.refresh();
    }

    pub(crate) fn backspace(&mut self) {
        if self.filter.pop().is_some() {
            self.selected = 0;
            self.refresh();
        } else {
            self.go_up();
        }
    }

    pub(crate) fn toggle_hidden(&mut self) {
        self.show_hidden = !self.show_hidden;
        self.selected = 0;
        self.refresh();
    }

    pub(crate) fn move_selection(&mut self, delta: i32) {
        if self.entries.is_empty() {
            self.selected = 0;
            return;
        }
        self.selected = if delta.is_negative() {
            self.selected.saturating_sub(delta.unsigned_abs() as usize)
        } else {
            self.selected.saturating_add(delta as usize)
        }
        .min(self.entries.len() - 1);
    }

    pub(crate) fn open_selected(&mut self) {
        self.open_entry(self.selected);
    }

    pub(crate) fn open_entry(&mut self, index: usize) {
        let Some(entry) = self.entries.get(index) else {
            return;
        };
        if !entry.is_dir {
            return;
        }
        self.directory.push(&entry.name);
        self.filter.clear();
        self.selected = 0;
        self.refresh();
    }

    pub(crate) fn jump_to(&mut self, directory: PathBuf) {
        if directory.is_dir() {
            self.directory = directory;
            self.filter.clear();
            self.selected = 0;
            self.refresh();
        }
    }

    pub(crate) fn go_up(&mut self) {
        let Some(parent) = self.directory.parent().map(Path::to_path_buf) else {
            return;
        };
        self.jump_to(parent);
    }

    pub(crate) fn path(&self) -> PathBuf {
        self.directory.clone()
    }
}

fn folder_browser_entries(
    directory: &Path,
    filter: &str,
    show_hidden: bool,
) -> Result<Vec<FolderBrowserEntry>, String> {
    let entries = std::fs::read_dir(directory)
        .map_err(|error| format!("Cannot read {}: {error}", directory.display()))?;
    let filter = filter.to_ascii_lowercase();
    let mut rows = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if (!show_hidden && name.starts_with('.'))
                || (!filter.is_empty() && !name.to_ascii_lowercase().contains(&filter))
            {
                return None;
            }
            let is_dir = entry.file_type().ok()?.is_dir();
            let branch = is_dir.then(|| git_head_label(&entry.path())).flatten();
            Some(FolderBrowserEntry {
                name,
                is_dir,
                branch,
            })
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        right
            .is_dir
            .cmp(&left.is_dir)
            .then_with(|| {
                left.name
                    .to_ascii_lowercase()
                    .cmp(&right.name.to_ascii_lowercase())
            })
            .then_with(|| left.name.cmp(&right.name))
    });
    rows.truncate(BROWSE_MAX_CHILDREN);
    Ok(rows)
}

fn git_head_label(directory: &Path) -> Option<String> {
    let dot_git = directory.join(".git");
    let head = if dot_git.is_dir() {
        dot_git.join("HEAD")
    } else {
        let git_file = std::fs::read_to_string(&dot_git).ok()?;
        let git_dir = git_file.trim().strip_prefix("gitdir: ")?;
        let git_dir = PathBuf::from(git_dir);
        let git_dir = if git_dir.is_absolute() {
            git_dir
        } else {
            directory.join(git_dir)
        };
        git_dir.join("HEAD")
    };
    let value = std::fs::read_to_string(head).ok()?;
    let value = value.trim();
    value
        .strip_prefix("ref: refs/heads/")
        .map(str::to_string)
        .or_else(|| (!value.is_empty()).then(|| value.chars().take(8).collect()))
}

/// The `Browse…` path input that takes over the picker's filter line.
///
/// The children are stored rather than read on demand: the picker is drawn
/// every frame and the render path must not touch the filesystem.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct HomeBrowse {
    pub(crate) input: String,
    pub(crate) children: Vec<String>,
    pub(crate) error: Option<String>,
}

/// A long directory would fill the screen with rows nobody reads; the input
/// narrows it faster than scrolling does.
const BROWSE_MAX_CHILDREN: usize = 100;

impl HomeBrowse {
    /// Start on the directory the composer already names, one separator in so
    /// its children are the first thing the list offers.
    pub(crate) fn starting_at(directory: &Path) -> Self {
        let mut input = directory.display().to_string();
        if !input.ends_with(std::path::MAIN_SEPARATOR) {
            input.push(std::path::MAIN_SEPARATOR);
        }
        let mut browse = Self {
            input,
            children: Vec::new(),
            error: None,
        };
        browse.refresh();
        browse
    }

    pub(crate) fn refresh(&mut self) {
        self.children = browse_children(&self.input);
        self.error = None;
    }

    pub(crate) fn push(&mut self, character: char) {
        self.input.push(character);
        self.refresh();
    }

    pub(crate) fn pop(&mut self) {
        self.input.pop();
        self.refresh();
    }

    /// `Tab`: take the next path component as far as the filesystem agrees.
    pub(crate) fn complete(&mut self) {
        if let Some(completed) = browse_completion(&self.input, &self.children) {
            self.input = completed;
            self.refresh();
        }
    }

    /// Click a listed child: adopt it and keep browsing below it.
    pub(crate) fn select_child(&mut self, index: usize) {
        let Some(child) = self.children.get(index).cloned() else {
            return;
        };
        let (parent, _) = browse_split(&self.input);
        let mut input = parent.join(child).display().to_string();
        input.push(std::path::MAIN_SEPARATOR);
        self.input = input;
        self.refresh();
    }

    pub(crate) fn path(&self) -> PathBuf {
        PathBuf::from(self.input.trim())
    }
}

/// `(directory to list, prefix the next component must start with)`.
///
/// Split on the typed text rather than on `Path`: a trailing `.` is a prefix
/// being typed here, and `Path` would resolve it away as the current directory.
fn browse_split(input: &str) -> (PathBuf, String) {
    match input.rfind(std::path::MAIN_SEPARATOR) {
        Some(index) => (
            PathBuf::from(&input[..=index]),
            input[index + 1..].to_string(),
        ),
        None => (PathBuf::new(), input.to_string()),
    }
}

/// Child directories of the typed prefix.
///
/// Hidden directories appear only once the typed component asks for one, so
/// browsing a home directory is not a wall of dotfiles.
pub(crate) fn browse_children(input: &str) -> Vec<String> {
    let (parent, fragment) = browse_split(input);
    let parent = if parent.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        parent
    };
    let Ok(entries) = std::fs::read_dir(&parent) else {
        return Vec::new();
    };
    let mut names = entries
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(&fragment))
        .filter(|name| fragment.starts_with('.') || !name.starts_with('.'))
        .collect::<Vec<_>>();
    names.sort();
    names.truncate(BROWSE_MAX_CHILDREN);
    names
}

/// The typed path extended by as much of the next component as every match
/// shares, plus a separator when only one directory can follow.
fn browse_completion(input: &str, children: &[String]) -> Option<String> {
    let (parent, _) = browse_split(input);
    let first = children.first()?;
    let common = children.iter().skip(1).fold(first.clone(), |common, name| {
        common
            .chars()
            .zip(name.chars())
            .take_while(|(left, right)| left == right)
            .map(|(left, _)| left)
            .collect()
    });
    if common.is_empty() {
        return None;
    }
    let mut completed = parent.join(&common).display().to_string();
    if children.len() == 1 {
        completed.push(std::path::MAIN_SEPARATOR);
    }
    (completed != input).then_some(completed)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HomePrContext {
    pub(crate) url: String,
    pub(crate) number: u64,
    pub(crate) repo: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HomeTicketContext {
    pub(crate) identifier: String,
    pub(crate) title: String,
    pub(crate) url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HomeMissiveContext {
    pub(crate) app_url: String,
    pub(crate) web_url: String,
    pub(crate) subject: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HomeDispatchPlan {
    pub(crate) agent: Agent,
    pub(crate) model: String,
    pub(crate) effort: Option<String>,
    pub(crate) directory: PathBuf,
    pub(crate) workspace: HomeWorkspace,
    pub(crate) git_ref: Option<HomeRef>,
    pub(crate) pr: Option<HomePrContext>,
    pub(crate) ticket: Option<HomeTicketContext>,
    pub(crate) missive: Option<HomeMissiveContext>,
    /// Manual context bound to the spawned pane in the same operation.
    pub(crate) work_context_patch: crate::work_context::PaneWorkContextPatch,
    pub(crate) target: HomeTarget,
    pub(crate) prompt: String,
    pub(crate) argv: Vec<String>,
    /// Environment the selected lane adds to the spawned pane.
    pub(crate) env: Vec<(String, String)>,
    /// The machine to dispatch on, when it is not this one.
    pub(crate) remote: Option<crate::app::machines::Machine>,
}

/// Shown when no pane in the selected directory has reported a branch yet.
pub(crate) const UNKNOWN_REF_LABEL: &str = "current branch";

/// Last path component, or the whole path when there is none (`/`).
fn directory_basename(directory: &Path) -> String {
    directory
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| directory.display().to_string())
}

/// The directory's own name, except for a home directory: its basename is the
/// account name, which reads as the machine rather than as the place.
fn directory_display_name(directory: &Path) -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if home.as_deref() == Some(directory) {
        return "~".to_string();
    }
    directory_basename(directory)
}

fn default_directory() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
}

/// A checkout offered by the new-thread project picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NewThreadOption {
    pub(crate) path: PathBuf,
    /// The directory name, which is what the row leads with.
    pub(crate) name: String,
    /// The project that claims this checkout, when more than one is configured.
    pub(crate) project: Option<String>,
}

pub(crate) fn directory_label(path: &Path) -> String {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return path.display().to_string();
    };
    path.strip_prefix(&home)
        .map(|relative| {
            if relative.as_os_str().is_empty() {
                "~".to_string()
            } else {
                format!("~/{}", relative.display())
            }
        })
        .unwrap_or_else(|_| path.display().to_string())
}

pub(crate) fn dispatchable_agents() -> &'static [Agent] {
    &[Agent::Claude, Agent::Codex]
}

/// Cursor and dispatch state for an open home view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HomeState {
    catalog: HomeCatalog,
    agent_choices: Vec<HomeAgentChoice>,
    selected: usize,
    pub(crate) focus: Option<HomeFocus>,
    pub(crate) prompt: String,
    pub(crate) reply: String,
    pub(crate) reply_error: Option<String>,
    pub(crate) agent: Agent,
    /// The lanes this composer offers, resolved from config at open time.
    /// Never empty: there is always at least one built-in per dispatchable
    /// agent, so the picker always has a row.
    profiles: Vec<crate::app::launch_profiles::LaunchProfile>,
    /// Id of the selected lane. Held as an id rather than an index so a config
    /// reload that reorders the list cannot silently repoint the selection.
    profile: String,
    /// Checkout groups this composer offers, resolved from config at open time.
    /// Empty when nothing is configured and `~/Repos` holds no checkouts.
    projects: Vec<crate::app::projects::Project>,
    /// Id of the selected group, held as an id for the same reason `profile` is.
    project: String,
    /// Name of the selected checkout inside that group, or nothing when the
    /// directory was reached some other way.
    repo: Option<String>,
    /// Machines this composer can dispatch to, resolved from the configured
    /// fleet at open time. Never empty: the local machine is always present.
    machines: Vec<crate::app::machines::Machine>,
    /// Name of the selected machine, held as a name for the same reason the
    /// lane and group selections are held as ids.
    machine: String,
    pub(crate) model: String,
    pub(crate) effort: Option<String>,
    pub(crate) access: Option<HomeAccess>,
    pub(crate) context_window: Option<String>,
    pub(crate) directory: PathBuf,
    pub(crate) workspace: HomeWorkspace,
    pub(crate) target: HomeTarget,
    pub(crate) picker: Option<HomePicker>,
    pub(crate) picker_selected: usize,
    pub(crate) directory_filter: DropdownFilterState,
    /// Set while the directory picker's filter line is a path input.
    pub(crate) browse: Option<HomeBrowse>,
    /// Centred add-project modal. TUI presentation state only.
    pub(crate) add_project: Option<AddProjectState>,
    /// Linked worktrees of the selected directory's repository, refreshed when
    /// the picker opens so the render path stays off `git`.
    pub(crate) worktree_options: Vec<PathBuf>,
    pub(crate) ref_filter: DropdownFilterState,
    pub(crate) selected_ref: Option<HomeRef>,
    /// Pull request that opened this composer. TUI-only launch context.
    pub(crate) pr: Option<HomePrContext>,
    /// Linear ticket that opened this composer. TUI-only launch context.
    pub(crate) ticket: Option<HomeTicketContext>,
    /// Missive conversation that opened this composer. TUI-only launch context.
    pub(crate) missive: Option<HomeMissiveContext>,
    /// Full runtime context the launch source attached to this draft.
    pub(crate) work_context_patch: crate::work_context::PaneWorkContextPatch,
    pub(crate) ref_repo_root: Option<PathBuf>,
    pub(crate) ref_directory: PathBuf,
    workspace_options: Vec<HomeWorkspace>,
    pub(crate) pending_dispatch: Option<HomeDispatchPlan>,
    pub(crate) dispatch_error: Option<String>,
}

impl Default for HomeState {
    fn default() -> Self {
        let mut home = Self {
            catalog: HomeCatalog::fallback(),
            agent_choices: Vec::new(),
            selected: 0,
            focus: Some(HomeFocus::Prompt),
            prompt: String::new(),
            reply: String::new(),
            reply_error: None,
            agent: Agent::Claude,
            profiles: crate::app::launch_profiles::resolve(&[]),
            profile: crate::detect::agent_label(Agent::Claude).to_ascii_lowercase(),
            projects: Vec::new(),
            project: String::new(),
            repo: None,
            machines: crate::app::machines::resolve(&crate::config::FleetConfig::default()),
            machine: String::new(),
            model: DEFAULT_MODEL.into(),
            effort: Some(AUTO_EFFORT.into()),
            access: default_access(Agent::Claude),
            context_window: None,
            directory: default_directory(),
            workspace: HomeWorkspace::CurrentCheckout,
            target: HomeTarget::NewSpace,
            picker: None,
            picker_selected: 0,
            directory_filter: DropdownFilterState::default(),
            browse: None,
            add_project: None,
            worktree_options: Vec::new(),
            ref_filter: DropdownFilterState::default(),
            selected_ref: None,
            pr: None,
            ticket: None,
            missive: None,
            work_context_patch: Default::default(),
            ref_repo_root: None,
            ref_directory: default_directory(),
            workspace_options: vec![HomeWorkspace::CurrentCheckout, HomeWorkspace::NewWorktree],
            pending_dispatch: None,
            dispatch_error: None,
        };
        // Resolve the opening provider through the same path a provider switch
        // takes, so the composer never opens on a selection the picker itself
        // would not have produced.
        home.apply_agent_choice(Agent::Claude);
        home
    }
}

impl HomeState {
    #[cfg(test)]
    pub(crate) fn test_with_prompt(prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            ..Self::default()
        }
    }

    #[cfg(test)]
    pub(crate) fn test_with_focus(focus: HomeFocus) -> Self {
        Self {
            focus: Some(focus),
            ..Self::default()
        }
    }

    #[cfg(test)]
    pub(crate) fn with_catalog(catalog: HomeCatalog) -> Self {
        let mut home = Self {
            catalog,
            ..Self::default()
        };
        home.apply_agent_choice(Agent::Claude);
        home
    }

    pub(crate) fn with_catalog_workspace_and_choices(
        catalog: HomeCatalog,
        workspace: HomeWorkspace,
        agent_choices: Vec<HomeAgentChoice>,
    ) -> Self {
        let mut home = Self {
            catalog,
            workspace,
            agent_choices,
            ..Self::default()
        };
        home.apply_agent_choice(Agent::Claude);
        home
    }

    pub(crate) fn replace_provider_catalog(&mut self, provider: HomeProviderCatalog) {
        let selected_agent = provider.agent == self.agent;
        self.catalog.replace(provider);
        if !selected_agent {
            return;
        }
        let selected_model_exists = self
            .catalog
            .provider(self.agent)
            .is_some_and(|provider| provider.model(&self.model).is_some());
        if !selected_model_exists {
            self.set_agent(self.agent);
        } else {
            let selected_effort_exists = self
                .effort
                .as_deref()
                .is_some_and(|effort| self.effort_options().iter().any(|known| known == effort));
            if !selected_effort_exists {
                self.effort = self.effort_options().first().cloned();
            }
            self.reconcile_context_window();
        }

        let picker_len = match self.picker {
            Some(HomePicker::Model) => Some(self.model_options().len()),
            Some(HomePicker::Effort) => Some(self.effort_options().len()),
            Some(HomePicker::Access) => Some(self.access_options().len()),
            Some(HomePicker::Context) => Some(self.context_options().len()),
            _ => None,
        };
        if let Some(picker_len) = picker_len {
            if picker_len == 0 {
                self.picker = None;
                self.picker_selected = 0;
            } else {
                self.picker_selected = self.picker_selected.min(picker_len - 1);
            }
        }
    }

    pub(crate) fn model_options(&self) -> &[super::home_catalog::HomeModelCatalogEntry] {
        self.catalog
            .provider(self.agent)
            .map(|provider| provider.models.as_slice())
            .unwrap_or(&[])
    }

    pub(crate) fn profile_models(&self, agent: Agent) -> String {
        self.catalog
            .provider(agent)
            .map(|provider| {
                provider
                    .models
                    .iter()
                    .take(3)
                    .map(|model| model.display_name.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .filter(|models| !models.is_empty())
            .unwrap_or_else(|| "—".into())
    }

    pub(crate) fn workspace_options(&self) -> &[HomeWorkspace] {
        &self.workspace_options
    }

    pub(crate) fn set_workspace_options(&mut self, options: Vec<HomeWorkspace>) {
        self.workspace_options = options;
        if !self.workspace_options.contains(&self.workspace) {
            self.workspace = self
                .workspace_options
                .first()
                .cloned()
                .unwrap_or(HomeWorkspace::CurrentCheckout);
        }
    }

    pub(crate) fn effort_options(&self) -> &[String] {
        self.catalog
            .provider(self.agent)
            .and_then(|provider| provider.model(&self.model))
            .map(|model| model.efforts.as_slice())
            .unwrap_or(&[])
    }

    pub(crate) fn access_options(&self) -> &'static [HomeAccess] {
        access_options(self.agent)
    }

    pub(crate) fn model_display_name(&self) -> &str {
        self.catalog
            .provider(self.agent)
            .and_then(|provider| provider.model(&self.model))
            .map(|model| model.display_name.as_str())
            .unwrap_or(&self.model)
    }

    pub(crate) fn profile_id(&self) -> &str {
        &self.profile
    }

    pub(crate) fn project_id(&self) -> &str {
        &self.project
    }

    pub(crate) fn context_options(&self) -> &'static [&'static str] {
        if self.context_visible() {
            &[DEFAULT_CONTEXT_WINDOW, LARGE_CONTEXT_WINDOW]
        } else {
            &[]
        }
    }

    /// The row the cursor is on, or nothing when the queue is empty.
    ///
    /// The cursor is clamped on read rather than on every queue change: agents
    /// block and unblock without anything telling the view, so a stored index is
    /// only ever a hint about where the operator was looking.
    pub(crate) fn current<'a>(&self, queue: &'a [BlockedAgent]) -> Option<&'a BlockedAgent> {
        queue.get(self.selected.min(queue.len().saturating_sub(1)))
    }

    /// Index of the selected row, clamped to the queue.
    pub(crate) fn selected(&self, queue: &[BlockedAgent]) -> usize {
        self.selected.min(queue.len().saturating_sub(1))
    }

    /// Put the cursor on a specific row. Clamped on read like every other
    /// cursor move, so a stale index from a click is harmless.
    pub(crate) fn select(&mut self, index: usize) {
        self.selected = index;
        self.reply_error = None;
    }

    /// Move down one row, stopping at the end.
    ///
    /// Deliberately does not wrap. The list is sorted by how long each agent has
    /// been waiting, so the ends mean something — arriving at the bottom should
    /// read as "that is all of them", not put you back on the oldest.
    pub(crate) fn select_next(&mut self, queue: &[BlockedAgent]) {
        self.reply_error = None;
        if queue.is_empty() {
            self.selected = 0;
            return;
        }
        self.selected = (self.selected(queue) + 1).min(queue.len() - 1);
    }

    /// Move up one row, stopping at the top.
    pub(crate) fn select_prev(&mut self, queue: &[BlockedAgent]) {
        self.selected = self.selected(queue).saturating_sub(1);
        self.reply_error = None;
    }

    /// First visible row, given how many rows fit.
    ///
    /// Scrolling is derived from the cursor instead of stored, so a queue that
    /// shrinks under the view cannot strand it on blank space below the list.
    pub(crate) fn scroll(&self, queue: &[BlockedAgent], visible_rows: usize) -> usize {
        if visible_rows == 0 || queue.len() <= visible_rows {
            return 0;
        }
        let selected = self.selected(queue);
        let max_scroll = queue.len() - visible_rows;
        selected.saturating_sub(visible_rows - 1).min(max_scroll)
    }

    pub(crate) fn effort_visible(&self) -> bool {
        self.effort.is_some()
    }

    pub(crate) fn access_visible(&self) -> bool {
        !self.profile().owns_its_flags() && self.access.is_some()
    }

    pub(crate) fn context_visible(&self) -> bool {
        !self.profile().owns_its_flags() && self.context_window.is_some()
    }

    /// One project is not a choice, so the chip only earns its width from two.
    pub(crate) fn project_visible(&self) -> bool {
        self.projects.len() > 1
    }

    /// A project with no checkouts yet has nothing to offer.
    pub(crate) fn repo_visible(&self) -> bool {
        self.project()
            .is_some_and(|project| !project.repos.is_empty())
    }

    /// One machine is not a choice, so the chip only earns its width from two.
    pub(crate) fn machine_visible(&self) -> bool {
        self.machines.len() > 1
    }

    pub(crate) fn machines(&self) -> &[crate::app::machines::Machine] {
        &self.machines
    }

    /// The selected machine, falling back to the local one. Never `None`:
    /// `resolve` always yields at least the local machine.
    pub(crate) fn machine(&self) -> Option<&crate::app::machines::Machine> {
        self.machines
            .iter()
            .find(|machine| machine.name == self.machine)
            .or_else(|| self.machines.first())
    }

    pub(crate) fn set_machines(&mut self, machines: Vec<crate::app::machines::Machine>) {
        self.machines = machines;
        if !self
            .machines
            .iter()
            .any(|machine| machine.name == self.machine)
        {
            self.machine = self
                .machines
                .first()
                .map(|machine| machine.name.clone())
                .unwrap_or_default();
        }
    }

    pub(crate) fn set_machine(&mut self, name: &str) {
        self.machine = name.to_string();
        self.dispatch_error = None;
    }

    pub(crate) fn field_visibility(&self) -> HomeFieldVisibility {
        HomeFieldVisibility {
            effort: self.effort_visible(),
            access: self.access_visible(),
            context: self.context_visible(),
            project: self.project_visible(),
            repo: self.repo_visible(),
            machine: self.machine_visible(),
        }
    }

    pub(crate) fn projects(&self) -> &[crate::app::projects::Project] {
        &self.projects
    }

    /// The selected project, or the first one when the selection is stale.
    pub(crate) fn project(&self) -> Option<&crate::app::projects::Project> {
        self.projects
            .iter()
            .find(|project| project.id == self.project)
            .or_else(|| self.projects.first())
    }

    pub(crate) fn repo_options(&self) -> &[crate::app::projects::ProjectRepo] {
        self.project()
            .map(|project| project.repos.as_slice())
            .unwrap_or(&[])
    }

    /// What the repo chip shows. No selection is not an error: a fresh composer
    /// still points at whatever directory it opened on.
    pub(crate) fn repo_label(&self) -> &str {
        self.repo.as_deref().unwrap_or("repo")
    }

    /// Replace the resolved project list, keeping a selection that survives.
    pub(crate) fn set_projects(&mut self, projects: Vec<crate::app::projects::Project>) {
        self.projects = projects;
        if !self
            .projects
            .iter()
            .any(|project| project.id == self.project)
        {
            self.project = self
                .projects
                .first()
                .map(|project| project.id.clone())
                .unwrap_or_default();
            self.repo = None;
        }
    }

    /// Switching project drops the repo: a checkout name means nothing outside
    /// the group it was chosen from.
    pub(crate) fn set_project(&mut self, id: &str) {
        if self.project == id {
            return;
        }
        self.project = id.to_string();
        self.repo = None;
        self.dispatch_error = None;
    }

    /// Point the composer at a directory and align its project fields with the
    /// most specific configured checkout that contains it.
    pub(crate) fn set_directory(&mut self, directory: PathBuf) {
        let canonical_directory = crate::worktree::canonical_or_original(&directory);
        let matched = self
            .projects
            .iter()
            .flat_map(|project| {
                project.repos.iter().filter_map(|repo| {
                    let path = crate::worktree::canonical_or_original(&repo.path);
                    canonical_directory.starts_with(&path).then(|| {
                        (
                            path.components().count(),
                            project.id.clone(),
                            repo.name.clone(),
                        )
                    })
                })
            })
            .max_by_key(|(depth, _, _)| *depth);

        self.directory = directory;
        if let Some((_, project, repo)) = matched {
            self.set_project(&project);
            self.repo = Some(repo);
        } else {
            self.repo = None;
        }
    }

    pub(crate) fn move_focus(&mut self, backwards: bool) {
        let current = self.focus.unwrap_or(HomeFocus::Prompt);
        let visibility = self.field_visibility();
        self.focus = Some(if backwards {
            current.previous(visibility)
        } else {
            current.next(visibility)
        });
    }

    pub(crate) fn close_composer_or_home(&mut self) -> bool {
        if self.pending_dispatch.is_some() {
            return false;
        }
        if self.picker.take().is_some() {
            return false;
        }
        if self.focus.take().is_some() {
            return false;
        }
        true
    }

    pub(crate) fn profiles(&self) -> &[crate::app::launch_profiles::LaunchProfile] {
        &self.profiles
    }

    /// The selected lane. Falls back to the first rather than returning
    /// `None`: an id that no longer resolves must not leave the composer with
    /// no lane at all.
    pub(crate) fn profile(&self) -> &crate::app::launch_profiles::LaunchProfile {
        self.profiles
            .iter()
            .find(|profile| profile.id == self.profile)
            .or_else(|| self.profiles.first())
            .expect("resolve() always yields at least one lane")
    }

    pub(crate) fn set_profiles(
        &mut self,
        profiles: Vec<crate::app::launch_profiles::LaunchProfile>,
    ) {
        if profiles.is_empty() {
            return;
        }
        self.profiles = profiles;
        let id = self.profile().id.clone();
        self.set_profile(&id);
    }

    pub(crate) fn set_profile(&mut self, id: &str) {
        let Some(profile) = self.profiles.iter().find(|profile| profile.id == id) else {
            return;
        };
        self.profile = profile.id.clone();
        let agent = profile.agent;
        self.set_agent(agent);
    }

    pub(crate) fn set_agent(&mut self, agent: Agent) {
        self.remember_current_choice();
        self.apply_agent_choice(agent);
    }

    fn apply_agent_choice(&mut self, agent: Agent) {
        self.agent = agent;
        let saved = self
            .agent_choices
            .iter()
            .find(|choice| choice.agent == agent)
            .cloned();
        let provider = self.catalog.provider(agent);
        self.model = saved
            .as_ref()
            .filter(|choice| {
                provider.is_some_and(|provider| provider.model(&choice.model).is_some())
            })
            .map(|choice| choice.model.clone())
            .or_else(|| {
                preferred_model(agent)
                    .filter(|model| {
                        provider.is_some_and(|provider| provider.model(model).is_some())
                    })
                    .map(str::to_string)
            })
            .or_else(|| {
                provider
                    .and_then(|provider| provider.models.first())
                    .map(|model| model.id.clone())
            })
            .unwrap_or_default();
        let efforts = provider
            .and_then(|provider| provider.model(&self.model))
            .map(|model| model.efforts.as_slice())
            .unwrap_or(&[]);
        self.effort = saved
            .as_ref()
            .and_then(|choice| choice.effort.clone())
            .filter(|effort| efforts.contains(effort))
            .or_else(|| {
                preferred_effort(agent)
                    .filter(|preferred| efforts.iter().any(|effort| effort == preferred))
                    .map(str::to_string)
            })
            .or_else(|| efforts.first().cloned());
        let options = access_options(agent);
        self.access = saved
            .as_ref()
            .and_then(|choice| choice.access)
            .filter(|access| options.contains(access))
            .or_else(|| default_access(agent));
        self.context_window = saved.and_then(|choice| choice.context_window);
        self.reconcile_context_window();
        self.remember_current_choice();
    }

    pub(crate) fn set_model(&mut self, model: impl Into<String>) {
        let model = model.into();
        let efforts = self
            .catalog
            .provider(self.agent)
            .and_then(|provider| provider.model(&model))
            .map(|model| model.efforts.clone())
            .unwrap_or_default();
        if efforts.is_empty() {
            return;
        }
        self.model = model;
        if !self
            .effort
            .as_deref()
            .is_some_and(|effort| efforts.iter().any(|known| known == effort))
        {
            self.effort = efforts.first().cloned();
        }
        self.reconcile_context_window();
        self.remember_current_choice();
    }

    pub(crate) fn set_effort(&mut self, effort: Option<String>) {
        self.effort = effort;
        self.remember_current_choice();
    }

    pub(crate) fn set_access(&mut self, access: HomeAccess) {
        if self.access_options().contains(&access) {
            self.access = Some(access);
            self.remember_current_choice();
        }
    }

    pub(crate) fn set_context_window(&mut self, context_window: Option<String>) {
        self.context_window = context_window;
        self.reconcile_context_window();
        self.remember_current_choice();
    }

    fn remember_current_choice(&mut self) {
        let choice = HomeAgentChoice {
            agent: self.agent,
            model: self.model.clone(),
            effort: self.effort.clone(),
            context_window: self.context_window.clone(),
            access: self.access,
        };
        store_agent_choice(&mut self.agent_choices, choice);
    }

    pub(crate) fn saved_agent_choices(&self) -> Vec<HomeAgentChoice> {
        let mut choices = self.agent_choices.clone();
        store_agent_choice(
            &mut choices,
            HomeAgentChoice {
                agent: self.agent,
                model: self.model.clone(),
                effort: self.effort.clone(),
                context_window: self.context_window.clone(),
                access: self.access,
            },
        );
        choices
    }

    fn reconcile_context_window(&mut self) {
        let supports_large_context = self.agent == Agent::Claude
            && self
                .catalog
                .provider(self.agent)
                .and_then(|provider| provider.model(&self.model))
                .is_some_and(|model| model.supports_large_context);
        if supports_large_context {
            let selected_is_valid = self.context_window.as_deref().is_some_and(|context| {
                matches!(context, DEFAULT_CONTEXT_WINDOW | LARGE_CONTEXT_WINDOW)
            });
            if !selected_is_valid {
                self.context_window = Some(LARGE_CONTEXT_WINDOW.into());
            }
        } else {
            self.context_window = None;
        }
    }

    pub(crate) fn append_prompt(&mut self, character: char) {
        self.prompt.push(character);
        self.dispatch_error = None;
    }

    pub(crate) fn append_reply(&mut self, character: char) {
        self.reply.push(character);
        self.reply_error = None;
    }

    pub(crate) fn backspace_reply(&mut self) {
        self.reply.pop();
        self.reply_error = None;
    }

    pub(crate) fn backspace_prompt(&mut self) {
        self.prompt.pop();
        self.dispatch_error = None;
    }

    /// The flags a dispatch would pass to the selected agent right now, for
    /// the settings Providers rows.
    pub(crate) fn launch_flags(&self) -> Option<Vec<String>> {
        let catalog = self.catalog.provider(self.agent)?;
        agent_launch_flags(
            self.agent,
            catalog,
            &self.model,
            self.effort.as_deref().unwrap_or(AUTO_EFFORT),
            self.access,
            self.context_window.as_deref(),
        )
    }

    pub(crate) fn dispatch_plan(&self) -> Result<HomeDispatchPlan, String> {
        let prompt = self.prompt.trim();
        if prompt.is_empty() {
            return Err("enter a prompt before dispatching".into());
        }
        let profile = self.profile();
        if profile.owns_its_flags() {
            // The lane supplies its own model, effort and permission flags, so
            // there is nothing here to validate against the probed catalog and
            // nothing of Herdr's to append but the prompt itself.
            let mut argv = profile.command.clone();
            argv.push(prompt.into());
            return Ok(self.plan_with(argv, profile.env.clone(), prompt));
        }
        let Some(catalog) = self.catalog.provider(self.agent) else {
            return Err("that agent cannot be dispatched from home".into());
        };
        let Some(model) = catalog.model(&self.model) else {
            return Err("select a model supported by that agent".into());
        };
        let effort = self.effort.as_deref().unwrap_or(AUTO_EFFORT);
        if !model.efforts.iter().any(|known| known == effort) {
            return Err("select an effort supported by that model".into());
        }
        if self.context_window.is_some() && !model.supports_large_context {
            return Err("select a context window supported by that model".into());
        }
        if self.context_window.as_deref().is_some_and(|context| {
            !matches!(context, DEFAULT_CONTEXT_WINDOW | LARGE_CONTEXT_WINDOW)
        }) {
            return Err("select a supported context window".into());
        }
        if !self
            .access
            .is_some_and(|access| self.access_options().contains(&access))
        {
            return Err("select an access mode supported by that agent".into());
        }

        let mut argv = vec![crate::detect::interactive_agent_executable(self.agent).into()];
        let Some(flags) = agent_launch_flags(
            self.agent,
            catalog,
            &self.model,
            effort,
            self.access,
            self.context_window.as_deref(),
        ) else {
            return Err("that agent cannot be dispatched from home".into());
        };
        argv.extend(flags);
        argv.push(prompt.into());

        Ok(self.plan_with(argv, self.profile().env.clone(), prompt))
    }

    /// The plan shared by both dispatch paths: everything that does not depend
    /// on how the argv was built.
    fn plan_with(
        &self,
        argv: Vec<String>,
        env: Vec<(String, String)>,
        prompt: &str,
    ) -> HomeDispatchPlan {
        let directory = match &self.workspace {
            HomeWorkspace::PreviousWorktree(path) => path.clone(),
            HomeWorkspace::CurrentCheckout | HomeWorkspace::NewWorktree => self.directory.clone(),
        };
        let mut work_context_patch = self.work_context_patch.clone();
        if work_context_patch.repo.is_none() {
            work_context_patch.repo = self.pr.as_ref().map(|pr| pr.repo.clone());
        }
        if work_context_patch.pr_urls.is_none() {
            work_context_patch.pr_urls = self.pr.as_ref().map(|pr| vec![pr.url.clone()]);
        }
        if work_context_patch.ticket_ids.is_none() {
            work_context_patch.ticket_ids = self
                .ticket
                .as_ref()
                .map(|ticket| vec![ticket.identifier.clone()]);
        }
        if work_context_patch.missive_urls.is_none() {
            work_context_patch.missive_urls = self
                .missive
                .as_ref()
                .map(|conversation| vec![conversation.web_url.clone()]);
        }
        HomeDispatchPlan {
            agent: self.agent,
            model: self.model.clone(),
            effort: self.effort.clone(),
            directory,
            workspace: self.workspace.clone(),
            git_ref: self.selected_ref.clone(),
            pr: self.pr.clone(),
            ticket: self.ticket.clone(),
            missive: self.missive.clone(),
            work_context_patch,
            target: self.target.clone(),
            prompt: prompt.into(),
            argv,
            env,
            remote: self
                .machine()
                .filter(|machine| !machine.is_local())
                .cloned(),
        }
    }
}

/// The flags Herdr passes to an agent after its executable, for the given
/// model, effort and context window.
///
/// The dispatch path and the settings Providers section read the same builder,
/// so the flags a row advertises are the flags a launch actually uses.
/// `None` means Herdr cannot launch that agent.
pub(crate) fn agent_launch_flags(
    agent: Agent,
    catalog: &crate::app::home_catalog::HomeProviderCatalog,
    model: &str,
    effort: &str,
    access: Option<HomeAccess>,
    context_window: Option<&str>,
) -> Option<Vec<String>> {
    let mut flags: Vec<String> = Vec::new();
    match agent {
        Agent::Claude => {
            let large_context = context_window == Some(LARGE_CONTEXT_WINDOW);
            let context_form = catalog.claude_context_form.as_ref();
            let model_arg = if large_context
                && !matches!(context_form, Some(ClaudeContextWindowForm::Flag(_)))
            {
                format!("{model}[1m]")
            } else {
                model.to_string()
            };
            if model != DEFAULT_MODEL {
                flags.extend(["--model".into(), model_arg]);
            }
            if large_context {
                if let Some(ClaudeContextWindowForm::Flag(flag)) = context_form {
                    flags.extend([flag.clone(), "1m".into()]);
                }
            }
            if effort != AUTO_EFFORT {
                flags.extend(["--effort".into(), effort.into()]);
            }
        }
        Agent::Codex => {
            if model != DEFAULT_MODEL {
                flags.extend(["--model".into(), model.to_string()]);
            }
            if effort != AUTO_EFFORT {
                flags.extend(["-c".into(), format!("model_reasoning_effort={effort}")]);
            }
        }
        _ => return None,
    }
    let access = access?;
    if !access_options(agent).contains(&access) {
        return None;
    }
    flags.extend(access.flags().iter().map(|flag| (*flag).to_string()));
    Some(flags)
}

/// Fleet-wide counts for the header line.
///
/// `agents` counts panes running a recognised agent, not panes: a shell you left
/// open is not something the fleet is doing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct HomeCounts {
    pub blocked: usize,
    pub attention: usize,
    pub agents: usize,
    pub spaces: usize,
}

pub(crate) fn home_workspace_options_from_entries(
    entries: &[crate::app::state::WorktreeOpenEntry],
) -> Vec<HomeWorkspace> {
    let mut options = vec![HomeWorkspace::CurrentCheckout, HomeWorkspace::NewWorktree];
    options.extend(
        entries
            .iter()
            .filter(|entry| entry.is_linked_worktree)
            .map(|entry| HomeWorkspace::PreviousWorktree(entry.path.clone())),
    );
    options
}

impl crate::app::state::AppState {
    pub(crate) fn pane_id_for_terminal(
        &self,
        terminal_id: &crate::terminal::TerminalId,
    ) -> Option<crate::layout::PaneId> {
        self.workspaces
            .iter()
            .flat_map(|workspace| workspace.tabs.iter())
            .flat_map(|tab| tab.panes.iter())
            .find_map(|(pane_id, pane)| {
                (pane.attached_terminal_id == *terminal_id).then_some(*pane_id)
            })
    }

    pub(crate) fn note_human_key(
        &mut self,
        pane_id: crate::layout::PaneId,
        key: &crate::input::TerminalKey,
    ) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        let has_terminal_modifier = key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER);
        if has_terminal_modifier {
            return;
        }
        // This hook runs only after pane bytes are successfully sent; unlike
        // agent-specific prompt hooks, it covers every supported agent.
        self.update_terminal_state(pane_id, |terminal| {
            terminal.note_user_reply();
            None
        });
        match key.code {
            KeyCode::Char(character) => {
                let character = key
                    .shifted_codepoint
                    .and_then(char::from_u32)
                    .unwrap_or(character);
                let draft = self.pending_human_drafts.entry(pane_id).or_default();
                for _ in 0..key.repeat_count.max(1) {
                    draft.push(character);
                }
            }
            KeyCode::Backspace => {
                if let Some(draft) = self.pending_human_drafts.get_mut(&pane_id) {
                    for _ in 0..key.repeat_count.max(1) {
                        draft.pop();
                    }
                    if draft.is_empty() {
                        self.pending_human_drafts.remove(&pane_id);
                    }
                }
            }
            KeyCode::Enter => {
                self.pending_human_drafts.remove(&pane_id);
            }
            _ => {}
        }
    }

    pub(crate) fn note_human_text(&mut self, pane_id: crate::layout::PaneId, text: &str) {
        if text.is_empty() {
            return;
        }
        self.update_terminal_state(pane_id, |terminal| {
            terminal.note_user_reply();
            None
        });
        if text.contains(['\r', '\n']) {
            self.pending_human_drafts.remove(&pane_id);
            return;
        }
        self.pending_human_drafts
            .entry(pane_id)
            .or_default()
            .push_str(text);
    }

    pub(crate) fn note_human_bytes(&mut self, pane_id: crate::layout::PaneId, bytes: &[u8]) {
        let text = String::from_utf8_lossy(bytes);
        let mut characters = text.chars().peekable();
        while let Some(character) = characters.next() {
            if character == '\u{1b}' {
                if characters
                    .peek()
                    .is_some_and(|next| matches!(next, '[' | 'O'))
                {
                    characters.next();
                    for sequence_character in characters.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&sequence_character) {
                            break;
                        }
                    }
                }
                continue;
            }
            match character {
                '\r' | '\n' => {
                    self.pending_human_drafts.remove(&pane_id);
                }
                '\u{7f}' | '\u{8}' => {
                    if let Some(draft) = self.pending_human_drafts.get_mut(&pane_id) {
                        draft.pop();
                        if draft.is_empty() {
                            self.pending_human_drafts.remove(&pane_id);
                        }
                    }
                }
                '\0'..='\u{1f}' => {}
                _ => self
                    .pending_human_drafts
                    .entry(pane_id)
                    .or_default()
                    .push(character),
            }
        }
    }

    pub(crate) fn toggle_home(&mut self) {
        if self.home.is_some() {
            self.clear_home();
        } else {
            // Home and the inbox both want the whole frame; opening one puts
            // the other away rather than stacking two overlays.
            self.inbox = None;
            self.board_view = None;
            self.home = Some(self.new_home_state());
        }
        if self.home.is_some() {
            // Resolve the directory's repository once, here: the headline names
            // it on every frame and the render path must not run `git`.
            self.reset_home_ref_context(false);
        }
    }

    /// The workspace a freshly opened composer preselects, from
    /// `ui.new_thread_workspace`.
    pub(crate) fn default_home_workspace(&self) -> HomeWorkspace {
        match self.new_thread_workspace {
            crate::config::NewThreadWorkspaceConfig::CurrentCheckout => {
                HomeWorkspace::CurrentCheckout
            }
            crate::config::NewThreadWorkspaceConfig::NewWorktree => HomeWorkspace::NewWorktree,
        }
    }

    pub(crate) fn new_home_state(&self) -> HomeState {
        let mut home = HomeState::with_catalog_workspace_and_choices(
            self.home_catalog.clone(),
            self.default_home_workspace(),
            self.home_agent_choices.clone(),
        );
        home.set_profiles(self.launch_profiles.clone());
        home.set_projects(self.projects.clone());
        home.set_machines(self.machines.clone());
        if let Some(profile) = self.next_home_profile.as_deref() {
            home.set_profile(profile);
        }
        if let Some(machine) = self.next_home_machine.as_deref() {
            home.set_machine(machine);
        }
        home
    }

    /// Use the first configured launch profile that has a quota window left
    /// for the next Home dispatch on this machine. Profile order is the local
    /// config's ranking; this changes only future Home launches.
    pub(crate) fn set_next_home_target(&mut self, machine: &str) -> bool {
        if !self
            .machines
            .iter()
            .any(|candidate| candidate.name == machine)
        {
            return false;
        }
        let profile = self.launch_profiles.iter().find(|profile| {
            profile.quota.is_some_and(|quota| {
                quota
                    .usage(&self.provider_usage)
                    .peak_percent()
                    .is_some_and(|used| used < 100)
            })
        });
        let Some(profile_id) = profile.map(|profile| profile.id.clone()) else {
            return false;
        };
        self.next_home_machine = Some(machine.to_string());
        self.next_home_profile = Some(profile_id.clone());
        if let Some(home) = self.home.as_mut() {
            home.set_machine(machine);
            home.set_profile(&profile_id);
        }
        true
    }

    pub(crate) fn home_target_for_directory(&self, directory: &Path) -> HomeTarget {
        let direct_match = self
            .workspaces
            .iter()
            .find(|workspace| {
                !workspace.repo_binding_cleared
                    && (workspace.identity_cwd == directory
                        || workspace
                            .tabs
                            .iter()
                            .flat_map(|tab| tab.panes.values())
                            .any(|pane| {
                                self.terminals
                                    .get(&pane.attached_terminal_id)
                                    .is_some_and(|terminal| terminal.cwd == directory)
                            }))
            })
            .map(|workspace| workspace.id.clone());
        let observed_repo = self.terminals.values().find_map(|terminal| {
            (terminal.cwd == directory)
                .then(|| terminal.work_context.git_observed_repo())
                .flatten()
        });
        let matched_id = direct_match.or_else(|| {
            let repo = observed_repo?;
            self.workspaces
                .iter()
                .find(|workspace| {
                    !workspace.repo_binding_cleared
                        && workspace.repo_binding.as_deref().is_some_and(|binding| {
                            crate::work_context::repo_slugs_match(binding, repo)
                        })
                })
                .map(|workspace| workspace.id.clone())
        });
        matched_id
            .map(HomeTarget::Existing)
            .unwrap_or(HomeTarget::NewSpace)
    }

    /// Open home as the launch screen, if the config wants it.
    ///
    /// Deliberately not done in `App::new`: that constructor is what dozens of
    /// tests build from, and a full-frame overlay that covers the panes and
    /// swallows input would change what every one of them is testing. Launching
    /// is a concern of the thing that starts a session, not of the constructor.
    pub(crate) fn open_home_on_launch(&mut self, config: &crate::config::Config) {
        if config.ui.show_home_on_start {
            self.home = Some(self.new_home_state());
            self.inbox = None;
            self.reset_home_ref_context(false);
        }
    }

    /// Build the same launch plan as the home composer without opening it.
    /// Unassigned sidebar rows use this for their one-key spawn action.
    pub(crate) fn sidebar_unassigned_dispatch_plan(
        &self,
        key: &str,
    ) -> Result<HomeDispatchPlan, String> {
        let activation = crate::ui::sidebar_work_group_activation(self, key)
            .ok_or_else(|| "unassigned object is no longer available".to_string())?;
        let mut home = self.new_home_state();
        home.prompt = activation.spawn_prompt;
        home.pr = activation.pr;
        home.ticket = activation.ticket;
        home.missive = activation.missive;
        home.work_context_patch = activation.work_context_patch;
        home.selected_ref = activation.git_ref;
        // MAT-159 AC4: an aloop finding launches on the producer host, not
        // wherever the composer happened to point last.
        if let Some(machine) = activation.machine.as_deref() {
            if home.machines().iter().any(|entry| entry.name == machine) {
                home.set_machine(machine);
            } else if machine != self.agent_host_name {
                return Err(format!(
                    "aloop producer host `{machine}` is not a configured machine"
                ));
            }
        }
        if let Some(directory) = activation.directory {
            home.set_directory(directory.clone());
            home.ref_directory = directory.clone();
            if home.workspace == HomeWorkspace::CurrentCheckout {
                home.target = self.home_target_for_directory(&directory);
            }
        }
        home.dispatch_plan()
    }

    pub(crate) fn open_home_composer_in_directory(
        &mut self,
        directory: PathBuf,
        workspace: HomeWorkspace,
    ) {
        // The composer is a typing surface, so it takes the keyboard the same
        // way a pane does. The picker that opens it hands the sidebar keyboard
        // focus first, and a sidebar that still owns bare keys answers Enter
        // and `n` itself instead of letting them reach the prompt.
        self.release_surface_focus_to_pane();
        let mut home = self.home.take().unwrap_or_else(|| self.new_home_state());
        home.prompt.clear();
        home.set_directory(directory.clone());
        home.workspace = workspace;
        if home.workspace == HomeWorkspace::CurrentCheckout {
            home.target = self.home_target_for_directory(&directory);
        }
        home.focus = Some(HomeFocus::Prompt);
        home.picker = None;
        self.inbox = None;
        self.home = Some(home);
        self.reset_home_ref_context(false);
    }

    /// Open the existing Add project modal from the sidebar header.
    pub(crate) fn open_add_project_from_sidebar(&mut self) {
        let start = self.home_browse_start_directory();
        let mut home = self.home.take().unwrap_or_else(|| self.new_home_state());
        home.picker = None;
        home.browse = None;
        home.directory_filter.set_query("");
        home.add_project = Some(AddProjectState::starting_at(&start));
        self.inbox = None;
        self.home = Some(home);
        self.reset_home_ref_context(false);
    }

    pub(crate) fn open_folder_from_sidebar(&mut self) {
        let start = self.home_browse_start_directory();
        let mut home = self.home.take().unwrap_or_else(|| self.new_home_state());
        home.focus = Some(HomeFocus::Directory);
        home.picker = Some(HomePicker::Directory);
        home.browse = Some(HomeBrowse::starting_at(&start));
        home.directory_filter.set_query("");
        home.add_project = None;
        self.inbox = None;
        self.home = Some(home);
        self.reset_home_ref_context(false);
    }

    pub(crate) fn clear_home(&mut self) {
        if let Some(home) = self.home.take() {
            self.home_agent_choices = home.saved_agent_choices();
        }
    }

    pub(crate) fn home_counts(&self, queue: &[BlockedAgent]) -> HomeCounts {
        let agents = self
            .workspaces
            .iter()
            .flat_map(|ws| ws.tabs.iter())
            .flat_map(|tab| tab.panes.values())
            .filter(|pane| {
                self.terminals
                    .get(&pane.attached_terminal_id)
                    .is_some_and(|terminal| terminal.effective_agent_label().is_some())
            })
            .count();
        HomeCounts {
            blocked: queue
                .iter()
                .filter(|agent| {
                    agent.attention_tier == crate::terminal::state::AttentionTier::Blocked
                })
                .count(),
            attention: queue
                .iter()
                .filter(|agent| {
                    agent.attention_tier == crate::terminal::state::AttentionTier::Attention
                })
                .count(),
            agents,
            spaces: self.workspaces.len(),
        }
    }

    /// One row of the new-thread project picker: a checkout the composer can
    /// open a thread in.
    ///
    /// Configured projects come first because picking the repository is the
    /// question the picker exists to answer, and the directories this session
    /// already touched follow, so a worktree no project root scans stays one
    /// keystroke away.
    pub(crate) fn new_thread_options(&self) -> Vec<NewThreadOption> {
        let name_of = |path: &Path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string())
        };
        // One project is the implicit `~/Repos` group or a single configured
        // one, and naming it on every row would say nothing.
        let name_projects = self.projects.len() > 1;
        let mut options: Vec<NewThreadOption> = Vec::new();
        for project in &self.projects {
            for repo in &project.repos {
                if options.iter().any(|option| option.path == repo.path) {
                    continue;
                }
                options.push(NewThreadOption {
                    path: repo.path.clone(),
                    name: repo.name.clone(),
                    project: name_projects.then(|| project.label.clone()),
                });
            }
        }
        for path in self.home_directory_options() {
            if options.iter().any(|option| option.path == path) {
                continue;
            }
            options.push(NewThreadOption {
                name: name_of(&path),
                path,
                project: None,
            });
        }
        options
    }

    pub(crate) fn home_directory_options(&self) -> Vec<PathBuf> {
        let mut options = Vec::new();
        if let Some(home) = &self.home {
            options.push(home.directory.clone());
        }
        if let Ok(current) = std::env::current_dir() {
            if !options.contains(&current) {
                options.push(current);
            }
        }
        for cwd in self
            .workspaces
            .iter()
            .map(|workspace| workspace.identity_cwd.clone())
        {
            if !options.contains(&cwd) {
                options.push(cwd);
            }
        }
        options
    }

    /// The rows the directory picker shows, in the order it shows them:
    /// what has been used, then the repository's linked worktrees, then the
    /// one row that can name a directory neither list has.
    pub(crate) fn home_directory_picker_options(&self) -> Vec<HomeDirectoryOption> {
        let mut options = self
            .home_directory_options()
            .into_iter()
            .map(HomeDirectoryOption::Recent)
            .collect::<Vec<_>>();
        if let Some(home) = self.home.as_ref() {
            for path in &home.worktree_options {
                if options
                    .iter()
                    .any(|option| option.path() == Some(path.as_path()))
                {
                    continue;
                }
                options.push(HomeDirectoryOption::Worktree(path.clone()));
            }
        }
        options.push(HomeDirectoryOption::Browse);
        options.push(HomeDirectoryOption::AddProject);
        options
    }

    fn refresh_home_worktree_options(&mut self) {
        let directory = self.home_directory();
        let paths = super::worktrees::worktree_repo_root(&directory)
            .and_then(|repo_root| {
                super::worktrees::worktree_entries_for_repo(&repo_root, |_| None).ok()
            })
            .unwrap_or_default()
            .into_iter()
            .filter(|entry| entry.is_linked_worktree)
            .map(|entry| entry.path)
            .collect();
        if let Some(home) = self.home.as_mut() {
            home.worktree_options = paths;
        }
    }

    /// Every pane work context observed for a workspace rooted at `directory`.
    ///
    /// Home names a directory, not a pane, so the repo and branch it shows are
    /// whatever the panes already working there have observed. Nothing is
    /// derived here: an unlinked directory simply yields nothing.
    fn work_contexts_for_directory<'a>(
        &'a self,
        directory: &'a Path,
    ) -> impl Iterator<Item = &'a crate::work_context::PaneWorkContext> + 'a {
        self.workspaces
            .iter()
            .filter(move |workspace| workspace.identity_cwd == directory)
            .flat_map(|workspace| workspace.tabs.iter())
            .flat_map(|tab| tab.panes.values())
            .filter_map(|pane| self.terminals.get(&pane.attached_terminal_id))
            .map(|terminal| terminal.effective_work_context())
    }

    /// Where `Browse…` opens: the configured `ui.add_project_start_dir` when
    /// it names a real directory, otherwise the directory Home already shows.
    ///
    /// The key is a user-typed path, so `~` is expanded and a stale or
    /// non-directory value falls back rather than opening on nothing.
    pub(crate) fn home_browse_start_directory(&self) -> PathBuf {
        let configured = self.add_project_start_dir.trim();
        if !configured.is_empty() {
            let expanded = crate::worktree::expand_tilde_path(configured);
            if expanded.is_dir() {
                return expanded;
            }
        }
        self.home_directory()
    }

    fn home_directory(&self) -> PathBuf {
        self.home
            .as_ref()
            .map(|home| home.directory.clone())
            .unwrap_or_else(default_directory)
    }

    /// What the headline calls the place this thread will start in.
    ///
    /// The declared repo outranks the path because a worktree directory is
    /// named after the task, not the project. A linked worktree is named after
    /// the task twice over: its own root is the task directory, so the
    /// repository is the one the git common directory belongs to.
    pub(crate) fn home_headline_name(&self) -> String {
        let directory = self.home_directory();
        // The strongest name a pane already declared for this directory.
        if let Some(repo) = self
            .work_contexts_for_directory(&directory)
            .find_map(|context| context.repo.as_deref())
        {
            let name = repo
                .rsplit('/')
                .next()
                .filter(|name| !name.is_empty())
                .unwrap_or(repo);
            return name.to_string();
        }
        // The repository the ref picker resolved for this very directory. It
        // comes from the git common directory, so a linked worktree resolves to
        // the main checkout rather than to its own task-named root. Resolved
        // off the render path when the directory was set; never re-run here.
        let common_root = {
            let canonical = crate::worktree::canonical_or_original(&directory);
            self.home
                .as_ref()
                .filter(|home| home.ref_directory == canonical)
                .and_then(|home| home.ref_repo_root.clone())
        };
        // The checkout root the pane cache resolved, which for a linked
        // worktree is the worktree itself. Weakest of the three, but still a
        // project name where a home directory's own basename names the account.
        let repo_root =
            common_root.or_else(|| self.git_root_for_cwd.get(&directory).cloned().flatten());
        match repo_root {
            Some(repo_root) => directory_basename(&repo_root),
            None => directory_display_name(&directory),
        }
    }

    pub(crate) fn home_ref_options(&self) -> Vec<HomeRef> {
        let Some(home) = self.home.as_ref() else {
            return Vec::new();
        };
        let Some(repo_root) = home.ref_repo_root.as_ref() else {
            return Vec::new();
        };
        self.home_ref_cache
            .get(repo_root)
            .map(|entry| entry.rows_for_directory(&home.ref_directory))
            .unwrap_or_default()
    }

    pub(crate) fn home_ref_label(&self) -> String {
        let Some(home) = self.home.as_ref() else {
            return UNKNOWN_REF_LABEL.to_string();
        };
        if let Some(selected) = home.selected_ref.as_ref() {
            return selected.name.clone();
        }
        if let Some(current) = self
            .home_ref_options()
            .into_iter()
            .find(HomeRef::is_current)
        {
            return current.name;
        }
        self.work_contexts_for_directory(&home.directory)
            .find_map(|context| context.branch.clone())
            .unwrap_or_else(|| UNKNOWN_REF_LABEL.to_string())
    }

    fn reset_home_ref_context(&mut self, request_refresh: bool) {
        let directory = crate::worktree::canonical_or_original(&self.home_directory());
        let repo_root = super::worktrees::worktree_repo_root(&directory)
            .map(|root| crate::worktree::canonical_or_original(&root));
        if let Some(home) = self.home.as_mut() {
            let context_changed =
                home.ref_directory != directory || home.ref_repo_root != repo_root;
            home.ref_directory = directory;
            home.ref_repo_root = repo_root.clone();
            if context_changed {
                home.selected_ref = None;
            }
            home.ref_filter.set_query("");
            home.dispatch_error = None;
        }
        if request_refresh {
            self.request_home_ref_refresh = repo_root.clone();
        }
        if let Some(repo_root) = repo_root.as_deref() {
            self.sync_home_ref_selection(repo_root);
        }
    }

    pub(crate) fn sync_home_ref_selection(&mut self, repo_root: &Path) {
        let relevant = self
            .home
            .as_ref()
            .is_some_and(|home| home.ref_repo_root.as_deref() == Some(repo_root));
        if !relevant {
            return;
        }
        let rows = self.home_ref_options();
        let selected_name = self
            .home
            .as_ref()
            .and_then(|home| home.selected_ref.as_ref())
            .map(|selected| selected.name.as_str());
        let selected = selected_name
            .and_then(|name| rows.iter().find(|row| row.name == name).cloned())
            .or_else(|| rows.iter().find(|row| row.is_current()).cloned());
        if let Some(home) = self.home.as_mut() {
            home.selected_ref = selected;
            let matches = home
                .ref_filter
                .matches(&rows.iter().map(|row| row.name.clone()).collect::<Vec<_>>())
                .len();
            home.ref_filter.selected = home.ref_filter.selected.min(matches.saturating_sub(1));
        }
    }

    pub(crate) fn home_workspace_options(&self) -> Vec<HomeWorkspace> {
        self.home
            .as_ref()
            .map(|home| home.workspace_options.clone())
            .unwrap_or_default()
    }

    fn refresh_home_workspace_options(&mut self) {
        let default_workspace = self.default_home_workspace();
        let directory = self.home_directory();
        let entries = super::worktrees::worktree_repo_root(&directory)
            .and_then(|repo_root| {
                super::worktrees::worktree_entries_for_repo(&repo_root, |_| None).ok()
            })
            .unwrap_or_default();
        let options = home_workspace_options_from_entries(&entries);
        if let Some(home) = self.home.as_mut() {
            home.workspace_options = options;
            if !home.workspace_options.contains(&home.workspace) {
                home.workspace = default_workspace;
            }
        }
    }

    pub(crate) fn home_target_options(&self) -> Vec<HomeTarget> {
        let mut options = vec![HomeTarget::NewSpace];
        options.extend(
            self.workspaces
                .iter()
                .map(|workspace| HomeTarget::Existing(workspace.id.clone())),
        );
        options
    }

    fn home_picker_len(&self, picker: HomePicker) -> usize {
        match picker {
            HomePicker::Agent => self
                .home
                .as_ref()
                .map(|home| home.profiles().len())
                .unwrap_or(0),
            HomePicker::Model => self
                .home
                .as_ref()
                .map(|home| home.model_options().len())
                .unwrap_or(0),
            HomePicker::Effort => self
                .home
                .as_ref()
                .map(|home| home.effort_options().len())
                .unwrap_or(0),
            HomePicker::Access => self
                .home
                .as_ref()
                .map(|home| home.access_options().len())
                .unwrap_or(0),
            HomePicker::Context => self
                .home
                .as_ref()
                .map(|home| home.context_options().len())
                .unwrap_or(0),
            HomePicker::Project => self
                .home
                .as_ref()
                .map(|home| home.projects().len())
                .unwrap_or(0),
            HomePicker::Repo => self
                .home
                .as_ref()
                .map(|home| home.repo_options().len())
                .unwrap_or(0),
            HomePicker::Machine => self
                .home
                .as_ref()
                .map(|home| home.machines().len())
                .unwrap_or(0),
            HomePicker::Directory => match self.home_browse() {
                Some(browse) => browse.children.len(),
                None => self.home_directory_match_indices().len(),
            },
            HomePicker::Workspace => self.home_workspace_options().len(),
            HomePicker::Ref => self.home_ref_match_indices().len(),
            HomePicker::Target => self.home_target_options().len(),
        }
    }

    fn home_directory_match_indices(&self) -> Vec<usize> {
        let labels = self
            .home_directory_picker_options()
            .iter()
            .map(HomeDirectoryOption::label)
            .collect::<Vec<_>>();
        self.home
            .as_ref()
            .map(|home| {
                home.directory_filter
                    .matches(&labels)
                    .into_iter()
                    .map(|(index, _)| index)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn home_ref_match_indices(&self) -> Vec<usize> {
        let names = self
            .home_ref_options()
            .iter()
            .map(|git_ref| git_ref.name.clone())
            .collect::<Vec<_>>();
        self.home
            .as_ref()
            .map(|home| {
                home.ref_filter
                    .matches(&names)
                    .into_iter()
                    .map(|(index, _)| index)
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(crate) fn home_open_picker(&mut self, picker: HomePicker) {
        if picker == HomePicker::Workspace {
            self.refresh_home_workspace_options();
        }
        if picker == HomePicker::Ref {
            self.reset_home_ref_context(true);
        }
        if picker == HomePicker::Directory {
            self.refresh_home_worktree_options();
            if let Some(home) = self.home.as_mut() {
                home.browse = None;
            }
        }
        if matches!(picker, HomePicker::Directory | HomePicker::Ref) {
            if let Some(home) = self.home.as_mut() {
                if picker == HomePicker::Directory {
                    home.directory_filter.set_query("");
                } else {
                    home.ref_filter.set_query("");
                }
            }
        }
        let length = self.home_picker_len(picker);
        if length == 0 && picker != HomePicker::Ref {
            return;
        }
        let current = self
            .home
            .as_ref()
            .and_then(|home| match picker {
                HomePicker::Agent => home
                    .profiles()
                    .iter()
                    .position(|profile| profile.id == home.profile().id),
                HomePicker::Model => home
                    .model_options()
                    .iter()
                    .position(|model| model.id == home.model),
                HomePicker::Effort => home.effort.as_deref().and_then(|effort| {
                    home.effort_options()
                        .iter()
                        .position(|option| *option == effort)
                }),
                HomePicker::Access => home.access.and_then(|access| {
                    home.access_options()
                        .iter()
                        .position(|option| *option == access)
                }),
                HomePicker::Context => home.context_window.as_deref().and_then(|context| {
                    home.context_options()
                        .iter()
                        .position(|option| *option == context)
                }),
                HomePicker::Project => home.projects().iter().position(|project| {
                    Some(project.id.as_str()) == home.project().map(|current| current.id.as_str())
                }),
                HomePicker::Repo => home.repo.as_deref().and_then(|name| {
                    home.repo_options()
                        .iter()
                        .position(|repo| repo.name == name)
                }),
                HomePicker::Machine => home.machines().iter().position(|machine| {
                    Some(machine.name.as_str())
                        == home.machine().map(|current| current.name.as_str())
                }),
                HomePicker::Directory => self
                    .home_directory_picker_options()
                    .iter()
                    .position(|option| option.path() == Some(home.directory.as_path())),
                HomePicker::Workspace => self
                    .home_workspace_options()
                    .iter()
                    .position(|workspace| workspace == &home.workspace),
                HomePicker::Ref => home.selected_ref.as_ref().and_then(|selected| {
                    self.home_ref_options()
                        .iter()
                        .position(|git_ref| git_ref.name == selected.name)
                }),
                HomePicker::Target => self
                    .home_target_options()
                    .iter()
                    .position(|target| target == &home.target),
            })
            .unwrap_or(0);
        if let Some(home) = self.home.as_mut() {
            home.picker = Some(picker);
            if picker == HomePicker::Directory {
                home.directory_filter.selected = current.min(length - 1);
            } else if picker == HomePicker::Ref {
                home.ref_filter.selected = current.min(length.saturating_sub(1));
            } else {
                home.picker_selected = current.min(length - 1);
            }
        }
    }

    pub(crate) fn home_move_picker(&mut self, delta: i32) {
        let Some(picker) = self.home.as_ref().and_then(|home| home.picker) else {
            return;
        };
        let length = self.home_picker_len(picker);
        if let Some(home) = self.home.as_mut() {
            if picker == HomePicker::Directory {
                home.directory_filter.move_selection(delta, length);
            } else if picker == HomePicker::Ref {
                home.ref_filter.move_selection(delta, length);
            } else if length > 0 {
                let selected =
                    (home.picker_selected as i32 + delta).clamp(0, length as i32 - 1) as usize;
                home.picker_selected = selected;
            }
        }
    }

    pub(crate) fn home_push_picker_filter(&mut self, character: char) {
        if let Some(browse) = self.home_browse_mut() {
            browse.push(character);
            return;
        }
        if let Some(home) = self
            .home
            .as_mut()
            .filter(|home| matches!(home.picker, Some(HomePicker::Directory | HomePicker::Ref)))
        {
            match home.picker {
                Some(HomePicker::Directory) => home.directory_filter.push(character),
                Some(HomePicker::Ref) => home.ref_filter.push(character),
                _ => {}
            }
        }
    }

    pub(crate) fn home_pop_picker_filter(&mut self) {
        if let Some(browse) = self.home_browse_mut() {
            browse.pop();
            return;
        }
        if let Some(home) = self
            .home
            .as_mut()
            .filter(|home| matches!(home.picker, Some(HomePicker::Directory | HomePicker::Ref)))
        {
            match home.picker {
                Some(HomePicker::Directory) => home.directory_filter.pop(),
                Some(HomePicker::Ref) => home.ref_filter.pop(),
                _ => {}
            }
        }
    }

    pub(crate) fn home_browse(&self) -> Option<&HomeBrowse> {
        self.home
            .as_ref()
            .filter(|home| home.picker == Some(HomePicker::Directory))
            .and_then(|home| home.browse.as_ref())
    }

    fn home_browse_mut(&mut self) -> Option<&mut HomeBrowse> {
        self.home
            .as_mut()
            .filter(|home| home.picker == Some(HomePicker::Directory))
            .and_then(|home| home.browse.as_mut())
    }

    pub(crate) fn home_browse_active(&self) -> bool {
        self.home_browse().is_some()
    }

    /// `Tab` in the path input: extend it by what the filesystem agrees on.
    pub(crate) fn home_browse_complete(&mut self) {
        if let Some(browse) = self.home_browse_mut() {
            browse.complete();
        }
    }

    /// Clicking a listed child descends into it instead of dispatching.
    pub(crate) fn home_browse_select(&mut self, index: usize) {
        if let Some(browse) = self.home_browse_mut() {
            browse.select_child(index);
        }
    }

    /// `Esc` in the path input returns to the option list.
    pub(crate) fn home_browse_cancel(&mut self) {
        if let Some(home) = self.home.as_mut() {
            home.browse = None;
            home.directory_filter.set_query("");
        }
    }

    /// `Enter` in the path input. A path that is not a directory is refused
    /// with a reason and leaves the composer exactly as it was.
    pub(crate) fn home_browse_accept(&mut self) {
        let Some(path) = self.home_browse().map(HomeBrowse::path) else {
            return;
        };
        if !path.is_dir() {
            if let Some(browse) = self.home_browse_mut() {
                browse.error = Some(NO_SUCH_DIRECTORY.to_string());
            }
            return;
        }
        self.home_set_directory(crate::worktree::canonical_or_original(&path));
        if let Some(home) = self.home.as_mut() {
            home.picker = None;
        }
    }

    pub(crate) fn home_set_directory(&mut self, directory: PathBuf) {
        if let Some(home) = self.home.as_mut() {
            home.set_directory(directory);
            home.browse = None;
            home.directory_filter.set_query("");
        }
        self.refresh_home_workspace_options();
        self.reset_home_ref_context(false);
    }

    pub(crate) fn home_accept_picker(&mut self) {
        let Some((picker, selected)) = self.home.as_ref().and_then(|home| {
            home.picker.map(|picker| {
                let selected = match picker {
                    HomePicker::Directory => home.directory_filter.selected,
                    HomePicker::Ref => home.ref_filter.selected,
                    _ => home.picker_selected,
                };
                (picker, selected)
            })
        }) else {
            return;
        };
        let selected = match picker {
            HomePicker::Directory => {
                let Some(selected) = self.home_directory_match_indices().get(selected).copied()
                else {
                    return;
                };
                selected
            }
            HomePicker::Ref => {
                let Some(selected) = self.home_ref_match_indices().get(selected).copied() else {
                    return;
                };
                selected
            }
            _ => selected,
        };
        match picker {
            HomePicker::Agent => {
                let id = self
                    .home
                    .as_ref()
                    .and_then(|home| home.profiles().get(selected))
                    .map(|profile| profile.id.clone());
                if let (Some(id), Some(home)) = (id, self.home.as_mut()) {
                    home.set_profile(&id);
                }
            }
            HomePicker::Model => {
                let model = self
                    .home
                    .as_ref()
                    .and_then(|home| home.model_options().get(selected))
                    .map(|model| model.id.clone());
                if let Some(model) = model {
                    if let Some(home) = self.home.as_mut() {
                        home.set_model(model);
                    }
                }
            }
            HomePicker::Effort => {
                let effort = self
                    .home
                    .as_ref()
                    .and_then(|home| home.effort_options().get(selected))
                    .cloned();
                if let Some(home) = self.home.as_mut() {
                    home.set_effort(effort);
                }
            }
            HomePicker::Access => {
                let access = self
                    .home
                    .as_ref()
                    .and_then(|home| home.access_options().get(selected))
                    .copied();
                if let (Some(home), Some(access)) = (self.home.as_mut(), access) {
                    home.set_access(access);
                }
            }
            HomePicker::Context => {
                let context = self
                    .home
                    .as_ref()
                    .and_then(|home| home.context_options().get(selected))
                    .map(|context| (*context).to_string());
                if let Some(home) = self.home.as_mut() {
                    home.set_context_window(context);
                }
            }
            HomePicker::Project => {
                let id = self
                    .home
                    .as_ref()
                    .and_then(|home| home.projects().get(selected))
                    .map(|project| project.id.clone());
                if let (Some(id), Some(home)) = (id, self.home.as_mut()) {
                    home.set_project(&id);
                }
            }
            HomePicker::Repo => {
                let repo = self
                    .home
                    .as_ref()
                    .and_then(|home| home.repo_options().get(selected))
                    .cloned();
                if let Some(repo) = repo {
                    // The checkout is the directory; recording the name as well
                    // is what lets the chip keep saying which repo it was.
                    if let Some(home) = self.home.as_mut() {
                        home.repo = Some(repo.name.clone());
                    }
                    self.home_set_directory(crate::worktree::canonical_or_original(&repo.path));
                }
            }
            HomePicker::Machine => {
                let name = self
                    .home
                    .as_ref()
                    .and_then(|home| home.machines().get(selected))
                    .map(|machine| machine.name.clone());
                if let (Some(name), Some(home)) = (name, self.home.as_mut()) {
                    home.set_machine(&name);
                }
            }
            HomePicker::Directory => {
                match self.home_directory_picker_options().get(selected).cloned() {
                    Some(HomeDirectoryOption::Recent(directory))
                    | Some(HomeDirectoryOption::Worktree(directory)) => {
                        self.home_set_directory(directory);
                    }
                    Some(HomeDirectoryOption::Browse) => {
                        // Browsing replaces the filter line rather than closing
                        // the picker, so the card stays open on the path input.
                        let directory = self.home_browse_start_directory();
                        if let Some(home) = self.home.as_mut() {
                            home.browse = Some(HomeBrowse::starting_at(&directory));
                        }
                        return;
                    }
                    Some(HomeDirectoryOption::AddProject) => {
                        let directory = self.home_browse_start_directory();
                        if let Some(home) = self.home.as_mut() {
                            home.picker = None;
                            home.browse = None;
                            home.directory_filter.set_query("");
                            home.add_project = Some(AddProjectState::starting_at(&directory));
                        }
                        return;
                    }
                    None => return,
                }
            }
            HomePicker::Workspace => {
                if let Some(workspace) = self.home_workspace_options().get(selected).cloned() {
                    if let Some(home) = self.home.as_mut() {
                        home.workspace = workspace;
                        home.dispatch_error = None;
                    }
                }
            }
            HomePicker::Ref => {
                if let Some(git_ref) = self.home_ref_options().get(selected).cloned() {
                    if let Some(home) = self.home.as_mut() {
                        home.selected_ref = Some(git_ref);
                        home.dispatch_error = None;
                    }
                }
            }
            HomePicker::Target => {
                if let Some(target) = self.home_target_options().get(selected).cloned() {
                    if let Some(home) = self.home.as_mut() {
                        home.target = target;
                    }
                }
            }
        }
        if let Some(home) = self.home.as_mut() {
            home.picker = None;
        }
        if let Some(choices) = self.home.as_ref().map(HomeState::saved_agent_choices) {
            self.home_agent_choices = choices;
        }
    }

    /// Focus the pane the cursor is on and leave home. Returns whether it moved.
    ///
    /// A row can name a pane that has since closed, so this reports failure
    /// rather than leaving home closed over nothing.
    pub(crate) fn jump_to_selected_home_agent(&mut self, queue: &[BlockedAgent]) -> bool {
        let Some(agent) = self.home.as_ref().and_then(|home| home.current(queue)) else {
            return false;
        };
        let (ws_idx, pane_id) = (agent.ws_idx, agent.pane_id);
        let pane_exists = self
            .workspaces
            .get(ws_idx)
            .is_some_and(|ws| ws.tabs.iter().any(|tab| tab.panes.contains_key(&pane_id)));
        if !pane_exists {
            return false;
        }
        self.focus_pane_in_workspace(ws_idx, pane_id);
        self.clear_home();
        true
    }
}

impl crate::app::App {
    pub(crate) fn reply_to_selected_home_agent(&mut self) {
        let queue = self.state.home_attention_agents();
        let Some((ws_idx, pane_id, reply)) = self.state.home.as_ref().and_then(|home| {
            home.current(&queue)
                .map(|agent| (agent.ws_idx, agent.pane_id, home.reply.clone()))
        }) else {
            if let Some(home) = self.state.home.as_mut() {
                home.reply_error = Some("no waiting pane is selected".into());
            }
            return;
        };
        if reply.trim().is_empty() {
            if let Some(home) = self.state.home.as_mut() {
                home.reply_error = Some("type a reply first".into());
            }
            return;
        }
        if self
            .state
            .pending_human_drafts
            .get(&pane_id)
            .is_some_and(|draft| !draft.is_empty())
        {
            if let Some(home) = self.state.home.as_mut() {
                home.reply_error = Some("human draft pending · clear it in the pane".into());
            }
            return;
        }
        let Some(public_pane_id) = self.public_pane_id(ws_idx, pane_id) else {
            if let Some(home) = self.state.home.as_mut() {
                home.reply_error = Some("pane is no longer available".into());
            }
            return;
        };
        match self.try_send_text_to_pane(&public_pane_id, &format!("{reply}\r")) {
            Ok(()) => {
                if let Some(home) = self.state.home.as_mut() {
                    home.reply.clear();
                    home.reply_error = None;
                    home.focus = None;
                }
            }
            Err(crate::app::api::PaneSendError::NotFound) => {
                if let Some(home) = self.state.home.as_mut() {
                    home.reply_error = Some("pane is no longer available".into());
                }
            }
            Err(crate::app::api::PaneSendError::Failed(error)) => {
                if let Some(home) = self.state.home.as_mut() {
                    home.reply_error = Some(format!("reply failed · {error}"));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_action_selects_the_best_profile_with_remaining_usage_for_future_home() {
        let mut app = crate::app::AppState::test_new();
        app.machines.push(crate::app::machines::Machine {
            name: "ub1".into(),
            icon: None,
            target: Some("ub1".into()),
            socket: None,
        });
        app.launch_profiles = crate::app::launch_profiles::resolve(&[
            crate::config::LaunchProfileConfig {
                id: "exhausted".into(),
                label: "Exhausted".into(),
                agent: "agy".into(),
                command: Vec::new(),
                env: Default::default(),
                usage: Some("agy".into()),
            },
            crate::config::LaunchProfileConfig {
                id: "available".into(),
                label: "Available".into(),
                agent: "codex".into(),
                command: Vec::new(),
                env: Default::default(),
                usage: Some("codex".into()),
            },
        ]);
        app.provider_usage
            .primary_usage_mut(crate::provider_usage::QuotaProvider::Codex)
            .seven_day = Some(crate::provider_usage::QuotaWindow {
            used_percent: 61,
            resets_at: None,
        });

        assert!(app.set_next_home_target("ub1"));
        assert_eq!(app.next_home_machine.as_deref(), Some("ub1"));
        assert_eq!(app.next_home_profile.as_deref(), Some("codex"));
        let home = app.new_home_state();
        assert_eq!(
            home.machine().map(|machine| machine.name.as_str()),
            Some("ub1")
        );
        assert_eq!(home.profile().id, "codex");
    }
    use crate::layout::PaneId;
    use crate::terminal::TerminalId;

    /// AC6: raw human bytes update drafts while editing and terminal escapes stay invisible.
    #[test]
    fn human_bytes_track_draft_edits_and_skip_terminal_sequences() {
        let mut state = crate::app::state::AppState::test_new();
        let pane_id = PaneId::alloc();

        state.note_human_bytes(pane_id, b"ab\x7f");
        assert_eq!(state.pending_human_drafts[&pane_id], "a");
        state.note_human_bytes(pane_id, b"\r");
        assert!(!state.pending_human_drafts.contains_key(&pane_id));

        state.note_human_bytes(pane_id, b"\x1b[A");
        assert!(!state.pending_human_drafts.contains_key(&pane_id));
        state.note_human_bytes(pane_id, b"\x1b[200~hi\x1b[201~");
        assert_eq!(state.pending_human_drafts[&pane_id], "hi");

        state.pending_human_drafts.remove(&pane_id);
        state.note_human_bytes(pane_id, b"x\ry");
        assert_eq!(state.pending_human_drafts[&pane_id], "y");
    }

    #[test]
    fn opening_the_new_thread_composer_takes_the_keyboard_off_the_sidebar() {
        use crate::app::SidebarWorkGroupKeyAction;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let mut state = crate::app::state::AppState::test_new();
        // What `prefix+c` leaves behind: the picker takes sidebar keyboard
        // focus, and the row the operator clicked before it is still selected.
        state.sidebar_focused = true;
        state.sidebar_selected_work_group = Some("linear:SCA-3102".into());
        let workspace = state.default_home_workspace();

        state.open_home_composer_in_directory(std::path::PathBuf::from("/tmp"), workspace);

        assert!(!state.sidebar_focused);
        assert_eq!(state.sidebar_selected_work_group, None);
        assert!(matches!(
            state.handle_sidebar_work_group_key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::empty()
            )),
            SidebarWorkGroupKeyAction::Ignored
        ));
        assert!(matches!(
            state.handle_sidebar_work_group_key(KeyEvent::new(
                KeyCode::Char('n'),
                KeyModifiers::empty()
            )),
            SidebarWorkGroupKeyAction::Ignored
        ));
        assert_eq!(
            state.home.as_ref().and_then(|home| home.focus),
            Some(HomeFocus::Prompt)
        );
    }

    #[test]
    fn opening_the_composer_reuses_a_space_with_the_selected_directory() {
        let directory = PathBuf::from("/repo/matching");
        let mut state = crate::app::state::AppState::test_new();
        let mut workspace = crate::workspace::Workspace::test_new("matching");
        workspace.identity_cwd = directory.clone();
        let workspace_id = workspace.id.clone();
        state.workspaces.push(workspace);

        state.open_home_composer_in_directory(directory, HomeWorkspace::CurrentCheckout);

        assert_eq!(
            state.home.as_ref().map(|home| &home.target),
            Some(&HomeTarget::Existing(workspace_id))
        );
    }

    #[test]
    fn opening_the_composer_reuses_the_space_bound_to_the_observed_repository() {
        let directory = PathBuf::from("/repo/selected");
        let mut state = crate::app::state::AppState::test_new();
        let mut observer = crate::workspace::Workspace::test_new("observer");
        observer.repo_binding_cleared = true;
        let mut bound = crate::workspace::Workspace::test_new("bound");
        bound.repo_binding = Some("owner/repo".into());
        let bound_id = bound.id.clone();
        let observer_terminal_id = observer.tabs[0].panes[&observer.tabs[0].root_pane]
            .attached_terminal_id
            .clone();
        state.workspaces = vec![observer, bound];
        state.ensure_test_terminals();
        let terminal = state
            .terminals
            .get_mut(&observer_terminal_id)
            .expect("observer terminal");
        terminal.cwd = directory.clone();
        terminal
            .replace_git_work_context(crate::work_context::PaneWorkContext {
                repo: Some("OWNER/REPO".into()),
                ..Default::default()
            })
            .expect("valid observed repository");

        state.open_home_composer_in_directory(directory, HomeWorkspace::CurrentCheckout);

        assert_eq!(
            state.home.as_ref().map(|home| &home.target),
            Some(&HomeTarget::Existing(bound_id))
        );
    }

    #[test]
    fn opening_the_composer_prefers_the_checkout_space_over_its_bound_sibling() {
        let directory = PathBuf::from("/repo/sibling");
        let mut state = crate::app::state::AppState::test_new();
        let mut bound = crate::workspace::Workspace::test_new("bound");
        bound.repo_binding = Some("owner/repo".into());
        let sibling = crate::workspace::Workspace::test_new("sibling");
        let sibling_id = sibling.id.clone();
        let sibling_terminal_id = sibling.tabs[0].panes[&sibling.tabs[0].root_pane]
            .attached_terminal_id
            .clone();
        state.workspaces = vec![bound, sibling];
        state.ensure_test_terminals();
        let terminal = state
            .terminals
            .get_mut(&sibling_terminal_id)
            .expect("sibling terminal");
        terminal.cwd = directory.clone();
        terminal
            .replace_git_work_context(crate::work_context::PaneWorkContext {
                repo: Some("owner/repo".into()),
                ..Default::default()
            })
            .expect("valid observed repository");

        state.open_home_composer_in_directory(directory, HomeWorkspace::CurrentCheckout);

        assert_eq!(
            state.home.as_ref().map(|home| &home.target),
            Some(&HomeTarget::Existing(sibling_id))
        );
    }

    #[test]
    fn opening_the_composer_does_not_reuse_a_space_with_a_cleared_binding() {
        let directory = PathBuf::from("/repo/cleared");
        let mut state = crate::app::state::AppState::test_new();
        let mut workspace = crate::workspace::Workspace::test_new("cleared");
        workspace.identity_cwd = directory.clone();
        workspace.repo_binding_cleared = true;
        state.workspaces.push(workspace);

        state.open_home_composer_in_directory(directory, HomeWorkspace::CurrentCheckout);

        assert_eq!(
            state.home.as_ref().map(|home| &home.target),
            Some(&HomeTarget::NewSpace)
        );
    }

    #[test]
    fn opening_the_composer_uses_new_space_when_no_eligible_space_matches() {
        let mut state = crate::app::state::AppState::test_new();
        let mut home = state.new_home_state();
        home.target = HomeTarget::Existing("stale-space".into());
        state.home = Some(home);

        state.open_home_composer_in_directory(
            PathBuf::from("/repo/unmatched"),
            HomeWorkspace::CurrentCheckout,
        );

        assert_eq!(
            state.home.as_ref().map(|home| &home.target),
            Some(&HomeTarget::NewSpace)
        );
    }

    fn queue(n: usize) -> Vec<BlockedAgent> {
        (0..n)
            .map(|i| BlockedAgent {
                ws_idx: 0,
                pane_id: PaneId::alloc(),
                terminal_id: TerminalId::alloc(),
                workspace_label: format!("ws{i}"),
                agent_label: format!("agent{i}"),
                blocked_since: None,
                seq: None,
                attention_tier: crate::terminal::state::AttentionTier::Blocked,
            })
            .collect()
    }

    fn home_with_codex_catalog() -> HomeState {
        let codex = super::super::home_catalog::parse_codex_catalog(
            br#"{"models":[
                {"slug":"gpt-5.6-sol","visibility":"list","priority":1,"supported_reasoning_levels":[{"effort":"low"},{"effort":"ultra"}]},
                {"slug":"gpt-5.6-luna","visibility":"list","priority":2,"supported_reasoning_levels":[{"effort":"low"},{"effort":"max"}]}
            ]}"#,
        )
        .expect("Codex fixture");
        HomeState::with_catalog(HomeCatalog::with_codex(codex))
    }

    #[test]
    fn an_empty_queue_selects_nothing() {
        assert!(HomeState::default().current(&[]).is_none());
    }

    #[test]
    fn the_cursor_starts_on_the_longest_wait() {
        let q = queue(3);
        let home = HomeState::default();
        assert_eq!(home.current(&q).map(|a| a.pane_id), Some(q[0].pane_id));
    }

    #[test]
    fn the_cursor_stops_at_both_ends_rather_than_wrapping() {
        let q = queue(3);
        let mut home = HomeState::default();
        home.select_prev(&q);
        assert_eq!(home.selected(&q), 0, "top must not wrap to the bottom");
        for _ in 0..5 {
            home.select_next(&q);
        }
        assert_eq!(home.selected(&q), 2, "bottom must not wrap to the top");
    }

    /// The queue shrinks whenever an agent answers its own gate, which happens
    /// without the view being told. A stale index must never index past the end.
    #[test]
    fn a_cursor_left_past_the_end_of_a_shrunken_queue_lands_on_the_last_row() {
        let mut home = HomeState::default();
        let long = queue(5);
        for _ in 0..4 {
            home.select_next(&long);
        }
        let short = queue(2);

        assert_eq!(home.selected(&short), 1);
        assert_eq!(
            home.current(&short).map(|a| a.pane_id),
            Some(short[1].pane_id)
        );
    }

    #[test]
    fn a_queue_that_fits_never_scrolls() {
        let q = queue(4);
        let mut home = HomeState::default();
        for _ in 0..4 {
            home.select_next(&q);
        }
        assert_eq!(home.scroll(&q, 4), 0);
        assert_eq!(home.scroll(&q, 9), 0);
    }

    #[test]
    fn scrolling_keeps_the_cursor_on_screen_without_running_past_the_last_row() {
        let q = queue(10);
        let mut home = HomeState::default();
        assert_eq!(home.scroll(&q, 3), 0, "the top of the list does not scroll");
        for _ in 0..5 {
            home.select_next(&q);
        }
        assert_eq!(home.scroll(&q, 3), 3, "the cursor stays on the last row");
        for _ in 0..9 {
            home.select_next(&q);
        }
        assert_eq!(
            home.scroll(&q, 3),
            7,
            "the last screen is full rather than mostly blank"
        );
    }

    #[test]
    fn a_view_with_no_room_asks_for_no_scroll() {
        assert_eq!(HomeState::default().scroll(&queue(5), 0), 0);
    }

    #[test]
    fn injected_catalog_exposes_provider_models_and_model_specific_efforts() {
        let mut home = home_with_codex_catalog();
        assert_eq!(
            home.model_options()
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            [
                "default",
                "claude-fable-5-1",
                "claude-opus-5",
                "claude-sonnet-5",
                "claude-haiku-4-5-20251001",
            ]
        );
        home.set_agent(Agent::Codex);
        home.set_model("gpt-5.6-sol");
        assert!(home.effort_options().iter().any(|effort| effort == "ultra"));
        home.set_model("gpt-5.6-luna");
        assert!(!home.effort_options().iter().any(|effort| effort == "ultra"));
    }

    #[test]
    fn changing_model_reconciles_an_unsupported_effort_to_auto() {
        let mut home = home_with_codex_catalog();
        home.set_agent(Agent::Codex);
        home.set_model("gpt-5.6-sol");
        home.effort = Some("ultra".into());

        home.set_model("gpt-5.6-luna");

        assert_eq!(home.effort.as_deref(), Some("auto"));
    }

    #[test]
    fn catalog_refresh_reconciles_a_removed_selected_model() {
        let mut home = home_with_codex_catalog();
        home.set_agent(Agent::Codex);
        home.set_model("gpt-5.6-sol");
        let replacement = super::super::home_catalog::parse_codex_catalog(
            br#"{"models":[
                {"slug":"new-model","visibility":"list","priority":1,"supported_reasoning_levels":[{"effort":"high"}]}
            ]}"#,
        )
        .expect("replacement catalog");

        home.replace_provider_catalog(replacement);

        assert_eq!(home.model, DEFAULT_MODEL);
        assert_eq!(home.effort.as_deref(), Some(AUTO_EFFORT));
    }

    #[test]
    fn catalog_refresh_clamps_an_open_picker_to_the_new_options() {
        let mut home = home_with_codex_catalog();
        home.set_agent(Agent::Codex);
        home.picker = Some(HomePicker::Model);
        home.picker_selected = home.model_options().len() - 1;
        let replacement = super::super::home_catalog::parse_codex_catalog(
            br#"{"models":[
                {"slug":"new-model","visibility":"list","priority":1,"supported_reasoning_levels":[{"effort":"high"}]}
            ]}"#,
        )
        .expect("replacement catalog");

        home.replace_provider_catalog(replacement);

        assert_eq!(home.picker, Some(HomePicker::Model));
        assert_eq!(home.picker_selected, home.model_options().len() - 1);
    }

    #[test]
    fn the_opening_model_is_preferred_while_effort_defers_to_the_provider() {
        let mut home = home_with_codex_catalog();
        home.prompt = "implement the retry cap".into();

        let plan = home.dispatch_plan().expect("prompt should dispatch");

        assert_eq!(home.model, "claude-opus-5");
        assert_eq!(
            home.effort.as_deref(),
            Some("auto"),
            "Claude names no preferred effort, so the catalog's own first stands"
        );
        assert_eq!(
            plan.argv,
            vec![
                "claude",
                "--model",
                "claude-opus-5[1m]",
                "--dangerously-skip-permissions",
                "implement the retry cap"
            ]
        );
    }

    #[test]
    fn dispatch_plan_carries_pull_request_context() {
        let mut home = home_with_codex_catalog();
        home.prompt = "review the requested changes".into();
        home.pr = Some(HomePrContext {
            url: "https://github.com/owner/repo/pull/42".into(),
            number: 42,
            repo: "owner/repo".into(),
        });

        let plan = home.dispatch_plan().expect("PR context should dispatch");

        assert_eq!(plan.pr, home.pr);
    }

    #[test]
    fn dispatch_plan_carries_missive_conversation_context() {
        let mut home = home_with_codex_catalog();
        home.prompt = "reply to the linked conversation".into();
        home.missive = Some(HomeMissiveContext {
            app_url: "missive://mail.missiveapp.com/#inbox/conversations/sample".into(),
            web_url: "https://mail.missiveapp.com/#inbox/conversations/sample".into(),
            subject: "Billing question".into(),
        });

        let plan = home
            .dispatch_plan()
            .expect("Missive context should dispatch");

        assert_eq!(plan.missive, home.missive);
    }

    /// Characterization: pins `dispatch_plan()` before the T3 card layout moves
    /// the fields around. Layout may change; the plan for the same inputs may
    /// not.
    #[test]
    fn dispatch_plan_is_frozen_for_a_fixed_set_of_inputs() {
        let mut home = home_with_codex_catalog();
        home.prompt = "  cap the retry loop\nand log it  ".into();
        home.set_agent(Agent::Codex);
        home.set_model("gpt-5.6-sol");
        home.effort = Some("ultra".into());
        // Explicit: the frozen argv pins the plan for these inputs, so it must
        // not move when the opening access policy does.
        home.set_access(HomeAccess::CodexReadOnly);
        home.directory = PathBuf::from("/tmp/frozen-plan");
        home.target = HomeTarget::Existing("space-7".into());

        let plan = home.dispatch_plan().expect("frozen inputs dispatch");

        assert_eq!(
            plan,
            HomeDispatchPlan {
                agent: Agent::Codex,
                model: "gpt-5.6-sol".into(),
                effort: Some("ultra".into()),
                directory: PathBuf::from("/tmp/frozen-plan"),
                workspace: HomeWorkspace::CurrentCheckout,
                git_ref: None,
                pr: None,
                ticket: None,
                missive: None,
                work_context_patch: crate::work_context::PaneWorkContextPatch::default(),
                target: HomeTarget::Existing("space-7".into()),
                prompt: "cap the retry loop\nand log it".into(),
                argv: vec![
                    "codex".into(),
                    "--model".into(),
                    "gpt-5.6-sol".into(),
                    "-c".into(),
                    "model_reasoning_effort=ultra".into(),
                    "-s".into(),
                    "read-only".into(),
                    "cap the retry loop\nand log it".into(),
                ],
                env: Vec::new(),
                remote: None,
            }
        );
    }

    #[test]
    fn workspace_options_keep_fixed_choices_first_then_linked_worktrees() {
        let entries = vec![
            crate::app::state::WorktreeOpenEntry {
                path: PathBuf::from("/repo/root"),
                branch: Some("main".into()),
                is_linked_worktree: false,
                already_open_ws_idx: None,
            },
            crate::app::state::WorktreeOpenEntry {
                path: PathBuf::from("/worktrees/alpha"),
                branch: Some("alpha".into()),
                is_linked_worktree: true,
                already_open_ws_idx: Some(1),
            },
            crate::app::state::WorktreeOpenEntry {
                path: PathBuf::from("/worktrees/beta"),
                branch: Some("beta".into()),
                is_linked_worktree: true,
                already_open_ws_idx: None,
            },
        ];

        assert_eq!(
            home_workspace_options_from_entries(&entries),
            vec![
                HomeWorkspace::CurrentCheckout,
                HomeWorkspace::NewWorktree,
                HomeWorkspace::PreviousWorktree(PathBuf::from("/worktrees/alpha")),
                HomeWorkspace::PreviousWorktree(PathBuf::from("/worktrees/beta")),
            ]
        );
    }

    #[test]
    fn opening_ref_picker_uses_cached_repo_rows_and_requests_refresh() {
        let directory = crate::worktree::canonical_or_original(
            &std::env::current_dir().expect("test current directory"),
        );
        let repo_root = super::super::worktrees::worktree_repo_root(&directory)
            .map(|root| crate::worktree::canonical_or_original(&root))
            .expect("tests run inside the Herdr repository");
        let mut app = crate::app::state::AppState::test_new();
        let home = HomeState {
            directory: directory.clone(),
            ..HomeState::default()
        };
        app.home = Some(home);
        app.home_ref_cache.insert(
            repo_root.clone(),
            super::super::home_refs::parse_ref_cache(
                "refs/heads/cached/topic\t1234567\t1\n",
                &format!(
                    "worktree {}\nHEAD 1234567890\nbranch refs/heads/cached/topic\n\n",
                    directory.display()
                ),
                "cached/topic\n",
            ),
        );

        app.home_open_picker(HomePicker::Ref);

        assert_eq!(
            app.home_ref_options()
                .iter()
                .map(|git_ref| git_ref.name.as_str())
                .collect::<Vec<_>>(),
            vec!["cached/topic"]
        );
        assert_eq!(
            app.home.as_ref().and_then(|home| home.picker),
            Some(HomePicker::Ref)
        );
        assert_eq!(app.request_home_ref_refresh, Some(repo_root));
    }

    #[test]
    fn dispatch_plan_applies_each_workspace_variant() {
        let mut home = HomeState {
            prompt: "run the checks".into(),
            directory: PathBuf::from("/repo/root"),
            workspace: HomeWorkspace::CurrentCheckout,
            ..Default::default()
        };
        home.set_access(HomeAccess::ClaudeAcceptEdits);
        let expected_argv = [
            "claude",
            "--model",
            "claude-opus-5[1m]",
            "--permission-mode",
            "acceptEdits",
            "run the checks",
        ];
        let current = home.dispatch_plan().expect("current checkout plan");
        assert_eq!(current.directory, PathBuf::from("/repo/root"));
        assert_eq!(current.workspace, HomeWorkspace::CurrentCheckout);
        assert_eq!(current.argv, expected_argv);

        home.workspace = HomeWorkspace::NewWorktree;
        let new_worktree = home.dispatch_plan().expect("new worktree plan");
        assert_eq!(new_worktree.directory, PathBuf::from("/repo/root"));
        assert_eq!(new_worktree.workspace, HomeWorkspace::NewWorktree);
        assert_eq!(new_worktree.argv, expected_argv);

        home.workspace = HomeWorkspace::PreviousWorktree(PathBuf::from("/worktrees/old"));
        let previous = home.dispatch_plan().expect("previous worktree plan");
        assert_eq!(previous.directory, PathBuf::from("/worktrees/old"));
        assert_eq!(
            previous.workspace,
            HomeWorkspace::PreviousWorktree(PathBuf::from("/worktrees/old"))
        );
        assert_eq!(previous.argv, expected_argv);
    }

    #[test]
    fn pending_worktree_creation_keeps_home_open() {
        let mut home = HomeState {
            prompt: "wait for the worktree".into(),
            workspace: HomeWorkspace::NewWorktree,
            ..Default::default()
        };
        home.pending_dispatch = Some(home.dispatch_plan().expect("pending plan"));

        assert!(!home.close_composer_or_home());
        assert_eq!(home.focus, Some(HomeFocus::Prompt));
        assert!(home.pending_dispatch.is_some());
    }

    #[test]
    fn launch_argv_uses_exact_provider_flags_for_explicit_choices() {
        let mut home = home_with_codex_catalog();
        home.prompt = "implement the retry cap".into();
        home.set_model("claude-fable-5-1");
        home.effort = Some("high".into());
        home.set_access(HomeAccess::ClaudeBypass);
        assert_eq!(
            home.dispatch_plan().expect("Claude plan").argv,
            vec![
                "claude",
                "--model",
                "claude-fable-5-1[1m]",
                "--effort",
                "high",
                "--dangerously-skip-permissions",
                "implement the retry cap",
            ]
        );

        home.set_agent(Agent::Codex);
        home.set_model("gpt-5.6-sol");
        home.effort = Some("ultra".into());
        home.set_access(HomeAccess::CodexReadOnly);
        assert_eq!(
            home.dispatch_plan().expect("Codex plan").argv,
            vec![
                "codex",
                "--model",
                "gpt-5.6-sol",
                "-c",
                "model_reasoning_effort=ultra",
                "-s",
                "read-only",
                "implement the retry cap",
            ]
        );
    }

    fn projects_for_test() -> Vec<crate::app::projects::Project> {
        use crate::app::projects::{Project, ProjectRepo};
        vec![
            Project {
                id: "scalable".into(),
                label: "scalable".into(),
                repos: vec![ProjectRepo {
                    name: "herdr".into(),
                    path: std::env::temp_dir().join("herdr"),
                }],
            },
            Project {
                id: "personal".into(),
                label: "personal".into(),
                repos: Vec::new(),
            },
        ]
    }

    #[test]
    fn switching_project_forgets_the_repo_and_hides_the_chip_when_the_group_is_empty() {
        let mut home = HomeState::default();
        assert!(
            !home.project_visible() && !home.repo_visible(),
            "an unconfigured composer offers no project fields"
        );

        home.set_projects(projects_for_test());
        assert!(home.project_visible(), "two groups is a choice");
        assert_eq!(
            home.project().map(|project| project.id.as_str()),
            Some("scalable")
        );
        assert!(home.repo_visible());

        home.repo = Some("herdr".into());
        home.set_project("personal");
        assert_eq!(
            home.repo, None,
            "a checkout name means nothing in another group"
        );
        assert!(
            !home.repo_visible(),
            "a group with no checkouts offers no repo chip"
        );
    }

    #[test]
    fn composer_directory_selects_its_configured_project_and_repo() {
        use crate::app::projects::{Project, ProjectRepo};

        let root = browse_fixture("project-selection");
        let checkout = root.join("alpha");
        let nested = checkout.join("src");
        std::fs::create_dir_all(&nested).expect("nested checkout directory");
        let configured_path = checkout.join("..").join("alpha");
        let mut app = crate::app::state::AppState::test_new();
        app.projects = vec![
            Project {
                id: "personal".into(),
                label: "personal".into(),
                repos: vec![ProjectRepo {
                    name: "notes".into(),
                    path: root.join("beta"),
                }],
            },
            Project {
                id: "work".into(),
                label: "work".into(),
                repos: vec![ProjectRepo {
                    name: "herdr".into(),
                    path: configured_path,
                }],
            },
        ];
        let mut home = app.new_home_state();
        home.repo = Some("notes".into());
        app.home = Some(home);

        app.home_set_directory(crate::worktree::canonical_or_original(&nested));

        let home = app.home.as_ref().expect("composer");
        assert_eq!(home.project, "work");
        assert_eq!(home.repo.as_deref(), Some("herdr"));

        app.home_set_directory(root.join("alpine"));
        let home = app.home.as_ref().expect("composer");
        assert_eq!(
            home.project, "work",
            "an unmatched directory keeps the group"
        );
        assert_eq!(
            home.repo, None,
            "an unmatched directory clears a stale repo"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn opening_composer_in_a_repo_preselects_its_project_and_repo() {
        use crate::app::projects::{Project, ProjectRepo};

        let root = browse_fixture("open-project-selection");
        let checkout = root.join("alpha");
        let mut app = crate::app::state::AppState::test_new();
        app.projects = vec![
            Project {
                id: "personal".into(),
                label: "personal".into(),
                repos: Vec::new(),
            },
            Project {
                id: "work".into(),
                label: "work".into(),
                repos: vec![ProjectRepo {
                    name: "herdr".into(),
                    path: checkout.clone(),
                }],
            },
        ];

        app.open_home_composer_in_directory(checkout, HomeWorkspace::CurrentCheckout);

        let home = app.home.as_ref().expect("composer");
        assert_eq!(home.project, "work");
        assert_eq!(home.repo.as_deref(), Some("herdr"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn accepting_a_repo_keeps_its_selection_after_canonicalizing_the_directory() {
        use crate::app::projects::{Project, ProjectRepo};

        let root = browse_fixture("repo-picker-selection");
        let checkout = root.join("alpha");
        let configured_path = checkout.join("..").join("alpha");
        let mut app = crate::app::state::AppState::test_new();
        app.projects = vec![Project {
            id: "work".into(),
            label: "work".into(),
            repos: vec![ProjectRepo {
                name: "herdr".into(),
                path: configured_path,
            }],
        }];
        let mut home = app.new_home_state();
        home.picker = Some(HomePicker::Repo);
        home.picker_selected = 0;
        app.home = Some(home);

        app.home_accept_picker();

        let home = app.home.as_ref().expect("composer");
        assert_eq!(
            home.directory,
            crate::worktree::canonical_or_original(&checkout)
        );
        assert_eq!(home.repo.as_deref(), Some("herdr"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn focus_traversal_skips_the_project_fields_it_is_not_drawing() {
        let visibility = HomeFieldVisibility {
            effort: false,
            access: false,
            context: false,
            project: false,
            repo: false,
            machine: false,
        };
        assert_eq!(HomeFocus::Model.next(visibility), HomeFocus::Directory);
        assert_eq!(HomeFocus::Directory.previous(visibility), HomeFocus::Model);

        let visibility = HomeFieldVisibility {
            project: true,
            repo: true,
            ..visibility
        };
        assert_eq!(HomeFocus::Model.next(visibility), HomeFocus::Project);
        assert_eq!(HomeFocus::Project.next(visibility), HomeFocus::Repo);
        assert_eq!(HomeFocus::Repo.next(visibility), HomeFocus::Directory);
        assert_eq!(HomeFocus::Directory.previous(visibility), HomeFocus::Repo);

        // The machine sits after the directory: you choose what to work on,
        // then where it runs.
        assert_eq!(HomeFocus::Directory.next(visibility), HomeFocus::Workspace);
        let visibility = HomeFieldVisibility {
            machine: true,
            ..visibility
        };
        assert_eq!(HomeFocus::Directory.next(visibility), HomeFocus::Machine);
        assert_eq!(HomeFocus::Machine.next(visibility), HomeFocus::Workspace);
        assert_eq!(
            HomeFocus::Workspace.previous(visibility),
            HomeFocus::Machine
        );

        // The ring wraps, and Reply enters it rather than sitting in it.
        assert_eq!(HomeFocus::Target.next(visibility), HomeFocus::Prompt);
        assert_eq!(HomeFocus::Prompt.previous(visibility), HomeFocus::Target);
        assert_eq!(HomeFocus::Reply.next(visibility), HomeFocus::Prompt);
        assert_eq!(HomeFocus::Reply.previous(visibility), HomeFocus::Target);
    }

    #[test]
    fn each_provider_dispatches_on_its_unattended_access_mode() {
        let home = HomeState::default();
        assert_eq!(home.agent, Agent::Claude);
        assert_eq!(home.access, Some(HomeAccess::ClaudeBypass));

        // Switching providers and back keeps the provider default rather than
        // falling to the first, most restricted option.
        let mut home = HomeState::default();
        home.set_agent(Agent::Codex);
        assert_eq!(home.access, Some(HomeAccess::CodexFull));
        home.set_agent(Agent::Claude);
        assert_eq!(home.access, Some(HomeAccess::ClaudeBypass));
    }

    #[test]
    fn access_modes_map_to_exact_provider_argv() {
        let cases = [
            (
                Agent::Claude,
                HomeAccess::ClaudeDefault,
                vec!["--permission-mode", "default"],
            ),
            (
                Agent::Claude,
                HomeAccess::ClaudeAcceptEdits,
                vec!["--permission-mode", "acceptEdits"],
            ),
            (
                Agent::Claude,
                HomeAccess::ClaudePlan,
                vec!["--permission-mode", "plan"],
            ),
            (
                Agent::Claude,
                HomeAccess::ClaudeBypass,
                vec!["--dangerously-skip-permissions"],
            ),
            (
                Agent::Codex,
                HomeAccess::CodexReadOnly,
                vec!["-s", "read-only"],
            ),
            (
                Agent::Codex,
                HomeAccess::CodexWorkspaceWrite,
                vec!["-s", "workspace-write"],
            ),
            (
                Agent::Codex,
                HomeAccess::CodexFull,
                vec![
                    "-s",
                    "danger-full-access",
                    "--dangerously-bypass-approvals-and-sandbox",
                ],
            ),
        ];

        for (agent, access, expected_flags) in cases {
            let mut home = home_with_codex_catalog();
            home.prompt = "ship it".into();
            home.set_agent(agent);
            // This case is about the access flags alone, so hold the model on
            // the provider-neutral entry that emits no `--model`.
            home.set_model(DEFAULT_MODEL);
            home.set_access(access);
            let plan = home.dispatch_plan().expect("access mode should dispatch");
            let mut expected = vec![crate::detect::interactive_agent_executable(agent)];
            expected.extend(expected_flags);
            expected.push("ship it");
            assert_eq!(plan.argv, expected, "{agent:?} {access:?}");
        }
    }

    #[test]
    fn provider_choices_survive_agent_switches_and_home_reopen() {
        let mut app = crate::app::state::AppState::test_new();
        app.home_catalog = home_with_codex_catalog().catalog;
        let mut home = app.new_home_state();
        home.set_model("claude-fable-5-1");
        home.set_effort(Some("high".into()));
        home.set_access(HomeAccess::ClaudePlan);
        home.set_agent(Agent::Codex);
        home.set_model("gpt-5.6-sol");
        home.set_effort(Some("ultra".into()));
        home.set_access(HomeAccess::CodexFull);
        home.set_agent(Agent::Claude);

        assert_eq!(home.model, "claude-fable-5-1");
        assert_eq!(home.effort.as_deref(), Some("high"));
        assert_eq!(home.access, Some(HomeAccess::ClaudePlan));
        app.home = Some(home);
        app.clear_home();

        let mut reopened = app.new_home_state();
        assert_eq!(reopened.model, "claude-fable-5-1");
        assert_eq!(reopened.effort.as_deref(), Some("high"));
        assert_eq!(reopened.access, Some(HomeAccess::ClaudePlan));
        reopened.set_agent(Agent::Codex);
        assert_eq!(reopened.model, "gpt-5.6-sol");
        assert_eq!(reopened.effort.as_deref(), Some("ultra"));
        assert_eq!(reopened.access, Some(HomeAccess::CodexFull));
    }

    #[test]
    fn context_window_is_visible_only_for_supported_claude_models() {
        let mut home = HomeState::default();
        home.set_model("claude-haiku-4-5-20251001");
        assert!(!home.context_visible());

        home.set_model("claude-fable-5-1");
        assert_eq!(home.context_window.as_deref(), Some(LARGE_CONTEXT_WINDOW));
        assert_eq!(
            home.context_options(),
            [DEFAULT_CONTEXT_WINDOW, LARGE_CONTEXT_WINDOW]
        );

        home.set_model("claude-haiku-4-5-20251001");
        assert!(!home.context_visible());
        home.set_agent(Agent::Codex);
        assert!(!home.context_visible());
    }

    #[test]
    fn explicit_narrow_context_survives_agent_and_large_model_switches() {
        let mut home = home_with_codex_catalog();
        home.set_context_window(Some(DEFAULT_CONTEXT_WINDOW.into()));
        home.set_model("claude-fable-5-1");

        home.set_agent(Agent::Codex);
        home.set_agent(Agent::Claude);
        assert_eq!(home.context_window.as_deref(), Some(DEFAULT_CONTEXT_WINDOW));

        home.set_model("claude-opus-5");
        assert_eq!(home.context_window.as_deref(), Some(DEFAULT_CONTEXT_WINDOW));
    }

    #[test]
    fn one_million_context_uses_the_fallback_model_alias() {
        let mut home = HomeState::test_with_prompt("use the larger window");
        home.set_model("claude-opus-5");
        home.context_window = Some(LARGE_CONTEXT_WINDOW.into());

        assert_eq!(
            home.dispatch_plan().expect("Claude plan").argv,
            [
                "claude",
                "--model",
                "claude-opus-5[1m]",
                "--dangerously-skip-permissions",
                "use the larger window",
            ]
        );
    }

    #[test]
    fn one_million_context_uses_a_documented_cli_flag() {
        let claude = super::super::home_catalog::parse_claude_help(
            "--effort <level> (low, medium, high)\n--context-window <size> (200k, 1m)\n",
        );
        let mut home = HomeState::with_catalog(HomeCatalog::with_claude(claude));
        home.prompt = "use the larger window".into();
        home.set_model("claude-sonnet-5");
        home.context_window = Some(LARGE_CONTEXT_WINDOW.into());

        assert_eq!(
            home.dispatch_plan().expect("Claude plan").argv,
            [
                "claude",
                "--model",
                "claude-sonnet-5",
                "--context-window",
                "1m",
                "--dangerously-skip-permissions",
                "use the larger window",
            ]
        );
    }

    /// A directory tree the browse input can be driven against.
    fn browse_fixture(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "herdr-home-browse-{}-{name}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|since| since.as_nanos())
                .unwrap_or_default()
        ));
        for child in ["alpha", "alpine", "beta", ".hidden"] {
            std::fs::create_dir_all(root.join(child)).expect("fixture directory");
        }
        std::fs::write(root.join("alpha.txt"), b"not a directory").expect("fixture file");
        root
    }

    fn app_with_home(directory: &Path) -> crate::app::state::AppState {
        let mut app = crate::app::state::AppState::test_new();
        app.home = Some(HomeState {
            directory: directory.to_path_buf(),
            ..HomeState::default()
        });
        app
    }

    #[test]
    fn the_focus_cycle_visits_the_headline_directory_between_the_chips_and_the_workspace() {
        let mut home = HomeState {
            context_window: Some(DEFAULT_CONTEXT_WINDOW.into()),
            focus: Some(HomeFocus::Prompt),
            ..HomeState::default()
        };

        let mut order = Vec::new();
        for _ in 0..9 {
            home.move_focus(false);
            order.push(home.focus.expect("focus"));
        }

        assert_eq!(
            order,
            vec![
                HomeFocus::Agent,
                HomeFocus::Model,
                HomeFocus::Effort,
                HomeFocus::Access,
                HomeFocus::Context,
                HomeFocus::Directory,
                HomeFocus::Workspace,
                HomeFocus::Ref,
                HomeFocus::Target,
            ]
        );

        // And the same stations backwards.
        let mut backwards = Vec::new();
        for _ in 0..9 {
            home.move_focus(true);
            backwards.push(home.focus.expect("focus"));
        }
        backwards.reverse();
        assert_eq!(backwards[1..], order[..order.len() - 1]);
    }

    #[test]
    fn the_directory_picker_lists_recents_then_worktrees_then_project_import() {
        let mut app = app_with_home(Path::new("/tmp/t3-f2-current"));
        app.home.as_mut().expect("home").worktree_options = vec![
            PathBuf::from("/tmp/t3-f2-current"),
            PathBuf::from("/tmp/t3-f2-linked"),
        ];

        let options = app.home_directory_picker_options();

        assert_eq!(
            options.first(),
            Some(&HomeDirectoryOption::Recent(PathBuf::from(
                "/tmp/t3-f2-current"
            ))),
            "the selected directory leads the recents"
        );
        assert_eq!(
            options.last(),
            Some(&HomeDirectoryOption::AddProject),
            "add project is always the last option"
        );
        assert_eq!(
            options.get(options.len().saturating_sub(2)),
            Some(&HomeDirectoryOption::Browse),
            "browse stays immediately before add project"
        );
        let worktrees = options
            .iter()
            .filter(|option| matches!(option, HomeDirectoryOption::Worktree(_)))
            .collect::<Vec<_>>();
        assert_eq!(
            worktrees,
            vec![&HomeDirectoryOption::Worktree(PathBuf::from(
                "/tmp/t3-f2-linked"
            ))],
            "a worktree already listed as a recent is not repeated"
        );
        let first_worktree = options
            .iter()
            .position(|option| matches!(option, HomeDirectoryOption::Worktree(_)))
            .expect("a linked worktree");
        let last_recent = options
            .iter()
            .rposition(|option| matches!(option, HomeDirectoryOption::Recent(_)))
            .expect("a recent directory");
        assert!(
            last_recent < first_worktree,
            "recents come first: {options:?}"
        );
        assert_eq!(
            options.last().map(HomeDirectoryOption::label),
            Some(ADD_PROJECT_OPTION_LABEL.to_string())
        );
    }

    #[test]
    fn choosing_add_project_opens_modal_and_keeps_prompt() {
        let directory = browse_fixture("add-project");
        let mut app = app_with_home(&directory);
        app.home.as_mut().expect("home").prompt = "preserve me".into();
        app.home_open_picker(HomePicker::Directory);
        let add_index = app
            .home_directory_picker_options()
            .iter()
            .position(|option| *option == HomeDirectoryOption::AddProject)
            .expect("add project option");
        app.home.as_mut().expect("home").directory_filter.selected = add_index;

        app.home_accept_picker();

        let home = app.home.as_ref().expect("home");
        assert_eq!(home.prompt, "preserve me");
        assert!(home.picker.is_none());
        assert_eq!(
            home.add_project.as_ref().map(|project| project.tab),
            Some(AddProjectTab::LocalFolder)
        );
    }

    #[test]
    fn browse_starts_in_the_configured_add_project_start_dir() {
        let directory = browse_fixture("home");
        let configured = browse_fixture("configured");
        let mut app = app_with_home(&directory);
        app.add_project_start_dir = configured.display().to_string();

        assert_eq!(app.home_browse_start_directory(), configured);

        app.home_open_picker(HomePicker::Directory);
        let browse_index = app
            .home_directory_picker_options()
            .iter()
            .position(|option| *option == HomeDirectoryOption::Browse)
            .expect("browse option");
        app.home.as_mut().expect("home").directory_filter.selected = browse_index;
        app.home_accept_picker();

        let browse = app.home_browse().expect("path input");
        assert!(
            browse.input.starts_with(&configured.display().to_string()),
            "the input starts on the configured directory: {:?}",
            browse.input
        );

        let _ = std::fs::remove_dir_all(&directory);
        let _ = std::fs::remove_dir_all(&configured);
    }

    #[test]
    fn browse_falls_back_when_the_configured_start_dir_is_unset_or_missing() {
        let directory = browse_fixture("fallback");
        let mut app = app_with_home(&directory);

        assert_eq!(app.home_browse_start_directory(), directory);

        app.add_project_start_dir = "   ".to_string();
        assert_eq!(app.home_browse_start_directory(), directory);

        app.add_project_start_dir = directory.join("does-not-exist").display().to_string();
        assert_eq!(app.home_browse_start_directory(), directory);

        // A file is not a directory the picker can open.
        app.add_project_start_dir = directory.join("alpha.txt").display().to_string();
        assert_eq!(app.home_browse_start_directory(), directory);

        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn choosing_browse_opens_the_path_input_instead_of_closing_the_picker() {
        let directory = browse_fixture("open");
        let mut app = app_with_home(&directory);
        app.home_open_picker(HomePicker::Directory);
        let browse_index = app
            .home_directory_picker_options()
            .iter()
            .position(|option| *option == HomeDirectoryOption::Browse)
            .expect("browse option");
        app.home.as_mut().expect("home").directory_filter.selected = browse_index;

        app.home_accept_picker();

        let home = app.home.as_ref().expect("home");
        assert_eq!(home.picker, Some(HomePicker::Directory));
        let browse = home.browse.as_ref().expect("path input");
        assert!(
            browse.input.starts_with(&directory.display().to_string()),
            "the input starts on the current directory: {:?}",
            browse.input
        );
        assert_eq!(browse.children, vec!["alpha", "alpine", "beta"]);

        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn tab_completes_the_next_path_component_from_the_filesystem() {
        let directory = browse_fixture("complete");
        let mut app = app_with_home(&directory);
        app.home_open_picker(HomePicker::Directory);
        if let Some(home) = app.home.as_mut() {
            home.browse = Some(HomeBrowse::starting_at(&directory));
        }

        // Two directories share the prefix, so completion stops where they part.
        app.home_push_picker_filter('a');
        app.home_push_picker_filter('l');
        app.home_browse_complete();
        assert_eq!(
            app.home_browse().map(|browse| browse.input.clone()),
            Some(directory.join("alp").display().to_string())
        );

        // One more character leaves a single match, which completes whole.
        app.home_push_picker_filter('h');
        app.home_browse_complete();
        assert_eq!(
            app.home_browse().map(|browse| browse.input.clone()),
            Some(format!(
                "{}{}",
                directory.join("alpha").display(),
                std::path::MAIN_SEPARATOR
            ))
        );
        assert!(
            app.home_browse()
                .is_some_and(|browse| browse.children.is_empty()),
            "the completed directory has no children of its own"
        );

        // Files never complete, and hidden directories only once asked for.
        app.home_pop_picker_filter();
        while app
            .home_browse()
            .is_some_and(|browse| browse.input != directory.join("").display().to_string())
        {
            app.home_pop_picker_filter();
        }
        assert!(app
            .home_browse()
            .is_some_and(|browse| !browse.children.contains(&"alpha.txt".to_string())));
        app.home_push_picker_filter('.');
        assert_eq!(
            app.home_browse().map(|browse| browse.children.clone()),
            Some(vec![".hidden".to_string()])
        );

        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn enter_on_a_path_that_is_not_a_directory_is_refused_and_keeps_the_composer_open() {
        let directory = browse_fixture("refused");
        let mut app = app_with_home(&directory);
        app.home_open_picker(HomePicker::Directory);
        if let Some(home) = app.home.as_mut() {
            home.browse = Some(HomeBrowse::starting_at(&directory));
        }
        for character in "nowhere".chars() {
            app.home_push_picker_filter(character);
        }

        app.home_browse_accept();

        let home = app.home.as_ref().expect("home");
        assert_eq!(home.directory, directory, "the directory is unchanged");
        assert_eq!(home.picker, Some(HomePicker::Directory));
        assert_eq!(
            home.browse
                .as_ref()
                .and_then(|browse| browse.error.as_deref()),
            Some(NO_SUCH_DIRECTORY)
        );
        assert!(home.focus.is_some(), "the composer stays open");

        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn enter_on_an_existing_path_sets_the_directory_and_the_headline_name() {
        let directory = browse_fixture("accepted");
        let mut app = app_with_home(&directory);
        app.home_open_picker(HomePicker::Directory);
        if let Some(home) = app.home.as_mut() {
            home.browse = Some(HomeBrowse::starting_at(&directory));
        }
        for character in "alpha".chars() {
            app.home_push_picker_filter(character);
        }

        app.home_browse_accept();

        let home = app.home.as_ref().expect("home");
        assert_eq!(
            home.directory,
            crate::worktree::canonical_or_original(&directory.join("alpha"))
        );
        assert!(home.picker.is_none(), "accepting closes the picker");
        assert!(home.browse.is_none());
        assert_eq!(app.home_headline_name(), "alpha");

        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn the_headline_names_the_repository_root_the_cache_resolved() {
        let mut app = app_with_home(Path::new("/tmp/t3-f2-worktree"));
        app.git_root_for_cwd.insert(
            PathBuf::from("/tmp/t3-f2-worktree"),
            Some(PathBuf::from("/tmp/checkouts/herdr")),
        );

        assert_eq!(app.home_headline_name(), "herdr");
    }

    /// A `git worktree add` checkout whose root is the task directory, next to
    /// the main checkout it belongs to.
    fn linked_worktree_fixture() -> Option<(PathBuf, PathBuf, PathBuf)> {
        let root = std::env::temp_dir().join(format!(
            "herdr-home-linked-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|since| since.as_nanos())
                .unwrap_or_default()
        ));
        let checkout = root.join("herdr");
        let linked = root.join("t3-f2");
        std::fs::create_dir_all(&checkout).expect("fixture directory");
        let git = |cwd: &Path, args: &[&str]| -> bool {
            std::process::Command::new("git")
                .args(args)
                .current_dir(cwd)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|status| status.success())
                .unwrap_or(false)
        };
        let prepared = git(&checkout, &["init", "--initial-branch", "main"])
            && git(&checkout, &["config", "user.email", "fixture@example.com"])
            && git(&checkout, &["config", "user.name", "fixture"])
            && git(&checkout, &["commit", "--allow-empty", "-m", "root"])
            && git(
                &checkout,
                &[
                    "worktree",
                    "add",
                    "-b",
                    "task",
                    linked.to_str().unwrap_or_default(),
                ],
            );
        if !prepared {
            let _ = std::fs::remove_dir_all(&root);
            return None;
        }
        Some((root, checkout, linked))
    }

    #[test]
    fn the_headline_names_the_repository_for_a_linked_worktree() {
        let Some((root, checkout, linked)) = linked_worktree_fixture() else {
            return;
        };

        let mut app = app_with_home(&checkout);
        app.home_set_directory(crate::worktree::canonical_or_original(&checkout));
        assert_eq!(app.home_headline_name(), "herdr", "the main checkout");

        // What a pane working in the linked worktree caches: its own checkout
        // root, which is named after the task and not after the repository.
        let linked = crate::worktree::canonical_or_original(&linked);
        app.git_root_for_cwd
            .insert(linked.clone(), Some(linked.clone()));
        app.home_set_directory(linked.clone());
        assert_eq!(
            directory_basename(&linked),
            "t3-f2",
            "the fixture worktree is task-named"
        );
        assert_eq!(
            app.home_headline_name(),
            "herdr",
            "a linked worktree is named after its repository, not its task"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_headline_never_reads_as_the_machine_for_a_home_directory() {
        let Some(home_directory) = std::env::var_os("HOME").map(PathBuf::from) else {
            return;
        };
        let app = app_with_home(&home_directory);

        let name = app.home_headline_name();

        assert_ne!(
            Some(name.as_str()),
            home_directory.file_name().and_then(|name| name.to_str()),
            "the account name is not a place name"
        );
        assert_eq!(name, "~");
    }

    #[test]
    fn claude_opens_on_opus_with_bypass_and_a_context_choice() {
        let home = HomeState::default();
        assert_eq!(home.agent, Agent::Claude);
        assert_eq!(home.model, "claude-opus-5");
        assert_eq!(home.access, Some(HomeAccess::ClaudeBypass));
        assert_eq!(home.context_window.as_deref(), Some(LARGE_CONTEXT_WINDOW));
        assert!(
            home.context_visible(),
            "opus carries a 1M window, so the composer must offer the choice"
        );
        assert_eq!(
            home.context_options(),
            [DEFAULT_CONTEXT_WINDOW, LARGE_CONTEXT_WINDOW]
        );
    }

    #[test]
    fn codex_opens_on_sol_at_medium_with_full_access() {
        let codex = super::super::home_catalog::parse_codex_catalog(
            br#"{"models":[
                {"slug":"gpt-5.6-luna","visibility":"list","priority":1,"supported_reasoning_levels":[{"effort":"low"},{"effort":"medium"},{"effort":"xhigh"}]},
                {"slug":"gpt-5.6-sol","visibility":"list","priority":2,"supported_reasoning_levels":[{"effort":"low"},{"effort":"medium"},{"effort":"high"}]}
            ]}"#,
        )
        .expect("Codex fixture");
        let mut home = HomeState::with_catalog(HomeCatalog::with_codex(codex));
        home.set_agent(Agent::Codex);
        assert_eq!(
            home.model, "gpt-5.6-sol",
            "the preference outranks the catalog's own first entry"
        );
        assert_eq!(home.effort.as_deref(), Some("medium"));
        assert_eq!(home.access, Some(HomeAccess::CodexFull));
    }

    #[test]
    fn a_preference_the_catalog_does_not_carry_falls_back_to_its_first_entry() {
        let codex = super::super::home_catalog::parse_codex_catalog(
            br#"{"models":[
                {"slug":"gpt-5.6-terra","visibility":"list","priority":1,"supported_reasoning_levels":[{"effort":"low"}]}
            ]}"#,
        )
        .expect("Codex fixture");
        let mut home = HomeState::with_catalog(HomeCatalog::with_codex(codex));
        home.set_agent(Agent::Codex);
        assert_eq!(
            home.model, "default",
            "sol is absent, so the catalog's own first entry stands"
        );
        assert_eq!(
            home.effort.as_deref(),
            Some(AUTO_EFFORT),
            "medium is not offered, so the catalog's own first effort stands"
        );
    }

    #[test]
    fn an_explicit_model_choice_survives_a_provider_round_trip() {
        let mut home = HomeState::default();
        home.set_model("claude-sonnet-5");
        home.set_agent(Agent::Codex);
        home.set_agent(Agent::Claude);
        assert_eq!(
            home.model, "claude-sonnet-5",
            "the preference must not overwrite what the operator picked"
        );
    }

    fn kimi_lane() -> crate::app::launch_profiles::LaunchProfile {
        crate::app::launch_profiles::resolve(&[crate::config::LaunchProfileConfig {
            id: "kimi".into(),
            label: "Kimi K3".into(),
            agent: "claude".into(),
            command: vec!["bash".into(), "-lc".into(), "cck3".into()],
            usage: Some("kimi".into()),
            ..Default::default()
        }])
        .pop()
        .expect("kimi lane")
    }

    #[test]
    fn a_lane_that_owns_its_flags_dispatches_its_command_and_hides_the_flag_pickers() {
        let mut home = HomeState::default();
        home.set_profiles(vec![
            crate::app::launch_profiles::LaunchProfile::builtin(Agent::Claude),
            kimi_lane(),
        ]);
        home.set_profile("kimi");
        home.prompt = "  cap the retry loop  ".into();

        assert_eq!(home.profile().label, "Kimi K3");
        assert_eq!(
            home.agent,
            Agent::Claude,
            "the lane still runs Claude Code, so detection and icons resolve"
        );
        assert!(
            !home.access_visible() && !home.context_visible(),
            "the lane sets its own permission mode and context window"
        );

        let plan = home.dispatch_plan().expect("the lane dispatches");
        assert_eq!(
            plan.argv,
            vec!["bash", "-lc", "cck3", "cap the retry loop"],
            "Herdr appends the prompt and nothing else"
        );
        assert!(plan.env.is_empty());
    }

    #[test]
    fn an_env_only_lane_still_builds_flags_and_carries_its_environment() {
        let lane = crate::app::launch_profiles::resolve(&[crate::config::LaunchProfileConfig {
            id: "codex-scalable".into(),
            agent: "codex".into(),
            env: [("CODEX_HOME".to_string(), "/tmp/codex-home".to_string())]
                .into_iter()
                .collect(),
            ..Default::default()
        }])
        .pop()
        .expect("codex lane");

        let mut home = home_with_codex_catalog();
        home.set_profiles(vec![
            crate::app::launch_profiles::LaunchProfile::builtin(Agent::Claude),
            lane,
        ]);
        home.set_profile("codex-scalable");
        home.prompt = "ship it".into();

        assert!(
            home.access_visible(),
            "no command, so the pickers still apply"
        );
        let plan = home.dispatch_plan().expect("the lane dispatches");
        assert_eq!(plan.argv.first().map(String::as_str), Some("codex"));
        assert!(
            plan.argv.iter().any(|argument| argument == "--model"),
            "{:?}",
            plan.argv
        );
        assert_eq!(
            plan.env,
            [("CODEX_HOME".to_string(), "/tmp/codex-home".to_string())]
        );
    }

    #[test]
    fn an_unknown_lane_id_leaves_the_selection_alone() {
        let mut home = HomeState::default();
        let before = home.profile().id.clone();
        home.set_profile("no-such-lane");
        assert_eq!(home.profile().id, before);
    }
}
