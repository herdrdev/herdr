//! Shepherd TUI Overlay (Phase F3b - adjusted per Owner Correction).
//!
//! Connects the Shepherd views (Tareas, Runs, Skills) into a cohesive
//! mouse-first and keyboard-driven modal overlay inside Shepherd's TUI.
//!
//! Owner correction:
//! - Agents do NOT have a separate tab or panel in this overlay; agents are integrated
//!   into Shepherd's existing agent sidebar and context menus.
//! - The Shepherd overlay contains only what Shepherd doesn't have: Tareas, Runs, and Skills.
//! - Forms and confirmations from `panel.rs` (SpawnForm, ConfirmKill, ConfirmClean)
//!   are reused as standalone dialogs/modals for agent actions.
//! - State lives in pure data structures (`ClientShepherdOverlay`).
//! - Rendering is pure (`render_shepherd_overlay`) and handles tiny screens gracefully without panicking.
//! - Keyboard navigation: Esc closes / cancels sub-modals, Tab/BackTab/Left/Right switches tabs.
//! - Mouse hit-testing supports clicking tabs, close button, rows, and form actions.
//! - Confirmation guards are strictly preserved: destructive actions (kill, clean, archive, cancel,
//!   approve gate, delete skill, restore skill) are NEVER dispatched without explicit human confirmation.

#![allow(dead_code)]

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use super::{
    git::{render_shepherd_git_panel, ShepherdGitAction, ShepherdGitRender, ShepherdGitState},
    panel::{contains_point, AgentsSubModal, ShepherdAgentsAction},
    skills::{
        render_shepherd_skills_panel, ShepherdSkillsAction, ShepherdSkillsRender,
        ShepherdSkillsState, SkillsSubModal,
    },
};
use crate::app::state::Palette;

pub const SHEPHERD_OVERLAY_MODAL_SIZE: (u16, u16) = (96, 28);

/// Tabs available in the temporary Shepherd overlay (Skills / Git).
/// Note (F10b): Tareas and Runs have been migrated to the in-place Shepherd section (Option D).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShepherdTab {
    Skills,
    Git,
}

impl ShepherdTab {
    pub const ALL: [ShepherdTab; 2] = [
        ShepherdTab::Skills,
        ShepherdTab::Git,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            ShepherdTab::Skills => "Skills",
            ShepherdTab::Git => "Git",
        }
    }

    pub fn index(&self) -> usize {
        match self {
            ShepherdTab::Skills => 0,
            ShepherdTab::Git => 1,
        }
    }

    pub fn from_index(index: usize) -> Self {
        match index % 2 {
            0 => ShepherdTab::Skills,
            _ => ShepherdTab::Git,
        }
    }
}

/// Pure presentation state of the temporary Shepherd overlay (Skills / Git).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientShepherdOverlay {
    pub active_tab: ShepherdTab,
    pub skills: ShepherdSkillsState,
    pub git: ShepherdGitState,
    pub status_message: Option<String>,
    pub loading: bool,
    pub last_dispatched: Option<String>,
}

impl Default for ClientShepherdOverlay {
    fn default() -> Self {
        Self {
            active_tab: ShepherdTab::Skills,
            skills: ShepherdSkillsState {
                open: true,
                ..ShepherdSkillsState::default()
            },
            git: ShepherdGitState::default(),
            status_message: None,
            loading: false,
            last_dispatched: None,
        }
    }
}

impl ClientShepherdOverlay {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn select_tab(&mut self, tab: ShepherdTab) {
        self.active_tab = tab;
        self.skills.open = tab == ShepherdTab::Skills;
        self.git.open = tab == ShepherdTab::Git;
    }

    pub fn next_tab(&mut self) {
        let next = (self.active_tab.index() + 1) % 2;
        self.select_tab(ShepherdTab::from_index(next));
    }

    pub fn prev_tab(&mut self) {
        let prev = (self.active_tab.index() + 1) % 2;
        self.select_tab(ShepherdTab::from_index(prev));
    }

    pub fn has_active_submodal(&self) -> bool {
        match self.active_tab {
            ShepherdTab::Skills => self.skills.sub_modal != SkillsSubModal::None,
            ShepherdTab::Git => false,
        }
    }

    pub fn cancel_active_submodal(&mut self) -> bool {
        match self.active_tab {
            ShepherdTab::Skills => {
                if self.skills.sub_modal != SkillsSubModal::None {
                    self.skills.sub_modal = SkillsSubModal::None;
                    true
                } else {
                    false
                }
            }
            ShepherdTab::Git => false,
        }
    }
}

/// Standalone agent modal (Spawn form, ConfirmKill, ConfirmClean) reused from `panel.rs`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientShepherdAgentModal {
    pub sub_modal: AgentsSubModal,
    pub last_action: Option<ShepherdAgentsAction>,
    pub error: Option<String>,
}

impl ClientShepherdAgentModal {
    pub fn new(sub_modal: AgentsSubModal) -> Self {
        Self {
            sub_modal,
            last_action: None,
            error: None,
        }
    }
}

/// Active panel rendered geometry saved for mouse click delegation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActivePanelRender {
    Skills(ShepherdSkillsRender),
    Git(ShepherdGitRender),
}

/// Geometry and hit-test targets returned by `render_shepherd_overlay`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShepherdOverlayHits {
    pub area: Rect,
    pub popup: Rect,
    pub tabs: Vec<(Rect, ShepherdTab)>,
    pub close: Rect,
    pub content: Rect,
    pub panel: Option<ActivePanelRender>,
}

/// Action dispatcher contract for Shepherd operations.
pub trait ShepherdActionDispatcher {
    fn dispatch_call(&mut self, op: &str, args: Value);
    fn focus_pane(&mut self, pane_id: &str, agent_id: &str);
    fn open_editor(&mut self, task_id: u32, file_path: &Path);
}

/// A fake action dispatcher used to verify dispatching in unit tests.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FakeShepherdDispatcher {
    pub calls: Vec<(String, Value)>,
    pub focused_panes: Vec<(String, String)>,
    pub opened_editors: Vec<(u32, PathBuf)>,
}

impl ShepherdActionDispatcher for FakeShepherdDispatcher {
    fn dispatch_call(&mut self, op: &str, args: Value) {
        self.calls.push((op.to_string(), args));
    }

    fn focus_pane(&mut self, pane_id: &str, agent_id: &str) {
        self.focused_panes
            .push((pane_id.to_string(), agent_id.to_string()));
    }

    fn open_editor(&mut self, task_id: u32, file_path: &Path) {
        self.opened_editors.push((task_id, file_path.to_path_buf()));
    }
}

/// Outcome of processing a keyboard event in the Shepherd overlay or agent modal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShepherdKeyOutcome {
    Handled,
    CloseOverlay,
    FocusPane { pane_id: String, agent_id: String },
    OpenEditor { task_id: u32, file_path: PathBuf },
    Ignored,
}

/// Pure render of the Shepherd overlay inside a Ratatui buffer.
/// Respects tiny areas gracefully and never panics.
pub fn render_shepherd_overlay(
    buffer: &mut Buffer,
    overlay: &ClientShepherdOverlay,
    palette: &Palette,
) -> Option<ShepherdOverlayHits> {
    let area = buffer.area;
    if area.width < 12 || area.height < 8 {
        return None;
    }

    // Modal popup dimensions
    let target_w = SHEPHERD_OVERLAY_MODAL_SIZE
        .0
        .min(area.width.saturating_sub(2));
    let target_h = SHEPHERD_OVERLAY_MODAL_SIZE
        .1
        .min(area.height.saturating_sub(2));
    if target_w < 10 || target_h < 6 {
        return None;
    }

    let popup_x = area.x + (area.width.saturating_sub(target_w)) / 2;
    let popup_y = area.y + (area.height.saturating_sub(target_h)) / 2;
    let popup_rect = Rect::new(popup_x, popup_y, target_w, target_h);

    // Dim background outside popup
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            if !contains_point(popup_rect, x, y) {
                let cell = &mut buffer[(x, y)];
                cell.set_style(cell.style().add_modifier(Modifier::DIM));
            }
        }
    }

    // Draw frame border
    let bg_style = Style::default().bg(palette.panel_bg);
    let border_style = Style::default().fg(palette.accent).bg(palette.panel_bg);

    for y in popup_rect.y..popup_rect.bottom() {
        for x in popup_rect.x..popup_rect.right() {
            buffer[(x, y)].set_symbol(" ").set_style(bg_style);
        }
    }

    // Borders
    for x in popup_rect.x..popup_rect.right() {
        buffer[(x, popup_rect.y)]
            .set_symbol(if x == popup_rect.x {
                "┌"
            } else if x + 1 == popup_rect.right() {
                "┐"
            } else {
                "─"
            })
            .set_style(border_style);
        buffer[(x, popup_rect.bottom() - 1)]
            .set_symbol(if x == popup_rect.x {
                "└"
            } else if x + 1 == popup_rect.right() {
                "┘"
            } else {
                "─"
            })
            .set_style(border_style);
    }
    for y in popup_rect.y + 1..popup_rect.bottom() - 1 {
        buffer[(popup_rect.x, y)]
            .set_symbol("│")
            .set_style(border_style);
        buffer[(popup_rect.right() - 1, y)]
            .set_symbol("│")
            .set_style(border_style);
    }

    let inner = Rect::new(
        popup_rect.x + 1,
        popup_rect.y + 1,
        popup_rect.width.saturating_sub(2),
        popup_rect.height.saturating_sub(2),
    );
    if inner.width < 8 || inner.height < 4 {
        return None;
    }

    // Header: title " Shepherd " and close button "[x]"
    let title_style = Style::default()
        .fg(palette.accent)
        .bg(palette.panel_bg)
        .add_modifier(Modifier::BOLD);
    let title_text = " Shepherd ";
    for (i, ch) in title_text.chars().enumerate() {
        let x = inner.x + i as u16;
        if x < inner.right() {
            buffer[(x, inner.y)]
                .set_symbol(&ch.to_string())
                .set_style(title_style);
        }
    }

    let close_text = "[x]";
    let close_x = inner.right().saturating_sub(3);
    let close_rect = Rect::new(close_x, inner.y, 3, 1);
    let close_style = Style::default()
        .fg(palette.overlay1)
        .bg(palette.panel_bg)
        .add_modifier(Modifier::BOLD);
    for (i, ch) in close_text.chars().enumerate() {
        let x = close_rect.x + i as u16;
        if x < inner.right() {
            buffer[(x, inner.y)]
                .set_symbol(&ch.to_string())
                .set_style(close_style);
        }
    }

    // Tab bar on row inner.y + 1: Tareas / Runs / Skills
    let mut tab_x = inner.x;
    let mut tab_hits = Vec::new();
    for tab in ShepherdTab::ALL {
        let label = format!(" {} ", tab.label());
        let width = label.len() as u16;
        if tab_x + width > inner.right() {
            break;
        }
        let tab_rect = Rect::new(tab_x, inner.y + 1, width, 1);
        let active = tab == overlay.active_tab;
        let tab_style = if active {
            Style::default()
                .fg(Color::Black)
                .bg(palette.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette.overlay1).bg(palette.panel_bg)
        };
        for (i, ch) in label.chars().enumerate() {
            buffer[(tab_rect.x + i as u16, tab_rect.y)]
                .set_symbol(&ch.to_string())
                .set_style(tab_style);
        }
        tab_hits.push((tab_rect, tab));
        tab_x += width + 1;
    }

    // Divider line on row inner.y + 2
    let divider_style = Style::default().fg(palette.surface0).bg(palette.panel_bg);
    for x in inner.x..inner.right() {
        buffer[(x, inner.y + 2)]
            .set_symbol("─")
            .set_style(divider_style);
    }

    // Content area
    let content_area = Rect::new(
        inner.x,
        inner.y + 3,
        inner.width,
        inner.height.saturating_sub(3),
    );

    // Render active tab view into content_area and store its rendered layout
    let active_render = match overlay.active_tab {
        ShepherdTab::Skills => {
            render_shepherd_skills_panel(buffer, content_area, &overlay.skills, palette)
                .map(ActivePanelRender::Skills)
        }
        ShepherdTab::Git => render_shepherd_git_panel(buffer, content_area, &overlay.git, palette)
            .map(ActivePanelRender::Git),
    };

    // Optional status message at the very bottom line of popup
    if let Some(ref msg) = overlay.status_message {
        let status_y = popup_rect.bottom().saturating_sub(1);
        let msg_style = Style::default()
            .fg(Color::Yellow)
            .bg(palette.panel_bg)
            .add_modifier(Modifier::BOLD);
        let display_msg = format!(" {msg} ");
        for (i, ch) in display_msg.chars().enumerate() {
            let x = inner.x + i as u16;
            if x < inner.right() {
                buffer[(x, status_y)]
                    .set_symbol(&ch.to_string())
                    .set_style(msg_style);
            }
        }
    }

    Some(ShepherdOverlayHits {
        area,
        popup: popup_rect,
        tabs: tab_hits,
        close: close_rect,
        content: content_area,
        panel: active_render,
    })
}

/// Routes a keyboard event through the Shepherd overlay.
pub fn route_shepherd_key(
    overlay: &mut ClientShepherdOverlay,
    code: KeyCode,
    modifiers: KeyModifiers,
    dispatcher: &mut dyn ShepherdActionDispatcher,
) -> ShepherdKeyOutcome {
    // 1. Esc: Cancel sub-modal if open; otherwise close the entire overlay.
    if code == KeyCode::Esc {
        if overlay.has_active_submodal() {
            overlay.cancel_active_submodal();
            return ShepherdKeyOutcome::Handled;
        } else {
            return ShepherdKeyOutcome::CloseOverlay;
        }
    }

    // 2. Tab switching with Tab / Shift+Tab or Left / Right (when no submodal text editor is active)
    let is_text_input = match overlay.active_tab {
        ShepherdTab::Skills => matches!(overlay.skills.sub_modal, SkillsSubModal::Create(_)),
        ShepherdTab::Git => false,
    };

    if !is_text_input {
        let prev_tab = overlay.active_tab;
        if (code == KeyCode::Tab && modifiers.is_empty())
            || (code == KeyCode::Right && modifiers.is_empty())
        {
            overlay.next_tab();
            if overlay.active_tab == ShepherdTab::Git && prev_tab != ShepherdTab::Git {
                dispatcher.dispatch_call("git_status", json!({}));
            }
            return ShepherdKeyOutcome::Handled;
        }
        if (code == KeyCode::BackTab)
            || (code == KeyCode::Tab && modifiers.contains(KeyModifiers::SHIFT))
            || (code == KeyCode::Left && modifiers.is_empty())
        {
            overlay.prev_tab();
            if overlay.active_tab == ShepherdTab::Git && prev_tab != ShepherdTab::Git {
                dispatcher.dispatch_call("git_status", json!({}));
            }
            return ShepherdKeyOutcome::Handled;
        }
    }

    let key_event = KeyEvent::new(code, modifiers);

    // 3. Delegate to the active tab's view
    match overlay.active_tab {
        ShepherdTab::Skills => {
            let action = overlay.skills.handle_key(key_event);
            if let Some(action) = action.or_else(|| overlay.skills.last_action.take()) {
                match action {
                    ShepherdSkillsAction::ToggleAgentSkill {
                        agent_id,
                        skill_name,
                        enable,
                    } => {
                        overlay.last_dispatched = Some(format!("skills_toggle:{skill_name}"));
                        dispatcher.dispatch_call(
                            "skills_toggle",
                            json!({ "agent_id": agent_id, "skill": skill_name, "enable": enable }),
                        );
                    }
                    ShepherdSkillsAction::RestoreEditedSkill {
                        agent_id,
                        skill_name,
                    } => {
                        overlay.last_dispatched = Some(format!("skills_restore:{skill_name}"));
                        dispatcher.dispatch_call(
                            "skills_restore",
                            json!({ "agent_id": agent_id, "skill": skill_name }),
                        );
                    }
                    ShepherdSkillsAction::CreateCatalogSkill {
                        name,
                        description,
                        scope,
                        is_global,
                    } => {
                        overlay.last_dispatched = Some(format!("skills_create:{name}"));
                        dispatcher.dispatch_call(
                            "skills_create",
                            json!({
                                "name": name,
                                "description": description,
                                "scope": format!("{scope:?}").to_lowercase(),
                                "is_global": is_global,
                            }),
                        );
                    }
                    ShepherdSkillsAction::ToggleCatalogGlobal { name, make_global } => {
                        overlay.last_dispatched = Some(format!("skills_toggle_global:{name}"));
                        dispatcher.dispatch_call(
                            "skills_toggle_global",
                            json!({ "name": name, "is_global": make_global }),
                        );
                    }
                    ShepherdSkillsAction::DeleteCatalogSkill { name, scope } => {
                        overlay.last_dispatched = Some(format!("skills_delete:{name}"));
                        dispatcher.dispatch_call(
                            "skills_delete",
                            json!({ "name": name, "scope": format!("{scope:?}").to_lowercase() }),
                        );
                    }
                }
            }
            ShepherdKeyOutcome::Handled
        }

        ShepherdTab::Git => {
            let action = overlay.git.handle_key(key_event);
            if let Some(action) = action.or_else(|| overlay.git.last_action.take()) {
                match action {
                    ShepherdGitAction::Refresh => {
                        overlay.last_dispatched = Some("git_status".into());
                        dispatcher.dispatch_call("git_status", json!({}));
                    }
                    ShepherdGitAction::CopyOrShowPath { path } => {
                        overlay.status_message = Some(format!("Worktree: {}", path.display()));
                    }
                }
            }
            ShepherdKeyOutcome::Handled
        }
    }
}

/// Routes a keyboard event through a standalone agent modal (SpawnForm, ConfirmKill, ConfirmClean).
pub fn route_agent_modal_key(
    modal: &mut ClientShepherdAgentModal,
    code: KeyCode,
    modifiers: KeyModifiers,
    dispatcher: &mut dyn ShepherdActionDispatcher,
) -> ShepherdKeyOutcome {
    if code == KeyCode::Esc {
        return ShepherdKeyOutcome::CloseOverlay;
    }

    let key_event = KeyEvent::new(code, modifiers);
    let mut temp_panel = super::panel::ShepherdAgentsPanelState {
        sub_modal: modal.sub_modal.clone(),
        ..super::panel::ShepherdAgentsPanelState::default()
    };
    let action = temp_panel.handle_key(key_event);
    modal.sub_modal = temp_panel.sub_modal;

    if let Some(action) = action.or(temp_panel.last_action) {
        match action {
            ShepherdAgentsAction::FocusPane { pane_id, agent_id } => {
                dispatcher.focus_pane(&pane_id, &agent_id);
                return ShepherdKeyOutcome::FocusPane { pane_id, agent_id };
            }
            ShepherdAgentsAction::Spawn {
                role,
                task,
                vendor,
                model,
                extra_skills,
            } => {
                let mut args = json!({ "role": role, "task": task });
                if let Some(v) = vendor {
                    args["vendor"] = json!(v);
                }
                if let Some(m) = model {
                    args["model"] = json!(m);
                }
                if !extra_skills.is_empty() {
                    args["extra_skills"] = json!(extra_skills);
                }
                dispatcher.dispatch_call("spawn", args);
                return ShepherdKeyOutcome::CloseOverlay;
            }
            ShepherdAgentsAction::Kill { agent_id } => {
                dispatcher.dispatch_call("kill", json!({ "id": agent_id }));
                return ShepherdKeyOutcome::CloseOverlay;
            }
            ShepherdAgentsAction::Clean { agent_id } => {
                dispatcher.dispatch_call("clean", json!({ "ids": [agent_id] }));
                return ShepherdKeyOutcome::CloseOverlay;
            }
            ShepherdAgentsAction::CleanAllFinished { workspace, .. } => {
                let mut args = json!({ "ids": [] });
                if let Some(ws) = workspace {
                    args["workspace"] = json!(ws);
                }
                dispatcher.dispatch_call("clean", args);
                return ShepherdKeyOutcome::CloseOverlay;
            }
        }
    }

    ShepherdKeyOutcome::Handled
}

/// Outcome of handling a mouse click in the Shepherd overlay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShepherdClickOutcome {
    Handled,
    CloseOverlay,
    FocusPane { pane_id: String, agent_id: String },
    OpenEditor { task_id: u32, file_path: PathBuf },
    Ignored,
}

/// Handles a mouse click on the Shepherd overlay given hit-test geometry.
pub fn handle_shepherd_mouse_click(
    overlay: &mut ClientShepherdOverlay,
    point: (u16, u16),
    hits: &ShepherdOverlayHits,
    dispatcher: &mut dyn ShepherdActionDispatcher,
) -> ShepherdClickOutcome {
    // Click outside popup or on close button: closes overlay
    if contains_point(hits.close, point.0, point.1) || !contains_point(hits.popup, point.0, point.1)
    {
        return ShepherdClickOutcome::CloseOverlay;
    }

    // Click on any tab header: switches active tab
    for (tab_rect, tab) in &hits.tabs {
        if contains_point(*tab_rect, point.0, point.1) {
            let prev_tab = overlay.active_tab;
            overlay.select_tab(*tab);
            if *tab == ShepherdTab::Git && prev_tab != ShepherdTab::Git {
                dispatcher.dispatch_call("git_status", json!({}));
            }
            return ShepherdClickOutcome::Handled;
        }
    }

    // Click inside content area: delegate to active view's mouse handling
    if contains_point(hits.content, point.0, point.1) {
        match (overlay.active_tab, &hits.panel) {
            (ShepherdTab::Skills, Some(ActivePanelRender::Skills(rendered))) => {
                overlay.skills.handle_click(point.0, point.1, rendered);
                return ShepherdClickOutcome::Handled;
            }
            (ShepherdTab::Git, Some(ActivePanelRender::Git(rendered))) => {
                if let Some(action) = overlay.git.handle_click(point.0, point.1, rendered) {
                    match action {
                        ShepherdGitAction::Refresh => {
                            overlay.last_dispatched = Some("git_status".into());
                            dispatcher.dispatch_call("git_status", json!({}));
                        }
                        ShepherdGitAction::CopyOrShowPath { path } => {
                            overlay.status_message = Some(format!("Worktree: {}", path.display()));
                        }
                    }
                }
                return ShepherdClickOutcome::Handled;
            }
            _ => {}
        }
    }

    ShepherdClickOutcome::Handled
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shepherd::skills::{AgentSkillItem, CatalogSkillItem, SkillsViewMode};
    use shepherd_core::skills::{Level, Scope};

    fn test_palette() -> Palette {
        Palette::catppuccin()
    }

    #[test]
    fn test_tiny_screen_does_not_panic() {
        let overlay = ClientShepherdOverlay::default();
        let palette = test_palette();

        // 0x0
        let mut buf0 = Buffer::empty(Rect::new(0, 0, 0, 0));
        assert!(render_shepherd_overlay(&mut buf0, &overlay, &palette).is_none());

        // 5x5
        let mut buf1 = Buffer::empty(Rect::new(0, 0, 5, 5));
        assert!(render_shepherd_overlay(&mut buf1, &overlay, &palette).is_none());

        // 11x7 (just below 12x8 threshold)
        let mut buf2 = Buffer::empty(Rect::new(0, 0, 11, 7));
        assert!(render_shepherd_overlay(&mut buf2, &overlay, &palette).is_none());

        // Valid small size
        let mut buf3 = Buffer::empty(Rect::new(0, 0, 80, 24));
        let hits = render_shepherd_overlay(&mut buf3, &overlay, &palette);
        assert!(hits.is_some());
    }

    #[test]
    fn test_tab_switching_via_keyboard_and_mouse() {
        let mut overlay = ClientShepherdOverlay::default();
        let mut dispatcher = FakeShepherdDispatcher::default();

        assert_eq!(overlay.active_tab, ShepherdTab::Skills);

        // Tab -> Git (dispatches git_status)
        let outcome = route_shepherd_key(
            &mut overlay,
            KeyCode::Tab,
            KeyModifiers::empty(),
            &mut dispatcher,
        );
        assert_eq!(outcome, ShepherdKeyOutcome::Handled);
        assert_eq!(overlay.active_tab, ShepherdTab::Git);
        assert!(dispatcher.calls.iter().any(|(op, _)| op == "git_status"));

        // Tab wraps -> Skills
        route_shepherd_key(
            &mut overlay,
            KeyCode::Tab,
            KeyModifiers::empty(),
            &mut dispatcher,
        );
        assert_eq!(overlay.active_tab, ShepherdTab::Skills);

        // BackTab wraps -> Git
        route_shepherd_key(
            &mut overlay,
            KeyCode::BackTab,
            KeyModifiers::empty(),
            &mut dispatcher,
        );
        assert_eq!(overlay.active_tab, ShepherdTab::Git);

        // Mouse click on Skills tab
        let mut buf = Buffer::empty(Rect::new(0, 0, 96, 28));
        let hits = render_shepherd_overlay(&mut buf, &overlay, &test_palette()).unwrap();
        let (skills_rect, _) = hits
            .tabs
            .iter()
            .find(|(_, t)| *t == ShepherdTab::Skills)
            .unwrap();
        let click_point = (skills_rect.x, skills_rect.y);

        let click_outcome =
            handle_shepherd_mouse_click(&mut overlay, click_point, &hits, &mut dispatcher);
        assert_eq!(click_outcome, ShepherdClickOutcome::Handled);
        assert_eq!(overlay.active_tab, ShepherdTab::Skills);

        // Mouse click on Git tab
        let (git_rect, _) = hits
            .tabs
            .iter()
            .find(|(_, t)| *t == ShepherdTab::Git)
            .unwrap();
        let click_git_point = (git_rect.x, git_rect.y);
        let click_git_outcome =
            handle_shepherd_mouse_click(&mut overlay, click_git_point, &hits, &mut dispatcher);
        assert_eq!(click_git_outcome, ShepherdClickOutcome::Handled);
        assert_eq!(overlay.active_tab, ShepherdTab::Git);
    }

    #[test]
    fn test_overlay_renders_single_frame_without_inner_double_box_or_redundant_esc_close() {
        let overlay = ClientShepherdOverlay::default();
        let palette = test_palette();
        let area = Rect::new(0, 0, 100, 30);
        let mut buffer = Buffer::empty(area);

        let hits =
            render_shepherd_overlay(&mut buffer, &overlay, &palette).expect("render overlay");
        assert_eq!(hits.area, area);

        let text = buffer
            .content
            .chunks(usize::from(area.width))
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");

        // The outer overlay frame contains " Shepherd " and the tabs
        assert!(text.contains(" Shepherd "));
        assert!(text.contains(" Skills "));
        assert!(text.contains(" Git "));

        // Single frame: the redundant inner title and "esc close" button must NOT be present
        assert!(!text.contains("esc close"));
    }

    #[test]
    fn test_esc_cancels_submodal_first_then_closes_overlay() {
        let mut overlay = ClientShepherdOverlay::default();
        let mut dispatcher = FakeShepherdDispatcher::default();

        // Open skills confirm restore submodal
        overlay.skills.sub_modal = SkillsSubModal::ConfirmRestore {
            agent_id: "agent-1".into(),
            skill_name: "test-skill".into(),
            message: "Restore?".into(),
        };
        assert!(overlay.has_active_submodal());

        // First Esc: cancels submodal
        let res1 = route_shepherd_key(
            &mut overlay,
            KeyCode::Esc,
            KeyModifiers::empty(),
            &mut dispatcher,
        );
        assert_eq!(res1, ShepherdKeyOutcome::Handled);
        assert!(!overlay.has_active_submodal());

        // Second Esc: closes overlay
        let res2 = route_shepherd_key(
            &mut overlay,
            KeyCode::Esc,
            KeyModifiers::empty(),
            &mut dispatcher,
        );
        assert_eq!(res2, ShepherdKeyOutcome::CloseOverlay);
    }

    #[test]
    fn test_destructive_guard_delete_skill() {
        let mut overlay = ClientShepherdOverlay::default();
        let mut dispatcher = FakeShepherdDispatcher::default();
        overlay.select_tab(ShepherdTab::Skills);
        overlay.skills.mode = SkillsViewMode::CatalogView;

        overlay.skills.catalog_skills = vec![CatalogSkillItem {
            name: "custom-tool".into(),
            description: "custom".into(),
            scope: Scope::User,
            is_global: false,
            enabled: true,
            problems: Vec::new(),
        }];

        // Press 'd' to delete catalog skill
        route_shepherd_key(
            &mut overlay,
            KeyCode::Char('d'),
            KeyModifiers::empty(),
            &mut dispatcher,
        );
        assert!(matches!(
            overlay.skills.sub_modal,
            SkillsSubModal::ConfirmDelete { .. }
        ));
        assert!(
            dispatcher.calls.is_empty(),
            "Delete skill must NOT be dispatched without confirmation"
        );

        // Confirm with Enter
        route_shepherd_key(
            &mut overlay,
            KeyCode::Enter,
            KeyModifiers::empty(),
            &mut dispatcher,
        );
        assert_eq!(dispatcher.calls.len(), 1);
        assert_eq!(dispatcher.calls[0].0, "skills_delete");
        assert_eq!(dispatcher.calls[0].1["name"], "custom-tool");
    }

    #[test]
    fn test_destructive_guard_restore_skill() {
        let mut overlay = ClientShepherdOverlay::default();
        let mut dispatcher = FakeShepherdDispatcher::default();
        overlay.select_tab(ShepherdTab::Skills);

        overlay.skills.selected_agent_id = "coder-1".into();
        overlay.skills.mode = crate::shepherd::skills::SkillsViewMode::AgentView;
        overlay.skills.agent_skills = vec![AgentSkillItem {
            name: "git-review".into(),
            description: "review".into(),
            level: Level::Session,
            scope: Scope::Repo,
            active: true,
            edited: true,
            shadowed_by: None,
        }];

        // Press 'r' to restore edited skill
        route_shepherd_key(
            &mut overlay,
            KeyCode::Char('r'),
            KeyModifiers::empty(),
            &mut dispatcher,
        );
        assert!(matches!(
            overlay.skills.sub_modal,
            SkillsSubModal::ConfirmRestore { .. }
        ));
        assert!(
            dispatcher.calls.is_empty(),
            "Restore skill must NOT be dispatched without confirmation"
        );

        // Confirm with Enter
        route_shepherd_key(
            &mut overlay,
            KeyCode::Enter,
            KeyModifiers::empty(),
            &mut dispatcher,
        );
        assert_eq!(dispatcher.calls.len(), 1);
        assert_eq!(dispatcher.calls[0].0, "skills_restore");
        assert_eq!(dispatcher.calls[0].1["skill"], "git-review");
    }

    #[test]
    fn test_standalone_agent_modals_kill_clean_spawn() {
        let mut dispatcher = FakeShepherdDispatcher::default();

        // 1. ConfirmKill
        let mut kill_modal = ClientShepherdAgentModal::new(AgentsSubModal::ConfirmKill {
            agent_id: "coder-99".into(),
            state: "working".into(),
            message: "Kill agent?".into(),
        });
        let outcome = route_agent_modal_key(
            &mut kill_modal,
            KeyCode::Enter,
            KeyModifiers::empty(),
            &mut dispatcher,
        );
        assert_eq!(outcome, ShepherdKeyOutcome::CloseOverlay);
        assert_eq!(dispatcher.calls.len(), 1);
        assert_eq!(dispatcher.calls[0].0, "kill");
        assert_eq!(dispatcher.calls[0].1["id"], "coder-99");

        // 2. ConfirmClean
        let mut clean_modal = ClientShepherdAgentModal::new(AgentsSubModal::ConfirmClean {
            agent_id: "coder-98".into(),
            state: "done".into(),
            message: "Clean agent?".into(),
        });
        let outcome = route_agent_modal_key(
            &mut clean_modal,
            KeyCode::Enter,
            KeyModifiers::empty(),
            &mut dispatcher,
        );
        assert_eq!(outcome, ShepherdKeyOutcome::CloseOverlay);
        assert_eq!(dispatcher.calls.len(), 2);
        assert_eq!(dispatcher.calls[1].0, "clean");
        assert_eq!(dispatcher.calls[1].1["ids"][0], "coder-98");
    }
}
