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

#[inline(always)]
fn cell_equals_with_link_map(
    existing: &protocol::CellData,
    desired: &protocol::CellData,
    link_map: &[u32],
) -> bool {
    if link_map.is_empty() {
        existing == desired
    } else {
        let d_link = desired
            .hyperlink
            .and_then(|idx| link_map.get(idx as usize).copied());
        existing.symbol == desired.symbol
            && existing.fg == desired.fg
            && existing.bg == desired.bg
            && existing.modifier == desired.modifier
            && existing.hyperlink == d_link
    }
}

#[inline(always)]
fn materialize_cells_with_link_map(
    desired: &[protocol::CellData],
    link_map: &[u32],
) -> Vec<protocol::CellData> {
    if link_map.is_empty() {
        desired.to_vec()
    } else {
        desired
            .iter()
            .map(|cell| {
                let mut c = cell.clone();
                if let Some(idx) = c.hyperlink {
                    c.hyperlink = link_map.get(idx as usize).copied();
                }
                c
            })
            .collect()
    }
}

fn patch_row_changed(frame: &FrameData, row: &protocol::PaneSurfacePatchRow) -> Option<bool> {
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
    Some(frame.cells[start..end] != row.cells)
}

fn changed_rows(
    frame: &FrameData,
    area: protocol::SurfaceRect,
    patch: &crate::pane::TerminalDirtyPatch,
    link_map: &[u32],
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
            if cell_equals_with_link_map(&existing[offset], &desired[offset], link_map) {
                offset += 1;
                continue;
            }
            let start = offset;
            offset += 1;
            while offset < width
                && !cell_equals_with_link_map(&existing[offset], &desired[offset], link_map)
            {
                offset += 1;
            }
            // Include the following cell so a wide-to-narrow (or
            // narrow-to-wide) transition repaints content covered by the old
            // grapheme width even when that logical neighbor is unchanged.
            let end = offset.saturating_add(1).min(width);
            rows.push(protocol::PaneSurfacePatchRow {
                x: area.x.checked_add(u16::try_from(start).ok()?)?,
                y,
                cells: materialize_cells_with_link_map(&desired[start..end], link_map),
            });
            offset = end;
        }
    }
    Some(rows)
}

fn detect_pane_row_move(
    frame: &FrameData,
    pane: &protocol::PaneSurfacePane,
    patch: &crate::pane::TerminalDirtyPatch,
    link_map: &[u32],
) -> Option<(protocol::PaneRowMove, Vec<protocol::PaneSurfacePatchRow>)> {
    if patch.rows.len() <= 1 {
        return None;
    }
    let height = usize::from(pane.inner_rect.height);
    let width = usize::from(pane.inner_rect.width);
    if height < 4 || width == 0 {
        return None;
    }

    let frame_width = usize::from(frame.width);
    let pane_x = usize::from(pane.inner_rect.x);
    let pane_y = usize::from(pane.inner_rect.y);

    let mut best_candidate = None;
    let mut best_saved = 0usize;

    for d in [-1isize, 1, -2, 2] {
        let (src_start, dst_start, max_count) = if d < 0 {
            let k = (-d) as usize;
            if k >= height {
                continue;
            }
            (k, 0, height - k)
        } else {
            let k = d as usize;
            if k >= height {
                continue;
            }
            (0, k, height - k)
        };

        let mut current_run_start = 0;
        let mut current_run_len = 0;
        let mut current_run_changed = 0;

        for i in 0..max_count {
            let src_local = src_start + i;
            let dst_local = dst_start + i;

            let src_frame_start = (pane_y + src_local) * frame_width + pane_x;
            let Some(src_cells) = frame.cells.get(src_frame_start..src_frame_start + width) else {
                current_run_len = 0;
                current_run_changed = 0;
                continue;
            };

            let dst_frame_start = (pane_y + dst_local) * frame_width + pane_x;
            let Some(dst_cells) = frame.cells.get(dst_frame_start..dst_frame_start + width) else {
                current_run_len = 0;
                current_run_changed = 0;
                continue;
            };

            let desired_cells =
                if let Some((_, row)) = patch.rows.iter().find(|(y, _)| *y == dst_local as u16) {
                    if row.len() < width {
                        current_run_len = 0;
                        current_run_changed = 0;
                        continue;
                    }
                    &row[..width]
                } else {
                    dst_cells
                };

            let mut matching = true;
            for col in 0..width {
                if !cell_equals_with_link_map(&src_cells[col], &desired_cells[col], link_map) {
                    matching = false;
                    break;
                }
            }

            if matching {
                let dst_changed = (0..width).any(|col| {
                    !cell_equals_with_link_map(&dst_cells[col], &desired_cells[col], link_map)
                });
                if current_run_len == 0 {
                    current_run_start = i;
                    current_run_len = 1;
                    current_run_changed = if dst_changed { 1 } else { 0 };
                } else {
                    current_run_len += 1;
                    if dst_changed {
                        current_run_changed += 1;
                    }
                }
                if current_run_changed >= 2 && current_run_changed > best_saved {
                    best_saved = current_run_changed;
                    let run_src = src_start + current_run_start;
                    let run_dst = dst_start + current_run_start;
                    let src_y = pane.inner_rect.y + run_src as u16;
                    let dst_y = pane.inner_rect.y + run_dst as u16;
                    best_candidate = Some((
                        protocol::PaneRowMove {
                            pane_id: pane.pane_id.clone(),
                            src_y,
                            dst_y,
                            count: current_run_len as u16,
                        },
                        d,
                        run_dst,
                        current_run_len,
                    ));
                }
            } else {
                current_run_len = 0;
                current_run_changed = 0;
            }
        }
    }

    let (row_move, d, run_dst_start, run_count) = best_candidate?;

    let mut residual_spans = Vec::new();

    for local_y in 0..height {
        let frame_start = (pane_y + local_y) * frame_width + pane_x;
        let Some(dst_cells) = frame.cells.get(frame_start..frame_start + width) else {
            continue;
        };
        let desired = if let Some((_, row)) = patch.rows.iter().find(|(y, _)| *y == local_y as u16)
        {
            if row.len() < width {
                continue;
            }
            &row[..width]
        } else {
            dst_cells
        };

        let post_move_retained = if local_y >= run_dst_start && local_y < run_dst_start + run_count
        {
            let src_local = if d < 0 {
                local_y + ((-d) as usize)
            } else {
                local_y - (d as usize)
            };
            let src_frame_start = (pane_y + src_local) * frame_width + pane_x;
            frame
                .cells
                .get(src_frame_start..src_frame_start + width)
                .unwrap_or(dst_cells)
        } else {
            dst_cells
        };

        let mut offset = 0;
        let y = pane.inner_rect.y + local_y as u16;
        while offset < width {
            if cell_equals_with_link_map(&post_move_retained[offset], &desired[offset], link_map) {
                offset += 1;
                continue;
            }
            let start = offset;
            offset += 1;
            while offset < width
                && !cell_equals_with_link_map(
                    &post_move_retained[offset],
                    &desired[offset],
                    link_map,
                )
            {
                offset += 1;
            }
            let end = offset.saturating_add(1).min(width);
            residual_spans.push(protocol::PaneSurfacePatchRow {
                x: pane.inner_rect.x + start as u16,
                y,
                cells: materialize_cells_with_link_map(&desired[start..end], link_map),
            });
            offset = end;
        }
    }

    Some((row_move, residual_spans))
}
fn retained_scrollbar_patch(
    app: &app::App,
    frame: &FrameData,
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
        if patch_row_changed(frame, &row)? {
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

struct RetainedRecipient<'a> {
    client_id: u64,
    surface: &'a protocol::PaneSurfaceFrame,
}

struct CollectedPanePatch {
    pane_id: String,
    patch: crate::pane::TerminalDirtyPatch,
    content_revision: u64,
    scroll_metrics: Option<crate::pane::ScrollMetrics>,
    mouse_reporting: bool,
    sgr_pixel_mouse: bool,
    alternate_screen_active: bool,
    graphics_may_have_placements: bool,
}

enum RetainedRecipientPayload {
    Patch(protocol::PaneSurfacePatch),
    Delta(Box<crate::protocol::delta::ClientShellSurfaceDelta>),
}

struct RetainedRecipientUpdate {
    client_id: u64,
    payload: RetainedRecipientPayload,
    graphics: Option<(
        protocol::PaneSurfaceFrame,
        crate::kitty_graphics::surface::DeliveryCache,
    )>,
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
        let mut targets = render_targets(&self.clients, self.foreground_client_id);
        targets.retain(|(client_id, _, _, _, mode)| {
            !matches!(mode, ClientConnectionMode::ClientShell)
                || self
                    .clients
                    .get(client_id)
                    .is_some_and(|client| client.shell_surface_active)
        });
        if targets.is_empty() {
            success!("no_active_surface");
        }
        if targets
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
            if client.render_state.requires_recompute() {
                fallback!("recompute_pending");
            }
            let is_v2 = client.surface_codec == crate::protocol::endpoint::SURFACE_CODEC_DELTA_V2;
            let Some(surface) = client.render_state.last_pane_surface() else {
                fallback!("no_baseline");
            };
            if surface.boot_id != self.client_shell_boot_id
                || (!is_v2 && surface.projection_revision != client.shell_projection_revision)
                || surface.frame.width != *cols
                || surface.frame.height != *rows
                || surface.popup.is_some()
                || !surface.graphics.assets.is_empty()
                || !surface.frame.graphics.is_empty()
            {
                fallback!("baseline_mismatch");
            }
            recipients.push(RetainedRecipient {
                client_id: *client_id,
                surface,
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
            let Some(snapshot) = runtime.collect_dirty_patch_snapshot(width, height) else {
                fallback!("terminal_snapshot");
            };
            let patch = match snapshot.patch {
                crate::pane::TerminalDirtyPatchOutcome::Clean => {
                    crate::render_prof::event("retained_surface.pane_clean");
                    crate::pane::TerminalDirtyPatch { rows: Vec::new() }
                }
                crate::pane::TerminalDirtyPatchOutcome::Patch(patch) => patch,
                crate::pane::TerminalDirtyPatchOutcome::Fallback => {
                    fallback!("terminal_patch");
                }
            };
            collected.push(CollectedPanePatch {
                pane_id: public_pane_id,
                patch,
                content_revision: snapshot.content_revision,
                scroll_metrics: snapshot.scroll_metrics,
                mouse_reporting: snapshot.mouse_reporting,
                sgr_pixel_mouse: snapshot.sgr_pixel_mouse,
                alternate_screen_active: snapshot.alternate_screen_active,
                graphics_may_have_placements: snapshot.graphics_may_have_placements,
            });
        }

        let mut updates = Vec::with_capacity(recipients.len());
        for recipient in recipients {
            let client_id = recipient.client_id;
            let surface = recipient.surface;
            let is_v2 = self.clients.get(&client_id).is_some_and(|c| {
                c.surface_codec == crate::protocol::endpoint::SURFACE_CODEC_DELTA_V2
            });
            let mut panes = surface.panes.clone();
            let projection_revision = self
                .clients
                .get(&client_id)
                .map_or(surface.projection_revision, |c| c.shell_projection_revision);
            let base_surface_revision = surface.surface_revision;
            let mut changed_panes = Vec::with_capacity(collected.len());
            let mut changed_panes_delta = Vec::with_capacity(collected.len());
            let mut patch_rows = Vec::new();
            let mut row_moves = Vec::new();
            let mut metadata_changed = false;
            let had_graphics = !surface.graphics.placements.is_empty()
                || !surface.graphics.retained_assets.is_empty();
            let mut may_have_graphics = false;
            let appended_hyperlinks = Vec::new();
            for collected_pane in &collected {
                let Some(pane) = panes
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
                may_have_graphics |= collected_pane.graphics_may_have_placements;
                let previous_pane = pane.clone();
                let link_map = Vec::new();
                if is_v2 {
                    if let Some((row_move, residual)) =
                        detect_pane_row_move(&surface.frame, pane, &collected_pane.patch, &link_map)
                    {
                        row_moves.push(row_move);
                        patch_rows.extend(residual);
                    } else if let Some(rows) = changed_rows(
                        &surface.frame,
                        pane.inner_rect,
                        &collected_pane.patch,
                        &link_map,
                    ) {
                        patch_rows.extend(rows);
                    } else {
                        fallback!("invalid_patch");
                    }
                } else {
                    let Some(rows) =
                        changed_rows(&surface.frame, pane.inner_rect, &collected_pane.patch, &[])
                    else {
                        fallback!("invalid_patch");
                    };
                    patch_rows.extend(rows);
                }
                let Some(scrollbar_rows) = retained_scrollbar_patch(
                    &self.app,
                    &surface.frame,
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
                metadata_changed |= !previous_pane.wire_visible_eq(pane);
                if is_v2 {
                    if let Some(pane_delta) =
                        crate::protocol::delta::PaneSurfacePaneDelta::diff(&previous_pane, pane)
                    {
                        changed_panes_delta.push(pane_delta);
                    }
                }
                if !previous_pane.wire_visible_eq(pane) {
                    changed_panes.push(pane.clone());
                }
            }

            let cursor = retained_cursor(&self.app, &panes);
            let cursor_changed = cursor != surface.frame.cursor;
            let patch_is_empty = patch_rows.is_empty() && row_moves.is_empty();
            let rebind_pending = is_v2 && projection_revision != surface.projection_revision;
            let payload = if is_v2 {
                let delta = crate::protocol::delta::ClientShellSurfaceDelta {
                    boot_id: self.client_shell_boot_id.clone(),
                    projection_revision,
                    base_surface_revision,
                    surface_revision: 0,
                    spans: patch_rows,
                    row_moves,
                    panes: changed_panes_delta,
                    splits: None,
                    cursor: crate::protocol::delta::SurfaceFieldUpdate::diff(
                        &surface.frame.cursor,
                        &cursor,
                    ),
                    appended_hyperlinks: appended_hyperlinks.clone(),
                    graphics: None,
                    popup: None,
                };
                match delta.into_legacy_patch(surface) {
                    Ok(patch) => RetainedRecipientPayload::Patch(patch),
                    Err(delta) => RetainedRecipientPayload::Delta(Box::new(delta)),
                }
            } else {
                let patch = protocol::PaneSurfacePatch {
                    boot_id: self.client_shell_boot_id.clone(),
                    projection_revision,
                    base_surface_revision,
                    surface_revision: 0,
                    rows: patch_rows,
                    panes: changed_panes,
                    cursor,
                };
                RetainedRecipientPayload::Patch(patch)
            };
            let refresh_graphics = had_graphics || may_have_graphics;
            let mut graphics_changed = false;
            let graphics = if refresh_graphics {
                let Some(target) = self.shell_target_for_client(client_id) else {
                    fallback!("graphics_target");
                };
                let client = &self.clients[&client_id];
                let mut next_surface = surface.clone();
                match &payload {
                    RetainedRecipientPayload::Patch(p) => {
                        crate::server::render_stream::apply_pane_surface_patch(
                            &mut next_surface,
                            p,
                        );
                    }
                    RetainedRecipientPayload::Delta(d) => {
                        let mut scene = crate::protocol::SurfaceGraphicsScene::default();
                        let _ = d.apply_to(&mut next_surface, &mut scene);
                    }
                }
                let Some((graphics, delivery)) =
                    crate::server::client_shell_graphics::collect_retained(
                        &self.app,
                        &next_surface,
                        target,
                        client.cell_size,
                        &client.shell_graphics_delivery,
                        client_id,
                    )
                else {
                    fallback!("graphics_geometry");
                };
                graphics_changed = graphics != surface.graphics;
                next_surface.graphics = graphics;
                Some((next_surface, delivery))
            } else {
                None
            };
            if patch_is_empty
                && !cursor_changed
                && !metadata_changed
                && !graphics_changed
                && appended_hyperlinks.is_empty()
            {
                // Metadata-only projection rebinds are applied locally on snapshot
                // apply; do not emit an empty surface frame.
                let _ = rebind_pending;
                continue;
            }
            updates.push(RetainedRecipientUpdate {
                client_id,
                payload,
                graphics,
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
                payload,
                graphics,
            } = update;
            let Some(client) = self.clients.get_mut(&client_id) else {
                continue;
            };
            let Some(writer) = client.writer.as_ref().cloned() else {
                client.defer_full_render();
                deferred += 1;
                continue;
            };
            let (prepared, graphics_delivery) = if let Some((surface, delivery)) = graphics {
                (
                    client.render_state.prepare_pane_surface(surface),
                    Some(delivery),
                )
            } else {
                match payload {
                    RetainedRecipientPayload::Patch(patch) => {
                        (client.render_state.prepare_pane_surface_patch(patch), None)
                    }
                    RetainedRecipientPayload::Delta(delta) => {
                        (client.render_state.prepare_pane_surface_delta(*delta), None)
                    }
                }
            };
            let Some(prepared) = prepared else {
                client.defer_full_render();
                deferred += 1;
                continue;
            };
            let max_frame_size = if graphics_delivery.is_some() {
                MAX_GRAPHICS_FRAME_SIZE
            } else {
                protocol::MAX_FRAME_SIZE
            };
            let serialized =
                match Self::frame_server_message_with_max(prepared.message(), max_frame_size) {
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
                    let graphics_pending = graphics_delivery
                        .as_ref()
                        .is_some_and(crate::kitty_graphics::surface::DeliveryCache::has_pending);
                    if let Some(delivery) = graphics_delivery {
                        client.shell_graphics_delivery = delivery;
                    }
                    if graphics_pending {
                        client.defer_full_render();
                    } else {
                        client.clear_deferred_render();
                    }
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
        let frame = FrameData {
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

        let rows = changed_rows(
            &frame,
            protocol::SurfaceRect {
                x: 1,
                y: 1,
                width: 4,
                height: 1,
            },
            &patch,
            &[],
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
        assert_eq!(frame.cells, vec![cell(" "); 12], "planning must not commit");
    }

    #[test]
    fn retained_rows_include_the_cell_after_a_width_transition() {
        let frame = FrameData {
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

        let rows = changed_rows(
            &frame,
            protocol::SurfaceRect {
                x: 0,
                y: 0,
                width: 3,
                height: 1,
            },
            &patch,
            &[],
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
        let frame = FrameData {
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

        let rows = changed_rows(
            &frame,
            protocol::SurfaceRect {
                x: 0,
                y: 0,
                width: 4,
                height: 2,
            },
            &patch,
            &[],
        )
        .expect("valid patch");

        assert!(rows.is_empty());
    }

    #[test]
    fn row_move_detects_scroll_and_emits_only_residual_spans() {
        let frame = FrameData {
            width: 4,
            height: 4,
            cells: vec![
                cell("0"),
                cell("0"),
                cell("0"),
                cell("0"),
                cell("1"),
                cell("1"),
                cell("1"),
                cell("1"),
                cell("2"),
                cell("2"),
                cell("2"),
                cell("2"),
                cell("3"),
                cell("3"),
                cell("3"),
                cell("3"),
            ],
            cursor: None,
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };
        let pane = protocol::PaneSurfacePane {
            pane_id: "pane-scroll".into(),
            content_revision: 1,
            scroll: None,
            focused: true,
            mouse_reporting: false,
            sgr_pixel_mouse: false,
            alternate_screen_active: false,
            scrollbar_rect: None,
            rect: protocol::SurfaceRect {
                x: 0,
                y: 0,
                width: 4,
                height: 4,
            },
            inner_rect: protocol::SurfaceRect {
                x: 0,
                y: 0,
                width: 4,
                height: 4,
            },
            pixel_width: 100,
            pixel_height: 100,
        };
        // After 1-line scroll up: row 0 becomes '1', row 1 becomes '2', row 2 becomes '3', row 3 has new line 'N'
        let patch = crate::pane::TerminalDirtyPatch {
            rows: vec![
                (0, vec![cell("1"), cell("1"), cell("1"), cell("1")]),
                (1, vec![cell("2"), cell("2"), cell("2"), cell("2")]),
                (2, vec![cell("3"), cell("3"), cell("3"), cell("3")]),
                (3, vec![cell("N"), cell("N"), cell("N"), cell("N")]),
            ],
        };
        let (row_move, residual) =
            detect_pane_row_move(&frame, &pane, &patch, &[]).expect("row move detected");
        assert_eq!(row_move.src_y, 1);
        assert_eq!(row_move.dst_y, 0);
        assert_eq!(row_move.count, 3);
        // Residual spans only contain row 3 ('N's)
        assert_eq!(residual.len(), 1);
        assert_eq!(residual[0].y, 3);
        assert_eq!(
            residual[0].cells,
            vec![cell("N"), cell("N"), cell("N"), cell("N")]
        );
    }

    #[test]
    fn row_move_detects_scroll_with_shared_prefix_and_trailing_blanks() {
        let frame = FrameData {
            width: 8,
            height: 4,
            cells: vec![
                cell("P"),
                cell("0"),
                cell("_"),
                cell("1"),
                cell("."),
                cell("."),
                cell(" "),
                cell(" "),
                cell("P"),
                cell("0"),
                cell("_"),
                cell("2"),
                cell("."),
                cell("."),
                cell(" "),
                cell(" "),
                cell("P"),
                cell("0"),
                cell("_"),
                cell("3"),
                cell("."),
                cell("."),
                cell(" "),
                cell(" "),
                cell("P"),
                cell("0"),
                cell("_"),
                cell("4"),
                cell("."),
                cell("."),
                cell(" "),
                cell(" "),
            ],
            cursor: None,
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };
        let pane = protocol::PaneSurfacePane {
            pane_id: "pane-scroll-realistic".into(),
            content_revision: 1,
            scroll: None,
            focused: true,
            mouse_reporting: false,
            sgr_pixel_mouse: false,
            alternate_screen_active: false,
            scrollbar_rect: None,
            rect: protocol::SurfaceRect {
                x: 0,
                y: 0,
                width: 8,
                height: 4,
            },
            inner_rect: protocol::SurfaceRect {
                x: 0,
                y: 0,
                width: 8,
                height: 4,
            },
            pixel_width: 100,
            pixel_height: 100,
        };
        // After 1-line scroll up with trailing blanks and prefix sharing:
        let patch = crate::pane::TerminalDirtyPatch {
            rows: vec![
                (
                    0,
                    vec![
                        cell("P"),
                        cell("0"),
                        cell("_"),
                        cell("2"),
                        cell("."),
                        cell("."),
                        cell(" "),
                        cell(" "),
                    ],
                ),
                (
                    1,
                    vec![
                        cell("P"),
                        cell("0"),
                        cell("_"),
                        cell("3"),
                        cell("."),
                        cell("."),
                        cell(" "),
                        cell(" "),
                    ],
                ),
                (
                    2,
                    vec![
                        cell("P"),
                        cell("0"),
                        cell("_"),
                        cell("4"),
                        cell("."),
                        cell("."),
                        cell(" "),
                        cell(" "),
                    ],
                ),
                (
                    3,
                    vec![
                        cell("P"),
                        cell("0"),
                        cell("_"),
                        cell("5"),
                        cell("."),
                        cell("."),
                        cell(" "),
                        cell(" "),
                    ],
                ),
            ],
        };
        let (row_move, residual) = detect_pane_row_move(&frame, &pane, &patch, &[])
            .expect("row move detected with blanks");
        assert_eq!(row_move.src_y, 1);
        assert_eq!(row_move.dst_y, 0);
        assert_eq!(residual.len(), 1);
        assert_eq!(residual[0].x, 3);
        assert_eq!(residual[0].y, 3);
        assert_eq!(residual[0].cells, vec![cell("5"), cell(".")]);
    }
}
