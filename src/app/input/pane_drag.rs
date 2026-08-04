use std::time::{Duration, Instant};

use crossterm::event::{
    KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;

use crate::api::schema::{PaneMoveDestination, PaneMoveParams, PanePlacement, PaneSwapParams};
use crate::app::state::{PaneDropPreview, ViewLayout};
use crate::app::{App, InputSourceId};
use crate::layout::PaneId;

const PANE_DRAG_THRESHOLD: u16 = 1;
const NAVIGATION_HOVER_DELAY: Duration = Duration::from_millis(400);
const AUTOSCROLL_DELAY: Duration = Duration::from_millis(120);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaneDragPhase {
    Pressed,
    Dragging,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PaneDragOrigin {
    workspace_id: String,
    tab_id: String,
    pane_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PaneDragHoverTarget {
    Workspace(String),
    Tab(String),
    WorkspaceScroll(i8),
    TabScroll(i8),
}

#[derive(Debug, Clone)]
struct PaneDragHover {
    target: PaneDragHoverTarget,
    deadline: Instant,
}

#[derive(Debug, Clone, PartialEq)]
enum PaneDropAction {
    Swap {
        source_pane_id: String,
        target_pane_id: String,
    },
    Move {
        source_pane_id: String,
        tab_id: String,
        target_pane_id: String,
        placement: PanePlacement,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct PaneDragController {
    owner_source_id: InputSourceId,
    phase: PaneDragPhase,
    source_pane_id: PaneId,
    source_public_pane_id: String,
    source_tab_id: String,
    origin: PaneDragOrigin,
    press_col: u16,
    press_row: u16,
    last_col: u16,
    last_row: u16,
    hover: Option<PaneDragHover>,
    drop_action: Option<PaneDropAction>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaneDropZone {
    Left,
    Right,
    Above,
    Below,
    Center,
}

impl PaneDropZone {
    fn placement(self) -> Option<PanePlacement> {
        match self {
            Self::Left => Some(PanePlacement::Left),
            Self::Right => Some(PanePlacement::Right),
            Self::Above => Some(PanePlacement::Above),
            Self::Below => Some(PanePlacement::Below),
            Self::Center => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Left => "move left",
            Self::Right => "move right",
            Self::Above => "move above",
            Self::Below => "move below",
            Self::Center => "swap panes",
        }
    }
}

fn rect_contains(rect: Rect, col: u16, row: u16) -> bool {
    rect.width > 0
        && rect.height > 0
        && col >= rect.x
        && col < rect.x.saturating_add(rect.width)
        && row >= rect.y
        && row < rect.y.saturating_add(rect.height)
}

fn pane_drop_zone(rect: Rect, col: u16, row: u16) -> PaneDropZone {
    let width = rect.width.max(1);
    let height = rect.height.max(1);
    let left = col.saturating_sub(rect.x).min(width.saturating_sub(1));
    let right = rect
        .x
        .saturating_add(width)
        .saturating_sub(1)
        .saturating_sub(col)
        .min(width.saturating_sub(1));
    let top = row.saturating_sub(rect.y).min(height.saturating_sub(1));
    let bottom = rect
        .y
        .saturating_add(height)
        .saturating_sub(1)
        .saturating_sub(row)
        .min(height.saturating_sub(1));

    let has_center = width >= 8 && height >= 5;
    if has_center
        && left.saturating_mul(4) >= width
        && right.saturating_mul(4) >= width
        && top.saturating_mul(4) >= height
        && bottom.saturating_mul(4) >= height
    {
        return PaneDropZone::Center;
    }

    // Compare normalized distances without floating point. Stable tie order
    // keeps tiny panes deterministic.
    let candidates = [
        (left as u32 * height as u32, PaneDropZone::Left),
        (right as u32 * height as u32, PaneDropZone::Right),
        (top as u32 * width as u32, PaneDropZone::Above),
        (bottom as u32 * width as u32, PaneDropZone::Below),
    ];
    candidates
        .into_iter()
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, zone)| zone)
        .unwrap_or(PaneDropZone::Left)
}

fn response_is_error(response: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(response)
        .ok()
        .and_then(|value| value.get("error").cloned())
        .is_some()
}

impl App {
    pub(crate) fn pane_drag_owned_by(&self, source_id: InputSourceId) -> bool {
        self.pane_drag
            .as_ref()
            .is_some_and(|drag| drag.owner_source_id == source_id)
    }

    pub(crate) fn pane_drag_deadline(&self) -> Option<Instant> {
        self.pane_drag
            .as_ref()
            .and_then(|drag| drag.hover.as_ref().map(|hover| hover.deadline))
    }

    pub(crate) fn handle_pane_drag_key(
        &mut self,
        source_id: InputSourceId,
        key: &crate::input::TerminalKey,
    ) -> bool {
        if key.kind != KeyEventKind::Press {
            return false;
        }
        let owned = self
            .pane_drag
            .as_ref()
            .is_some_and(|drag| drag.owner_source_id == source_id);
        if !owned || matches!(key.code, KeyCode::Modifier(_)) {
            return false;
        }
        let consumed = key.code == KeyCode::Esc;
        if owned {
            self.cancel_pane_drag(source_id, true);
        }
        consumed
    }

    pub(crate) fn cancel_pane_drag(&mut self, source_id: InputSourceId, restore_origin: bool) {
        let Some(drag) = self.pane_drag.take() else {
            return;
        };
        if drag.owner_source_id != source_id {
            self.pane_drag = Some(drag);
            return;
        }
        self.state.pane_drop_preview = None;
        if restore_origin {
            self.restore_pane_drag_origin(&drag.origin);
        }
    }

    pub(crate) fn handle_pane_drag_mouse(
        &mut self,
        source_id: InputSourceId,
        mouse: MouseEvent,
    ) -> bool {
        if let Some(owner) = self.pane_drag.as_ref().map(|drag| drag.owner_source_id) {
            if owner != source_id {
                return matches!(
                    mouse.kind,
                    MouseEventKind::Down(MouseButton::Left)
                        | MouseEventKind::Drag(MouseButton::Left)
                        | MouseEventKind::Up(MouseButton::Left)
                );
            }
            return self.handle_active_pane_drag(mouse);
        }

        if !self.state.mouse_capture
            || self.state.view.layout != ViewLayout::Desktop
            || mouse.modifiers != KeyModifiers::empty()
            || !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
        {
            return false;
        }
        let Some(hit) = self
            .state
            .view
            .pane_title_hit_areas
            .iter()
            .find(|hit| rect_contains(hit.rect, mouse.column, mouse.row))
            .copied()
        else {
            return false;
        };
        let Some(ws_idx) = self.state.active else {
            return false;
        };
        let Some(tab_idx) = self.state.workspaces[ws_idx].find_tab_index_for_pane(hit.pane_id)
        else {
            return false;
        };
        let Some(source_public_pane_id) = self.public_pane_id(ws_idx, hit.pane_id) else {
            return false;
        };
        let Some(source_tab_id) = self.public_tab_id(ws_idx, tab_idx) else {
            return false;
        };
        let Some(origin_pane) = self.state.workspaces[ws_idx].focused_pane_id() else {
            return false;
        };
        let Some(origin_pane_id) = self.public_pane_id(ws_idx, origin_pane) else {
            return false;
        };
        let origin = PaneDragOrigin {
            workspace_id: self.public_workspace_id(ws_idx),
            tab_id: source_tab_id.clone(),
            pane_id: origin_pane_id,
        };

        self.state.clear_selection();
        self.selection_autoscroll_deadline = None;
        self.state.selection_autoscroll = None;
        self.state.drag = None;
        self.state.workspace_press = None;
        self.state.tab_press = None;
        self.state.pane_drop_preview = None;
        self.last_pane_click = None;
        let _ = self.runtime_pane_focus("tui.mouse.pane_drag.focus", source_public_pane_id.clone());
        self.pane_drag = Some(PaneDragController {
            owner_source_id: source_id,
            phase: PaneDragPhase::Pressed,
            source_pane_id: hit.pane_id,
            source_public_pane_id,
            source_tab_id,
            origin,
            press_col: mouse.column,
            press_row: mouse.row,
            last_col: mouse.column,
            last_row: mouse.row,
            hover: None,
            drop_action: None,
        });
        true
    }

    fn handle_active_pane_drag(&mut self, mouse: MouseEvent) -> bool {
        match mouse.kind {
            MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Moved => {
                let Some(mut drag) = self.pane_drag.take() else {
                    return false;
                };
                drag.last_col = mouse.column;
                drag.last_row = mouse.row;
                if drag.phase == PaneDragPhase::Pressed {
                    let delta_col = drag.press_col.abs_diff(mouse.column);
                    let delta_row = drag.press_row.abs_diff(mouse.row);
                    if delta_col.max(delta_row) >= PANE_DRAG_THRESHOLD {
                        drag.phase = PaneDragPhase::Dragging;
                    }
                }
                if drag.phase == PaneDragPhase::Dragging {
                    self.update_pane_drag_target(&mut drag, mouse.column, mouse.row);
                }
                self.pane_drag = Some(drag);
                true
            }
            MouseEventKind::Up(MouseButton::Left) => {
                let Some(drag) = self.pane_drag.take() else {
                    return false;
                };
                self.state.pane_drop_preview = None;
                if drag.phase == PaneDragPhase::Pressed {
                    return true;
                }
                let Some(action) = drag.drop_action else {
                    self.restore_pane_drag_origin(&drag.origin);
                    return true;
                };
                let response = match action {
                    PaneDropAction::Swap {
                        source_pane_id,
                        target_pane_id,
                    } => self.runtime_pane_swap(
                        "tui.mouse.pane_drag.swap",
                        PaneSwapParams {
                            source_pane_id: Some(source_pane_id),
                            target_pane_id: Some(target_pane_id),
                            ..PaneSwapParams::default()
                        },
                    ),
                    PaneDropAction::Move {
                        source_pane_id,
                        tab_id,
                        target_pane_id,
                        placement,
                    } => self.runtime_pane_move(
                        "tui.mouse.pane_drag.move",
                        PaneMoveParams {
                            pane_id: source_pane_id,
                            destination: PaneMoveDestination::TabPlacement {
                                tab_id,
                                target_pane_id,
                                placement,
                                moved_ratio: None,
                            },
                            focus: true,
                        },
                    ),
                };
                if response_is_error(&response) {
                    self.restore_pane_drag_origin(&drag.origin);
                }
                true
            }
            MouseEventKind::Down(MouseButton::Left) => true,
            MouseEventKind::Down(_) => {
                let Some(drag) = self.pane_drag.take() else {
                    return false;
                };
                self.state.pane_drop_preview = None;
                self.restore_pane_drag_origin(&drag.origin);
                true
            }
            _ => false,
        }
    }

    fn update_pane_drag_target(&mut self, drag: &mut PaneDragController, col: u16, row: u16) {
        drag.drop_action = None;
        self.state.pane_drop_preview = None;

        if let Some(info) = self
            .state
            .view
            .pane_infos
            .iter()
            .find(|info| rect_contains(info.rect, col, row))
            .cloned()
        {
            drag.hover = None;
            self.update_pane_drop_preview(drag, info.id, info.rect, col, row);
            return;
        }

        if let Some(direction) = self.pane_drag_tab_autoscroll_target(col, row) {
            Self::set_pane_drag_hover(
                drag,
                PaneDragHoverTarget::TabScroll(direction),
                AUTOSCROLL_DELAY,
            );
            return;
        }

        if let Some(direction) = self.pane_drag_workspace_autoscroll_target(col, row) {
            Self::set_pane_drag_hover(
                drag,
                PaneDragHoverTarget::WorkspaceScroll(direction),
                AUTOSCROLL_DELAY,
            );
            return;
        }

        if let Some(card) = self
            .state
            .view
            .workspace_card_areas
            .iter()
            .find(|card| rect_contains(card.rect, col, row))
            .copied()
        {
            if let Some(workspace_id) = self
                .state
                .workspaces
                .get(card.ws_idx)
                .map(|_| self.public_workspace_id(card.ws_idx))
            {
                Self::set_pane_drag_hover(
                    drag,
                    PaneDragHoverTarget::Workspace(workspace_id),
                    NAVIGATION_HOVER_DELAY,
                );
                return;
            }
        }

        if rect_contains(self.state.view.tab_bar_rect, col, row) {
            if let Some(tab_idx) = self
                .state
                .view
                .tab_hit_areas
                .iter()
                .position(|rect| rect_contains(*rect, col, row))
            {
                if let Some(ws_idx) = self.state.active {
                    if let Some(tab_id) = self.public_tab_id(ws_idx, tab_idx) {
                        let current_tab = self.state.workspaces[ws_idx].active_tab_index();
                        if current_tab != tab_idx {
                            Self::set_pane_drag_hover(
                                drag,
                                PaneDragHoverTarget::Tab(tab_id),
                                NAVIGATION_HOVER_DELAY,
                            );
                            return;
                        }
                    }
                }
            }
        }

        drag.hover = None;
    }

    fn update_pane_drop_preview(
        &mut self,
        drag: &mut PaneDragController,
        target_pane_id: PaneId,
        target_rect: Rect,
        col: u16,
        row: u16,
    ) {
        let zone = pane_drop_zone(target_rect, col, row);
        let Some(target_ws_idx) = self.state.active else {
            return;
        };
        let Some(target_tab_idx) =
            self.state.workspaces[target_ws_idx].find_tab_index_for_pane(target_pane_id)
        else {
            return;
        };
        let Some(target_tab_id) = self.public_tab_id(target_ws_idx, target_tab_idx) else {
            return;
        };
        let Some(target_public_pane_id) = self.public_pane_id(target_ws_idx, target_pane_id) else {
            return;
        };
        let same_tab = target_tab_id == drag.source_tab_id;

        if same_tab && target_pane_id == drag.source_pane_id {
            self.state.pane_drop_preview = Some(PaneDropPreview {
                rects: vec![target_rect],
                label: "choose another pane".into(),
                valid: false,
            });
            return;
        }

        if zone == PaneDropZone::Center {
            if !same_tab {
                self.state.pane_drop_preview = Some(PaneDropPreview {
                    rects: vec![target_rect],
                    label: "move toward an edge".into(),
                    valid: false,
                });
                return;
            }
            let source_rect = self
                .state
                .view
                .pane_infos
                .iter()
                .find(|info| info.id == drag.source_pane_id)
                .map(|info| info.rect);
            let mut rects = vec![target_rect];
            if let Some(source_rect) = source_rect {
                rects.push(source_rect);
            }
            drag.drop_action = Some(PaneDropAction::Swap {
                source_pane_id: drag.source_public_pane_id.clone(),
                target_pane_id: target_public_pane_id,
            });
            self.state.pane_drop_preview = Some(PaneDropPreview {
                rects,
                label: zone.label().into(),
                valid: true,
            });
            return;
        }

        let Some(placement) = zone.placement() else {
            return;
        };
        let tab = &self.state.workspaces[target_ws_idx].tabs[target_tab_idx];
        let projection = if same_tab {
            tab.layout
                .projected_relocation(drag.source_pane_id, target_pane_id, placement, 0.5)
        } else {
            tab.layout
                .projected_insert(target_pane_id, drag.source_pane_id, placement, 0.5)
        };
        let Some(projected_rect) = projection.and_then(|layout| {
            layout
                .panes(self.state.view.terminal_area)
                .into_iter()
                .find(|pane| pane.id == drag.source_pane_id)
                .map(|pane| pane.rect)
        }) else {
            self.state.pane_drop_preview = Some(PaneDropPreview {
                rects: vec![target_rect],
                label: "cannot place pane".into(),
                valid: false,
            });
            return;
        };
        drag.drop_action = Some(PaneDropAction::Move {
            source_pane_id: drag.source_public_pane_id.clone(),
            tab_id: target_tab_id,
            target_pane_id: target_public_pane_id,
            placement,
        });
        self.state.pane_drop_preview = Some(PaneDropPreview {
            rects: vec![projected_rect],
            label: zone.label().into(),
            valid: true,
        });
    }

    fn set_pane_drag_hover(
        drag: &mut PaneDragController,
        target: PaneDragHoverTarget,
        delay: Duration,
    ) {
        if drag
            .hover
            .as_ref()
            .is_some_and(|hover| hover.target == target)
        {
            return;
        }
        drag.hover = Some(PaneDragHover {
            target,
            deadline: Instant::now() + delay,
        });
    }

    fn pane_drag_tab_autoscroll_target(&self, col: u16, row: u16) -> Option<i8> {
        if rect_contains(self.state.view.tab_scroll_left_hit_area, col, row)
            && self.state.tab_scroll > 0
        {
            Some(-1)
        } else if rect_contains(self.state.view.tab_scroll_right_hit_area, col, row) {
            Some(1)
        } else {
            None
        }
    }

    fn pane_drag_workspace_autoscroll_target(&self, col: u16, row: u16) -> Option<i8> {
        if !rect_contains(self.state.view.sidebar_rect, col, row) {
            return None;
        }
        let first = self.state.view.workspace_card_areas.first()?;
        let last = self.state.view.workspace_card_areas.last()?;
        let workspace_area = self.state.workspace_list_rect();
        let max_scroll = crate::ui::workspace_list_scroll_metrics(&self.state, workspace_area)
            .max_offset_from_bottom;
        if row <= first.rect.y && self.state.workspace_scroll > 0 {
            Some(-1)
        } else if row
            >= last
                .rect
                .y
                .saturating_add(last.rect.height)
                .saturating_sub(1)
            && self.state.workspace_scroll < max_scroll
        {
            Some(1)
        } else {
            None
        }
    }

    pub(crate) fn tick_pane_drag(&mut self, now: Instant) -> bool {
        let due_pointer = self.pane_drag.as_ref().and_then(|drag| {
            drag.hover
                .as_ref()
                .filter(|hover| now >= hover.deadline)
                .map(|_| (drag.last_col, drag.last_row))
        });
        let mut refreshed = false;
        if let Some((col, row)) = due_pointer {
            if let Some(mut drag) = self.pane_drag.take() {
                self.update_pane_drag_target(&mut drag, col, row);
                self.pane_drag = Some(drag);
                refreshed = true;
            }
        }

        let Some(target) = self
            .pane_drag
            .as_ref()
            .and_then(|drag| drag.hover.as_ref())
            .filter(|hover| now >= hover.deadline)
            .map(|hover| hover.target.clone())
        else {
            return refreshed;
        };

        match target {
            PaneDragHoverTarget::Workspace(workspace_id) => {
                if let Some(ws_idx) = self.parse_workspace_id(&workspace_id) {
                    let _ = self.runtime_workspace_focus(
                        "tui.mouse.pane_drag.workspace_hover",
                        workspace_id,
                    );
                    if let Some(tab_id) = self.public_tab_id(ws_idx, 0) {
                        let _ =
                            self.runtime_tab_focus("tui.mouse.pane_drag.first_tab_hover", tab_id);
                    }
                }
                if let Some(drag) = self.pane_drag.as_mut() {
                    drag.hover = None;
                    drag.drop_action = None;
                }
            }
            PaneDragHoverTarget::Tab(tab_id) => {
                let _ = self.runtime_tab_focus("tui.mouse.pane_drag.tab_hover", tab_id);
                if let Some(drag) = self.pane_drag.as_mut() {
                    drag.hover = None;
                    drag.drop_action = None;
                }
            }
            PaneDragHoverTarget::WorkspaceScroll(direction) => {
                let workspace_area = self.state.workspace_list_rect();
                let max_scroll =
                    crate::ui::workspace_list_scroll_metrics(&self.state, workspace_area)
                        .max_offset_from_bottom;
                if direction < 0 {
                    self.state.workspace_scroll = self.state.workspace_scroll.saturating_sub(1);
                } else {
                    self.state.workspace_scroll = self
                        .state
                        .workspace_scroll
                        .saturating_add(1)
                        .min(max_scroll);
                }
                if let Some(drag) = self.pane_drag.as_mut() {
                    if let Some(hover) = drag.hover.as_mut() {
                        hover.deadline = now + AUTOSCROLL_DELAY;
                    }
                }
            }
            PaneDragHoverTarget::TabScroll(direction) => {
                if direction < 0 {
                    self.state.tab_scroll = self.state.tab_scroll.saturating_sub(1);
                } else {
                    let last_tab_idx = self
                        .state
                        .active
                        .and_then(|ws_idx| self.state.workspaces.get(ws_idx))
                        .map(|workspace| workspace.tabs.len().saturating_sub(1))
                        .unwrap_or(0);
                    self.state.tab_scroll =
                        self.state.tab_scroll.saturating_add(1).min(last_tab_idx);
                }
                self.state.tab_scroll_follow_active = false;
                if let Some(drag) = self.pane_drag.as_mut() {
                    if let Some(hover) = drag.hover.as_mut() {
                        hover.deadline = now + AUTOSCROLL_DELAY;
                    }
                }
            }
        }
        self.state.pane_drop_preview = None;
        true
    }

    fn restore_pane_drag_origin(&mut self, origin: &PaneDragOrigin) {
        if self.parse_workspace_id(&origin.workspace_id).is_some() {
            let _ = self.runtime_workspace_focus(
                "tui.mouse.pane_drag.restore_workspace",
                origin.workspace_id.clone(),
            );
        }
        if self.parse_tab_id(&origin.tab_id).is_some() {
            let _ =
                self.runtime_tab_focus("tui.mouse.pane_drag.restore_tab", origin.tab_id.clone());
        }
        if self.parse_pane_id(&origin.pane_id).is_some() {
            let _ =
                self.runtime_pane_focus("tui.mouse.pane_drag.restore_pane", origin.pane_id.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Mode;
    use crate::config::Config;
    use crate::workspace::Workspace;
    use crossterm::event::MouseEvent;

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::empty(),
        }
    }

    fn test_app() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("one")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.mouse_capture = true;
        app.state.pane_borders = true;
        app.state.sidebar_collapsed = false;
        app.state.ensure_test_terminals();
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 30));
        app
    }

    #[test]
    fn drop_zone_has_center_and_four_edges() {
        let rect = Rect::new(10, 10, 40, 20);
        assert_eq!(pane_drop_zone(rect, 10, 19), PaneDropZone::Left);
        assert_eq!(pane_drop_zone(rect, 49, 19), PaneDropZone::Right);
        assert_eq!(pane_drop_zone(rect, 30, 10), PaneDropZone::Above);
        assert_eq!(pane_drop_zone(rect, 30, 29), PaneDropZone::Below);
        assert_eq!(pane_drop_zone(rect, 30, 20), PaneDropZone::Center);
    }

    #[test]
    fn tiny_drop_zone_is_always_an_edge() {
        let rect = Rect::new(0, 0, 3, 3);
        for row in 0..3 {
            for col in 0..3 {
                assert_ne!(pane_drop_zone(rect, col, row), PaneDropZone::Center);
            }
        }
    }

    #[test]
    fn title_drag_center_swaps_without_terminal_selection() {
        let mut app = test_app();
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let target = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        app.state.ensure_test_terminals();
        app.state.workspaces[0].tabs[0].layout.focus_pane(target);
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 30));
        let source_title = app
            .state
            .view
            .pane_title_hit_areas
            .iter()
            .find(|hit| hit.pane_id == source)
            .copied()
            .expect("source title hit area");
        let target_rect = app
            .state
            .view
            .pane_infos
            .iter()
            .find(|pane| pane.id == target)
            .map(|pane| pane.rect)
            .expect("target pane");
        let target_center = (
            target_rect.x + target_rect.width / 2,
            target_rect.y + target_rect.height / 2,
        );
        let source_before = app.state.workspaces[0].tabs[0]
            .layout
            .panes(app.state.view.terminal_area)
            .into_iter()
            .find(|pane| pane.id == source)
            .expect("source before")
            .rect;

        assert!(app.handle_pane_drag_mouse(
            41,
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                source_title.rect.x,
                source_title.rect.y,
            ),
        ));
        assert!(app.handle_pane_drag_mouse(
            41,
            mouse(
                MouseEventKind::Drag(MouseButton::Left),
                target_center.0,
                target_center.1,
            ),
        ));
        assert!(app
            .state
            .pane_drop_preview
            .as_ref()
            .is_some_and(|preview| preview.valid && preview.rects.len() == 2));
        assert!(app.state.selection.is_none());
        assert!(app.handle_pane_drag_mouse(
            41,
            mouse(
                MouseEventKind::Up(MouseButton::Left),
                target_center.0,
                target_center.1,
            ),
        ));

        let source_after = app.state.workspaces[0].tabs[0]
            .layout
            .panes(app.state.view.terminal_area)
            .into_iter()
            .find(|pane| pane.id == source)
            .expect("source after")
            .rect;
        assert_ne!(source_after, source_before);
        assert_eq!(source_after, target_rect);
        assert!(app.pane_drag.is_none());
        assert!(app.state.pane_drop_preview.is_none());
    }

    #[test]
    fn title_drag_to_target_edge_reorients_same_tab_with_exact_preview() {
        let mut app = test_app();
        let target = app.state.workspaces[0].tabs[0].root_pane;
        let source = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        app.state.ensure_test_terminals();
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 30));

        let source_title = app
            .state
            .view
            .pane_title_hit_areas
            .iter()
            .find(|hit| hit.pane_id == source)
            .copied()
            .expect("source title hit area");
        let target_rect = app
            .state
            .view
            .pane_infos
            .iter()
            .find(|pane| pane.id == target)
            .map(|pane| pane.rect)
            .expect("target pane");
        let drop_point = (
            target_rect.x + target_rect.width / 2,
            target_rect.y + target_rect.height.saturating_sub(1),
        );
        let next_auto_number = app.state.workspaces[0].tabs[0].next_auto_pane_number;

        assert!(app.handle_pane_drag_mouse(
            43,
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                source_title.rect.x,
                source_title.rect.y,
            ),
        ));
        assert!(app.handle_pane_drag_mouse(
            43,
            mouse(
                MouseEventKind::Drag(MouseButton::Left),
                drop_point.0,
                drop_point.1,
            ),
        ));
        let preview_rect = app
            .state
            .pane_drop_preview
            .as_ref()
            .filter(|preview| preview.valid)
            .and_then(|preview| preview.rects.first())
            .copied()
            .expect("valid projected placement");
        assert!(app.handle_pane_drag_mouse(
            43,
            mouse(
                MouseEventKind::Up(MouseButton::Left),
                drop_point.0,
                drop_point.1,
            ),
        ));

        let panes = app.state.workspaces[0].tabs[0]
            .layout
            .panes(app.state.view.terminal_area);
        let source_rect = panes
            .iter()
            .find(|pane| pane.id == source)
            .expect("moved pane")
            .rect;
        let target_rect = panes
            .iter()
            .find(|pane| pane.id == target)
            .expect("target pane")
            .rect;
        assert_eq!(source_rect, preview_rect);
        assert_eq!(source_rect.x, target_rect.x);
        assert_eq!(source_rect.width, target_rect.width);
        assert_eq!(source_rect.y, target_rect.y + target_rect.height);
        assert_eq!(
            app.state.workspaces[0].tabs[0].next_auto_pane_number,
            next_auto_number
        );
        app.state.assert_invariants_for_test();
    }

    #[test]
    fn title_drag_hovers_to_another_tab_then_moves_the_pane() {
        let mut app = test_app();
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let target_tab = app.state.workspaces[0].test_add_tab(Some("target"));
        let target = app.state.workspaces[0].tabs[target_tab].root_pane;
        app.state.workspaces[0].switch_tab(0);
        app.state.ensure_test_terminals();
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 30));

        let source_title = app
            .state
            .view
            .pane_title_hit_areas
            .iter()
            .find(|hit| hit.pane_id == source)
            .copied()
            .expect("source title hit area");
        let target_tab_rect = app.state.view.tab_hit_areas[target_tab];
        let tab_hover_point = (
            target_tab_rect.x + target_tab_rect.width / 2,
            target_tab_rect.y,
        );

        assert!(app.handle_pane_drag_mouse(
            44,
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                source_title.rect.x,
                source_title.rect.y,
            ),
        ));
        assert!(app.handle_pane_drag_mouse(
            44,
            mouse(
                MouseEventKind::Drag(MouseButton::Left),
                tab_hover_point.0,
                tab_hover_point.1,
            ),
        ));
        let deadline = app.pane_drag_deadline().expect("tab hover deadline");
        assert!(!app.tick_pane_drag(
            deadline
                .checked_sub(Duration::from_millis(1))
                .expect("deadline should allow subtraction")
        ));
        assert!(app.tick_pane_drag(deadline));
        assert_eq!(app.state.workspaces[0].active_tab_index(), target_tab);

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 30));
        let target_rect = app
            .state
            .view
            .pane_infos
            .iter()
            .find(|pane| pane.id == target)
            .map(|pane| pane.rect)
            .expect("target pane");
        let drop_point = (target_rect.x, target_rect.y + target_rect.height / 2);
        assert!(app.handle_pane_drag_mouse(
            44,
            mouse(
                MouseEventKind::Drag(MouseButton::Left),
                drop_point.0,
                drop_point.1,
            ),
        ));
        assert!(app
            .state
            .pane_drop_preview
            .as_ref()
            .is_some_and(|preview| preview.valid));
        assert!(app.handle_pane_drag_mouse(
            44,
            mouse(
                MouseEventKind::Up(MouseButton::Left),
                drop_point.0,
                drop_point.1,
            ),
        ));

        assert_eq!(app.state.workspaces[0].tabs.len(), 1);
        let tab = &app.state.workspaces[0].tabs[0];
        assert_eq!(tab.layout.pane_count(), 2);
        assert_eq!(tab.auto_pane_label(target).as_deref(), Some("pane 1"));
        assert_eq!(tab.auto_pane_label(source).as_deref(), Some("pane 2"));
        assert_eq!(tab.next_auto_pane_number, 3);
        app.state.assert_invariants_for_test();
    }

    #[test]
    fn title_drag_autoscrolls_from_first_to_last_workspace_then_moves() {
        let mut app = test_app();
        for number in 2..=12 {
            app.state
                .workspaces
                .push(Workspace::test_new(&format!("workspace {number}")));
        }
        let last_workspace_idx = app.state.workspaces.len() - 1;
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let target = app.state.workspaces[last_workspace_idx].tabs[0].root_pane;
        app.state.ensure_test_terminals();
        let screen = Rect::new(0, 0, 120, 18);
        crate::ui::compute_view(&mut app.state, screen);
        assert!(
            app.state
                .view
                .workspace_card_areas
                .iter()
                .all(|card| card.ws_idx != last_workspace_idx),
            "last workspace should initially require scrolling"
        );

        let source_title = app
            .state
            .view
            .pane_title_hit_areas
            .iter()
            .find(|hit| hit.pane_id == source)
            .copied()
            .expect("source title hit area");
        let bottom_card = app
            .state
            .view
            .workspace_card_areas
            .last()
            .copied()
            .expect("visible workspace card");
        let scroll_point = (
            bottom_card.rect.x,
            bottom_card
                .rect
                .y
                .saturating_add(bottom_card.rect.height)
                .saturating_sub(1),
        );

        assert!(app.handle_pane_drag_mouse(
            45,
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                source_title.rect.x,
                source_title.rect.y,
            ),
        ));
        assert!(app.handle_pane_drag_mouse(
            45,
            mouse(
                MouseEventKind::Drag(MouseButton::Left),
                scroll_point.0,
                scroll_point.1,
            ),
        ));

        for _ in 0..app.state.workspaces.len().saturating_mul(3) {
            if app.state.active == Some(last_workspace_idx) {
                break;
            }
            let deadline = app
                .pane_drag_deadline()
                .expect("autoscroll or workspace hover deadline");
            assert!(app.tick_pane_drag(deadline));
            crate::ui::compute_view(&mut app.state, screen);
        }
        assert_eq!(
            app.state.active,
            Some(last_workspace_idx),
            "stationary pointer should leave autoscroll and hover the final workspace"
        );

        let target_rect = app
            .state
            .view
            .pane_infos
            .iter()
            .find(|pane| pane.id == target)
            .map(|pane| pane.rect)
            .expect("target pane");
        let drop_point = (target_rect.x, target_rect.y + target_rect.height / 2);
        assert!(app.handle_pane_drag_mouse(
            45,
            mouse(
                MouseEventKind::Drag(MouseButton::Left),
                drop_point.0,
                drop_point.1,
            ),
        ));
        assert!(app
            .state
            .pane_drop_preview
            .as_ref()
            .is_some_and(|preview| preview.valid));
        assert!(app.handle_pane_drag_mouse(
            45,
            mouse(
                MouseEventKind::Up(MouseButton::Left),
                drop_point.0,
                drop_point.1,
            ),
        ));

        let destination_tab = app
            .state
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.tabs)
            .find(|tab| tab.panes.contains_key(&target))
            .expect("destination tab");
        assert!(destination_tab.panes.contains_key(&source));
        assert_eq!(destination_tab.layout.pane_count(), 2);
        app.state.assert_invariants_for_test();
    }

    #[test]
    fn only_owner_can_release_drag_and_disconnect_restores_hover_origin() {
        let mut app = test_app();
        app.state.workspaces.push(Workspace::test_new("two"));
        app.state.ensure_test_terminals();
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 30));
        let title = app.state.view.pane_title_hit_areas[0];
        let target_card = app
            .state
            .view
            .workspace_card_areas
            .iter()
            .find(|card| card.ws_idx == 1)
            .copied()
            .expect("second workspace card");

        assert!(app.handle_pane_drag_mouse(
            42,
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                title.rect.x,
                title.rect.y,
            ),
        ));
        assert!(app.handle_pane_drag_mouse(
            42,
            mouse(
                MouseEventKind::Drag(MouseButton::Left),
                target_card.rect.x,
                target_card.rect.y,
            ),
        ));
        assert!(app.handle_pane_drag_mouse(
            99,
            mouse(
                MouseEventKind::Up(MouseButton::Left),
                target_card.rect.x,
                target_card.rect.y,
            ),
        ));
        assert!(app.pane_drag.is_some());

        let deadline = app.pane_drag_deadline().expect("workspace hover deadline");
        assert!(app.tick_pane_drag(deadline));
        assert_eq!(app.state.active, Some(1));

        app.release_input_source_headless(42);
        assert!(app.pane_drag.is_none());
        assert_eq!(app.state.active, Some(0));
    }
}
