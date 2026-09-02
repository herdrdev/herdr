use super::*;

fn restore_mode_bar(
    frame: &mut FrameData,
    bar: Option<Rect>,
    cells: Option<&[crate::protocol::CellData]>,
) {
    let (Some(bar), Some(cells)) = (bar, cells) else {
        return;
    };
    let start = usize::from(bar.y) * usize::from(frame.width) + usize::from(bar.x);
    frame.cells[start..start + usize::from(bar.width)].clone_from_slice(cells);
    if frame
        .cursor
        .as_ref()
        .is_some_and(|cursor| cursor.y == bar.y)
    {
        frame.cursor = None;
    }
}

impl ClientShellState {
    pub(crate) fn compose(&mut self, cols: u16, rows: u16) -> Option<FrameData> {
        self.last_composed_size = Some((cols, rows));
        let snapshot = self.snapshot.as_deref()?;
        let surface = self.pane_surface.as_ref()?;
        if snapshot.revision != surface.projection_revision {
            return None;
        }
        let layout = self.layout(cols, rows);
        if self.last_tab_bar_width != Some(layout.tab_bar.width) {
            self.last_tab_bar_width = Some(layout.tab_bar.width);
            self.reveal_focused_tab = true;
        }
        let tab_drag_insert_index = match &self.chrome_drag {
            Some(ClientChromeDrag::Tab { insert_index, .. }) => *insert_index,
            _ => None,
        };
        let (dragged_workspace_id, workspace_drop_indicator_row) = match &self.chrome_drag {
            Some(ClientChromeDrag::Workspace {
                source_workspace_id,
                target,
            }) => (
                Some(source_workspace_id.as_str()),
                target.as_ref().map(|(_, row)| *row),
            ),
            _ => (None, None),
        };
        let mut buffer = Buffer::empty(Rect::new(0, 0, cols, rows));
        self.hits = render::render_shell(
            &mut buffer,
            layout,
            snapshot,
            &self.config,
            render::ShellRenderState {
                collapsed_groups: &self.collapsed_groups,
                workspace_scroll: &mut self.workspace_scroll,
                agent_scroll: &mut self.agent_scroll,
                tab_scroll: &mut self.tab_scroll,
                reveal_focused_tab: &mut self.reveal_focused_tab,
                sidebar_collapsed: self.sidebar_collapsed,
                sidebar_section_split: self.sidebar_section_split,
                tab_drag_insert_index,
                selected_workspace_id: (self.mode == ClientShellMode::Navigate)
                    .then_some(self.navigate_workspace_id.as_deref())
                    .flatten(),
                dragged_workspace_id,
                workspace_drop_indicator_row,
            },
        );
        self.hits.panes = surface
            .panes
            .iter()
            .map(|pane| PaneHit {
                rect: Rect::new(
                    layout.pane_surface.x.saturating_add(pane.rect.x),
                    layout.pane_surface.y.saturating_add(pane.rect.y),
                    pane.rect.width,
                    pane.rect.height,
                ),
                inner_rect: Rect::new(
                    layout.pane_surface.x.saturating_add(pane.inner_rect.x),
                    layout.pane_surface.y.saturating_add(pane.inner_rect.y),
                    pane.inner_rect.width,
                    pane.inner_rect.height,
                ),
                scrollbar_rect: pane.scrollbar_rect.map(|rect| {
                    Rect::new(
                        layout.pane_surface.x.saturating_add(rect.x),
                        layout.pane_surface.y.saturating_add(rect.y),
                        rect.width,
                        rect.height,
                    )
                }),
                scroll: pane.scroll.map(|metrics| crate::pane::ScrollMetrics {
                    offset_from_bottom: usize::try_from(metrics.offset_from_bottom)
                        .unwrap_or(usize::MAX),
                    max_offset_from_bottom: usize::try_from(metrics.max_offset_from_bottom)
                        .unwrap_or(usize::MAX),
                    viewport_rows: usize::try_from(metrics.viewport_rows).unwrap_or(usize::MAX),
                }),
                pane_id: pane.pane_id.clone(),
                popup: false,
                mouse_reporting: pane.mouse_reporting,
                sgr_pixel_mouse: pane.sgr_pixel_mouse,
                pixel_width: pane.pixel_width,
                pixel_height: pane.pixel_height,
            })
            .collect();
        let topology_signature = pane_surface_topology_signature(surface);
        self.hits.pane_splits = surface
            .splits
            .iter()
            .map(|split| PaneSplitHit {
                direction: split.direction,
                pos: match split.direction {
                    crate::protocol::PaneSurfaceSplitDirection::Horizontal => {
                        layout.pane_surface.x.saturating_add(split.pos)
                    }
                    crate::protocol::PaneSurfaceSplitDirection::Vertical => {
                        layout.pane_surface.y.saturating_add(split.pos)
                    }
                },
                area: Rect::new(
                    layout.pane_surface.x.saturating_add(split.area.x),
                    layout.pane_surface.y.saturating_add(split.area.y),
                    split.area.width,
                    split.area.height,
                ),
                hit_rect: Rect::new(
                    layout.pane_surface.x.saturating_add(split.hit_rect.x),
                    layout.pane_surface.y.saturating_add(split.hit_rect.y),
                    split.hit_rect.width,
                    split.hit_rect.height,
                ),
                path: split.path.clone(),
                topology_signature,
            })
            .collect();
        if !self.config.mouse_capture {
            self.hits.pane_splits.clear();
        }
        let mode_bar_area = if layout.mobile_header.is_empty()
            && self.config.tab_bar_position == TabBarPositionConfig::Bottom
            && !layout.tab_bar.is_empty()
        {
            layout.tab_bar
        } else {
            layout.pane_surface
        };
        let mobile_navigate_panel = !layout.mobile_header.is_empty()
            && self.mode == ClientShellMode::Navigate
            && self.endpoint_error.is_none();
        let mode_bar = if mobile_navigate_panel || self.overlay.is_some() {
            None
        } else {
            render::render_mode_bar(
                &mut buffer,
                mode_bar_area,
                self.mode,
                self.copy_mode.as_ref(),
                self.endpoint_error.as_deref(),
                snapshot.update_available.is_some(),
                &self.config.keybinds,
                &self.config.palette,
            )
        };
        if mode_bar == Some(layout.tab_bar) {
            self.hits.tabs.clear();
            self.hits.new_tab = Rect::default();
            self.hits.tab_scroll_left = Rect::default();
            self.hits.tab_scroll_right = Rect::default();
        }
        let mut frame = FrameData::from_ratatui_buffer_with_hyperlinks(&buffer, None, &[]);
        let mode_bar_cells = mode_bar.map(|bar| {
            let start = usize::from(bar.y) * usize::from(frame.width) + usize::from(bar.x);
            frame.cells[start..start + usize::from(bar.width)].to_vec()
        });
        blit_pane_surface(&mut frame, &surface.frame, layout.pane_surface);
        restore_mode_bar(&mut frame, mode_bar, mode_bar_cells.as_deref());
        let has_selection = self
            .selection
            .as_ref()
            .is_some_and(|selection| selection.is_visible());
        let has_search = self
            .copy_mode
            .as_ref()
            .is_some_and(|copy_mode| !copy_mode.search_matches.is_empty());
        if has_selection || has_search {
            let cursor = frame.cursor.clone();
            let mut composed = frame.to_ratatui_buffer()?;
            for hit in &self.hits.panes {
                let copy_surface_coherent =
                    client_copy_surface_coherent(self.copy_mode.as_ref(), hit);
                if copy_surface_coherent {
                    render_client_copy_search_highlights(
                        &mut composed,
                        self.copy_mode.as_ref(),
                        hit,
                        &self.config.palette,
                        false,
                    );
                }
                let selection_is_stale_copy_projection = !copy_surface_coherent
                    && self.copy_mode.as_ref().is_some_and(|copy_mode| {
                        copy_mode.pane_id == hit.pane_id
                            && self
                                .selection
                                .as_ref()
                                .is_some_and(|selection| selection.pane_id == hit.pane_id)
                    });
                if !selection_is_stale_copy_projection {
                    crate::ui::render_selection_highlight(
                        self.selection.as_ref(),
                        &mut composed,
                        &hit.pane_id,
                        hit.inner_rect,
                        hit.scroll,
                        &self.config.palette,
                        crate::terminal_theme::TerminalTheme::default(),
                    );
                }
                if copy_surface_coherent {
                    render_client_copy_search_highlights(
                        &mut composed,
                        self.copy_mode.as_ref(),
                        hit,
                        &self.config.palette,
                        true,
                    );
                }
            }
            frame.replace_from_ratatui_buffer_preserving_effects(&composed, cursor);
        }
        if self.mode == ClientShellMode::Copy {
            frame.cursor = None;
            if let Some(copy_mode) = self.copy_mode.as_ref() {
                if let Some(hit) = self.hits.panes.iter().find(|hit| {
                    hit.pane_id == copy_mode.pane_id
                        && client_copy_surface_coherent(Some(copy_mode), hit)
                }) {
                    let viewport_top = copy_mode
                        .max_offset_from_bottom
                        .saturating_sub(copy_mode.offset_from_bottom)
                        .min(u32::MAX as usize) as u32;
                    let viewport_row = copy_mode.cursor.row.saturating_sub(viewport_top);
                    if viewport_row < u32::from(hit.inner_rect.height)
                        && copy_mode.cursor.col < hit.inner_rect.width
                    {
                        let mut composed = frame.to_ratatui_buffer()?;
                        let x = hit.inner_rect.x + copy_mode.cursor.col;
                        let y = hit.inner_rect.y + viewport_row as u16;
                        composed[(x, y)].set_style(
                            Style::default()
                                .fg(match self.config.palette.panel_bg {
                                    ratatui::style::Color::Reset => self.config.palette.surface_dim,
                                    color => color,
                                })
                                .bg(self.config.palette.accent)
                                .add_modifier(Modifier::BOLD),
                        );
                        frame.replace_from_ratatui_buffer_preserving_effects(&composed, None);
                    } else {
                        frame.cursor = None;
                    }
                }
            }
        }
        restore_mode_bar(&mut frame, mode_bar, mode_bar_cells.as_deref());
        self.hits.notification_toast = Rect::default();
        let has_config_diagnostic = self.config_diagnostic.is_some();
        if has_config_diagnostic
            || self.visible_endpoint_notice.is_some()
            || self.visible_notification.is_some()
        {
            let cursor = frame.cursor.clone();
            let mut composed = frame.to_ratatui_buffer()?;
            if let Some(diagnostic) = self.config_diagnostic.as_deref() {
                let diagnostic_area = if layout.mobile_header.is_empty() {
                    Rect::new(0, 0, cols, rows)
                } else {
                    layout.pane_surface
                };
                crate::ui::render_config_diagnostic_buffer(
                    &mut composed,
                    diagnostic_area,
                    diagnostic,
                    &self.config.palette,
                );
            }
            if let Some(notice) = self.visible_endpoint_notice.as_ref() {
                self.hits.notification_toast = if layout.mobile_header.is_empty() {
                    notifications::render_endpoint_notice(
                        &mut composed,
                        Rect::new(0, 0, cols, rows),
                        notice,
                        u16::from(has_config_diagnostic),
                        &self.config.palette,
                    )
                } else {
                    notifications::render_mobile_endpoint_notice_banner(
                        &mut composed,
                        Rect::new(0, 0, cols, rows),
                        notice,
                        has_config_diagnostic,
                        &self.config.palette,
                    )
                };
            } else if let Some(notification) = self.visible_notification.as_ref() {
                self.hits.notification_toast = if layout.mobile_header.is_empty() {
                    notifications::render_visible_notification(
                        &mut composed,
                        Rect::new(0, 0, cols, rows),
                        notification,
                        self.config.toast_position,
                        u16::from(has_config_diagnostic),
                        &self.config.palette,
                    )
                } else {
                    notifications::render_mobile_notification_banner(
                        &mut composed,
                        Rect::new(0, 0, cols, rows),
                        notification,
                        has_config_diagnostic,
                        &self.config.palette,
                    )
                };
            }
            frame.replace_from_ratatui_buffer_preserving_effects(&composed, cursor);
        }
        if let Some(feedback) = self.copy_feedback.as_ref() {
            let cursor = frame.cursor.clone();
            let mut composed = frame.to_ratatui_buffer()?;
            let base_offset = u16::from(has_config_diagnostic);
            let feedback_area = if layout.mobile_header.is_empty() {
                layout.pane_surface
            } else {
                Rect::new(0, 0, cols, rows)
            };
            let offset = crate::ui::copy_feedback_offset_for_toast(
                feedback_area,
                feedback,
                base_offset,
                self.config.clipboard_toast_position,
                self.hits.notification_toast,
            );
            crate::ui::render_copy_feedback_buffer(
                &mut composed,
                feedback_area,
                feedback,
                offset,
                self.config.clipboard_toast_position,
                &self.config.palette,
            );
            frame.replace_from_ratatui_buffer_preserving_effects(&composed, cursor);
        }
        self.hits.popup = None;
        if let Some(popup) = surface.popup.as_deref() {
            let width = popup.width.map(client_popup_size);
            let height = popup.height.map(client_popup_size);
            if let Some(geometry) =
                crate::popup_size::resolve_popup_geometry(width, height, layout.pane_surface)
            {
                let mut composed = frame.to_ratatui_buffer()?;
                let block = ratatui::widgets::Block::default()
                    .borders(ratatui::widgets::Borders::ALL)
                    .border_style(ratatui::style::Style::default().fg(self.config.palette.accent))
                    .title(popup.title.clone())
                    .style(ratatui::style::Style::default().bg(self.config.palette.panel_bg));
                ratatui::widgets::Widget::render(
                    ratatui::widgets::Clear,
                    geometry.outer,
                    &mut composed,
                );
                ratatui::widgets::Widget::render(block, geometry.outer, &mut composed);
                frame.replace_from_ratatui_buffer_preserving_effects(&composed, None);
                blit_pane_surface(&mut frame, &popup.frame, geometry.inner);
                self.hits.popup = Some(PaneHit {
                    rect: geometry.outer,
                    inner_rect: geometry.inner,
                    scrollbar_rect: None,
                    scroll: None,
                    pane_id: popup.terminal_id.clone(),
                    popup: true,
                    mouse_reporting: popup.mouse_reporting,
                    sgr_pixel_mouse: popup.sgr_pixel_mouse,
                    pixel_width: popup.pixel_width,
                    pixel_height: popup.pixel_height,
                });
            }
        }
        if !layout.mobile_header.is_empty()
            && self.mode == ClientShellMode::Navigate
            && self.overlay.is_none()
        {
            let mut composed = frame.to_ratatui_buffer()?;
            super::mobile::render_mobile_switcher(
                &mut composed,
                Rect::new(0, 0, cols, rows),
                snapshot,
                &self.config,
                self.navigate_workspace_id.as_deref(),
                &mut self.mobile_switcher_scroll,
                &mut self.reveal_mobile_workspace,
                &mut self.hits,
            );
            frame.replace_from_ratatui_buffer_preserving_effects(&composed, None);
            self.hits.panes.clear();
            self.hits.pane_splits.clear();
            self.hits.popup = None;
        }
        restore_mode_bar(&mut frame, mode_bar, mode_bar_cells.as_deref());
        if let Some(overlay) = self.overlay.as_ref() {
            let mut composed = frame.to_ratatui_buffer()?;
            let cursor = if let ClientShellOverlay::ContextMenu(menu) = overlay {
                self.hits.context_menu_rows =
                    render::render_context_menu(&mut composed, menu, &self.config.palette)?;
                None
            } else if let ClientShellOverlay::GlobalMenu(menu) = overlay {
                self.hits.global_menu_rows = render::render_global_menu(
                    &mut composed,
                    self.hits.global_launcher,
                    menu,
                    snapshot,
                    &self.config.palette,
                )?;
                None
            } else {
                let rendered = render::render_client_overlay(
                    &mut composed,
                    overlay,
                    snapshot,
                    &self.config.keybinds,
                    &self.config.palette,
                )?;
                self.hits.overlay_primary = rendered.primary;
                self.hits.overlay_clear = rendered.clear;
                self.hits.overlay_cancel = rendered.cancel;
                self.hits.navigator_popup = rendered.navigator_popup;
                self.hits.navigator_search = rendered.navigator_search;
                self.hits.navigator_rows = rendered.navigator_rows;
                self.hits.worktree_search = rendered.worktree_search;
                self.hits.worktree_rows = rendered.worktree_rows;
                self.hits.help_popup = rendered.help_popup;
                self.hits.help_scrollbar = rendered.help_scrollbar;
                self.hits.help_scroll_metrics = rendered.help_scroll_metrics;
                self.hits.help_max_scroll = rendered.help_max_scroll;
                self.hits.settings_popup = rendered.settings_popup;
                self.hits.settings_tabs = rendered.settings_tabs;
                self.hits.settings_choices = rendered.settings_choices;
                self.hits.product_announcement_scrollbar = rendered.product_announcement_scrollbar;
                self.hits.product_announcement_scroll_metrics =
                    rendered.product_announcement_scroll_metrics;
                self.hits.product_announcement_max_scroll =
                    rendered.product_announcement_max_scroll;
                self.hits.release_notes_scrollbar = rendered.release_notes_scrollbar;
                self.hits.release_notes_scroll_metrics = rendered.release_notes_scroll_metrics;
                self.hits.release_notes_max_scroll = rendered.release_notes_max_scroll;
                rendered.cursor
            };
            frame.replace_from_ratatui_buffer_preserving_effects(&composed, cursor);
        }
        if let Some(ClientShellOverlay::Help(help)) = self.overlay.as_mut() {
            help.scroll = help.scroll.min(self.hits.help_max_scroll);
        }
        if let Some(ClientShellOverlay::ProductAnnouncement(announcement)) = self.overlay.as_mut() {
            announcement.scroll = announcement
                .scroll
                .min(u16::try_from(self.hits.product_announcement_max_scroll).unwrap_or(u16::MAX));
        }
        if let Some(ClientShellOverlay::ReleaseNotes(notes)) = self.overlay.as_mut() {
            notes.scroll = notes
                .scroll
                .min(u16::try_from(self.hits.release_notes_max_scroll).unwrap_or(u16::MAX));
        }
        self.compose_graphics(&mut frame, layout);
        Some(frame)
    }
}

fn client_copy_surface_coherent(copy_mode: Option<&ClientCopyModeState>, hit: &PaneHit) -> bool {
    copy_mode
        .filter(|copy_mode| copy_mode.pane_id == hit.pane_id)
        .is_none_or(|copy_mode| {
            copy_mode.geometry == (hit.inner_rect.width, hit.inner_rect.height)
                && hit.scroll.is_some_and(|scroll| {
                    scroll.offset_from_bottom == copy_mode.offset_from_bottom
                        && scroll.max_offset_from_bottom == copy_mode.max_offset_from_bottom
                })
        })
}

fn render_client_copy_search_highlights(
    buffer: &mut Buffer,
    copy_mode: Option<&ClientCopyModeState>,
    hit: &PaneHit,
    palette: &Palette,
    current_only: bool,
) {
    let Some(copy_mode) = copy_mode.filter(|copy_mode| copy_mode.pane_id == hit.pane_id) else {
        return;
    };
    if hit.inner_rect.is_empty() {
        return;
    }
    let top = copy_mode
        .max_offset_from_bottom
        .saturating_sub(copy_mode.offset_from_bottom)
        .min(u32::MAX as usize) as u32;
    let bottom = top.saturating_add(u32::from(hit.inner_rect.height.saturating_sub(1)));
    let style = if current_only {
        Style::default()
            .fg(panel_contrast_fg(palette))
            .bg(palette.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(palette.text).bg(palette.surface1)
    };
    for (index, text_match) in copy_mode.search_matches.iter().enumerate() {
        if (copy_mode.search_current == Some(index)) != current_only
            || text_match.end.row < top
            || text_match.start.row > bottom
        {
            continue;
        }
        let start_row = text_match.start.row.max(top);
        let end_row = text_match.end.row.min(bottom);
        for absolute_row in start_row..=end_row {
            let viewport_row = absolute_row.saturating_sub(top) as u16;
            let start_col = if absolute_row == text_match.start.row {
                text_match.start.col
            } else {
                0
            };
            let end_col = if absolute_row == text_match.end.row {
                text_match.end.col
            } else {
                hit.inner_rect.width.saturating_sub(1)
            };
            for col in start_col..=end_col.min(hit.inner_rect.width.saturating_sub(1)) {
                buffer[(
                    hit.inner_rect.x.saturating_add(col),
                    hit.inner_rect.y.saturating_add(viewport_row),
                )]
                    .set_style(style);
            }
        }
    }
}

fn client_popup_size(size: crate::protocol::ClientShellPopupSize) -> crate::popup_size::PopupSize {
    match size {
        crate::protocol::ClientShellPopupSize::Cells(cells) => {
            crate::popup_size::PopupSize::Cells(cells)
        }
        crate::protocol::ClientShellPopupSize::Percent(percent) => {
            crate::popup_size::PopupSize::Percent(percent)
        }
    }
}
