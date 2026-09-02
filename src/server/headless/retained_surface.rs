use super::*;

fn rect_fits_frame(rect: protocol::SurfaceRect, frame: &FrameData) -> bool {
    rect.x.saturating_add(rect.width) <= frame.width
        && rect.y.saturating_add(rect.height) <= frame.height
}

fn patch_intersects_hyperlinks(
    frame: &FrameData,
    area: protocol::SurfaceRect,
    patch: &crate::pane::TerminalDirtyPatch,
) -> bool {
    if frame.hyperlinks.is_empty() || !rect_fits_frame(area, frame) {
        return false;
    }
    let width = usize::from(frame.width);
    patch
        .rows
        .iter()
        .filter(|(local_y, _)| *local_y < area.height)
        .any(|(local_y, _)| {
            let start = usize::from(area.y + *local_y) * width + usize::from(area.x);
            let end = start + usize::from(area.width);
            end > frame.cells.len()
                || frame.cells[start..end]
                    .iter()
                    .any(|cell| cell.hyperlink.is_some())
        })
}

fn apply_patch_row(frame: &mut FrameData, row: &protocol::PaneSurfacePatchRow) -> Option<bool> {
    if row.y >= frame.height
        || row.x.saturating_add(u16::try_from(row.cells.len()).ok()?) > frame.width
    {
        return None;
    }
    let start = usize::from(row.y) * usize::from(frame.width) + usize::from(row.x);
    let end = start + row.cells.len();
    if end > frame.cells.len() {
        return None;
    }
    let changed = frame.cells[start..end] != row.cells;
    frame.cells[start..end].clone_from_slice(&row.cells);
    Some(changed)
}

fn apply_rows(
    frame: &mut FrameData,
    area: protocol::SurfaceRect,
    patch: &crate::pane::TerminalDirtyPatch,
) -> Option<Vec<protocol::PaneSurfacePatchRow>> {
    if !rect_fits_frame(area, frame) {
        return None;
    }
    let mut rows = Vec::new();
    for (local_y, cells) in &patch.rows {
        if *local_y >= area.height {
            continue;
        }
        let width = usize::from(area.width);
        if cells.len() < width {
            return None;
        }
        let y = area.y + *local_y;
        let frame_start = usize::from(y) * usize::from(frame.width) + usize::from(area.x);
        let frame_end = frame_start.checked_add(width)?;
        let existing = frame.cells.get(frame_start..frame_end)?;
        let desired = &cells[..width];
        let mut offset = 0;
        while offset < width {
            if existing[offset] == desired[offset] {
                offset += 1;
                continue;
            }
            let start = offset;
            offset += 1;
            while offset < width && existing[offset] != desired[offset] {
                offset += 1;
            }
            // Include the following cell so a wide-to-narrow (or
            // narrow-to-wide) transition repaints content covered by the old
            // grapheme width even when that logical neighbor is unchanged.
            let end = offset.saturating_add(1).min(width);
            rows.push(protocol::PaneSurfacePatchRow {
                x: area.x.checked_add(u16::try_from(start).ok()?)?,
                y,
                cells: desired[start..end].to_vec(),
            });
            offset = end;
        }
    }
    for row in &rows {
        apply_patch_row(frame, row)?;
    }
    Some(rows)
}

fn retained_scrollbar_patch(
    app: &app::App,
    frame: &mut FrameData,
    pane: &mut protocol::PaneSurfacePane,
    alternate_screen_active: bool,
    metrics: Option<crate::pane::ScrollMetrics>,
) -> Option<Vec<protocol::PaneSurfacePatchRow>> {
    let next_rect = metrics
        .filter(|metrics| metrics.max_offset_from_bottom > 0)
        .filter(|_| app.state.pane_scrollbars && !alternate_screen_active)
        .and_then(|_| {
            let rect = protocol::SurfaceRect {
                x: pane.inner_rect.x.checked_add(pane.inner_rect.width)?,
                y: pane.inner_rect.y,
                width: 1,
                height: pane.inner_rect.height,
            };
            (rect_fits_frame(rect, frame)
                && rect.x >= pane.rect.x
                && rect.x < pane.rect.x.saturating_add(pane.rect.width))
            .then_some(rect)
        });
    let patch_rect = next_rect.or(pane.scrollbar_rect);
    pane.scrollbar_rect = next_rect;
    let Some(rect) = patch_rect else {
        return Some(Vec::new());
    };

    let track = Rect::new(0, 0, 1, rect.height);
    let mut buffer = ratatui::buffer::Buffer::empty(track);
    if let (Some(metrics), Some(_)) = (metrics, next_rect) {
        crate::ui::render_pane_scrollbar_buffer(
            &mut buffer,
            metrics,
            track,
            &app.state.palette,
            pane.focused,
        );
    }
    let cells = buffer
        .content
        .iter()
        .map(protocol::CellData::from_ratatui_cell)
        .collect::<Vec<_>>();
    let mut rows = Vec::new();
    for (offset, cell) in cells.into_iter().enumerate() {
        let row = protocol::PaneSurfacePatchRow {
            x: rect.x,
            y: rect.y.checked_add(u16::try_from(offset).ok()?)?,
            cells: vec![cell],
        };
        if apply_patch_row(frame, &row)? {
            rows.push(row);
        }
    }
    Some(rows)
}

fn retained_cursor(
    app: &app::App,
    panes: &[protocol::PaneSurfacePane],
) -> Option<protocol::CursorState> {
    let pane = panes.iter().find(|pane| pane.focused)?;
    let (workspace_index, pane_id) = app.parse_pane_id(&pane.pane_id)?;
    if !app.state.pane_exposes_host_cursor(workspace_index, pane_id) {
        return None;
    }
    let runtime = app.state.runtime_for_pane_in_workspace(
        &app.terminal_runtimes,
        workspace_index,
        pane_id,
    )?;
    if runtime.synchronized_output_active() {
        return None;
    }
    let area = Rect::new(
        pane.inner_rect.x,
        pane.inner_rect.y,
        pane.inner_rect.width,
        pane.inner_rect.height,
    );
    runtime
        .cursor_state(area, true)
        .map(|cursor| protocol::CursorState {
            x: cursor.x,
            y: cursor.y,
            visible: cursor.visible && !crate::ui::pane_is_scrolled_back(runtime),
            shape: cursor.shape,
        })
}

struct RetainedRecipient {
    client_id: u64,
    surface: protocol::PaneSurfaceFrame,
}

struct CollectedPanePatch {
    pane_id: String,
    patch: crate::pane::TerminalDirtyPatch,
    content_revision: u64,
    scroll_metrics: Option<crate::pane::ScrollMetrics>,
    mouse_reporting: bool,
    sgr_pixel_mouse: bool,
    alternate_screen_active: bool,
}

struct RetainedRecipientUpdate {
    client_id: u64,
    surface: protocol::PaneSurfaceFrame,
    patch: protocol::PaneSurfacePatch,
}

impl HeadlessServer {
    /// Applies terminal dirty rows to the committed origin-relative pane surface.
    /// Any presentation or geometry uncertainty falls back to the complete renderer.
    pub(super) fn render_retained_pane_surface_and_stream(
        &mut self,
        pty_sources: &HashSet<crate::layout::PaneId>,
    ) -> bool {
        crate::render_prof::event("retained_surface.attempt");
        let started = crate::render_prof::timer();
        macro_rules! fallback {
            ($reason:literal) => {{
                crate::render_prof::event(concat!("retained_surface.fallback.", $reason));
                crate::render_prof::duration_since("retained_surface.total", started);
                return false;
            }};
        }
        macro_rules! success {
            ($reason:literal) => {{
                crate::render_prof::event("retained_surface.success");
                crate::render_prof::event(concat!("retained_surface.success.", $reason));
                crate::render_prof::duration_since("retained_surface.total", started);
                return true;
            }};
        }

        if pty_sources.is_empty()
            || self.app.full_redraw_pending
            || self.app.state.popup_pane.is_some()
            || self.app.state.reveal_hidden_cursor_for_cjk_ime
        {
            fallback!("unsafe_state");
        }
        let targets = render_targets(&self.clients, self.foreground_client_id);
        if targets.is_empty()
            || targets
                .iter()
                .any(|target| !matches!(target.4, ClientConnectionMode::ClientShell))
        {
            fallback!("non_shell_target");
        }

        let mut recipients = Vec::with_capacity(targets.len());
        for (client_id, (cols, rows), _, _, _) in &targets {
            let Some(client) = self.clients.get(client_id) else {
                fallback!("client_missing");
            };
            if client.deferred_render() != DeferredRender::None {
                crate::render_prof::event("retained_surface.recipient_deferred");
                continue;
            }
            let Some(surface) = client.render_state.last_pane_surface() else {
                fallback!("no_baseline");
            };
            if surface.boot_id != self.client_shell_boot_id
                || surface.projection_revision != client.shell_projection_revision
                || surface.frame.width != *cols
                || surface.frame.height != *rows
                || surface.popup.is_some()
                || !surface.graphics.assets.is_empty()
                || !surface.graphics.placements.is_empty()
                || !surface.graphics.retained_assets.is_empty()
                || !surface.frame.graphics.is_empty()
            {
                fallback!("baseline_mismatch");
            }
            recipients.push(RetainedRecipient {
                client_id: *client_id,
                surface: surface.clone(),
            });
        }
        if recipients.is_empty() {
            success!("all_recipients_deferred");
        }

        let mut collected = Vec::with_capacity(pty_sources.len());
        for source in pty_sources {
            let mut public_pane_id = None;
            let mut width = 0u16;
            let mut height = 0u16;
            for recipient in &recipients {
                let Some(pane) = recipient.surface.panes.iter().find(|pane| {
                    self.app
                        .parse_pane_id(&pane.pane_id)
                        .is_some_and(|(_, pane_id)| pane_id == *source)
                }) else {
                    continue;
                };
                public_pane_id.get_or_insert_with(|| pane.pane_id.clone());
                width = width.max(pane.inner_rect.width);
                height = height.max(pane.inner_rect.height);
            }
            let Some(public_pane_id) = public_pane_id else {
                continue;
            };
            let Some((workspace_index, pane_id)) = self.app.parse_pane_id(&public_pane_id) else {
                fallback!("pane_missing");
            };
            let Some(runtime) = self.app.state.runtime_for_pane_in_workspace(
                &self.app.terminal_runtimes,
                workspace_index,
                pane_id,
            ) else {
                fallback!("runtime_missing");
            };
            let revision_before = runtime.content_seq();
            if !revision_before.is_multiple_of(2) {
                fallback!("unstable_content");
            }
            let patch = match runtime.collect_dirty_patch(width, height) {
                crate::pane::TerminalDirtyPatchOutcome::Clean => {
                    crate::render_prof::event("retained_surface.pane_clean");
                    crate::pane::TerminalDirtyPatch { rows: Vec::new() }
                }
                crate::pane::TerminalDirtyPatchOutcome::Patch(patch) => patch,
                crate::pane::TerminalDirtyPatchOutcome::Fallback => {
                    fallback!("terminal_patch");
                }
            };
            let revision = runtime.content_seq();
            if revision != revision_before || !revision.is_multiple_of(2) {
                fallback!("content_changed");
            }
            collected.push(CollectedPanePatch {
                pane_id: public_pane_id,
                patch,
                content_revision: revision,
                scroll_metrics: runtime.scroll_metrics(),
                mouse_reporting: runtime.mouse_reporting_enabled(),
                sgr_pixel_mouse: runtime.sgr_pixel_mouse_enabled(),
                alternate_screen_active: runtime.alternate_screen_active(),
            });
        }

        let mut updates = Vec::with_capacity(recipients.len());
        for recipient in recipients {
            let client_id = recipient.client_id;
            let mut surface = recipient.surface;
            let projection_revision = surface.projection_revision;
            let base_surface_revision = surface.surface_revision;
            let mut changed_panes = Vec::with_capacity(collected.len());
            let mut patch_rows = Vec::new();
            let mut metadata_changed = false;
            for collected_pane in &collected {
                let Some(pane) = surface
                    .panes
                    .iter_mut()
                    .find(|pane| pane.pane_id == collected_pane.pane_id)
                else {
                    continue;
                };
                // Alternate-screen transitions change whether the pane reserves
                // a scrollbar gutter. Recompute layout and resize the runtime
                // through the complete renderer before retaining further rows.
                if pane.alternate_screen_active != collected_pane.alternate_screen_active {
                    fallback!("alternate_screen_geometry");
                }
                if patch_intersects_hyperlinks(
                    &surface.frame,
                    pane.inner_rect,
                    &collected_pane.patch,
                ) {
                    fallback!("hyperlink");
                }
                let previous_pane = pane.clone();
                let Some(rows) =
                    apply_rows(&mut surface.frame, pane.inner_rect, &collected_pane.patch)
                else {
                    fallback!("invalid_patch");
                };
                patch_rows.extend(rows);
                let Some(scrollbar_rows) = retained_scrollbar_patch(
                    &self.app,
                    &mut surface.frame,
                    pane,
                    collected_pane.alternate_screen_active,
                    collected_pane.scroll_metrics,
                ) else {
                    fallback!("scrollbar_patch");
                };
                patch_rows.extend(scrollbar_rows);
                pane.content_revision = collected_pane.content_revision;
                pane.mouse_reporting = collected_pane.mouse_reporting;
                pane.sgr_pixel_mouse = collected_pane.sgr_pixel_mouse;
                pane.alternate_screen_active = collected_pane.alternate_screen_active;
                pane.scroll = collected_pane.scroll_metrics.map(|metrics| {
                    protocol::PaneSurfaceScrollMetrics {
                        offset_from_bottom: metrics.offset_from_bottom as u64,
                        max_offset_from_bottom: metrics.max_offset_from_bottom as u64,
                        viewport_rows: metrics.viewport_rows as u64,
                    }
                });
                metadata_changed |= *pane != previous_pane;
                changed_panes.push(pane.clone());
            }

            let cursor = retained_cursor(&self.app, &surface.panes);
            let cursor_changed = cursor != surface.frame.cursor;
            surface.frame.cursor = cursor.clone();
            if patch_rows.is_empty() && !cursor_changed && !metadata_changed {
                continue;
            }
            let patch = protocol::PaneSurfacePatch {
                boot_id: self.client_shell_boot_id.clone(),
                projection_revision,
                base_surface_revision,
                surface_revision: 0,
                rows: patch_rows,
                panes: changed_panes,
                cursor,
            };
            updates.push(RetainedRecipientUpdate {
                client_id,
                surface,
                patch,
            });
        }
        if updates.is_empty() {
            success!("unchanged");
        }

        let mut sent = 0u64;
        let mut deferred = 0u64;
        let mut disconnected = Vec::new();
        for update in updates {
            let RetainedRecipientUpdate {
                client_id,
                surface,
                patch,
            } = update;
            let Some(client) = self.clients.get_mut(&client_id) else {
                continue;
            };
            let Some(writer) = client.writer.as_ref().cloned() else {
                client.defer_full_render();
                deferred += 1;
                continue;
            };
            let Some(prepared) = client
                .render_state
                .prepare_pane_surface_patch(patch, surface)
            else {
                client.defer_full_render();
                deferred += 1;
                continue;
            };
            let serialized = match Self::frame_server_message(prepared.message()) {
                Ok(serialized) => serialized,
                Err(error) => {
                    warn!(
                        client_id,
                        %error,
                        "failed to serialize retained pane surface patch"
                    );
                    client.defer_full_render();
                    deferred += 1;
                    continue;
                }
            };
            crate::render_prof::counter("retained_surface.bytes", serialized.len() as u64);
            match writer.render.try_send(serialized) {
                Ok(()) => {
                    client.clear_deferred_render();
                    client.render_state.commit_sent_frame(prepared);
                    sent += 1;
                }
                Err(std::sync::mpsc::TrySendError::Full(_)) => {
                    client.defer_full_render();
                    deferred += 1;
                }
                Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                    disconnected.push(client_id);
                }
            }
        }
        for client_id in disconnected {
            self.remove_client_and_resize_if_needed(client_id);
        }
        crate::render_prof::counter("retained_surface.recipients.sent", sent);
        crate::render_prof::counter("retained_surface.recipients.deferred", deferred);
        if sent > 0 {
            success!("sent");
        }
        success!("recovery_queued");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(symbol: &str) -> protocol::CellData {
        protocol::CellData {
            symbol: symbol.to_owned(),
            fg: 0,
            bg: 0,
            modifier: 0,
            skip: false,
            hyperlink: None,
        }
    }

    #[test]
    fn retained_rows_send_only_changed_cell_spans() {
        let mut frame = FrameData {
            width: 6,
            height: 2,
            cells: vec![cell(" "); 12],
            cursor: None,
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };
        let patch = crate::pane::TerminalDirtyPatch {
            rows: vec![(0, vec![cell(" "), cell("x"), cell("y"), cell(" ")])],
        };

        let rows = apply_rows(
            &mut frame,
            protocol::SurfaceRect {
                x: 1,
                y: 1,
                width: 4,
                height: 1,
            },
            &patch,
        )
        .expect("valid patch");

        assert_eq!(
            rows,
            vec![protocol::PaneSurfacePatchRow {
                x: 2,
                y: 1,
                cells: vec![cell("x"), cell("y"), cell(" ")],
            }]
        );
        assert_eq!(frame.cells[8], cell("x"));
        assert_eq!(frame.cells[9], cell("y"));
    }

    #[test]
    fn retained_rows_include_the_cell_after_a_width_transition() {
        let mut frame = FrameData {
            width: 3,
            height: 1,
            cells: vec![cell("界"), cell("z"), cell("q")],
            cursor: None,
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };
        let patch = crate::pane::TerminalDirtyPatch {
            rows: vec![(0, vec![cell("x"), cell("z"), cell("q")])],
        };

        let rows = apply_rows(
            &mut frame,
            protocol::SurfaceRect {
                x: 0,
                y: 0,
                width: 3,
                height: 1,
            },
            &patch,
        )
        .expect("valid patch");

        assert_eq!(
            rows,
            vec![protocol::PaneSurfacePatchRow {
                x: 0,
                y: 0,
                cells: vec![cell("x"), cell("z")],
            }]
        );
    }

    #[test]
    fn retained_rows_omit_unchanged_full_dirty_rows() {
        let mut frame = FrameData {
            width: 4,
            height: 2,
            cells: vec![cell(" "); 8],
            cursor: None,
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };
        let patch = crate::pane::TerminalDirtyPatch {
            rows: vec![(0, vec![cell(" "); 4]), (1, vec![cell(" "); 4])],
        };

        let rows = apply_rows(
            &mut frame,
            protocol::SurfaceRect {
                x: 0,
                y: 0,
                width: 4,
                height: 2,
            },
            &patch,
        )
        .expect("valid patch");

        assert!(rows.is_empty());
    }
}
