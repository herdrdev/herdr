use super::*;

/// Held second press. Keep only one row read in flight and use the latest
/// pointer position when it returns, so remote latency cannot queue up motion.
#[derive(Debug)]
pub(super) struct ClientWordSelection {
    pub(super) pane_id: String,
    pub(super) focus_confirmed: bool,
    anchor: (u32, u16),
    anchor_bounds: Option<(u16, u16)>,
    cursor: (u32, u16),
    end_col: u16,
    content_revision: Option<u64>,
    cached_row: Option<(u32, String)>,
    pending_row: Option<u32>,
    pub(super) dragged: bool,
    pub(super) released: bool,
}

impl ClientShellState {
    pub(super) fn request_word_selection(
        &mut self,
        hit: &PaneHit,
        viewport_row: u16,
        col: u16,
        outcome: &mut ClientShellInput,
    ) {
        let row = crate::selection::absolute_row_for_viewport(viewport_row, hit.scroll);
        self.word_selection_generation = self.word_selection_generation.saturating_add(1);
        self.word_selection_gesture = Some(ClientWordSelection {
            pane_id: hit.pane_id.clone(),
            focus_confirmed: self
                .snapshot
                .as_deref()
                .and_then(|snapshot| snapshot.focused_pane_id.as_deref())
                == Some(hit.pane_id.as_str()),
            anchor: (row, col),
            anchor_bounds: None,
            cursor: (row, col),
            end_col: hit.inner_rect.width.saturating_sub(1),
            content_revision: self.pane_surface.as_ref().and_then(|surface| {
                surface
                    .panes
                    .iter()
                    .find(|pane| pane.pane_id == hit.pane_id)
                    .map(|pane| pane.content_revision)
            }),
            cached_row: None,
            pending_row: None,
            dragged: false,
            released: false,
        });
        self.request_word_selection_row(row, outcome);
    }

    fn cancel_word_selection(&mut self) {
        self.word_selection_gesture = None;
        self.selection = None;
        self.stop_selection_autoscroll();
    }

    fn request_word_selection_row(&mut self, row: u32, outcome: &mut ClientShellInput) {
        let Some(gesture) = self.word_selection_gesture.as_mut() else {
            return;
        };
        if gesture.pending_row.is_some() {
            return;
        }
        gesture.pending_row = Some(row);
        let pane_id = gesture.pane_id.clone();
        let params = crate::api::schema::PaneSelectionReadParams {
            pane_id: pane_id.clone(),
            anchor: crate::api::schema::PaneTextPoint { row, col: 0 },
            cursor: crate::api::schema::PaneTextPoint {
                row,
                col: gesture.end_col,
            },
            content_revision: gesture.content_revision,
        };
        if !self.push_endpoint_method_with_kind(
            crate::api::schema::Method::PaneSelectionRead(params),
            PendingEndpointKind::WordSelection {
                pane_id,
                absolute_row: row,
                generation: self.word_selection_generation,
            },
            outcome,
        ) {
            self.cancel_word_selection();
        }
    }

    pub(super) fn drag_word_selection(
        &mut self,
        cursor: (u32, u16),
        outcome: &mut ClientShellInput,
    ) {
        let Some(gesture) = self.word_selection_gesture.as_mut() else {
            return;
        };
        if gesture.released || gesture.cursor == cursor {
            return;
        }
        gesture.cursor = cursor;
        gesture.dragged = true;
        self.update_word_selection(outcome);
    }

    pub(super) fn finish_word_selection(&mut self, outcome: &mut ClientShellInput) {
        self.stop_selection_autoscroll();
        if let Some(gesture) = self.word_selection_gesture.as_mut() {
            gesture.released = true;
        }
        // A pending row reply will finish the selection if its bounds are not ready yet.
        self.update_word_selection(outcome);
    }

    fn update_word_selection(&mut self, outcome: &mut ClientShellInput) {
        let Some(gesture) = self.word_selection_gesture.as_ref() else {
            return;
        };
        let Some((anchor_start, anchor_end)) = gesture.anchor_bounds else {
            return;
        };
        let Some((_, text)) = gesture
            .cached_row
            .as_ref()
            .filter(|(row, _)| *row == gesture.cursor.0)
        else {
            self.request_word_selection_row(gesture.cursor.0, outcome);
            return;
        };
        let (start_col, end_col) =
            crate::app::actions::word_bounds_at_column(text, gesture.cursor.1)
                .unwrap_or((gesture.cursor.1, gesture.cursor.1));
        let start = (gesture.anchor.0, anchor_start).min((gesture.cursor.0, start_col));
        let end = (gesture.anchor.0, anchor_end).max((gesture.cursor.0, end_col));
        self.selection = Some(crate::selection::Selection::absolute_range(
            gesture.pane_id.clone(),
            start,
            end,
        ));
        if gesture.released {
            let dragged = gesture.dragged;
            if let Some(selection) = self.selection.as_mut() {
                selection.finish();
            }
            self.word_selection_gesture = None;
            if self.config.copy_on_select {
                self.request_selection_copy(outcome, false);
                if dragged {
                    self.selection = None;
                } else {
                    self.selection_highlight_clear_deadline =
                        Some(std::time::Instant::now() + std::time::Duration::from_millis(500));
                }
            }
        }
        outcome.repaint = true;
    }

    pub(super) fn complete_word_selection_row(
        &mut self,
        pane_id: String,
        absolute_row: u32,
        generation: u64,
        result: Result<crate::api::schema::ResponseResult, ClientShellEndpointError>,
    ) -> (bool, Vec<ClientShellAction>) {
        if self.word_selection_generation != generation
            || self.word_selection_gesture.as_ref().is_none_or(|gesture| {
                gesture.pane_id != pane_id || gesture.pending_row != Some(absolute_row)
            })
        {
            return (false, Vec::new());
        }
        if self
            .snapshot
            .as_deref()
            .is_none_or(|snapshot| !snapshot.panes.iter().any(|pane| pane.pane_id == pane_id))
        {
            self.cancel_word_selection();
            return (true, Vec::new());
        }
        let text = match result {
            Ok(crate::api::schema::ResponseResult::PaneSelection {
                pane_id: returned_pane_id,
                text,
            }) if returned_pane_id == pane_id => text,
            other => {
                if matches!(other, Ok(value) if !matches!(value, crate::api::schema::ResponseResult::PaneSelection { .. }))
                {
                    self.endpoint_error =
                        Some("endpoint returned an unexpected word-selection result".to_owned());
                }
                self.cancel_word_selection();
                return (true, Vec::new());
            }
        };
        let Some(gesture) = self.word_selection_gesture.as_mut() else {
            return (false, Vec::new());
        };
        gesture.pending_row = None;
        if gesture.anchor_bounds.is_none() {
            gesture.anchor_bounds =
                crate::app::actions::word_bounds_at_column(&text, gesture.anchor.1);
            if gesture.anchor_bounds.is_none() {
                self.cancel_word_selection();
                return (true, Vec::new());
            }
        }
        gesture.cached_row = Some((absolute_row, text));
        let mut outcome = ClientShellInput::default();
        self.update_word_selection(&mut outcome);
        (outcome.repaint, outcome.actions)
    }
}
