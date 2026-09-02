use super::*;
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

const SELECTION_AUTOSCROLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(30);

impl ClientShellState {
    fn set_sidebar_width_from_column(&mut self, column: u16, outcome: &mut ClientShellInput) {
        let (min, max) = crate::config::validated_sidebar_bounds(
            self.config.sidebar_min_width,
            self.config.sidebar_max_width,
        )
        .unwrap_or((18, 36));
        let width = column.saturating_add(1).clamp(min, max);
        if self.sidebar_width != width {
            self.sidebar_width = width;
            self.sidebar_width_manual = true;
            self.invalidate_pane_surface();
            outcome.repaint = true;
            outcome.resize = true;
        }
    }

    fn set_sidebar_section_from_row(&mut self, row: u16, outcome: &mut ClientShellInput) {
        let divider = self.hits.sidebar_divider;
        if divider.height == 0 {
            return;
        }
        let ratio = row.saturating_sub(divider.y) as f32 / divider.height as f32;
        let ratio = ratio.clamp(0.1, 0.9);
        if (self.sidebar_section_split - ratio).abs() > f32::EPSILON {
            self.sidebar_section_split = ratio;
            self.sidebar_section_split_manual = true;
            outcome.repaint = true;
        }
    }

    fn pane_scrollbar_offset(
        hit: &PaneHit,
        row: u16,
        grab_row_offset: Option<u16>,
    ) -> Option<usize> {
        let track = hit.scrollbar_rect?;
        let metrics = hit.scroll?;
        (metrics.max_offset_from_bottom > 0).then(|| match grab_row_offset {
            Some(grab_row_offset) => {
                crate::ui::scrollbar_offset_from_drag_row(metrics, track, row, grab_row_offset)
            }
            None => crate::ui::scrollbar_offset_from_row(metrics, track, row),
        })
    }

    pub(super) fn push_pane_scroll_offset(
        &mut self,
        pane_id: String,
        offset_from_bottom: usize,
        outcome: &mut ClientShellInput,
    ) {
        self.pane_scroll_targets
            .insert(pane_id.clone(), offset_from_bottom);
        if self.pane_scroll_in_flight.contains_key(&pane_id) {
            self.pane_scroll_queued.insert(pane_id, offset_from_bottom);
            return;
        }
        self.dispatch_pane_scroll_offset(pane_id, offset_from_bottom, outcome);
    }

    fn dispatch_pane_scroll_offset(
        &mut self,
        pane_id: String,
        offset_from_bottom: usize,
        outcome: &mut ClientShellInput,
    ) {
        if self.snapshot.is_none() {
            return;
        }
        self.next_scroll_serial = self.next_scroll_serial.saturating_add(1);
        let serial = self.next_scroll_serial;
        self.pane_scroll_targets
            .insert(pane_id.clone(), offset_from_bottom);
        self.pane_scroll_in_flight.insert(pane_id.clone(), serial);
        if !self.push_endpoint_method_with_kind(
            crate::api::schema::Method::PaneScroll(crate::api::schema::PaneScrollParams {
                pane_id: pane_id.clone(),
                offset_from_bottom: offset_from_bottom as u64,
            }),
            PendingEndpointKind::PaneScroll {
                pane_id: pane_id.clone(),
                serial,
            },
            outcome,
        ) {
            self.pane_scroll_targets.remove(&pane_id);
            self.pane_scroll_in_flight.remove(&pane_id);
        }
    }

    pub(super) fn complete_pane_scroll(
        &mut self,
        pane_id: String,
        serial: u64,
        result: Result<crate::api::schema::ResponseResult, ClientShellEndpointError>,
        outcome: &mut ClientShellInput,
    ) -> bool {
        if self.pane_scroll_in_flight.get(&pane_id).copied() != Some(serial) {
            return false;
        }
        self.pane_scroll_in_flight.remove(&pane_id);
        let repaint = match result {
            Ok(crate::api::schema::ResponseResult::PaneInfo { pane })
                if pane.pane_id == pane_id =>
            {
                if let Some(scroll) = pane.scroll {
                    if self.pane_scroll_targets.contains_key(&pane_id) {
                        self.pane_scroll_targets.insert(
                            pane_id.clone(),
                            usize::try_from(scroll.offset_from_bottom).unwrap_or(usize::MAX),
                        );
                    }
                }
                false
            }
            Ok(_) => {
                self.pane_scroll_queued.remove(&pane_id);
                self.pane_scroll_targets.remove(&pane_id);
                self.endpoint_error =
                    Some("endpoint returned an unexpected pane-scroll result".to_owned());
                true
            }
            Err(_) => {
                self.pane_scroll_queued.remove(&pane_id);
                self.pane_scroll_targets.remove(&pane_id);
                true
            }
        };
        if let Some(offset) = self.pane_scroll_queued.remove(&pane_id) {
            self.dispatch_pane_scroll_offset(pane_id, offset, outcome);
        }
        repaint
    }

    pub(super) fn stop_selection_autoscroll(&mut self) {
        self.selection_autoscroll = None;
        self.selection_autoscroll_deadline = None;
    }

    fn selection_edge_scroll_lines(distance: u16) -> usize {
        usize::from(distance).saturating_mul(3).clamp(3, 15)
    }

    fn selection_scroll_metrics(&self, hit: &PaneHit) -> Option<crate::pane::ScrollMetrics> {
        let metrics = hit.scroll?;
        Some(
            self.selection_autoscroll
                .as_ref()
                .filter(|autoscroll| autoscroll.pane_id == hit.pane_id)
                .map_or(metrics, |autoscroll| crate::pane::ScrollMetrics {
                    offset_from_bottom: autoscroll.offset_from_bottom,
                    max_offset_from_bottom: autoscroll.max_offset_from_bottom,
                    viewport_rows: metrics.viewport_rows,
                }),
        )
    }

    fn update_selection_cursor_with_metrics(
        &mut self,
        hit: &PaneHit,
        column: u16,
        row: u16,
        metrics: Option<crate::pane::ScrollMetrics>,
    ) {
        if let Some(selection) = self.selection.as_mut() {
            selection.drag(column, row, hit.inner_rect, metrics);
        }
    }

    fn update_selection_drag(
        &mut self,
        hit: &PaneHit,
        column: u16,
        row: u16,
        outcome: &mut ClientShellInput,
    ) {
        let metrics = self.selection_scroll_metrics(hit);
        let was_dragging = self
            .selection
            .as_ref()
            .is_some_and(crate::selection::Selection::is_dragging);
        let moved_from_anchor = self.selection.as_ref().is_some_and(|selection| {
            let (anchor_row, anchor_col) = selection.anchor_screen_pos(hit.inner_rect, metrics);
            anchor_row != row || anchor_col != column
        });
        let is_dragging = was_dragging || moved_from_anchor;
        self.update_selection_cursor_with_metrics(hit, column, row, metrics);
        if is_dragging {
            if let Some(selection) = self.selection.as_mut() {
                if selection.is_just_click() {
                    selection.force_dragging();
                }
            }
            self.last_pane_click = None;
        }
        if !is_dragging {
            self.stop_selection_autoscroll();
            return;
        }

        let Some(metrics) = metrics else {
            self.stop_selection_autoscroll();
            return;
        };
        let top = hit.inner_rect.y;
        let bottom = hit.inner_rect.y + hit.inner_rect.height.saturating_sub(1);
        let (direction, immediate_lines) = if row < top {
            (
                ClientSelectionAutoscrollDirection::Up,
                Self::selection_edge_scroll_lines(top - row),
            )
        } else if row > bottom {
            (
                ClientSelectionAutoscrollDirection::Down,
                Self::selection_edge_scroll_lines(row - bottom),
            )
        } else if row == top {
            (ClientSelectionAutoscrollDirection::Up, 0)
        } else if row == bottom {
            (ClientSelectionAutoscrollDirection::Down, 0)
        } else {
            self.stop_selection_autoscroll();
            return;
        };

        let offset_from_bottom = match direction {
            ClientSelectionAutoscrollDirection::Up => metrics
                .offset_from_bottom
                .saturating_add(immediate_lines)
                .min(metrics.max_offset_from_bottom),
            ClientSelectionAutoscrollDirection::Down => {
                metrics.offset_from_bottom.saturating_sub(immediate_lines)
            }
        };
        if offset_from_bottom != metrics.offset_from_bottom {
            let projected = crate::pane::ScrollMetrics {
                offset_from_bottom,
                ..metrics
            };
            self.update_selection_cursor_with_metrics(hit, column, row, Some(projected));
            self.push_pane_scroll_offset(hit.pane_id.clone(), offset_from_bottom, outcome);
        }
        self.selection_autoscroll = Some(ClientSelectionAutoscroll {
            pane_id: hit.pane_id.clone(),
            direction,
            last_mouse_column: column,
            last_mouse_row: row,
            inner_rect: hit.inner_rect,
            offset_from_bottom,
            max_offset_from_bottom: metrics.max_offset_from_bottom,
        });
        self.selection_autoscroll_deadline =
            Some(std::time::Instant::now() + SELECTION_AUTOSCROLL_INTERVAL);
    }

    fn scroll_in_progress_selection(
        &mut self,
        mouse: MouseEvent,
        outcome: &mut ClientShellInput,
    ) -> bool {
        if !matches!(
            mouse.kind,
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
        ) || !self
            .selection
            .as_ref()
            .is_some_and(crate::selection::Selection::is_in_progress)
        {
            return false;
        }
        let Some(hit) = self.selection.as_ref().and_then(|selection| {
            self.hits
                .panes
                .iter()
                .find(|hit| hit.pane_id == selection.pane_id)
                .cloned()
        }) else {
            return false;
        };
        let Some(metrics) = self.selection_scroll_metrics(&hit) else {
            return false;
        };
        let offset_from_bottom = match mouse.kind {
            MouseEventKind::ScrollUp => metrics
                .offset_from_bottom
                .saturating_add(self.config.mouse_scroll_lines)
                .min(metrics.max_offset_from_bottom),
            MouseEventKind::ScrollDown => metrics
                .offset_from_bottom
                .saturating_sub(self.config.mouse_scroll_lines),
            _ => unreachable!(),
        };
        if offset_from_bottom != metrics.offset_from_bottom {
            let projected = crate::pane::ScrollMetrics {
                offset_from_bottom,
                ..metrics
            };
            self.update_selection_cursor_with_metrics(
                &hit,
                mouse.column,
                mouse.row,
                Some(projected),
            );
            self.push_pane_scroll_offset(hit.pane_id, offset_from_bottom, outcome);
            outcome.repaint = true;
        }
        true
    }

    pub(crate) fn tick_selection_autoscroll(
        &mut self,
        now: std::time::Instant,
    ) -> ClientShellInput {
        let mut outcome = ClientShellInput::default();
        if self
            .selection_autoscroll_deadline
            .is_none_or(|deadline| now < deadline)
        {
            return outcome;
        }
        let Some(mut autoscroll) = self.selection_autoscroll.clone() else {
            self.selection_autoscroll_deadline = None;
            return outcome;
        };
        if !self.selection.as_ref().is_some_and(|selection| {
            selection.pane_id == autoscroll.pane_id && selection.is_dragging()
        }) {
            self.stop_selection_autoscroll();
            return outcome;
        }
        let Some(hit) = self
            .hits
            .panes
            .iter()
            .find(|hit| hit.pane_id == autoscroll.pane_id)
            .cloned()
        else {
            self.stop_selection_autoscroll();
            return outcome;
        };
        if hit.inner_rect != autoscroll.inner_rect {
            self.stop_selection_autoscroll();
            return outcome;
        }
        let next_offset = match autoscroll.direction {
            ClientSelectionAutoscrollDirection::Up => autoscroll
                .offset_from_bottom
                .saturating_add(1)
                .min(autoscroll.max_offset_from_bottom),
            ClientSelectionAutoscrollDirection::Down => {
                autoscroll.offset_from_bottom.saturating_sub(1)
            }
        };
        if next_offset == autoscroll.offset_from_bottom {
            self.stop_selection_autoscroll();
            return outcome;
        }
        autoscroll.offset_from_bottom = next_offset;
        let metrics = crate::pane::ScrollMetrics {
            offset_from_bottom: next_offset,
            max_offset_from_bottom: autoscroll.max_offset_from_bottom,
            viewport_rows: hit.scroll.map_or(0, |metrics| metrics.viewport_rows),
        };
        self.update_selection_cursor_with_metrics(
            &hit,
            autoscroll.last_mouse_column,
            autoscroll.last_mouse_row,
            Some(metrics),
        );
        self.push_pane_scroll_offset(autoscroll.pane_id.clone(), next_offset, &mut outcome);
        self.selection_autoscroll = Some(autoscroll);
        self.selection_autoscroll_deadline = Some(now + SELECTION_AUTOSCROLL_INTERVAL);
        outcome.repaint = true;
        outcome
    }

    fn pane_split_target_is_current(&self, hit: &PaneSplitHit, tab_id: &str) -> Option<bool> {
        let snapshot = self.snapshot.as_deref()?;
        let surface = self.pane_surface.as_ref()?;
        if snapshot.revision != surface.projection_revision {
            return None;
        }
        Some(
            snapshot.focused_tab_id.as_deref() == Some(tab_id)
                && pane_surface_topology_signature(surface) == hit.topology_signature,
        )
    }

    fn pane_split_ratio(hit: &PaneSplitHit, grab_offset: i32, point: (u16, u16)) -> f32 {
        let (pointer, origin, length) = match hit.direction {
            crate::protocol::PaneSurfaceSplitDirection::Horizontal => {
                (i32::from(point.0), i32::from(hit.area.x), hit.area.width)
            }
            crate::protocol::PaneSurfaceSplitDirection::Vertical => {
                (i32::from(point.1), i32::from(hit.area.y), hit.area.height)
            }
        };
        ((pointer + grab_offset - origin) as f32 / f32::from(length.max(1))).clamp(0.1, 0.9)
    }

    fn tab_drop_index_at(&self, point: (u16, u16)) -> Option<usize> {
        let snapshot = self.snapshot.as_deref()?;
        let workspace_id = snapshot.focused_workspace_id.as_deref()?;
        let tabs = snapshot
            .tabs
            .iter()
            .filter(|tab| tab.workspace_id == workspace_id)
            .collect::<Vec<_>>();
        let visible = self
            .hits
            .tabs
            .iter()
            .filter_map(|(rect, tab_id)| {
                tabs.iter()
                    .position(|tab| tab.tab_id == *tab_id)
                    .map(|index| (index, *rect))
            })
            .collect::<Vec<_>>();
        let (first_index, first_rect) = *visible.first()?;
        let (last_index, last_rect) = *visible.last()?;
        let on_tab_row = point.1 == first_rect.y;
        if !on_tab_row {
            return None;
        }
        if super::contains(self.hits.tab_scroll_left, point) {
            return Some(0);
        }
        if super::contains(self.hits.tab_scroll_right, point) {
            return Some(tabs.len());
        }
        let left_edge = if first_index == 0 {
            first_rect.x
        } else {
            self.hits.tab_scroll_left.right()
        };
        let right_edge = if last_index + 1 >= tabs.len() {
            last_rect.right()
        } else {
            self.hits.tab_scroll_right.x.saturating_sub(1)
        };
        if point.0 <= left_edge {
            return Some(first_index);
        }
        if point.0 >= right_edge {
            return Some(last_index + 1);
        }
        for (index, rect) in visible {
            let midpoint = rect.x + rect.width / 2;
            if point.0 < midpoint {
                return Some(index);
            }
            if point.0 < rect.right() {
                return Some(index + 1);
            }
        }
        Some(last_index + 1)
    }

    fn workspace_drop_target_at(&self, point: (u16, u16)) -> Option<(Option<String>, u16)> {
        if self.hits.workspace_body.height == 0
            || point.1 < self.hits.workspace_body.y.saturating_sub(1)
            || point.1 >= self.hits.new_workspace.y
        {
            return None;
        }
        let mut slots = self
            .hits
            .workspaces
            .iter()
            .filter(|hit| !hit.indented)
            .map(|hit| (Some(hit.workspace_id.clone()), hit.rect.y.saturating_sub(1)))
            .collect::<Vec<_>>();
        let snapshot = self.snapshot.as_deref()?;
        let entries = render::workspace_entries(snapshot, &self.collapsed_groups);
        let last_hit = self.hits.workspaces.last()?;
        let last_position = entries.iter().position(|entry| {
            snapshot
                .workspaces
                .get(entry.index)
                .is_some_and(|workspace| workspace.workspace_id == last_hit.workspace_id)
        })?;
        let next = entries.get(last_position + 1);
        if !next.is_some_and(|entry| entry.indented) {
            let before = next.and_then(|entry| {
                snapshot
                    .workspaces
                    .get(entry.index)
                    .map(|workspace| workspace.workspace_id.clone())
            });
            let row = last_hit.rect.bottom();
            if row < self.hits.new_workspace.y {
                slots.push((before, row));
            }
        }
        slots
            .into_iter()
            .enumerate()
            .min_by_key(|(index, (_, row))| (point.1.abs_diff(*row), *index))
            .map(|(_, target)| target)
    }

    fn workspace_move_method(
        &self,
        source_workspace_id: &str,
        before_workspace_id: Option<&str>,
    ) -> Option<crate::api::schema::Method> {
        let snapshot = self.snapshot.as_deref()?;
        let source = snapshot
            .workspaces
            .iter()
            .find(|workspace| workspace.workspace_id == source_workspace_id)?;
        if source
            .worktree
            .as_ref()
            .is_some_and(|worktree| worktree.is_linked_worktree)
        {
            return None;
        }
        if before_workspace_id == Some(source_workspace_id) {
            return None;
        }
        let roots = snapshot
            .workspaces
            .iter()
            .filter(|workspace| {
                !workspace
                    .worktree
                    .as_ref()
                    .is_some_and(|worktree| worktree.is_linked_worktree)
            })
            .collect::<Vec<_>>();
        let source_position = roots
            .iter()
            .position(|workspace| workspace.workspace_id == source_workspace_id)?;
        let remaining = roots
            .iter()
            .copied()
            .filter(|workspace| workspace.workspace_id != source_workspace_id)
            .collect::<Vec<_>>();
        let insert_position = match before_workspace_id {
            Some(target) => remaining
                .iter()
                .position(|workspace| workspace.workspace_id == target)?,
            None => remaining.len(),
        };
        if insert_position == source_position {
            return None;
        }

        if let Some(worktree) = source.worktree.as_ref() {
            let workspace_ids = std::iter::once(source.workspace_id.clone())
                .chain(
                    snapshot
                        .workspaces
                        .iter()
                        .filter(|workspace| workspace.workspace_id != source.workspace_id)
                        .filter(|workspace| {
                            workspace
                                .worktree
                                .as_ref()
                                .is_some_and(|candidate| candidate.key == worktree.key)
                        })
                        .map(|workspace| workspace.workspace_id.clone()),
                )
                .collect();
            Some(crate::api::schema::Method::WorkspaceMoveBlock(
                crate::api::schema::WorkspaceMoveBlockParams {
                    workspace_ids,
                    before_workspace_id: before_workspace_id.map(str::to_owned),
                },
            ))
        } else {
            let insert_index = before_workspace_id
                .and_then(|target| {
                    snapshot
                        .workspaces
                        .iter()
                        .position(|workspace| workspace.workspace_id == target)
                })
                .unwrap_or(snapshot.workspaces.len());
            Some(crate::api::schema::Method::WorkspaceMove(
                crate::api::schema::WorkspaceMoveParams {
                    workspace_id: source.workspace_id.clone(),
                    insert_index,
                },
            ))
        }
    }

    pub(super) fn handle_mouse(&mut self, mouse: MouseEvent, outcome: &mut ClientShellInput) {
        let point = (mouse.column, mouse.row);
        if matches!(self.overlay, Some(ClientShellOverlay::Onboarding)) {
            if mouse.kind == MouseEventKind::Down(MouseButton::Left)
                && super::contains(self.hits.overlay_primary, point)
            {
                self.complete_onboarding(outcome);
            }
            return;
        }
        if matches!(
            self.overlay,
            Some(ClientShellOverlay::ProductAnnouncement(_))
        ) {
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left)
                    if super::contains(self.hits.overlay_primary, point) =>
                {
                    self.dismiss_product_announcement(outcome);
                }
                MouseEventKind::Down(MouseButton::Left)
                    if super::contains(self.hits.product_announcement_scrollbar, point) =>
                {
                    if let Some(metrics) = self.hits.product_announcement_scroll_metrics {
                        if let Some(grab_row_offset) = crate::ui::scrollbar_thumb_grab_offset(
                            metrics,
                            self.hits.product_announcement_scrollbar,
                            mouse.row,
                        ) {
                            self.chrome_drag =
                                Some(ClientChromeDrag::ProductAnnouncementScrollbar {
                                    grab_row_offset,
                                });
                        } else {
                            let offset = crate::ui::scrollbar_offset_from_row(
                                metrics,
                                self.hits.product_announcement_scrollbar,
                                mouse.row,
                            );
                            self.set_product_announcement_offset_from_bottom(offset);
                            outcome.repaint = true;
                        }
                    }
                }
                MouseEventKind::Drag(MouseButton::Left) => {
                    if let (
                        Some(ClientChromeDrag::ProductAnnouncementScrollbar { grab_row_offset }),
                        Some(metrics),
                    ) = (
                        self.chrome_drag.as_ref(),
                        self.hits.product_announcement_scroll_metrics,
                    ) {
                        let offset = crate::ui::scrollbar_offset_from_drag_row(
                            metrics,
                            self.hits.product_announcement_scrollbar,
                            mouse.row,
                            *grab_row_offset,
                        );
                        self.set_product_announcement_offset_from_bottom(offset);
                        outcome.repaint = true;
                    }
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    self.chrome_drag = None;
                }
                MouseEventKind::ScrollUp => {
                    self.scroll_product_announcement(-3);
                    outcome.repaint = true;
                }
                MouseEventKind::ScrollDown => {
                    self.scroll_product_announcement(3);
                    outcome.repaint = true;
                }
                _ => {}
            }
            return;
        }
        if matches!(self.overlay, Some(ClientShellOverlay::ReleaseNotes(_))) {
            let (close, track, metrics) = self
                .current_release_notes_input_geometry()
                .map(|(close, track, metrics)| (close, track, Some(metrics)))
                .unwrap_or((
                    self.hits.overlay_primary,
                    (!self.hits.release_notes_scrollbar.is_empty())
                        .then_some(self.hits.release_notes_scrollbar),
                    self.hits.release_notes_scroll_metrics,
                ));
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) if super::contains(close, point) => {
                    self.dismiss_release_notes(outcome);
                }
                MouseEventKind::Down(MouseButton::Left)
                    if track.is_some_and(|track| super::contains(track, point)) =>
                {
                    if let (Some(track), Some(metrics)) = (track, metrics) {
                        if let Some(grab_row_offset) =
                            crate::ui::scrollbar_thumb_grab_offset(metrics, track, mouse.row)
                        {
                            self.chrome_drag =
                                Some(ClientChromeDrag::ReleaseNotesScrollbar { grab_row_offset });
                        } else {
                            let offset =
                                crate::ui::scrollbar_offset_from_row(metrics, track, mouse.row);
                            self.set_release_notes_offset_from_bottom(offset);
                            outcome.repaint = true;
                        }
                    }
                }
                MouseEventKind::Drag(MouseButton::Left) => {
                    if let (
                        Some(ClientChromeDrag::ReleaseNotesScrollbar { grab_row_offset }),
                        Some(track),
                        Some(metrics),
                    ) = (self.chrome_drag.as_ref(), track, metrics)
                    {
                        let offset = crate::ui::scrollbar_offset_from_drag_row(
                            metrics,
                            track,
                            mouse.row,
                            *grab_row_offset,
                        );
                        self.set_release_notes_offset_from_bottom(offset);
                        outcome.repaint = true;
                    }
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    self.chrome_drag = None;
                }
                MouseEventKind::ScrollUp => {
                    self.scroll_release_notes(-3);
                    outcome.repaint = true;
                }
                MouseEventKind::ScrollDown => {
                    self.scroll_release_notes(3);
                    outcome.repaint = true;
                }
                _ => {}
            }
            return;
        }
        if self.url_click_consumes_until_up {
            match mouse.kind {
                MouseEventKind::Drag(MouseButton::Left) => return,
                MouseEventKind::Up(MouseButton::Left) => {
                    self.url_click_consumes_until_up = false;
                    return;
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    self.url_click_consumes_until_up = false;
                }
                _ => {}
            }
        }
        if !self.replaying_url_click
            && matches!(
                mouse.kind,
                MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left)
            )
        {
            if let Some(fallback_events) =
                self.pending_requests
                    .values_mut()
                    .find_map(|pending| match &mut pending.kind {
                        PendingEndpointKind::PaneLinkActivate {
                            fallback_events, ..
                        } if !fallback_events
                            .iter()
                            .any(|event| event.kind == MouseEventKind::Up(MouseButton::Left)) =>
                        {
                            Some(fallback_events)
                        }
                        _ => None,
                    })
            {
                fallback_events.push(mouse);
                return;
            }
        }
        if let Some(gesture) = self.pane_mouse_gesture.as_ref() {
            let gesture_event = matches!(
                mouse.kind,
                MouseEventKind::Drag(button) | MouseEventKind::Up(button)
                    if button == gesture.button
            );
            if gesture_event {
                let button = gesture.button;
                let modifiers = mouse.modifiers.difference(gesture.stripped_modifiers);
                let hit = if gesture.hit.popup {
                    self.hits
                        .popup
                        .as_ref()
                        .filter(|hit| hit.pane_id == gesture.hit.pane_id)
                        .cloned()
                } else {
                    self.hits
                        .panes
                        .iter()
                        .find(|hit| hit.pane_id == gesture.hit.pane_id)
                        .cloned()
                }
                .unwrap_or_else(|| gesture.hit.clone());
                let position = self.pane_mouse_position(&hit, mouse);
                if let Some(gesture) = self.pane_mouse_gesture.as_mut() {
                    gesture.last_event = mouse;
                    gesture.last_position = position;
                }
                self.push_pane_mouse_event(&hit, mouse, modifiers, outcome);
                if mouse.kind == MouseEventKind::Up(button) {
                    self.pane_mouse_gesture = None;
                }
                return;
            }
            if matches!(
                mouse.kind,
                MouseEventKind::Down(_) | MouseEventKind::Drag(_) | MouseEventKind::Up(_)
            ) {
                return;
            }
        }
        if self.popup_pending {
            return;
        }
        if let Some(hit) = self.hits.popup.clone() {
            if super::contains(hit.inner_rect, point) {
                match mouse.kind {
                    MouseEventKind::Down(button) => {
                        self.push_pane_mouse_event(&hit, mouse, mouse.modifiers, outcome);
                        if hit.mouse_reporting {
                            self.pane_mouse_gesture = Some(ClientPaneMouseGesture {
                                last_position: self.pane_mouse_position(&hit, mouse),
                                hit,
                                button,
                                stripped_modifiers: crossterm::event::KeyModifiers::empty(),
                                last_event: mouse,
                            });
                        }
                    }
                    MouseEventKind::Moved if hit.mouse_reporting => {
                        self.push_pane_mouse_event(&hit, mouse, mouse.modifiers, outcome);
                    }
                    MouseEventKind::ScrollUp
                    | MouseEventKind::ScrollDown
                    | MouseEventKind::ScrollLeft
                    | MouseEventKind::ScrollRight => {
                        self.push_pane_mouse_event(&hit, mouse, mouse.modifiers, outcome);
                    }
                    MouseEventKind::Up(_) | MouseEventKind::Drag(_) | MouseEventKind::Moved => {}
                }
            }
            return;
        }
        if self.popup_terminal_id.is_some() {
            return;
        }
        if !self.replaying_url_click
            && self.overlay.is_none()
            && self.mode == ClientShellMode::Terminal
            && mouse.kind == MouseEventKind::Down(MouseButton::Left)
            && mouse
                .modifiers
                .contains(crossterm::event::KeyModifiers::CONTROL)
        {
            if let Some(hit) = self
                .hits
                .panes
                .iter()
                .find(|hit| super::contains(hit.inner_rect, point))
                .cloned()
            {
                let viewport_row = mouse.row.saturating_sub(hit.inner_rect.y);
                let col = mouse.column.saturating_sub(hit.inner_rect.x);
                let content_revision = self
                    .pane_surface
                    .as_ref()
                    .and_then(|surface| {
                        surface
                            .panes
                            .iter()
                            .find(|pane| pane.pane_id == hit.pane_id)
                    })
                    .map(|pane| pane.content_revision);
                self.last_pane_click = None;
                let pane_id = hit.pane_id.clone();
                self.push_endpoint_method_with_kind(
                    crate::api::schema::Method::PaneLinkActivate(
                        crate::api::schema::PaneLinkActivateParams {
                            pane_id: pane_id.clone(),
                            viewport_row,
                            col,
                            content_revision,
                            offset_from_bottom: hit
                                .scroll
                                .map(|metrics| metrics.offset_from_bottom as u64),
                        },
                    ),
                    PendingEndpointKind::PaneLinkActivate {
                        pane_id,
                        inner_rect: hit.inner_rect,
                        fallback_events: vec![mouse],
                    },
                    outcome,
                );
                return;
            }
        }
        if self.visible_endpoint_notice.is_some()
            && mouse.kind == MouseEventKind::Down(MouseButton::Left)
            && super::contains(self.hits.notification_toast, point)
        {
            self.visible_endpoint_notice = None;
            outcome.repaint = true;
            return;
        }
        if self.overlay.is_none()
            && self.mode == ClientShellMode::Terminal
            && self
                .visible_notification
                .as_ref()
                .is_some_and(|notification| notification.event.pane_id.is_some())
            && mouse.kind == MouseEventKind::Down(MouseButton::Left)
            && super::contains(self.hits.notification_toast, point)
        {
            self.focus_visible_notification(outcome);
            return;
        }
        if self.handle_mobile_mouse(mouse, outcome) {
            return;
        }
        if mouse.kind == MouseEventKind::Drag(MouseButton::Left) {
            match self.chrome_drag.as_ref() {
                Some(ClientChromeDrag::SidebarWidth) => {
                    self.set_sidebar_width_from_column(mouse.column, outcome);
                    return;
                }
                Some(ClientChromeDrag::SidebarSection) => {
                    self.set_sidebar_section_from_row(mouse.row, outcome);
                    return;
                }
                Some(ClientChromeDrag::WorkspaceScrollbar { grab_row_offset }) => {
                    if let Some(metrics) = self.hits.workspace_scroll_metrics {
                        let offset = crate::ui::scrollbar_offset_from_drag_row(
                            metrics,
                            self.hits.workspace_scrollbar,
                            mouse.row,
                            *grab_row_offset,
                        );
                        let next = metrics.max_offset_from_bottom.saturating_sub(offset);
                        if next != self.workspace_scroll {
                            self.workspace_scroll = next;
                            outcome.repaint = true;
                        }
                    }
                    return;
                }
                Some(ClientChromeDrag::AgentScrollbar { grab_row_offset }) => {
                    if let Some(metrics) = self.hits.agent_scroll_metrics {
                        let offset = crate::ui::scrollbar_offset_from_drag_row(
                            metrics,
                            self.hits.agent_scrollbar,
                            mouse.row,
                            *grab_row_offset,
                        );
                        let next = metrics.max_offset_from_bottom.saturating_sub(offset);
                        if next != self.agent_scroll {
                            self.agent_scroll = next;
                            outcome.repaint = true;
                        }
                    }
                    return;
                }
                Some(ClientChromeDrag::HelpScrollbar { grab_row_offset }) => {
                    if let (Some(metrics), Some(ClientShellOverlay::Help(help))) =
                        (self.hits.help_scroll_metrics, self.overlay.as_mut())
                    {
                        let offset = crate::ui::scrollbar_offset_from_drag_row(
                            metrics,
                            self.hits.help_scrollbar,
                            mouse.row,
                            *grab_row_offset,
                        );
                        let next = metrics.max_offset_from_bottom.saturating_sub(offset);
                        if next != help.scroll {
                            help.scroll = next;
                            outcome.repaint = true;
                        }
                    }
                    return;
                }
                Some(
                    ClientChromeDrag::ProductAnnouncementScrollbar { .. }
                    | ClientChromeDrag::ReleaseNotesScrollbar { .. },
                ) => {
                    self.chrome_drag = None;
                    return;
                }
                Some(ClientChromeDrag::PaneScrollbar {
                    hit,
                    grab_row_offset,
                    last_sent_offset,
                    last_sent_at,
                }) => {
                    let current_hit = self
                        .hits
                        .panes
                        .iter()
                        .find(|current| current.pane_id == hit.pane_id)
                        .cloned()
                        .unwrap_or_else(|| hit.clone());
                    let Some(offset) = Self::pane_scrollbar_offset(
                        &current_hit,
                        mouse.row,
                        Some(*grab_row_offset),
                    ) else {
                        self.chrome_drag = None;
                        return;
                    };
                    let now = std::time::Instant::now();
                    let should_send = *last_sent_offset != Some(offset)
                        && last_sent_at.is_none_or(|last| {
                            now.duration_since(last) >= std::time::Duration::from_millis(33)
                        });
                    if should_send {
                        if let Some(ClientChromeDrag::PaneScrollbar {
                            last_sent_offset,
                            last_sent_at,
                            ..
                        }) = self.chrome_drag.as_mut()
                        {
                            *last_sent_offset = Some(offset);
                            *last_sent_at = Some(now);
                        }
                        self.push_pane_scroll_offset(current_hit.pane_id, offset, outcome);
                    }
                    return;
                }
                Some(ClientChromeDrag::PaneSplit {
                    hit,
                    tab_id,
                    grab_offset,
                    last_sent_at,
                    ..
                }) => {
                    let hit = hit.clone();
                    let tab_id = tab_id.clone();
                    let grab_offset = *grab_offset;
                    match self.pane_split_target_is_current(&hit, &tab_id) {
                        Some(true) => {}
                        Some(false) => {
                            self.chrome_drag = None;
                            return;
                        }
                        None => return,
                    }
                    let ratio = Self::pane_split_ratio(&hit, grab_offset, point);
                    let now = std::time::Instant::now();
                    let should_send = last_sent_at.is_none_or(|last| {
                        now.duration_since(last) >= std::time::Duration::from_millis(33)
                    });
                    if let Some(ClientChromeDrag::PaneSplit {
                        last_sent_ratio,
                        last_sent_at,
                        ..
                    }) = self.chrome_drag.as_mut()
                    {
                        if should_send {
                            *last_sent_ratio = Some(ratio);
                            *last_sent_at = Some(now);
                        }
                    }
                    if should_send {
                        self.push_endpoint_method(
                            crate::api::schema::Method::LayoutSetSplitRatio(
                                crate::api::schema::LayoutSetSplitRatioParams {
                                    tab_id: Some(tab_id),
                                    pane_id: None,
                                    path: hit.path,
                                    ratio,
                                },
                            ),
                            outcome,
                        );
                    }
                    return;
                }
                Some(ClientChromeDrag::Tab { .. }) => {
                    let insert_index = self.tab_drop_index_at(point);
                    if let Some(ClientChromeDrag::Tab {
                        insert_index: current,
                        ..
                    }) = self.chrome_drag.as_mut()
                    {
                        *current = insert_index;
                    }
                    outcome.repaint = true;
                    return;
                }
                Some(ClientChromeDrag::Workspace { .. }) => {
                    let target = self.workspace_drop_target_at(point);
                    if let Some(ClientChromeDrag::Workspace {
                        target: current, ..
                    }) = self.chrome_drag.as_mut()
                    {
                        *current = target;
                    }
                    outcome.repaint = true;
                    return;
                }
                None => {}
            }
            if let Some(press) = self.workspace_press.as_ref() {
                let delta = mouse
                    .column
                    .abs_diff(press.start_column)
                    .max(mouse.row.abs_diff(press.start_row));
                if delta >= 1 {
                    let source_workspace_id = press.workspace_id.clone();
                    let draggable = self
                        .snapshot
                        .as_deref()
                        .and_then(|snapshot| {
                            snapshot
                                .workspaces
                                .iter()
                                .find(|workspace| workspace.workspace_id == source_workspace_id)
                        })
                        .is_some_and(|workspace| {
                            !workspace
                                .worktree
                                .as_ref()
                                .is_some_and(|worktree| worktree.is_linked_worktree)
                        });
                    if draggable {
                        if let Some(target) = self.workspace_drop_target_at(point) {
                            self.chrome_drag = Some(ClientChromeDrag::Workspace {
                                source_workspace_id,
                                target: Some(target),
                            });
                            outcome.repaint = true;
                        }
                    }
                }
                return;
            }
            if let Some(press) = self.tab_press.as_ref() {
                let delta = mouse
                    .column
                    .abs_diff(press.start_column)
                    .max(mouse.row.abs_diff(press.start_row));
                if delta >= 1 {
                    if let Some(insert_index) = self.tab_drop_index_at(point) {
                        self.chrome_drag = Some(ClientChromeDrag::Tab {
                            tab_id: press.tab_id.clone(),
                            workspace_id: press.workspace_id.clone(),
                            insert_index: Some(insert_index),
                        });
                        outcome.repaint = true;
                    }
                }
                return;
            }
        }
        if mouse.kind == MouseEventKind::Up(MouseButton::Left) {
            if let Some(drag) = self.chrome_drag.take() {
                self.workspace_press = None;
                self.tab_press = None;
                match drag {
                    ClientChromeDrag::Tab {
                        tab_id,
                        workspace_id,
                        ..
                    } => {
                        let insert_index = self.tab_drop_index_at(point);
                        let valid_drop = self.snapshot.as_deref().is_some_and(|snapshot| {
                            snapshot.focused_workspace_id.as_deref() == Some(workspace_id.as_str())
                                && snapshot.tabs.iter().any(|tab| {
                                    tab.tab_id == tab_id && tab.workspace_id == workspace_id
                                })
                                && insert_index.is_some_and(|index| {
                                    index
                                        <= snapshot
                                            .tabs
                                            .iter()
                                            .filter(|tab| tab.workspace_id == workspace_id)
                                            .count()
                                })
                        });
                        if valid_drop {
                            self.push_endpoint_method(
                                crate::api::schema::Method::TabMove(
                                    crate::api::schema::TabMoveParams {
                                        tab_id,
                                        insert_index: insert_index.unwrap_or_default(),
                                    },
                                ),
                                outcome,
                            );
                        }
                        outcome.repaint = true;
                    }
                    ClientChromeDrag::Workspace {
                        source_workspace_id,
                        target,
                    } => {
                        if let Some((before_workspace_id, _)) = target {
                            if let Some(method) = self.workspace_move_method(
                                &source_workspace_id,
                                before_workspace_id.as_deref(),
                            ) {
                                self.push_endpoint_method(method, outcome);
                            }
                        }
                        outcome.repaint = true;
                    }
                    ClientChromeDrag::PaneScrollbar {
                        hit,
                        grab_row_offset,
                        last_sent_offset,
                        ..
                    } => {
                        let current_hit = self
                            .hits
                            .panes
                            .iter()
                            .find(|current| current.pane_id == hit.pane_id)
                            .cloned()
                            .unwrap_or(hit);
                        if let Some(offset) = Self::pane_scrollbar_offset(
                            &current_hit,
                            mouse.row,
                            Some(grab_row_offset),
                        ) {
                            if last_sent_offset != Some(offset) {
                                self.push_pane_scroll_offset(current_hit.pane_id, offset, outcome);
                            }
                        }
                    }
                    ClientChromeDrag::PaneSplit {
                        hit,
                        tab_id,
                        grab_offset,
                        last_sent_ratio,
                        ..
                    } => {
                        let target_is_current =
                            self.pane_split_target_is_current(&hit, &tab_id) == Some(true);
                        let ratio = Self::pane_split_ratio(&hit, grab_offset, point);
                        if target_is_current
                            && last_sent_ratio
                                .is_none_or(|sent| (sent - ratio).abs() > f32::EPSILON)
                        {
                            self.push_endpoint_method(
                                crate::api::schema::Method::LayoutSetSplitRatio(
                                    crate::api::schema::LayoutSetSplitRatioParams {
                                        tab_id: Some(tab_id),
                                        pane_id: None,
                                        path: hit.path,
                                        ratio,
                                    },
                                ),
                                outcome,
                            );
                        }
                    }
                    ClientChromeDrag::SidebarWidth | ClientChromeDrag::SidebarSection => {
                        self.persist_chrome_preferences(outcome);
                    }
                    ClientChromeDrag::WorkspaceScrollbar { .. }
                    | ClientChromeDrag::AgentScrollbar { .. }
                    | ClientChromeDrag::HelpScrollbar { .. }
                    | ClientChromeDrag::ProductAnnouncementScrollbar { .. }
                    | ClientChromeDrag::ReleaseNotesScrollbar { .. } => {}
                }
                return;
            }
            if let Some(press) = self.workspace_press.take() {
                self.push_endpoint_method(
                    crate::api::schema::Method::WorkspaceFocus(
                        crate::api::schema::WorkspaceTarget {
                            workspace_id: press.workspace_id,
                        },
                    ),
                    outcome,
                );
                return;
            }
            if let Some(press) = self.tab_press.take() {
                self.push_endpoint_method(
                    crate::api::schema::Method::TabFocus(crate::api::schema::TabTarget {
                        tab_id: press.tab_id,
                    }),
                    outcome,
                );
                return;
            }
        }
        if matches!(self.overlay, Some(ClientShellOverlay::GlobalMenu(_))) {
            let row_hit = self
                .hits
                .global_menu_rows
                .iter()
                .find(|(rect, _)| super::contains(*rect, point))
                .copied();
            match mouse.kind {
                MouseEventKind::Moved => {
                    if let (Some((_, index)), Some(ClientShellOverlay::GlobalMenu(menu))) =
                        (row_hit, self.overlay.as_mut())
                    {
                        menu.highlighted = index;
                        outcome.repaint = true;
                    }
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    if super::contains(self.hits.global_launcher, point) {
                        self.toggle_global_menu();
                        outcome.repaint = true;
                    } else if let Some((_, index)) = row_hit {
                        self.activate_global_menu_item(index, outcome);
                    } else {
                        self.overlay = None;
                        outcome.repaint = true;
                    }
                }
                _ => {}
            }
            return;
        }
        if matches!(self.overlay, Some(ClientShellOverlay::ContextMenu(_))) {
            let row_hit = self
                .hits
                .context_menu_rows
                .iter()
                .find(|(rect, _)| super::contains(*rect, point))
                .copied();
            match mouse.kind {
                MouseEventKind::Moved => {
                    if let (Some((_, index)), Some(ClientShellOverlay::ContextMenu(menu))) =
                        (row_hit, self.overlay.as_mut())
                    {
                        menu.highlighted = index;
                        outcome.repaint = true;
                    }
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some((_, index)) = row_hit {
                        self.activate_context_menu_item(index, outcome);
                    } else {
                        self.overlay = None;
                        outcome.repaint = true;
                    }
                }
                _ => {}
            }
            return;
        }
        if matches!(
            self.overlay,
            Some(
                ClientShellOverlay::WorktreeCreate(_)
                    | ClientShellOverlay::WorktreeOpen(_)
                    | ClientShellOverlay::WorktreeRemove(_)
            )
        ) {
            match mouse.kind {
                MouseEventKind::ScrollUp
                    if matches!(self.overlay, Some(ClientShellOverlay::WorktreeOpen(_))) =>
                {
                    self.move_worktree_open_selection(-1);
                    outcome.repaint = true;
                }
                MouseEventKind::ScrollDown
                    if matches!(self.overlay, Some(ClientShellOverlay::WorktreeOpen(_))) =>
                {
                    self.move_worktree_open_selection(1);
                    outcome.repaint = true;
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    if super::contains(self.hits.overlay_cancel, point) {
                        let busy =
                            matches!(
                                self.overlay,
                                Some(
                                    ClientShellOverlay::WorktreeCreate(
                                        ClientWorktreeCreateOverlay { creating: true, .. }
                                    ) | ClientShellOverlay::WorktreeOpen(
                                        ClientWorktreeOpenOverlay { opening: true, .. }
                                    ) | ClientShellOverlay::WorktreeRemove(
                                        ClientWorktreeRemoveOverlay { removing: true, .. }
                                    )
                                )
                            );
                        if !busy {
                            self.overlay = None;
                            outcome.repaint = true;
                        }
                    } else if super::contains(self.hits.worktree_search, point) {
                        if let Some(ClientShellOverlay::WorktreeOpen(open)) = self.overlay.as_mut()
                        {
                            open.search_focused = true;
                            outcome.repaint = true;
                        }
                    } else if let Some((_, index)) = self
                        .hits
                        .worktree_rows
                        .iter()
                        .find(|(rect, _)| super::contains(*rect, point))
                        .copied()
                    {
                        if let Some(ClientShellOverlay::WorktreeOpen(open)) = self.overlay.as_mut()
                        {
                            open.selected = index;
                        }
                        self.submit_worktree_open(outcome);
                    } else if super::contains(self.hits.overlay_primary, point) {
                        match self.overlay.as_ref() {
                            Some(ClientShellOverlay::WorktreeCreate(_)) => {
                                self.submit_worktree_create(outcome)
                            }
                            Some(ClientShellOverlay::WorktreeOpen(_)) => {
                                self.submit_worktree_open(outcome)
                            }
                            Some(ClientShellOverlay::WorktreeRemove(_)) => {
                                self.submit_worktree_remove(outcome)
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
            return;
        }
        if matches!(self.overlay, Some(ClientShellOverlay::Settings(_))) {
            if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                if let Some((_, section)) = self
                    .hits
                    .settings_tabs
                    .iter()
                    .find(|(rect, _)| super::contains(*rect, point))
                    .copied()
                {
                    self.select_settings_section(section, outcome);
                } else if let Some((_, index)) = self
                    .hits
                    .settings_choices
                    .iter()
                    .find(|(rect, _)| super::contains(*rect, point))
                    .copied()
                {
                    self.select_settings_choice(index);
                    let immediate = matches!(
                        self.overlay,
                        Some(ClientShellOverlay::Settings(ClientSettingsOverlay {
                            section: ClientSettingsSection::Indicators
                                | ClientSettingsSection::Sound
                                | ClientSettingsSection::Toast,
                            ..
                        }))
                    );
                    if immediate {
                        self.apply_settings_choice(outcome);
                    }
                    outcome.repaint = true;
                } else if super::contains(self.hits.overlay_primary, point) {
                    self.apply_settings_choice(outcome);
                } else if super::contains(self.hits.overlay_cancel, point)
                    || !super::contains(self.hits.settings_popup, point)
                {
                    let installing = matches!(
                        self.overlay,
                        Some(ClientShellOverlay::Settings(ClientSettingsOverlay {
                            installing_integrations: true,
                            ..
                        }))
                    );
                    if !installing {
                        self.cancel_settings_overlay();
                        outcome.repaint = true;
                    }
                }
            }
            return;
        }
        if matches!(self.overlay, Some(ClientShellOverlay::Help(_))) {
            match mouse.kind {
                MouseEventKind::ScrollUp => {
                    if let Some(ClientShellOverlay::Help(help)) = self.overlay.as_mut() {
                        let next = help.scroll.saturating_sub(3);
                        if next != help.scroll {
                            help.scroll = next;
                            outcome.repaint = true;
                        }
                    }
                }
                MouseEventKind::ScrollDown => {
                    if let Some(ClientShellOverlay::Help(help)) = self.overlay.as_mut() {
                        let next = help.scroll.saturating_add(3).min(self.hits.help_max_scroll);
                        if next != help.scroll {
                            help.scroll = next;
                            outcome.repaint = true;
                        }
                    }
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    if super::contains(self.hits.help_scrollbar, point) {
                        if let Some(metrics) = self.hits.help_scroll_metrics {
                            if let Some(grab_row_offset) = crate::ui::scrollbar_thumb_grab_offset(
                                metrics,
                                self.hits.help_scrollbar,
                                mouse.row,
                            ) {
                                self.chrome_drag =
                                    Some(ClientChromeDrag::HelpScrollbar { grab_row_offset });
                            } else {
                                let offset = crate::ui::scrollbar_offset_from_row(
                                    metrics,
                                    self.hits.help_scrollbar,
                                    mouse.row,
                                );
                                if let Some(ClientShellOverlay::Help(help)) = self.overlay.as_mut()
                                {
                                    help.scroll =
                                        metrics.max_offset_from_bottom.saturating_sub(offset);
                                    outcome.repaint = true;
                                }
                            }
                        }
                    } else if super::contains(self.hits.overlay_cancel, point) {
                        let search_focused = matches!(
                            self.overlay,
                            Some(ClientShellOverlay::Help(ClientHelpOverlay {
                                search_focused: true,
                                ..
                            }))
                        );
                        if search_focused {
                            if let Some(ClientShellOverlay::Help(help)) = self.overlay.as_mut() {
                                help.search_focused = false;
                                help.query.clear();
                                help.scroll = 0;
                            }
                        } else {
                            self.overlay = None;
                        }
                        outcome.repaint = true;
                    } else if !super::contains(self.hits.help_popup, point) {
                        self.overlay = None;
                        outcome.repaint = true;
                    }
                }
                _ => {}
            }
            return;
        }
        if matches!(self.overlay, Some(ClientShellOverlay::Navigator(_))) {
            let row_hit = self
                .hits
                .navigator_rows
                .iter()
                .find(|(rect, _)| super::contains(*rect, point))
                .copied();
            match mouse.kind {
                MouseEventKind::Moved => {
                    if let Some((_, index)) = row_hit {
                        if let Some(ClientShellOverlay::Navigator(navigator)) =
                            self.overlay.as_mut()
                        {
                            navigator.selected = index;
                        }
                        outcome.repaint = true;
                    }
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    if super::contains(self.hits.navigator_search, point) {
                        if let Some(ClientShellOverlay::Navigator(navigator)) =
                            self.overlay.as_mut()
                        {
                            navigator.search_focused = true;
                            navigator.filter = None;
                        }
                        outcome.repaint = true;
                    } else if let Some((rect, index)) = row_hit {
                        if let Some(ClientShellOverlay::Navigator(navigator)) =
                            self.overlay.as_mut()
                        {
                            navigator.selected = index;
                        }
                        let workspace = self
                            .snapshot
                            .as_deref()
                            .zip(self.overlay.as_ref())
                            .and_then(|(snapshot, overlay)| match overlay {
                                ClientShellOverlay::Navigator(navigator) => {
                                    render::client_navigator_rows(snapshot, navigator)
                                        .get(index)
                                        .map(|row| {
                                            matches!(
                                                row.target,
                                                ClientNavigatorTarget::Workspace(_)
                                            )
                                        })
                                }
                                _ => None,
                            })
                            .unwrap_or(false);
                        if workspace && mouse.column <= rect.x.saturating_add(3) {
                            self.toggle_selected_navigator_workspace();
                            outcome.repaint = true;
                        } else {
                            self.accept_navigator_selection(outcome);
                        }
                    } else if !super::contains(self.hits.navigator_popup, point) {
                        self.overlay = None;
                        outcome.repaint = true;
                    }
                }
                MouseEventKind::ScrollUp => {
                    self.move_navigator_selection(-3);
                    outcome.repaint = true;
                }
                MouseEventKind::ScrollDown => {
                    self.move_navigator_selection(3);
                    outcome.repaint = true;
                }
                _ => {}
            }
            return;
        }
        if self.overlay.is_some() {
            if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
                return;
            }
            if super::contains(self.hits.overlay_primary, point) {
                match self.overlay.as_ref() {
                    Some(ClientShellOverlay::Rename(_)) => self.save_rename_overlay(outcome),
                    Some(ClientShellOverlay::ConfirmClose(_)) => {
                        let Some(ClientShellOverlay::ConfirmClose(confirm)) = self.overlay.take()
                        else {
                            return;
                        };
                        self.push_endpoint_method(
                            crate::api::schema::Method::WorkspaceClose(
                                crate::api::schema::WorkspaceCloseParams {
                                    workspace_id: confirm.workspace_id,
                                    close_group: true,
                                },
                            ),
                            outcome,
                        );
                        outcome.repaint = true;
                    }
                    _ => {}
                }
            } else if super::contains(self.hits.overlay_clear, point) {
                if let Some(ClientShellOverlay::Rename(rename)) = self.overlay.as_mut() {
                    rename.input.clear();
                    rename.replace_on_type = false;
                    outcome.repaint = true;
                }
            } else {
                self.overlay = None;
                outcome.repaint = true;
            }
            return;
        }

        if mouse.kind == MouseEventKind::Drag(MouseButton::Left) {
            let selection_hit = self.selection.as_ref().and_then(|selection| {
                self.hits
                    .panes
                    .iter()
                    .find(|hit| hit.pane_id == selection.pane_id)
                    .cloned()
            });
            if let Some(hit) = selection_hit {
                self.update_selection_drag(&hit, mouse.column, mouse.row, outcome);
                outcome.repaint = true;
                return;
            }
        }
        if mouse.kind == MouseEventKind::Up(MouseButton::Left) && self.selection.is_some() {
            self.stop_selection_autoscroll();
            let copied = self
                .selection
                .as_mut()
                .is_some_and(crate::selection::Selection::finish);
            if copied && self.config.copy_on_select {
                self.request_selection_copy(outcome);
                self.selection = None;
            } else if !copied {
                self.selection = None;
            }
            if copied {
                self.last_pane_click = None;
            }
            outcome.repaint = true;
            return;
        }
        if self.scroll_in_progress_selection(mouse, outcome) {
            return;
        }

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Right) => {
                let pane_hit = self
                    .hits
                    .panes
                    .iter()
                    .find(|hit| super::contains(hit.inner_rect, point))
                    .cloned();
                if let Some(hit) = pane_hit {
                    let pane_owns_right_click = self
                        .snapshot
                        .as_deref()
                        .and_then(|snapshot| {
                            snapshot
                                .panes
                                .iter()
                                .find(|pane| pane.pane_id == hit.pane_id)
                        })
                        .is_some_and(|pane| pane.right_click_passthrough)
                        && mouse.modifiers.is_empty();
                    let configured_modifiers = self
                        .config
                        .right_click_passthrough_modifiers
                        .filter(|modifiers| *modifiers == mouse.modifiers);
                    if hit.mouse_reporting
                        && (pane_owns_right_click || configured_modifiers.is_some())
                    {
                        let stripped_modifiers =
                            configured_modifiers.unwrap_or(crossterm::event::KeyModifiers::empty());
                        self.push_pane_mouse_event(
                            &hit,
                            mouse,
                            mouse.modifiers.difference(stripped_modifiers),
                            outcome,
                        );
                        self.push_endpoint_method(
                            crate::api::schema::Method::PaneFocus(crate::api::schema::PaneTarget {
                                pane_id: hit.pane_id.clone(),
                            }),
                            outcome,
                        );
                        self.pane_mouse_gesture = Some(ClientPaneMouseGesture {
                            last_position: self.pane_mouse_position(&hit, mouse),
                            hit,
                            button: MouseButton::Right,
                            stripped_modifiers,
                            last_event: mouse,
                        });
                        return;
                    }
                }
                if !self.config.mouse_capture {
                    return;
                }
                let workspace_id = (!self.sidebar_collapsed)
                    .then(|| {
                        self.hits
                            .workspaces
                            .iter()
                            .find(|hit| super::contains(hit.rect, point))
                            .map(|hit| hit.workspace_id.clone())
                    })
                    .flatten();
                if let Some(workspace_id) = workspace_id {
                    self.open_workspace_context_menu(workspace_id, mouse.column, mouse.row);
                    outcome.repaint = true;
                    return;
                }
                let tab_id = self
                    .hits
                    .tabs
                    .iter()
                    .find(|(rect, _)| super::contains(*rect, point))
                    .map(|(_, tab_id)| tab_id.clone());
                if let Some(tab_id) = tab_id {
                    self.open_tab_context_menu(tab_id, mouse.column, mouse.row);
                    outcome.repaint = true;
                    return;
                }
                let pane_id = self
                    .hits
                    .panes
                    .iter()
                    .find(|hit| super::contains(hit.rect, point))
                    .map(|hit| hit.pane_id.clone());
                if let Some(pane_id) = pane_id {
                    self.open_pane_context_menu(pane_id, mouse.column, mouse.row);
                    outcome.repaint = true;
                }
            }
            MouseEventKind::ScrollUp
                if self
                    .hits
                    .tabs
                    .iter()
                    .any(|(rect, _)| super::contains(*rect, point))
                    || super::contains(self.hits.tab_scroll_left, point)
                    || super::contains(self.hits.tab_scroll_right, point)
                    || super::contains(self.hits.new_tab, point) =>
            {
                self.record_binding(
                    crate::input::KeybindMatch::Action(crate::input::KeybindAction::PreviousTab),
                    outcome,
                );
            }
            MouseEventKind::ScrollDown
                if self
                    .hits
                    .tabs
                    .iter()
                    .any(|(rect, _)| super::contains(*rect, point))
                    || super::contains(self.hits.tab_scroll_left, point)
                    || super::contains(self.hits.tab_scroll_right, point)
                    || super::contains(self.hits.new_tab, point) =>
            {
                self.record_binding(
                    crate::input::KeybindMatch::Action(crate::input::KeybindAction::NextTab),
                    outcome,
                );
            }
            MouseEventKind::ScrollUp if super::contains(self.hits.agent_body, point) => {
                let next = self.agent_scroll.saturating_sub(1);
                if next != self.agent_scroll {
                    self.agent_scroll = next;
                    outcome.repaint = true;
                }
            }
            MouseEventKind::ScrollDown if super::contains(self.hits.agent_body, point) => {
                let next = self
                    .agent_scroll
                    .saturating_add(1)
                    .min(self.hits.agent_max_scroll);
                if next != self.agent_scroll {
                    self.agent_scroll = next;
                    outcome.repaint = true;
                }
            }
            MouseEventKind::ScrollUp if super::contains(self.hits.workspace_body, point) => {
                let next = self.workspace_scroll.saturating_sub(1);
                if next != self.workspace_scroll {
                    self.workspace_scroll = next;
                    outcome.repaint = true;
                }
            }
            MouseEventKind::ScrollDown if super::contains(self.hits.workspace_body, point) => {
                let next = self
                    .workspace_scroll
                    .saturating_add(1)
                    .min(self.hits.workspace_max_scroll);
                if next != self.workspace_scroll {
                    self.workspace_scroll = next;
                    outcome.repaint = true;
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if self.selection.take().is_some() {
                    outcome.repaint = true;
                }
                self.stop_selection_autoscroll();
                self.selection_highlight_clear_deadline = None;
                self.pending_word_selection = None;
                let previous_pane_click = self.last_pane_click.take();
                self.workspace_press = None;
                self.tab_press = None;
                self.chrome_drag = None;
                if super::contains(self.hits.sidebar_divider, point)
                    && !super::contains(self.hits.sidebar_toggle, point)
                {
                    let now = std::time::Instant::now();
                    let double_click = self.last_sidebar_divider_click.is_some_and(|last| {
                        now.duration_since(last) <= std::time::Duration::from_millis(350)
                    });
                    self.last_sidebar_divider_click = Some(now);
                    if double_click {
                        self.sidebar_width = self.config.sidebar_width;
                        self.sidebar_width_manual = false;
                        self.invalidate_pane_surface();
                        outcome.repaint = true;
                        outcome.resize = true;
                        self.persist_chrome_preferences(outcome);
                    } else {
                        self.chrome_drag = Some(ClientChromeDrag::SidebarWidth);
                        self.set_sidebar_width_from_column(mouse.column, outcome);
                    }
                    return;
                }
                if super::contains(self.hits.sidebar_section_divider, point) {
                    self.chrome_drag = Some(ClientChromeDrag::SidebarSection);
                    self.set_sidebar_section_from_row(mouse.row, outcome);
                    return;
                }
                if super::contains(self.hits.workspace_scrollbar, point) {
                    if let Some(metrics) = self.hits.workspace_scroll_metrics {
                        if let Some(grab_row_offset) = crate::ui::scrollbar_thumb_grab_offset(
                            metrics,
                            self.hits.workspace_scrollbar,
                            mouse.row,
                        ) {
                            self.chrome_drag =
                                Some(ClientChromeDrag::WorkspaceScrollbar { grab_row_offset });
                        } else {
                            let offset = crate::ui::scrollbar_offset_from_row(
                                metrics,
                                self.hits.workspace_scrollbar,
                                mouse.row,
                            );
                            let next = metrics.max_offset_from_bottom.saturating_sub(offset);
                            if next != self.workspace_scroll {
                                self.workspace_scroll = next;
                                outcome.repaint = true;
                            }
                        }
                    }
                    return;
                }
                if super::contains(self.hits.agent_scrollbar, point) {
                    if let Some(metrics) = self.hits.agent_scroll_metrics {
                        if let Some(grab_row_offset) = crate::ui::scrollbar_thumb_grab_offset(
                            metrics,
                            self.hits.agent_scrollbar,
                            mouse.row,
                        ) {
                            self.chrome_drag =
                                Some(ClientChromeDrag::AgentScrollbar { grab_row_offset });
                        } else {
                            let offset = crate::ui::scrollbar_offset_from_row(
                                metrics,
                                self.hits.agent_scrollbar,
                                mouse.row,
                            );
                            let next = metrics.max_offset_from_bottom.saturating_sub(offset);
                            if next != self.agent_scroll {
                                self.agent_scroll = next;
                                outcome.repaint = true;
                            }
                        }
                    }
                    return;
                }
                if super::contains(self.hits.agent_sort_toggle, point) {
                    let sort = match self.config.agent_panel_sort {
                        crate::config::AgentPanelSortConfig::Spaces => {
                            crate::config::AgentPanelSortConfig::Priority
                        }
                        crate::config::AgentPanelSortConfig::Priority => {
                            crate::config::AgentPanelSortConfig::Spaces
                        }
                    };
                    self.config.agent_panel_sort = sort;
                    self.agent_panel_sort_manual = true;
                    self.agent_scroll = 0;
                    self.persist_chrome_preferences(outcome);
                    outcome.repaint = true;
                    return;
                }
                if super::contains(self.hits.global_launcher, point) {
                    self.toggle_global_menu();
                    outcome.repaint = true;
                    return;
                }
                if super::contains(self.hits.new_workspace, point) {
                    self.record_binding(
                        crate::input::KeybindMatch::Action(
                            crate::input::KeybindAction::NewWorkspace,
                        ),
                        outcome,
                    );
                    return;
                }
                if super::contains(self.hits.new_tab, point) {
                    self.record_binding(
                        crate::input::KeybindMatch::Action(crate::input::KeybindAction::NewTab),
                        outcome,
                    );
                    return;
                }
                if super::contains(self.hits.tab_scroll_left, point) {
                    self.tab_scroll = self.tab_scroll.saturating_sub(1);
                    outcome.repaint = true;
                    return;
                }
                if super::contains(self.hits.tab_scroll_right, point) {
                    let tab_count = self
                        .snapshot
                        .as_deref()
                        .and_then(|snapshot| {
                            snapshot.focused_workspace_id.as_deref().map(|id| {
                                snapshot
                                    .tabs
                                    .iter()
                                    .filter(|tab| tab.workspace_id == id)
                                    .count()
                            })
                        })
                        .unwrap_or(0);
                    self.tab_scroll = self
                        .tab_scroll
                        .saturating_add(1)
                        .min(tab_count.saturating_sub(1));
                    outcome.repaint = true;
                    return;
                }
                if super::contains(self.hits.sidebar_toggle, point) {
                    self.sidebar_collapsed = !self.sidebar_collapsed;
                    self.sidebar_collapsed_manual = true;
                    self.invalidate_pane_surface();
                    outcome.repaint = true;
                    outcome.resize = true;
                    self.persist_chrome_preferences(outcome);
                    return;
                }
                for hit in &self.hits.workspaces {
                    if let Some((rect, key)) = &hit.group_toggle {
                        if super::contains(*rect, point) {
                            if !self.collapsed_groups.remove(key) {
                                self.collapsed_groups.insert(key.clone());
                            }
                            outcome.repaint = true;
                            self.persist_chrome_preferences(outcome);
                            return;
                        }
                    }
                }
                let workspace_press = self
                    .hits
                    .workspaces
                    .iter()
                    .find(|hit| super::contains(hit.rect, point))
                    .map(|hit| ClientWorkspacePress {
                        workspace_id: hit.workspace_id.clone(),
                        start_column: mouse.column,
                        start_row: mouse.row,
                    });
                if let Some(workspace_press) = workspace_press {
                    self.workspace_press = Some(workspace_press);
                    return;
                }
                let tab_press = self
                    .config
                    .mouse_capture
                    .then(|| {
                        self.hits
                            .tabs
                            .iter()
                            .find(|(rect, _)| super::contains(*rect, point))
                            .and_then(|(_, tab_id)| {
                                let tab = self
                                    .snapshot
                                    .as_deref()?
                                    .tabs
                                    .iter()
                                    .find(|tab| tab.tab_id == *tab_id)?;
                                Some(ClientTabPress {
                                    tab_id: tab.tab_id.clone(),
                                    workspace_id: tab.workspace_id.clone(),
                                    start_column: mouse.column,
                                    start_row: mouse.row,
                                })
                            })
                    })
                    .flatten();
                if let Some(tab_press) = tab_press {
                    self.tab_press = Some(tab_press);
                    return;
                }
                let agent_pane_id = self
                    .hits
                    .agents
                    .iter()
                    .find(|(rect, _)| super::contains(*rect, point))
                    .map(|(_, pane_id)| pane_id.clone());
                if let Some(pane_id) = agent_pane_id {
                    self.push_endpoint_method(
                        crate::api::schema::Method::PaneFocus(crate::api::schema::PaneTarget {
                            pane_id,
                        }),
                        outcome,
                    );
                    return;
                }
                let scrollbar_hit = self
                    .hits
                    .panes
                    .iter()
                    .find(|hit| {
                        hit.scrollbar_rect
                            .is_some_and(|rect| super::contains(rect, point))
                            && hit
                                .scroll
                                .is_some_and(|metrics| metrics.max_offset_from_bottom > 0)
                    })
                    .cloned();
                if let Some(hit) = scrollbar_hit {
                    self.mode = ClientShellMode::Terminal;
                    self.push_endpoint_method(
                        crate::api::schema::Method::PaneFocus(crate::api::schema::PaneTarget {
                            pane_id: hit.pane_id.clone(),
                        }),
                        outcome,
                    );
                    let (Some(track), Some(metrics)) = (hit.scrollbar_rect, hit.scroll) else {
                        return;
                    };
                    if let Some(grab_row_offset) =
                        crate::ui::scrollbar_thumb_grab_offset(metrics, track, mouse.row)
                    {
                        self.chrome_drag = Some(ClientChromeDrag::PaneScrollbar {
                            hit,
                            grab_row_offset,
                            last_sent_offset: None,
                            last_sent_at: None,
                        });
                    } else if let Some(offset) = Self::pane_scrollbar_offset(&hit, mouse.row, None)
                    {
                        self.push_pane_scroll_offset(hit.pane_id, offset, outcome);
                    }
                    return;
                }
                let split_hit = self
                    .hits
                    .pane_splits
                    .iter()
                    .find(|hit| super::contains(hit.hit_rect, point))
                    .cloned();
                if let Some(hit) = split_hit {
                    let Some(tab_id) = self
                        .snapshot
                        .as_deref()
                        .and_then(|snapshot| snapshot.focused_tab_id.clone())
                    else {
                        return;
                    };
                    let pointer = match hit.direction {
                        crate::protocol::PaneSurfaceSplitDirection::Horizontal => mouse.column,
                        crate::protocol::PaneSurfaceSplitDirection::Vertical => mouse.row,
                    };
                    self.chrome_drag = Some(ClientChromeDrag::PaneSplit {
                        grab_offset: i32::from(hit.pos) - i32::from(pointer),
                        last_sent_ratio: None,
                        last_sent_at: None,
                        hit,
                        tab_id,
                    });
                    return;
                }
                let pane_hit = self
                    .hits
                    .panes
                    .iter()
                    .find(|hit| super::contains(hit.rect, point))
                    .cloned();
                if let Some(hit) = pane_hit {
                    if hit.mouse_reporting && super::contains(hit.inner_rect, point) {
                        self.push_pane_mouse_event(&hit, mouse, mouse.modifiers, outcome);
                        self.pane_mouse_gesture = Some(ClientPaneMouseGesture {
                            last_position: self.pane_mouse_position(&hit, mouse),
                            hit: hit.clone(),
                            button: MouseButton::Left,
                            stripped_modifiers: crossterm::event::KeyModifiers::empty(),
                            last_event: mouse,
                        });
                    } else if super::contains(hit.inner_rect, point) {
                        let click = ClientPaneClick {
                            pane_id: hit.pane_id.clone(),
                            viewport_row: mouse.row.saturating_sub(hit.inner_rect.y),
                            col: mouse.column.saturating_sub(hit.inner_rect.x),
                            at: std::time::Instant::now(),
                        };
                        if mouse.modifiers.is_empty()
                            && previous_pane_click
                                .as_ref()
                                .is_some_and(|previous| previous.is_double_click_for(&click))
                        {
                            self.request_word_selection(
                                &hit,
                                click.viewport_row,
                                click.col,
                                outcome,
                            );
                        } else {
                            if mouse.modifiers.is_empty() {
                                self.last_pane_click = Some(click);
                            }
                            self.selection = Some(crate::selection::Selection::anchor(
                                hit.pane_id.clone(),
                                mouse.row.saturating_sub(hit.inner_rect.y),
                                mouse.column.saturating_sub(hit.inner_rect.x),
                                hit.scroll,
                            ));
                        }
                    }
                    self.push_endpoint_method(
                        crate::api::schema::Method::PaneFocus(crate::api::schema::PaneTarget {
                            pane_id: hit.pane_id,
                        }),
                        outcome,
                    );
                }
            }
            MouseEventKind::Down(MouseButton::Middle) => {
                if let Some(hit) = self
                    .hits
                    .panes
                    .iter()
                    .find(|hit| super::contains(hit.inner_rect, point) && hit.mouse_reporting)
                    .cloned()
                {
                    self.push_pane_mouse_event(&hit, mouse, mouse.modifiers, outcome);
                    self.pane_mouse_gesture = Some(ClientPaneMouseGesture {
                        last_position: self.pane_mouse_position(&hit, mouse),
                        hit,
                        button: MouseButton::Middle,
                        stripped_modifiers: crossterm::event::KeyModifiers::empty(),
                        last_event: mouse,
                    });
                }
            }
            MouseEventKind::Up(MouseButton::Left | MouseButton::Middle)
            | MouseEventKind::Drag(MouseButton::Left | MouseButton::Middle) => {}
            MouseEventKind::Moved => {
                if let Some(hit) = self
                    .hits
                    .panes
                    .iter()
                    .find(|hit| super::contains(hit.inner_rect, point) && hit.mouse_reporting)
                    .cloned()
                {
                    self.push_pane_mouse_event(&hit, mouse, mouse.modifiers, outcome);
                }
            }
            MouseEventKind::ScrollUp
            | MouseEventKind::ScrollDown
            | MouseEventKind::ScrollLeft
            | MouseEventKind::ScrollRight => {
                if let Some(hit) = self
                    .hits
                    .panes
                    .iter()
                    .find(|hit| super::contains(hit.inner_rect, point))
                    .cloned()
                {
                    if self.focused_pane_id().as_deref() != Some(hit.pane_id.as_str()) {
                        self.push_endpoint_method(
                            crate::api::schema::Method::PaneFocus(crate::api::schema::PaneTarget {
                                pane_id: hit.pane_id.clone(),
                            }),
                            outcome,
                        );
                    }
                    self.push_pane_mouse_event(&hit, mouse, mouse.modifiers, outcome);
                }
            }
            _ => {}
        }
    }

    fn pane_mouse_position(&self, hit: &PaneHit, mouse: MouseEvent) -> ClientMousePosition {
        let cell = ClientMousePosition::Cell {
            column: mouse.column.saturating_sub(hit.inner_rect.x),
            row: mouse.row.saturating_sub(hit.inner_rect.y),
        };
        if hit.sgr_pixel_mouse && hit.pixel_width > 0 && hit.pixel_height > 0 {
            self.host_mouse_pixels
                .and_then(|pixels| {
                    pixels
                        .pane_position(hit.inner_rect, hit.pixel_width, hit.pixel_height)
                        .and_then(|position| match position {
                            crate::input::mouse::Position::Pixels { x, y } => {
                                Some(ClientMousePosition::Pixels {
                                    x,
                                    y,
                                    column: mouse.column.saturating_sub(hit.inner_rect.x),
                                    row: mouse.row.saturating_sub(hit.inner_rect.y),
                                })
                            }
                            crate::input::mouse::Position::Cell { .. } => None,
                        })
                })
                .unwrap_or(cell)
        } else {
            cell
        }
    }

    pub(super) fn push_pane_mouse_event(
        &self,
        hit: &PaneHit,
        mouse: MouseEvent,
        modifiers: crossterm::event::KeyModifiers,
        outcome: &mut ClientShellInput,
    ) {
        let Some(kind) = crate::protocol::ClientMouseKind::from_crossterm(mouse.kind) else {
            return;
        };
        let position = self.pane_mouse_position(hit, mouse);
        let geometry = matches!(position, ClientMousePosition::Pixels { .. }).then_some(
            crate::protocol::ClientMouseGeometry {
                cols: hit.inner_rect.width,
                rows: hit.inner_rect.height,
                width_px: hit.pixel_width,
                height_px: hit.pixel_height,
            },
        );
        let target = if hit.popup {
            ClientInputTarget::Popup(hit.pane_id.clone())
        } else {
            ClientInputTarget::Pane(hit.pane_id.clone())
        };
        push_target_event(
            target,
            ClientPaneInputEvent::Mouse {
                kind,
                position,
                geometry,
                modifiers: modifiers.bits(),
                lines: self.config.mouse_scroll_lines.min(u16::MAX as usize) as u16,
            },
            outcome,
        );
    }
}
