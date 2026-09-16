use serde::{Deserialize, Serialize};

use super::wire::{
    ClientShellAgent, ClientShellCommand, ClientShellCommandAction, ClientShellPane,
    ClientShellPopupSurface, ClientShellProductAnnouncement, ClientShellReleaseNotes,
    ClientShellSnapshot, ClientShellTab, ClientShellTabStatusSegment, ClientShellWorkspace,
    ClientShellWorktree, CursorState, PaneSurfaceFrame, PaneSurfacePane, PaneSurfacePatch,
    PaneSurfacePatchRow, PaneSurfaceScrollMetrics, PaneSurfaceSplit, SurfaceGraphicsAsset,
    SurfaceGraphicsAssetKey, SurfaceGraphicsPlacement, SurfaceGraphicsScene, SurfaceRect,
    MAX_FRAME_SIZE,
};

pub const SNAPSHOT_CODEC_DELTA_V2: &str = "shell.snapshot.delta.v2";
pub const SURFACE_CODEC_DELTA_V2: &str = "shell.surface.delta.v2";

/// Sparse field update for JSON metadata deltas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
pub enum FieldUpdate<T> {
    #[default]
    Unchanged,
    Set(T),
}

impl<T> FieldUpdate<T> {
    pub fn is_unchanged(&self) -> bool {
        matches!(self, Self::Unchanged)
    }

    pub fn apply_to(self, target: &mut T) {
        if let Self::Set(val) = self {
            *target = val;
        }
    }
}

impl<T: PartialEq + Clone> FieldUpdate<T> {
    pub fn diff(base: &T, next: &T) -> Self {
        if base == next {
            Self::Unchanged
        } else {
            Self::Set(next.clone())
        }
    }
}

/// Fixed-layout field update for binary bincode surface DTOs (no serde tag/content).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SurfaceFieldUpdate<T> {
    #[default]
    Unchanged,
    Set(T),
}

impl<T> SurfaceFieldUpdate<T> {
    pub fn apply_to(&self, target: &mut T)
    where
        T: Clone,
    {
        if let Self::Set(val) = self {
            target.clone_from(val);
        }
    }
}

impl<T: PartialEq + Clone> SurfaceFieldUpdate<T> {
    pub fn diff(base: &T, next: &T) -> Self {
        if base == next {
            Self::Unchanged
        } else {
            Self::Set(next.clone())
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ClientShellTokenDelta {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub set: Vec<(String, String)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remove: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<Vec<String>>,
}

impl ClientShellTokenDelta {
    pub fn is_empty(&self) -> bool {
        self.set.is_empty() && self.remove.is_empty() && self.order.is_none()
    }

    fn validate(&self, tokens: &[(String, String)]) -> Result<(), String> {
        validate_unique(self.set.iter().map(|(key, _)| key.as_str()), "token set")?;
        validate_unique(self.remove.iter().map(String::as_str), "token removal")?;
        for key in &self.remove {
            if !tokens.iter().any(|(existing, _)| existing == key)
                || self.set.iter().any(|(updated, _)| updated == key)
            {
                return Err("unknown or conflicting token removal".into());
            }
        }
        if let Some(order) = &self.order {
            let added = self
                .set
                .iter()
                .filter(|(key, _)| !tokens.iter().any(|(existing, _)| existing == key))
                .count();
            validate_order(order, tokens.len() - self.remove.len() + added, |key| {
                self.set.iter().any(|(updated, _)| updated == key)
                    || (tokens.iter().any(|(existing, _)| existing == key)
                        && !self.remove.iter().any(|removed| removed == key))
            })?;
        }
        Ok(())
    }

    fn apply_validated(&self, tokens: &mut Vec<(String, String)>) {
        if !self.remove.is_empty() {
            tokens.retain(|(key, _)| !self.remove.contains(key));
        }
        for (key, val) in &self.set {
            if let Some(existing) = tokens.iter_mut().find(|(k, _)| k == key) {
                existing.1 = val.clone();
            } else {
                tokens.push((key.clone(), val.clone()));
            }
        }
        if let Some(order) = &self.order {
            reorder_entities(tokens, order, |(key, _)| key);
        }
    }

    pub fn diff(base: &[(String, String)], next: &[(String, String)]) -> Option<Self> {
        if base == next {
            return None;
        }
        let mut set = Vec::new();
        let mut remove = Vec::new();
        for (b_key, _) in base {
            if !next.iter().any(|(n_key, _)| n_key == b_key) {
                remove.push(b_key.clone());
            }
        }
        for (n_key, n_val) in next {
            if let Some((_, b_val)) = base.iter().find(|(b_key, _)| b_key == n_key) {
                if b_val != n_val {
                    set.push((n_key.clone(), n_val.clone()));
                }
            } else {
                set.push((n_key.clone(), n_val.clone()));
            }
        }
        let natural_order_matches = base
            .iter()
            .filter(|(key, _)| !remove.contains(key))
            .map(|(key, _)| key)
            .chain(
                set.iter()
                    .filter(|(key, _)| !base.iter().any(|(existing, _)| existing == key))
                    .map(|(key, _)| key),
            )
            .eq(next.iter().map(|(key, _)| key));
        let order =
            (!natural_order_matches).then(|| next.iter().map(|(key, _)| key.clone()).collect());
        let delta = Self { set, remove, order };
        (!delta.is_empty()).then_some(delta)
    }
}

fn validate_unique<K: Eq + std::hash::Hash>(
    keys: impl ExactSizeIterator<Item = K>,
    description: &str,
) -> Result<(), String> {
    if keys.len() < 2 {
        return Ok(());
    }
    let mut seen = std::collections::HashSet::with_capacity(keys.len());
    for key in keys {
        if !seen.insert(key) {
            return Err(format!("duplicate {description}"));
        }
    }
    Ok(())
}

fn validate_order(
    order: &[String],
    expected_len: usize,
    contains: impl Fn(&str) -> bool,
) -> Result<(), String> {
    if order.len() != expected_len || order.iter().any(|key| !contains(key)) {
        return Err("order is not a permutation of the resulting collection".into());
    }
    validate_unique(order.iter().map(String::as_str), "order ID")
}

fn reorder_entities<T>(target: &mut [T], order: &[String], id: impl Fn(&T) -> &str) {
    for (destination, key) in order.iter().enumerate() {
        if let Some(offset) = target[destination..]
            .iter()
            .position(|item| id(item) == key)
        {
            target.swap(destination, destination + offset);
        }
    }
}

/// Validate a newly received raw metadata seed, before lossy UI projection.
pub fn validate_snapshot_seed(snapshot: &ClientShellSnapshot) -> Result<(), String> {
    validate_unique(
        snapshot
            .workspaces
            .iter()
            .map(|item| item.workspace_id.as_str()),
        "workspace ID",
    )?;
    validate_unique(
        snapshot.tabs.iter().map(|item| item.tab_id.as_str()),
        "tab ID",
    )?;
    validate_unique(
        snapshot.panes.iter().map(|item| item.pane_id.as_str()),
        "pane ID",
    )?;
    validate_unique(
        snapshot.agents.iter().map(|item| item.pane_id.as_str()),
        "agent ID",
    )?;
    validate_unique(
        snapshot
            .commands
            .iter()
            .map(|item| item.command_id.as_str()),
        "command ID",
    )?;
    for workspace in &snapshot.workspaces {
        validate_unique(
            workspace.tokens.iter().map(|(key, _)| key.as_str()),
            "workspace token",
        )?;
    }
    for agent in &snapshot.agents {
        validate_unique(
            agent.tokens.iter().map(|(key, _)| key.as_str()),
            "agent token",
        )?;
    }
    Ok(())
}

/// Validate a newly received surface seed.
pub fn validate_surface_seed(surface: &PaneSurfaceFrame) -> Result<(), String> {
    validate_unique(
        surface.panes.iter().map(|pane| pane.pane_id.as_str()),
        "pane ID in surface seed",
    )?;
    let expected_cells = usize::from(surface.frame.width) * usize::from(surface.frame.height);
    if surface.frame.cells.len() != expected_cells {
        return Err(format!(
            "surface seed cells len mismatch: expected {expected_cells} ({}x{}) got {}",
            surface.frame.width,
            surface.frame.height,
            surface.frame.cells.len()
        ));
    }
    let max_hyperlink = surface.frame.hyperlinks.len();
    for cell in &surface.frame.cells {
        if let Some(idx) = cell.hyperlink {
            if idx as usize >= max_hyperlink {
                return Err(format!(
                    "surface seed cell hyperlink index {idx} >= table len {max_hyperlink}"
                ));
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(bound(deserialize = "T: Deserialize<'de>, D: Deserialize<'de>"))]
pub struct EntityCollectionDelta<T, D> {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub added: Vec<T>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub updated: Vec<D>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<Vec<String>>,
}

impl<T, D> Default for EntityCollectionDelta<T, D> {
    fn default() -> Self {
        Self {
            added: Vec::new(),
            updated: Vec::new(),
            removed: Vec::new(),
            order: None,
        }
    }
}

impl<T, D> EntityCollectionDelta<T, D> {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty()
            && self.updated.is_empty()
            && self.removed.is_empty()
            && self.order.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ClientShellWorkspaceDelta {
    pub workspace_id: String,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub active_tab_id: FieldUpdate<String>,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub new_workspace_cwd: FieldUpdate<String>,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub number: FieldUpdate<usize>,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub label: FieldUpdate<String>,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub custom_label: FieldUpdate<bool>,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub branch: FieldUpdate<Option<String>>,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub git_ahead_behind: FieldUpdate<Option<(usize, usize)>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<ClientShellTokenDelta>,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub worktree: FieldUpdate<Option<ClientShellWorktree>>,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub focused: FieldUpdate<bool>,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub agent_status: FieldUpdate<crate::api::schema::AgentStatus>,
}

impl ClientShellWorkspaceDelta {
    pub fn diff(base: &ClientShellWorkspace, next: &ClientShellWorkspace) -> Option<Self> {
        if base == next {
            return None;
        }
        let ClientShellWorkspace {
            workspace_id: _,
            active_tab_id: _,
            new_workspace_cwd: _,
            number: _,
            label: _,
            custom_label: _,
            branch: _,
            git_ahead_behind: _,
            tokens: _,
            worktree: _,
            focused: _,
            agent_status: _,
        } = next;
        let ClientShellWorkspace {
            workspace_id: _,
            active_tab_id: _,
            new_workspace_cwd: _,
            number: _,
            label: _,
            custom_label: _,
            branch: _,
            git_ahead_behind: _,
            tokens: _,
            worktree: _,
            focused: _,
            agent_status: _,
        } = base;
        Some(Self {
            workspace_id: next.workspace_id.clone(),
            active_tab_id: FieldUpdate::diff(&base.active_tab_id, &next.active_tab_id),
            new_workspace_cwd: FieldUpdate::diff(&base.new_workspace_cwd, &next.new_workspace_cwd),
            number: FieldUpdate::diff(&base.number, &next.number),
            label: FieldUpdate::diff(&base.label, &next.label),
            custom_label: FieldUpdate::diff(&base.custom_label, &next.custom_label),
            branch: FieldUpdate::diff(&base.branch, &next.branch),
            git_ahead_behind: FieldUpdate::diff(&base.git_ahead_behind, &next.git_ahead_behind),
            tokens: ClientShellTokenDelta::diff(&base.tokens, &next.tokens),
            worktree: FieldUpdate::diff(&base.worktree, &next.worktree),
            focused: FieldUpdate::diff(&base.focused, &next.focused),
            agent_status: FieldUpdate::diff(&base.agent_status, &next.agent_status),
        })
    }

    fn apply_to(self, target: &mut ClientShellWorkspace) {
        self.active_tab_id.apply_to(&mut target.active_tab_id);
        self.new_workspace_cwd
            .apply_to(&mut target.new_workspace_cwd);
        self.number.apply_to(&mut target.number);
        self.label.apply_to(&mut target.label);
        self.custom_label.apply_to(&mut target.custom_label);
        self.branch.apply_to(&mut target.branch);
        self.git_ahead_behind.apply_to(&mut target.git_ahead_behind);
        if let Some(tokens_delta) = self.tokens {
            tokens_delta.apply_validated(&mut target.tokens);
        }
        self.worktree.apply_to(&mut target.worktree);
        self.focused.apply_to(&mut target.focused);
        self.agent_status.apply_to(&mut target.agent_status);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ClientShellTabDelta {
    pub tab_id: String,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub workspace_id: FieldUpdate<String>,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub number: FieldUpdate<usize>,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub label: FieldUpdate<String>,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub custom_label: FieldUpdate<bool>,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub zoomed: FieldUpdate<bool>,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub focused: FieldUpdate<bool>,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub agent_status: FieldUpdate<crate::api::schema::AgentStatus>,
}

impl ClientShellTabDelta {
    pub fn diff(base: &ClientShellTab, next: &ClientShellTab) -> Option<Self> {
        if base == next {
            return None;
        }
        let ClientShellTab {
            tab_id: _,
            workspace_id: _,
            number: _,
            label: _,
            custom_label: _,
            zoomed: _,
            focused: _,
            agent_status: _,
        } = next;
        let ClientShellTab {
            tab_id: _,
            workspace_id: _,
            number: _,
            label: _,
            custom_label: _,
            zoomed: _,
            focused: _,
            agent_status: _,
        } = base;
        Some(Self {
            tab_id: next.tab_id.clone(),
            workspace_id: FieldUpdate::diff(&base.workspace_id, &next.workspace_id),
            number: FieldUpdate::diff(&base.number, &next.number),
            label: FieldUpdate::diff(&base.label, &next.label),
            custom_label: FieldUpdate::diff(&base.custom_label, &next.custom_label),
            zoomed: FieldUpdate::diff(&base.zoomed, &next.zoomed),
            focused: FieldUpdate::diff(&base.focused, &next.focused),
            agent_status: FieldUpdate::diff(&base.agent_status, &next.agent_status),
        })
    }

    fn apply_to(self, target: &mut ClientShellTab) {
        self.workspace_id.apply_to(&mut target.workspace_id);
        self.number.apply_to(&mut target.number);
        self.label.apply_to(&mut target.label);
        self.custom_label.apply_to(&mut target.custom_label);
        self.zoomed.apply_to(&mut target.zoomed);
        self.focused.apply_to(&mut target.focused);
        self.agent_status.apply_to(&mut target.agent_status);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ClientShellPaneDelta {
    pub pane_id: String,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub workspace_id: FieldUpdate<String>,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub tab_id: FieldUpdate<String>,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub label: FieldUpdate<Option<String>>,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub cwd: FieldUpdate<Option<String>>,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub foreground_cwd: FieldUpdate<Option<String>>,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub focused: FieldUpdate<bool>,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub right_click_passthrough: FieldUpdate<bool>,
}

impl ClientShellPaneDelta {
    pub fn diff(base: &ClientShellPane, next: &ClientShellPane) -> Option<Self> {
        if base == next {
            return None;
        }
        let ClientShellPane {
            pane_id: _,
            workspace_id: _,
            tab_id: _,
            label: _,
            cwd: _,
            foreground_cwd: _,
            focused: _,
            right_click_passthrough: _,
        } = next;
        let ClientShellPane {
            pane_id: _,
            workspace_id: _,
            tab_id: _,
            label: _,
            cwd: _,
            foreground_cwd: _,
            focused: _,
            right_click_passthrough: _,
        } = base;
        Some(Self {
            pane_id: next.pane_id.clone(),
            workspace_id: FieldUpdate::diff(&base.workspace_id, &next.workspace_id),
            tab_id: FieldUpdate::diff(&base.tab_id, &next.tab_id),
            label: FieldUpdate::diff(&base.label, &next.label),
            cwd: FieldUpdate::diff(&base.cwd, &next.cwd),
            foreground_cwd: FieldUpdate::diff(&base.foreground_cwd, &next.foreground_cwd),
            focused: FieldUpdate::diff(&base.focused, &next.focused),
            right_click_passthrough: FieldUpdate::diff(
                &base.right_click_passthrough,
                &next.right_click_passthrough,
            ),
        })
    }

    fn apply_to(self, target: &mut ClientShellPane) {
        self.workspace_id.apply_to(&mut target.workspace_id);
        self.tab_id.apply_to(&mut target.tab_id);
        self.label.apply_to(&mut target.label);
        self.cwd.apply_to(&mut target.cwd);
        self.foreground_cwd.apply_to(&mut target.foreground_cwd);
        self.focused.apply_to(&mut target.focused);
        self.right_click_passthrough
            .apply_to(&mut target.right_click_passthrough);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ClientShellAgentDelta {
    pub pane_id: String,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub workspace_id: FieldUpdate<String>,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub tab_id: FieldUpdate<String>,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub name: FieldUpdate<Option<String>>,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub display_agent: FieldUpdate<Option<String>>,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub agent: FieldUpdate<Option<String>>,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub title: FieldUpdate<Option<String>>,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub terminal_title: FieldUpdate<Option<String>>,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub terminal_title_stripped: FieldUpdate<Option<String>>,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub agent_status: FieldUpdate<crate::api::schema::AgentStatus>,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub state_change_seq: FieldUpdate<u64>,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub state_labels: FieldUpdate<Vec<(String, String)>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<ClientShellTokenDelta>,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub focused: FieldUpdate<bool>,
}

impl ClientShellAgentDelta {
    pub fn diff(base: &ClientShellAgent, next: &ClientShellAgent) -> Option<Self> {
        if base == next {
            return None;
        }
        let ClientShellAgent {
            pane_id: _,
            workspace_id: _,
            tab_id: _,
            name: _,
            display_agent: _,
            agent: _,
            title: _,
            terminal_title: _,
            terminal_title_stripped: _,
            agent_status: _,
            state_change_seq: _,
            state_labels: _,
            tokens: _,
            focused: _,
        } = next;
        let ClientShellAgent {
            pane_id: _,
            workspace_id: _,
            tab_id: _,
            name: _,
            display_agent: _,
            agent: _,
            title: _,
            terminal_title: _,
            terminal_title_stripped: _,
            agent_status: _,
            state_change_seq: _,
            state_labels: _,
            tokens: _,
            focused: _,
        } = base;
        Some(Self {
            pane_id: next.pane_id.clone(),
            workspace_id: FieldUpdate::diff(&base.workspace_id, &next.workspace_id),
            tab_id: FieldUpdate::diff(&base.tab_id, &next.tab_id),
            name: FieldUpdate::diff(&base.name, &next.name),
            display_agent: FieldUpdate::diff(&base.display_agent, &next.display_agent),
            agent: FieldUpdate::diff(&base.agent, &next.agent),
            title: FieldUpdate::diff(&base.title, &next.title),
            terminal_title: FieldUpdate::diff(&base.terminal_title, &next.terminal_title),
            terminal_title_stripped: FieldUpdate::diff(
                &base.terminal_title_stripped,
                &next.terminal_title_stripped,
            ),
            agent_status: FieldUpdate::diff(&base.agent_status, &next.agent_status),
            state_change_seq: FieldUpdate::diff(&base.state_change_seq, &next.state_change_seq),
            state_labels: FieldUpdate::diff(&base.state_labels, &next.state_labels),
            tokens: ClientShellTokenDelta::diff(&base.tokens, &next.tokens),
            focused: FieldUpdate::diff(&base.focused, &next.focused),
        })
    }

    fn apply_to(self, target: &mut ClientShellAgent) {
        self.workspace_id.apply_to(&mut target.workspace_id);
        self.tab_id.apply_to(&mut target.tab_id);
        self.name.apply_to(&mut target.name);
        self.display_agent.apply_to(&mut target.display_agent);
        self.agent.apply_to(&mut target.agent);
        self.title.apply_to(&mut target.title);
        self.terminal_title.apply_to(&mut target.terminal_title);
        self.terminal_title_stripped
            .apply_to(&mut target.terminal_title_stripped);
        self.agent_status.apply_to(&mut target.agent_status);
        self.state_change_seq.apply_to(&mut target.state_change_seq);
        self.state_labels.apply_to(&mut target.state_labels);
        if let Some(tokens_delta) = self.tokens {
            tokens_delta.apply_validated(&mut target.tokens);
        }
        self.focused.apply_to(&mut target.focused);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ClientShellCommandDelta {
    pub command_id: String,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub binding_label: FieldUpdate<String>,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub binding_labels: FieldUpdate<Vec<String>>,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub action: FieldUpdate<ClientShellCommandAction>,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub description: FieldUpdate<Option<String>>,
}

impl ClientShellCommandDelta {
    pub fn diff(base: &ClientShellCommand, next: &ClientShellCommand) -> Option<Self> {
        if base == next {
            return None;
        }
        let ClientShellCommand {
            command_id: _,
            binding_label: _,
            binding_labels: _,
            action: _,
            description: _,
        } = next;
        let ClientShellCommand {
            command_id: _,
            binding_label: _,
            binding_labels: _,
            action: _,
            description: _,
        } = base;
        Some(Self {
            command_id: next.command_id.clone(),
            binding_label: FieldUpdate::diff(&base.binding_label, &next.binding_label),
            binding_labels: FieldUpdate::diff(&base.binding_labels, &next.binding_labels),
            action: FieldUpdate::diff(&base.action, &next.action),
            description: FieldUpdate::diff(&base.description, &next.description),
        })
    }

    fn apply_to(self, target: &mut ClientShellCommand) {
        self.binding_label.apply_to(&mut target.binding_label);
        self.binding_labels.apply_to(&mut target.binding_labels);
        self.action.apply_to(&mut target.action);
        self.description.apply_to(&mut target.description);
    }
}

fn diff_entities<T: Clone + PartialEq, D>(
    base: &[T],
    next: &[T],
    id_fn: impl Fn(&T) -> &str,
    diff_fn: impl Fn(&T, &T) -> Option<D>,
) -> EntityCollectionDelta<T, D> {
    let mut added = Vec::new();
    let mut updated = Vec::new();
    let mut removed = Vec::new();

    for b in base {
        let b_id = id_fn(b);
        if !next.iter().any(|n| id_fn(n) == b_id) {
            removed.push(b_id.to_owned());
        }
    }

    for n in next {
        let n_id = id_fn(n);
        if let Some(b) = base.iter().find(|b| id_fn(b) == n_id) {
            if let Some(d) = diff_fn(b, n) {
                updated.push(d);
            }
        } else {
            added.push(n.clone());
        }
    }

    let natural_order_matches = base
        .iter()
        .filter(|item| !removed.iter().any(|key| key == id_fn(item)))
        .chain(added.iter())
        .map(&id_fn)
        .eq(next.iter().map(&id_fn));
    let order =
        (!natural_order_matches).then(|| next.iter().map(|item| id_fn(item).to_owned()).collect());

    EntityCollectionDelta {
        added,
        updated,
        removed,
        order,
    }
}

fn validate_entity_delta<T, D>(
    target: &[T],
    delta: &EntityCollectionDelta<T, D>,
    id: impl Fn(&T) -> &str,
    updated_id: impl Fn(&D) -> &str,
) -> Result<(), String> {
    if delta.is_empty() {
        return Ok(());
    }
    validate_unique(delta.added.iter().map(&id), "added entity ID")?;
    validate_unique(delta.updated.iter().map(&updated_id), "updated entity ID")?;
    validate_unique(
        delta.removed.iter().map(String::as_str),
        "removed entity ID",
    )?;
    for added in &delta.added {
        if target.iter().any(|item| id(item) == id(added)) {
            return Err("entity addition reuses an existing ID".into());
        }
    }
    for removed in &delta.removed {
        if !target.iter().any(|item| id(item) == removed) {
            return Err("entity removal refers to an unknown ID".into());
        }
    }
    for updated in &delta.updated {
        let key = updated_id(updated);
        if !target.iter().any(|item| id(item) == key)
            || delta.removed.iter().any(|removed| removed == key)
        {
            return Err("entity update refers to an unknown or removed ID".into());
        }
    }
    if let Some(order) = &delta.order {
        validate_order(
            order,
            target.len() - delta.removed.len() + delta.added.len(),
            |key| {
                delta.added.iter().any(|item| id(item) == key)
                    || (target.iter().any(|item| id(item) == key)
                        && !delta.removed.iter().any(|removed| removed == key))
            },
        )?;
    }
    Ok(())
}

fn apply_entity_delta<T, D>(
    target: &mut Vec<T>,
    delta: EntityCollectionDelta<T, D>,
    id_fn: impl Fn(&T) -> &str,
    delta_id_fn: impl Fn(&D) -> &str,
    update_fn: impl Fn(&mut T, D),
) {
    if !delta.removed.is_empty() {
        target.retain(|item| !delta.removed.iter().any(|r| r == id_fn(item)));
    }
    for d in delta.updated {
        let d_id = delta_id_fn(&d);
        if let Some(item) = target.iter_mut().find(|item| id_fn(item) == d_id) {
            update_fn(item, d);
        }
    }
    target.extend(delta.added);
    if let Some(order) = delta.order {
        reorder_entities(target, &order, id_fn);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ClientShellSnapshotDelta {
    pub boot_id: String,
    pub base_revision: u64,
    pub revision: u64,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub config_diagnostic: FieldUpdate<Option<String>>,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub product_announcement: FieldUpdate<Option<ClientShellProductAnnouncement>>,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub update_available: FieldUpdate<Option<String>>,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub update_install_command: FieldUpdate<String>,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub server_keybindings_toml: FieldUpdate<Option<String>>,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub latest_release_notes_available: FieldUpdate<bool>,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub integration_updates_available: FieldUpdate<bool>,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub worktree_directory: FieldUpdate<String>,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub release_notes: FieldUpdate<Option<ClientShellReleaseNotes>>,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub focused_workspace_id: FieldUpdate<Option<String>>,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub focused_tab_id: FieldUpdate<Option<String>>,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub focused_pane_id: FieldUpdate<Option<String>>,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub tab_bar_right: FieldUpdate<Vec<ClientShellTabStatusSegment>>,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub tab_bar_right_separator: FieldUpdate<String>,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub agent_view_label: FieldUpdate<Option<String>>,
    #[serde(default, skip_serializing_if = "FieldUpdate::is_unchanged")]
    pub agent_order: FieldUpdate<Vec<String>>,
    #[serde(default, skip_serializing_if = "EntityCollectionDelta::is_empty")]
    pub workspaces: EntityCollectionDelta<ClientShellWorkspace, ClientShellWorkspaceDelta>,
    #[serde(default, skip_serializing_if = "EntityCollectionDelta::is_empty")]
    pub tabs: EntityCollectionDelta<ClientShellTab, ClientShellTabDelta>,
    #[serde(default, skip_serializing_if = "EntityCollectionDelta::is_empty")]
    pub panes: EntityCollectionDelta<ClientShellPane, ClientShellPaneDelta>,
    #[serde(default, skip_serializing_if = "EntityCollectionDelta::is_empty")]
    pub agents: EntityCollectionDelta<ClientShellAgent, ClientShellAgentDelta>,
    #[serde(default, skip_serializing_if = "EntityCollectionDelta::is_empty")]
    pub commands: EntityCollectionDelta<ClientShellCommand, ClientShellCommandDelta>,
}

impl ClientShellSnapshotDelta {
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn is_noop(&self) -> bool {
        self.config_diagnostic.is_unchanged()
            && self.product_announcement.is_unchanged()
            && self.update_available.is_unchanged()
            && self.update_install_command.is_unchanged()
            && self.server_keybindings_toml.is_unchanged()
            && self.latest_release_notes_available.is_unchanged()
            && self.integration_updates_available.is_unchanged()
            && self.worktree_directory.is_unchanged()
            && self.release_notes.is_unchanged()
            && self.focused_workspace_id.is_unchanged()
            && self.focused_tab_id.is_unchanged()
            && self.focused_pane_id.is_unchanged()
            && self.tab_bar_right.is_unchanged()
            && self.tab_bar_right_separator.is_unchanged()
            && self.agent_view_label.is_unchanged()
            && self.agent_order.is_unchanged()
            && self.workspaces.is_empty()
            && self.tabs.is_empty()
            && self.panes.is_empty()
            && self.agents.is_empty()
            && self.commands.is_empty()
    }

    pub fn diff(base: &ClientShellSnapshot, next: &ClientShellSnapshot) -> Option<Self> {
        if base.boot_id != next.boot_id || base == next || next.revision <= base.revision {
            return None;
        }

        let ClientShellSnapshot {
            boot_id: _,
            revision: _,
            config_diagnostic: _,
            product_announcement: _,
            update_available: _,
            update_install_command: _,
            server_keybindings_toml: _,
            latest_release_notes_available: _,
            integration_updates_available: _,
            worktree_directory: _,
            release_notes: _,
            focused_workspace_id: _,
            focused_tab_id: _,
            focused_pane_id: _,
            tab_bar_right: _,
            tab_bar_right_separator: _,
            agent_view_label: _,
            agent_order: _,
            workspaces: _,
            tabs: _,
            panes: _,
            agents: _,
            commands: _,
        } = next;

        let ClientShellSnapshot {
            boot_id: _,
            revision: _,
            config_diagnostic: _,
            product_announcement: _,
            update_available: _,
            update_install_command: _,
            server_keybindings_toml: _,
            latest_release_notes_available: _,
            integration_updates_available: _,
            worktree_directory: _,
            release_notes: _,
            focused_workspace_id: _,
            focused_tab_id: _,
            focused_pane_id: _,
            tab_bar_right: _,
            tab_bar_right_separator: _,
            agent_view_label: _,
            agent_order: _,
            workspaces: _,
            tabs: _,
            panes: _,
            agents: _,
            commands: _,
        } = base;

        let workspaces = diff_entities(
            &base.workspaces,
            &next.workspaces,
            |w| &w.workspace_id,
            ClientShellWorkspaceDelta::diff,
        );
        let tabs = diff_entities(
            &base.tabs,
            &next.tabs,
            |t| &t.tab_id,
            ClientShellTabDelta::diff,
        );
        let panes = diff_entities(
            &base.panes,
            &next.panes,
            |p| &p.pane_id,
            ClientShellPaneDelta::diff,
        );
        let agents = diff_entities(
            &base.agents,
            &next.agents,
            |a| &a.pane_id,
            ClientShellAgentDelta::diff,
        );
        let commands = diff_entities(
            &base.commands,
            &next.commands,
            |c| &c.command_id,
            ClientShellCommandDelta::diff,
        );

        Some(Self {
            boot_id: next.boot_id.clone(),
            base_revision: base.revision,
            revision: next.revision,
            config_diagnostic: FieldUpdate::diff(&base.config_diagnostic, &next.config_diagnostic),
            product_announcement: FieldUpdate::diff(
                &base.product_announcement,
                &next.product_announcement,
            ),
            update_available: FieldUpdate::diff(&base.update_available, &next.update_available),
            update_install_command: FieldUpdate::diff(
                &base.update_install_command,
                &next.update_install_command,
            ),
            server_keybindings_toml: FieldUpdate::diff(
                &base.server_keybindings_toml,
                &next.server_keybindings_toml,
            ),
            latest_release_notes_available: FieldUpdate::diff(
                &base.latest_release_notes_available,
                &next.latest_release_notes_available,
            ),
            integration_updates_available: FieldUpdate::diff(
                &base.integration_updates_available,
                &next.integration_updates_available,
            ),
            worktree_directory: FieldUpdate::diff(
                &base.worktree_directory,
                &next.worktree_directory,
            ),
            release_notes: FieldUpdate::diff(&base.release_notes, &next.release_notes),
            focused_workspace_id: FieldUpdate::diff(
                &base.focused_workspace_id,
                &next.focused_workspace_id,
            ),
            focused_tab_id: FieldUpdate::diff(&base.focused_tab_id, &next.focused_tab_id),
            focused_pane_id: FieldUpdate::diff(&base.focused_pane_id, &next.focused_pane_id),
            tab_bar_right: FieldUpdate::diff(&base.tab_bar_right, &next.tab_bar_right),
            tab_bar_right_separator: FieldUpdate::diff(
                &base.tab_bar_right_separator,
                &next.tab_bar_right_separator,
            ),
            agent_view_label: FieldUpdate::diff(&base.agent_view_label, &next.agent_view_label),
            agent_order: FieldUpdate::diff(&base.agent_order, &next.agent_order),
            workspaces,
            tabs,
            panes,
            agents,
            commands,
        })
    }

    /// The target is the validated raw seed/cache, never its UI projection.
    pub fn validate(&self, target: &ClientShellSnapshot) -> Result<(), String> {
        if target.boot_id != self.boot_id {
            return Err(format!(
                "mismatched snapshot boot_id: target {} != delta {}",
                target.boot_id, self.boot_id
            ));
        }
        if target.revision != self.base_revision || self.revision <= self.base_revision {
            return Err("snapshot delta does not continue the accepted metadata baseline".into());
        }
        validate_entity_delta(
            &target.workspaces,
            &self.workspaces,
            |item| &item.workspace_id,
            |delta| &delta.workspace_id,
        )?;
        validate_entity_delta(
            &target.tabs,
            &self.tabs,
            |item| &item.tab_id,
            |delta| &delta.tab_id,
        )?;
        validate_entity_delta(
            &target.panes,
            &self.panes,
            |item| &item.pane_id,
            |delta| &delta.pane_id,
        )?;
        validate_entity_delta(
            &target.agents,
            &self.agents,
            |item| &item.pane_id,
            |delta| &delta.pane_id,
        )?;
        validate_entity_delta(
            &target.commands,
            &self.commands,
            |item| &item.command_id,
            |delta| &delta.command_id,
        )?;
        for workspace in &self.workspaces.added {
            validate_unique(
                workspace.tokens.iter().map(|(key, _)| key.as_str()),
                "workspace token",
            )?;
        }
        for delta in &self.workspaces.updated {
            if let Some(tokens) = &delta.tokens {
                let workspace = target
                    .workspaces
                    .iter()
                    .find(|item| item.workspace_id == delta.workspace_id)
                    .ok_or_else(|| {
                        format!(
                            "workspace update references unknown id {}",
                            delta.workspace_id
                        )
                    })?;
                tokens.validate(&workspace.tokens)?;
            }
        }
        for agent in &self.agents.added {
            validate_unique(
                agent.tokens.iter().map(|(key, _)| key.as_str()),
                "agent token",
            )?;
        }
        for delta in &self.agents.updated {
            if let Some(tokens) = &delta.tokens {
                let agent = target
                    .agents
                    .iter()
                    .find(|item| item.pane_id == delta.pane_id)
                    .ok_or_else(|| {
                        format!("agent update references unknown pane id {}", delta.pane_id)
                    })?;
                tokens.validate(&agent.tokens)?;
            }
        }
        Ok(())
    }

    pub fn apply_to(&self, target: &mut ClientShellSnapshot) -> Result<(), String> {
        self.validate(target)?;
        self.config_diagnostic
            .clone()
            .apply_to(&mut target.config_diagnostic);
        self.product_announcement
            .clone()
            .apply_to(&mut target.product_announcement);
        self.update_available
            .clone()
            .apply_to(&mut target.update_available);
        self.update_install_command
            .clone()
            .apply_to(&mut target.update_install_command);
        self.server_keybindings_toml
            .clone()
            .apply_to(&mut target.server_keybindings_toml);
        self.latest_release_notes_available
            .apply_to(&mut target.latest_release_notes_available);
        self.integration_updates_available
            .apply_to(&mut target.integration_updates_available);
        self.worktree_directory
            .clone()
            .apply_to(&mut target.worktree_directory);
        self.release_notes
            .clone()
            .apply_to(&mut target.release_notes);
        self.focused_workspace_id
            .clone()
            .apply_to(&mut target.focused_workspace_id);
        self.focused_tab_id
            .clone()
            .apply_to(&mut target.focused_tab_id);
        self.focused_pane_id
            .clone()
            .apply_to(&mut target.focused_pane_id);
        self.tab_bar_right
            .clone()
            .apply_to(&mut target.tab_bar_right);
        self.tab_bar_right_separator
            .clone()
            .apply_to(&mut target.tab_bar_right_separator);
        self.agent_view_label
            .clone()
            .apply_to(&mut target.agent_view_label);
        self.agent_order.clone().apply_to(&mut target.agent_order);

        apply_entity_delta(
            &mut target.workspaces,
            self.workspaces.clone(),
            |w| &w.workspace_id,
            |w| &w.workspace_id,
            |w, d| d.apply_to(w),
        );
        apply_entity_delta(
            &mut target.tabs,
            self.tabs.clone(),
            |t| &t.tab_id,
            |t| &t.tab_id,
            |t, d| d.apply_to(t),
        );
        apply_entity_delta(
            &mut target.panes,
            self.panes.clone(),
            |p| &p.pane_id,
            |p| &p.pane_id,
            |p, d| d.apply_to(p),
        );
        apply_entity_delta(
            &mut target.agents,
            self.agents.clone(),
            |a| &a.pane_id,
            |a| &a.pane_id,
            |a, d| d.apply_to(a),
        );
        apply_entity_delta(
            &mut target.commands,
            self.commands.clone(),
            |c| &c.command_id,
            |c| &c.command_id,
            |c, d| d.apply_to(c),
        );

        target.revision = self.revision;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneRowMove {
    pub pane_id: String,
    pub src_y: u16,
    pub dst_y: u16,
    pub count: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SurfaceGraphicsPlacementKey {
    pub asset: SurfaceGraphicsAssetKey,
    pub logical_placement_id: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceGraphicsDelta {
    pub added_assets: Vec<SurfaceGraphicsAsset>,
    pub removed_assets: Vec<SurfaceGraphicsAssetKey>,
    pub added_placements: Vec<SurfaceGraphicsPlacement>,
    pub removed_placements: Vec<SurfaceGraphicsPlacementKey>,
    pub retained_assets: Vec<SurfaceGraphicsAssetKey>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSurfacePaneDelta {
    pub pane_id: String,
    pub content_revision: SurfaceFieldUpdate<u64>,
    pub scroll: SurfaceFieldUpdate<Option<PaneSurfaceScrollMetrics>>,
    pub focused: SurfaceFieldUpdate<bool>,
    pub mouse_reporting: SurfaceFieldUpdate<bool>,
    pub sgr_pixel_mouse: SurfaceFieldUpdate<bool>,
    pub alternate_screen_active: SurfaceFieldUpdate<bool>,
    pub scrollbar_rect: SurfaceFieldUpdate<Option<SurfaceRect>>,
    pub rect: SurfaceFieldUpdate<SurfaceRect>,
    pub inner_rect: SurfaceFieldUpdate<SurfaceRect>,
    pub pixel_width: SurfaceFieldUpdate<u32>,
    pub pixel_height: SurfaceFieldUpdate<u32>,
}

impl PaneSurfacePaneDelta {
    pub fn diff(base: &PaneSurfacePane, next: &PaneSurfacePane) -> Option<Self> {
        if base.wire_visible_eq(next) {
            return None;
        }
        if base == next {
            return None;
        }
        let PaneSurfacePane {
            pane_id: _,
            content_revision: _,
            scroll: _,
            focused: _,
            mouse_reporting: _,
            sgr_pixel_mouse: _,
            alternate_screen_active: _,
            scrollbar_rect: _,
            rect: _,
            inner_rect: _,
            pixel_width: _,
            pixel_height: _,
        } = next;
        let PaneSurfacePane {
            pane_id: _,
            content_revision: _,
            scroll: _,
            focused: _,
            mouse_reporting: _,
            sgr_pixel_mouse: _,
            alternate_screen_active: _,
            scrollbar_rect: _,
            rect: _,
            inner_rect: _,
            pixel_width: _,
            pixel_height: _,
        } = base;

        Some(Self {
            pane_id: next.pane_id.clone(),
            content_revision: SurfaceFieldUpdate::diff(
                &base.content_revision,
                &next.content_revision,
            ),
            scroll: SurfaceFieldUpdate::diff(&base.scroll, &next.scroll),
            focused: SurfaceFieldUpdate::diff(&base.focused, &next.focused),
            mouse_reporting: SurfaceFieldUpdate::diff(&base.mouse_reporting, &next.mouse_reporting),
            sgr_pixel_mouse: SurfaceFieldUpdate::diff(&base.sgr_pixel_mouse, &next.sgr_pixel_mouse),
            alternate_screen_active: SurfaceFieldUpdate::diff(
                &base.alternate_screen_active,
                &next.alternate_screen_active,
            ),
            scrollbar_rect: SurfaceFieldUpdate::diff(&base.scrollbar_rect, &next.scrollbar_rect),
            rect: SurfaceFieldUpdate::diff(&base.rect, &next.rect),
            inner_rect: SurfaceFieldUpdate::diff(&base.inner_rect, &next.inner_rect),
            pixel_width: SurfaceFieldUpdate::diff(&base.pixel_width, &next.pixel_width),
            pixel_height: SurfaceFieldUpdate::diff(&base.pixel_height, &next.pixel_height),
        })
    }

    pub fn apply_to(self, target: &mut PaneSurfacePane) {
        self.content_revision.apply_to(&mut target.content_revision);
        self.scroll.apply_to(&mut target.scroll);
        self.focused.apply_to(&mut target.focused);
        self.mouse_reporting.apply_to(&mut target.mouse_reporting);
        self.sgr_pixel_mouse.apply_to(&mut target.sgr_pixel_mouse);
        self.alternate_screen_active
            .apply_to(&mut target.alternate_screen_active);
        self.scrollbar_rect.apply_to(&mut target.scrollbar_rect);
        self.rect.apply_to(&mut target.rect);
        self.inner_rect.apply_to(&mut target.inner_rect);
        self.pixel_width.apply_to(&mut target.pixel_width);
        self.pixel_height.apply_to(&mut target.pixel_height);
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientShellPopupDelta {
    pub seed: Option<Box<ClientShellPopupSurface>>,
    pub clear: bool,
    pub spans: Vec<PaneSurfacePatchRow>,
    pub cursor: SurfaceFieldUpdate<Option<CursorState>>,
    pub appended_hyperlinks: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientShellSurfaceDelta {
    pub boot_id: String,
    pub projection_revision: u64,
    pub base_surface_revision: u64,
    pub surface_revision: u64,
    pub spans: Vec<PaneSurfacePatchRow>,
    pub row_moves: Vec<PaneRowMove>,
    pub panes: Vec<PaneSurfacePaneDelta>,
    pub splits: Option<Vec<PaneSurfaceSplit>>,
    pub cursor: SurfaceFieldUpdate<Option<CursorState>>,
    pub appended_hyperlinks: Vec<String>,
    pub graphics: Option<SurfaceGraphicsDelta>,
    pub popup: Option<ClientShellPopupDelta>,
}

pub(crate) fn pane_geometry_matches_legacy_patch(
    left: &PaneSurfacePane,
    right: &PaneSurfacePane,
) -> bool {
    left.pane_id == right.pane_id
        && left.rect == right.rect
        && left.inner_rect == right.inner_rect
        && left.focused == right.focused
        && left.pixel_width == right.pixel_width
        && left.pixel_height == right.pixel_height
}

fn span_hits_pane_legacy_patch(span: &PaneSurfacePatchRow, pane: &PaneSurfacePane) -> bool {
    let Ok(len) = u16::try_from(span.cells.len()) else {
        return false;
    };
    let terminal_row = span.x >= pane.inner_rect.x
        && span.y >= pane.inner_rect.y
        && span.y < pane.inner_rect.y.saturating_add(pane.inner_rect.height)
        && span.x.saturating_add(len) <= pane.inner_rect.x.saturating_add(pane.inner_rect.width);
    let scrollbar_row = pane.scrollbar_rect.is_some_and(|rect| {
        span.x == rect.x
            && span.y >= rect.y
            && span.y < rect.y.saturating_add(rect.height)
            && span.cells.len() == usize::from(rect.width)
    });
    terminal_row || scrollbar_row
}

impl ClientShellSurfaceDelta {
    // Returning Self on conversion failure allows the caller to retain the prepared delta without extra heap boxing.
    #[allow(clippy::result_large_err)]
    pub fn into_legacy_patch(self, last: &PaneSurfaceFrame) -> Result<PaneSurfacePatch, Self> {
        if last.boot_id != self.boot_id
            || last.surface_revision != self.base_surface_revision
            || last.projection_revision != self.projection_revision
            || last.popup.is_some()
            || !last.graphics.placements.is_empty()
            || !last.graphics.retained_assets.is_empty()
            || !last.graphics.assets.is_empty()
            || !self.row_moves.is_empty()
            || self.splits.is_some()
            || !self.appended_hyperlinks.is_empty()
            || self.graphics.is_some()
            || self.popup.is_some()
        {
            return Err(self);
        }

        let mut panes = Vec::new();
        for pane_delta in &self.panes {
            let Some(base) = last
                .panes
                .iter()
                .find(|pane| pane.pane_id == pane_delta.pane_id)
            else {
                return Err(self);
            };
            let mut updated = base.clone();
            pane_delta.clone().apply_to(&mut updated);
            if !pane_geometry_matches_legacy_patch(base, &updated) {
                return Err(self);
            }
            panes.push(updated);
        }
        for span in &self.spans {
            if !last
                .panes
                .iter()
                .any(|pane| span_hits_pane_legacy_patch(span, pane))
            {
                return Err(self);
            }
        }

        let cursor = match &self.cursor {
            SurfaceFieldUpdate::Unchanged => last.frame.cursor.clone(),
            SurfaceFieldUpdate::Set(cursor) => cursor.clone(),
        };
        Ok(PaneSurfacePatch {
            boot_id: self.boot_id,
            projection_revision: self.projection_revision,
            base_surface_revision: self.base_surface_revision,
            surface_revision: self.surface_revision,
            rows: self.spans,
            panes,
            cursor,
        })
    }

    pub fn validate(
        &self,
        surface: &PaneSurfaceFrame,
        graphics_scene: &SurfaceGraphicsScene,
    ) -> Result<(), String> {
        if surface.boot_id != self.boot_id {
            return Err(format!(
                "surface delta boot_id mismatch: surface {} != delta {}",
                surface.boot_id, self.boot_id
            ));
        }
        if surface.surface_revision != self.base_surface_revision {
            return Err(format!(
                "surface delta base revision mismatch: surface {} != delta base {}",
                surface.surface_revision, self.base_surface_revision
            ));
        }
        if self.surface_revision <= self.base_surface_revision {
            return Err("surface delta revision does not advance".into());
        }
        if self.projection_revision < surface.projection_revision {
            return Err("surface delta projection revision is non-monotonic".into());
        }

        // Pre-validate all row moves against pane boundaries BEFORE mutating.
        for row_move in &self.row_moves {
            if row_move.count == 0 {
                return Err("row move count is zero".into());
            }
            let Some(pane) = surface.panes.iter().find(|p| p.pane_id == row_move.pane_id) else {
                return Err(format!("row move unknown pane: {}", row_move.pane_id));
            };
            let pane_top = pane.inner_rect.y;
            let pane_bottom = pane.inner_rect.y.saturating_add(pane.inner_rect.height);
            let pane_right = pane.inner_rect.x.saturating_add(pane.inner_rect.width);
            if pane_right > surface.frame.width || pane_bottom > surface.frame.height {
                return Err(format!(
                    "pane {} rect outside frame dimensions ({}x{})",
                    pane.pane_id, surface.frame.width, surface.frame.height
                ));
            }
            if row_move.src_y < pane_top
                || row_move.src_y.saturating_add(row_move.count) > pane_bottom
                || row_move.dst_y < pane_top
                || row_move.dst_y.saturating_add(row_move.count) > pane_bottom
            {
                return Err(format!(
                    "row move out of pane bounds: src {} dst {} count {} pane [{}..{}]",
                    row_move.src_y, row_move.dst_y, row_move.count, pane_top, pane_bottom
                ));
            }
        }

        // Pre-validate all spans against frame dimensions
        for span in &self.spans {
            if span.cells.is_empty() {
                return Err("span has empty cells".into());
            }
            let Ok(len) = u16::try_from(span.cells.len()) else {
                return Err("span cell length overflow".into());
            };
            if span.y >= surface.frame.height || span.x.saturating_add(len) > surface.frame.width {
                return Err(format!(
                    "span out of frame bounds: at ({}, {}) len {} frame ({}x{})",
                    span.x, span.y, len, surface.frame.width, surface.frame.height
                ));
            }
        }

        // Pre-validate pane deltas reference valid panes
        for pane_delta in &self.panes {
            if !surface
                .panes
                .iter()
                .any(|p| p.pane_id == pane_delta.pane_id)
            {
                return Err(format!(
                    "pane delta refers to unknown pane ID: {}",
                    pane_delta.pane_id
                ));
            }
        }

        // Hyperlink validation and growth budget
        let total_bytes: usize = self
            .appended_hyperlinks
            .iter()
            .map(|s| s.len().saturating_add(std::mem::size_of::<String>()))
            .sum();
        if total_bytes > MAX_FRAME_SIZE {
            return Err(format!(
                "hyperlink table growth budget exceeded: {total_bytes} > {MAX_FRAME_SIZE}"
            ));
        }
        let new_table_len = surface
            .frame
            .hyperlinks
            .len()
            .saturating_add(self.appended_hyperlinks.len());
        for row in &self.spans {
            for cell in &row.cells {
                if let Some(idx) = cell.hyperlink {
                    if idx as usize >= new_table_len {
                        return Err(format!(
                            "invalid hyperlink index {idx} in span (table len {new_table_len})"
                        ));
                    }
                }
            }
        }

        // Popup validation
        if let Some(popup_delta) = &self.popup {
            if !popup_delta.clear && popup_delta.seed.is_none() {
                let Some(popup) = &surface.popup else {
                    return Err("popup delta spans provided for missing popup surface".into());
                };
                let popup_total_bytes: usize = popup_delta
                    .appended_hyperlinks
                    .iter()
                    .map(|s| s.len().saturating_add(std::mem::size_of::<String>()))
                    .sum();
                if popup_total_bytes > MAX_FRAME_SIZE {
                    return Err(format!(
                        "popup hyperlink table growth budget exceeded: {popup_total_bytes} > {MAX_FRAME_SIZE}"
                    ));
                }
                let popup_table_len = popup
                    .frame
                    .hyperlinks
                    .len()
                    .saturating_add(popup_delta.appended_hyperlinks.len());
                for span in &popup_delta.spans {
                    let Ok(len) = u16::try_from(span.cells.len()) else {
                        return Err("popup span cell length overflow".into());
                    };
                    if span.y >= popup.frame.height
                        || span.x.saturating_add(len) > popup.frame.width
                    {
                        return Err(format!(
                            "popup span out of popup bounds: at ({}, {}) len {} popup ({}x{})",
                            span.x, span.y, len, popup.frame.width, popup.frame.height
                        ));
                    }
                    for cell in &span.cells {
                        if let Some(idx) = cell.hyperlink {
                            if idx as usize >= popup_table_len {
                                return Err(format!(
                                    "invalid hyperlink index {idx} in popup span (table len {popup_table_len})"
                                ));
                            }
                        }
                    }
                }
            }
        }

        // Graphics validation
        if let Some(gfx_delta) = &self.graphics {
            for add in &gfx_delta.added_assets {
                if add.data.len() as u64 != add.key.data_len {
                    return Err(format!(
                        "graphics asset data length mismatch: key data_len {} != data len {}",
                        add.key.data_len,
                        add.data.len()
                    ));
                }
            }
            for rem in &gfx_delta.removed_assets {
                if !graphics_scene.assets.iter().any(|a| &a.key == rem)
                    && !graphics_scene.retained_assets.iter().any(|k| k == rem)
                {
                    return Err("graphics delta removes unknown asset key".into());
                }
            }
            for rem in &gfx_delta.removed_placements {
                if !graphics_scene.placements.iter().any(|p| {
                    p.asset == rem.asset && p.logical_placement_id == rem.logical_placement_id
                }) {
                    return Err("graphics delta removes unknown placement key".into());
                }
            }
        }

        Ok(())
    }

    pub fn apply_to(
        &self,
        surface: &mut PaneSurfaceFrame,
        graphics_scene: &mut SurfaceGraphicsScene,
    ) -> Result<(), String> {
        self.validate(surface, graphics_scene)?;

        if !self.appended_hyperlinks.is_empty() {
            surface
                .frame
                .hyperlinks
                .extend(self.appended_hyperlinks.iter().cloned());
        }

        surface.projection_revision = self.projection_revision;
        surface.surface_revision = self.surface_revision;

        // Apply row moves in-place with directional copy on surface.frame.cells
        let frame_width = usize::from(surface.frame.width);
        for row_move in &self.row_moves {
            if let Some(pane) = surface.panes.iter().find(|p| p.pane_id == row_move.pane_id) {
                let pane_left = usize::from(pane.inner_rect.x);
                let pane_width = usize::from(pane.inner_rect.width);
                let count = usize::from(row_move.count);
                let src_y = usize::from(row_move.src_y);
                let dst_y = usize::from(row_move.dst_y);

                if dst_y < src_y {
                    // Move up: forward copy
                    for i in 0..count {
                        let s = src_y + i;
                        let d = dst_y + i;
                        let s_start = s * frame_width + pane_left;
                        let d_start = d * frame_width + pane_left;
                        if d_start < s_start && s_start + pane_width <= surface.frame.cells.len() {
                            let (first, rest) = surface.frame.cells.split_at_mut(s_start);
                            first[d_start..d_start + pane_width]
                                .clone_from_slice(&rest[..pane_width]);
                        }
                    }
                } else if dst_y > src_y {
                    // Move down: backward copy
                    for i in (0..count).rev() {
                        let s = src_y + i;
                        let d = dst_y + i;
                        let s_start = s * frame_width + pane_left;
                        let d_start = d * frame_width + pane_left;
                        if s_start < d_start && d_start + pane_width <= surface.frame.cells.len() {
                            let (first, rest) = surface.frame.cells.split_at_mut(d_start);
                            rest[..pane_width]
                                .clone_from_slice(&first[s_start..s_start + pane_width]);
                        }
                    }
                }
            }
        }

        // Apply spans to surface.frame.cells
        for span in &self.spans {
            let row_idx = usize::from(span.y);
            let start = row_idx * frame_width + usize::from(span.x);
            let end = start + span.cells.len();
            if end <= surface.frame.cells.len() {
                surface.frame.cells[start..end].clone_from_slice(&span.cells);
            }
        }

        // Apply pane deltas
        for pane_delta in &self.panes {
            if let Some(existing) = surface
                .panes
                .iter_mut()
                .find(|p| p.pane_id == pane_delta.pane_id)
            {
                pane_delta.clone().apply_to(existing);
            }
        }

        if let Some(splits) = &self.splits {
            surface.splits = splits.clone();
        }

        self.cursor.apply_to(&mut surface.frame.cursor);

        // Apply popup delta
        if let Some(popup_delta) = &self.popup {
            if popup_delta.clear {
                surface.popup = None;
            } else if let Some(seed) = &popup_delta.seed {
                surface.popup = Some(seed.clone());
            } else if let Some(popup) = surface.popup.as_mut() {
                if !popup_delta.appended_hyperlinks.is_empty() {
                    popup
                        .frame
                        .hyperlinks
                        .extend(popup_delta.appended_hyperlinks.iter().cloned());
                }
                popup_delta.cursor.apply_to(&mut popup.frame.cursor);
                let popup_width = usize::from(popup.frame.width);
                for span in &popup_delta.spans {
                    let start = usize::from(span.y) * popup_width + usize::from(span.x);
                    let end = start + span.cells.len();
                    if end <= popup.frame.cells.len() {
                        popup.frame.cells[start..end].clone_from_slice(&span.cells);
                    }
                }
            }
        }

        // Apply graphics delta to graphics_scene
        if let Some(gfx_delta) = &self.graphics {
            for rem in &gfx_delta.removed_assets {
                graphics_scene.assets.retain(|a| &a.key != rem);
            }
            for add in &gfx_delta.added_assets {
                if !graphics_scene.assets.iter().any(|a| a.key == add.key) {
                    graphics_scene.assets.push(add.clone());
                }
            }
            for rem in &gfx_delta.removed_placements {
                graphics_scene.placements.retain(|p| {
                    !(p.asset == rem.asset && p.logical_placement_id == rem.logical_placement_id)
                });
            }
            for add in &gfx_delta.added_placements {
                graphics_scene.placements.push(add.clone());
            }
            graphics_scene.retained_assets = gfx_delta.retained_assets.clone();
        }

        Ok(())
    }
}

pub fn encode_snapshot_delta(delta: &ClientShellSnapshotDelta) -> serde_json::Result<String> {
    serde_json::to_string(delta)
}

pub fn decode_snapshot_delta(data: &str) -> serde_json::Result<ClientShellSnapshotDelta> {
    serde_json::from_str(data)
}

pub fn encode_surface_delta(delta: &ClientShellSurfaceDelta) -> Result<String, String> {
    use base64::Engine as _;
    let bytes = bincode::serde::encode_to_vec(delta, bincode::config::standard())
        .map_err(|err| format!("failed to encode surface delta: {err}"))?;
    if bytes.len() > MAX_FRAME_SIZE {
        return Err(format!(
            "surface delta bincode {} exceeds MAX_FRAME_SIZE {MAX_FRAME_SIZE}",
            bytes.len()
        ));
    }
    Ok(base64::engine::general_purpose::STANDARD_NO_PAD.encode(bytes))
}

pub fn decode_surface_delta(data: &str) -> Result<ClientShellSurfaceDelta, String> {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD_NO_PAD
        .decode(data)
        .or_else(|_| base64::engine::general_purpose::STANDARD.decode(data))
        .map_err(|err| format!("invalid surface delta base64: {err}"))?;
    let (delta, consumed_len): (ClientShellSurfaceDelta, usize) =
        bincode::serde::decode_from_slice(&bytes, bincode::config::standard())
            .map_err(|err| format!("failed to decode surface delta: {err}"))?;
    if consumed_len != bytes.len() {
        return Err(format!(
            "surface delta trailing unconsumed bytes: consumed {consumed_len} of {}",
            bytes.len()
        ));
    }
    Ok(delta)
}

#[cfg(test)]
mod metadata_tests {
    use super::*;

    fn snapshot() -> ClientShellSnapshot {
        serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/endpoint-snapshot-v1.json"
        )))
        .unwrap()
    }

    #[test]
    fn revision_only_snapshot_diff_is_noop() {
        let base = snapshot();
        let mut next = base.clone();
        next.revision += 1;
        let delta = ClientShellSnapshotDelta::diff(&base, &next).unwrap();
        assert!(delta.is_noop());
        next.agent_order.push("extra".into());
        let delta = ClientShellSnapshotDelta::diff(&base, &next).unwrap();
        assert!(!delta.is_noop());
    }

    #[test]
    fn metadata_delta_preserves_token_insertion_reorder_and_nullable_clears() {
        let mut base = snapshot();
        base.agents[0].tokens = vec![
            ("alpha".into(), "unchanged-a".into()),
            ("zeta".into(), "unchanged-z".into()),
        ];
        let mut next = base.clone();
        next.revision += 1;
        next.config_diagnostic = None;
        next.panes[0].label = None;
        next.workspaces[0]
            .tokens
            .insert(0, ("alpha".into(), "new value".into()));
        next.agents[0].tokens.reverse();
        let delta = ClientShellSnapshotDelta::diff(&base, &next).unwrap();
        let workspace_tokens = delta.workspaces.updated[0].tokens.as_ref().unwrap();
        assert_eq!(
            workspace_tokens.set,
            vec![("alpha".to_owned(), "new value".to_owned())]
        );
        assert_eq!(
            workspace_tokens.order.as_deref(),
            Some(["alpha".to_owned(), "model".to_owned()].as_slice())
        );
        let agent_tokens = delta.agents.updated[0].tokens.as_ref().unwrap();
        assert!(agent_tokens.set.is_empty());
        assert!(agent_tokens.remove.is_empty());
        assert_eq!(
            agent_tokens.order.as_deref(),
            Some(["zeta".to_owned(), "alpha".to_owned()].as_slice())
        );
        let decoded = decode_snapshot_delta(&encode_snapshot_delta(&delta).unwrap()).unwrap();
        decoded.apply_to(&mut base).unwrap();
        assert_eq!(base, next);
    }

    #[test]
    fn metadata_delta_rejects_wrong_base_nonadvancing_revision_and_replay() {
        let base = snapshot();
        let mut next = base.clone();
        next.revision += 1;
        next.config_diagnostic = None;
        let delta = ClientShellSnapshotDelta::diff(&base, &next).unwrap();
        for base_revision in [base.revision - 1, base.revision + 1] {
            let mut wrong = delta.clone();
            wrong.base_revision = base_revision;
            let mut received = base.clone();
            assert!(wrong.apply_to(&mut received).is_err());
            assert_eq!(received, base);
        }
        let mut nonadvancing = delta.clone();
        nonadvancing.revision = base.revision;
        let mut received = base.clone();
        assert!(nonadvancing.apply_to(&mut received).is_err());
        assert_eq!(received, base);
        delta.apply_to(&mut received).unwrap();
        assert!(delta.apply_to(&mut received).is_err());
        assert_eq!(received, next);
    }

    #[test]
    fn invalid_final_metadata_operations_leave_the_entire_raw_snapshot_unchanged() {
        let base = snapshot();
        let mut next = base.clone();
        next.revision += 1;
        next.config_diagnostic = None;
        next.workspaces[0].label = "renamed".into();
        let valid = ClientShellSnapshotDelta::diff(&base, &next).unwrap();
        let mut unknown = valid.clone();
        unknown.commands.updated.push(ClientShellCommandDelta {
            command_id: "missing-command".into(),
            description: FieldUpdate::Set(None),
            ..ClientShellCommandDelta::default()
        });
        let mut duplicate = valid.clone();
        duplicate.panes.added.push(base.panes[0].clone());
        let mut removal = valid.clone();
        removal.tabs.removed.push("missing-tab".into());
        let mut conflict = valid.clone();
        conflict
            .workspaces
            .removed
            .push(base.workspaces[0].workspace_id.clone());
        let mut order = valid.clone();
        order.commands.order = Some(Vec::new());
        let mut token_order = valid.clone();
        token_order.workspaces.updated[0].tokens = Some(ClientShellTokenDelta {
            order: Some(vec!["missing-token".into()]),
            ..ClientShellTokenDelta::default()
        });
        for invalid in [unknown, duplicate, removal, conflict, order, token_order] {
            let mut received = base.clone();
            assert!(invalid.apply_to(&mut received).is_err());
            assert_eq!(received, base);
        }
    }

    #[test]
    fn entity_order_changes_do_not_resend_existing_records() {
        let mut base = snapshot();
        let mut second = base.commands[0].clone();
        second.command_id = "second".into();
        base.commands.push(second);
        let mut next = base.clone();
        next.revision += 1;
        next.commands.reverse();
        let delta = ClientShellSnapshotDelta::diff(&base, &next).unwrap();
        assert!(delta.commands.added.is_empty());
        assert!(delta.commands.updated.is_empty());
        delta.apply_to(&mut base).unwrap();
        assert_eq!(base, next);
    }
}

#[cfg(test)]
mod surface_tests {
    use super::*;
    use crate::protocol::wire::{CellData, FrameData};

    fn make_test_surface(width: u16, height: u16) -> PaneSurfaceFrame {
        let cell = CellData {
            symbol: " ".into(),
            fg: 0,
            bg: 0,
            modifier: 0,
            skip: false,
            hyperlink: None,
        };
        let cells = vec![cell; (width as usize) * (height as usize)];
        PaneSurfaceFrame {
            boot_id: "test-boot".into(),
            projection_revision: 1,
            surface_revision: 10,
            frame: FrameData {
                cells,
                width,
                height,
                cursor: None,
                hyperlinks: vec!["https://initial.example.com".into()],
                graphics: Vec::new(),
            },
            panes: vec![PaneSurfacePane {
                pane_id: "pane-1".into(),
                content_revision: 1,
                scroll: None,
                focused: true,
                mouse_reporting: false,
                sgr_pixel_mouse: false,
                alternate_screen_active: false,
                scrollbar_rect: None,
                rect: SurfaceRect {
                    x: 0,
                    y: 0,
                    width,
                    height,
                },
                inner_rect: SurfaceRect {
                    x: 0,
                    y: 0,
                    width,
                    height,
                },
                pixel_width: 800,
                pixel_height: 600,
            }],
            splits: Vec::new(),
            popup: None,
            graphics: SurfaceGraphicsScene::default(),
        }
    }

    #[test]
    fn surface_delta_directional_row_moves_and_spans() {
        let mut surface = make_test_surface(10, 6);
        let mut scene = SurfaceGraphicsScene::default();

        // Fill row 1 with 'A's and row 2 with 'B's
        for x in 0..10 {
            surface.frame.cells[10 + x].symbol = "A".into();
            surface.frame.cells[20 + x].symbol = "B".into();
        }

        // Delta moves row 1..3 up to 0..2 (move up)
        let delta = ClientShellSurfaceDelta {
            boot_id: "test-boot".into(),
            projection_revision: 1,
            base_surface_revision: 10,
            surface_revision: 11,
            spans: vec![PaneSurfacePatchRow {
                x: 0,
                y: 5,
                cells: vec![CellData {
                    symbol: "Z".into(),
                    fg: 1,
                    bg: 0,
                    modifier: 0,
                    skip: false,
                    hyperlink: None,
                }],
            }],
            row_moves: vec![PaneRowMove {
                pane_id: "pane-1".into(),
                src_y: 1,
                dst_y: 0,
                count: 2,
            }],
            panes: Vec::new(),
            splits: None,
            cursor: SurfaceFieldUpdate::Unchanged,
            appended_hyperlinks: Vec::new(),
            graphics: None,
            popup: None,
        };

        delta.apply_to(&mut surface, &mut scene).unwrap();
        assert_eq!(surface.surface_revision, 11);
        assert_eq!(surface.frame.cells[0].symbol, "A");
        assert_eq!(surface.frame.cells[10].symbol, "B");
        assert_eq!(surface.frame.cells[50].symbol, "Z");
    }

    #[test]
    fn surface_delta_validates_and_rejects_out_of_bounds() {
        let surface = make_test_surface(10, 6);
        let scene = SurfaceGraphicsScene::default();

        let invalid_move = ClientShellSurfaceDelta {
            boot_id: "test-boot".into(),
            projection_revision: 1,
            base_surface_revision: 10,
            surface_revision: 11,
            spans: Vec::new(),
            row_moves: vec![PaneRowMove {
                pane_id: "pane-1".into(),
                src_y: 5,
                dst_y: 0,
                count: 3, // 5 + 3 > 6 height!
            }],
            panes: Vec::new(),
            splits: None,
            cursor: SurfaceFieldUpdate::Unchanged,
            appended_hyperlinks: Vec::new(),
            graphics: None,
            popup: None,
        };

        let mut received_surface = surface.clone();
        let mut received_scene = scene.clone();
        assert!(invalid_move
            .apply_to(&mut received_surface, &mut received_scene)
            .is_err());
        assert_eq!(received_surface, surface);
    }

    #[test]
    fn surface_delta_atomic_rejection_leaves_state_unchanged() {
        let surface = make_test_surface(10, 6);
        let scene = SurfaceGraphicsScene::default();

        let bad_delta = ClientShellSurfaceDelta {
            boot_id: "test-boot".into(),
            projection_revision: 1,
            base_surface_revision: 10,
            surface_revision: 11,
            spans: vec![PaneSurfacePatchRow {
                x: 5,
                y: 10, // out of bounds!
                cells: vec![CellData {
                    symbol: "X".into(),
                    fg: 0,
                    bg: 0,
                    modifier: 0,
                    skip: false,
                    hyperlink: None,
                }],
            }],
            row_moves: Vec::new(),
            panes: Vec::new(),
            splits: None,
            cursor: SurfaceFieldUpdate::Unchanged,
            appended_hyperlinks: Vec::new(),
            graphics: None,
            popup: None,
        };

        let mut received_surface = surface.clone();
        let mut received_scene = scene.clone();
        assert!(bad_delta
            .apply_to(&mut received_surface, &mut received_scene)
            .is_err());
        assert_eq!(received_surface, surface);
    }

    #[test]
    fn surface_delta_roundtrip_codec() {
        let delta = ClientShellSurfaceDelta {
            boot_id: "test-boot".into(),
            projection_revision: 2,
            base_surface_revision: 10,
            surface_revision: 11,
            spans: vec![PaneSurfacePatchRow {
                x: 1,
                y: 2,
                cells: vec![CellData {
                    symbol: "R".into(),
                    fg: 2,
                    bg: 3,
                    modifier: 0,
                    skip: false,
                    hyperlink: Some(0),
                }],
            }],
            row_moves: vec![PaneRowMove {
                pane_id: "pane-1".into(),
                src_y: 2,
                dst_y: 1,
                count: 3,
            }],
            panes: Vec::new(),
            splits: None,
            cursor: SurfaceFieldUpdate::Set(Some(CursorState {
                x: 4,
                y: 5,
                visible: true,
                shape: 1,
            })),
            appended_hyperlinks: vec!["https://test.com".into()],
            graphics: None,
            popup: None,
        };

        let encoded = encode_surface_delta(&delta).expect("encode surface delta");
        assert!(
            !encoded.ends_with('='),
            "encode must use unpadded base64, got {encoded}"
        );
        let decoded = decode_surface_delta(&encoded).expect("decode surface delta");
        assert_eq!(delta, decoded);
    }
    #[test]
    fn cell_only_surface_delta_converts_to_legacy_patch() {
        let surface = make_test_surface(10, 6);
        let delta = ClientShellSurfaceDelta {
            boot_id: "test-boot".into(),
            projection_revision: 1,
            base_surface_revision: 10,
            surface_revision: 11,
            spans: vec![PaneSurfacePatchRow {
                x: 0,
                y: 0,
                cells: vec![CellData {
                    symbol: "/".into(),
                    fg: 0,
                    bg: 0,
                    modifier: 0,
                    skip: false,
                    hyperlink: None,
                }],
            }],
            row_moves: Vec::new(),
            panes: Vec::new(),
            splits: None,
            cursor: SurfaceFieldUpdate::Unchanged,
            appended_hyperlinks: Vec::new(),
            graphics: None,
            popup: None,
        };
        let patch = delta.into_legacy_patch(&surface).expect("legacy patch");
        assert_eq!(patch.rows.len(), 1);
        assert_eq!(patch.rows[0].cells[0].symbol, "/");
        assert!(
            patch.panes.is_empty(),
            "cell-only ticks must not clone pane metadata"
        );
    }

    #[test]
    fn row_move_surface_delta_stays_on_v2() {
        let surface = make_test_surface(10, 6);
        let delta = ClientShellSurfaceDelta {
            boot_id: "test-boot".into(),
            projection_revision: 1,
            base_surface_revision: 10,
            surface_revision: 11,
            spans: Vec::new(),
            row_moves: vec![PaneRowMove {
                pane_id: "pane-1".into(),
                src_y: 1,
                dst_y: 0,
                count: 1,
            }],
            panes: Vec::new(),
            splits: None,
            cursor: SurfaceFieldUpdate::Unchanged,
            appended_hyperlinks: Vec::new(),
            graphics: None,
            popup: None,
        };
        assert!(delta.into_legacy_patch(&surface).is_err());
    }
}
