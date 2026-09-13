use super::*;
use crate::raw_input::RawInputEvent;
use crossterm::event::{KeyEventKind, KeyModifiers};
use std::time::Instant;

impl ClientShellState {
    pub(super) fn prediction_allowed(&self) -> bool {
        self.prediction_context_allowed() && self.pending_pane_surface.is_none()
    }

    pub(super) fn prediction_context_allowed(&self) -> bool {
        self.config.remote_predict_input
            && (self.primary_remote || !self.active_endpoint_id.is_local())
            && self.endpoint_is_online(&self.active_endpoint_id)
            && self.mode == ClientShellMode::Terminal
            && self.overlay.is_none()
            && self.selection.is_none()
            && self.copy_mode.is_none()
            && !self.popup_pending
            && self.popup_terminal_id.is_none()
            && self.endpoint_error.is_none()
            && self.outer_focused != Some(false)
    }

    pub(super) fn prepare_prediction_input(
        &mut self,
        event: &RawInputEvent,
        outcome: &mut ClientShellInput,
    ) {
        // Reset before routing controls, even when the client consumes them. Printable
        // shortcuts are never predicted unless they actually reach a remote pane below.
        let simple = match event {
            RawInputEvent::Key(key) => {
                key.kind == KeyEventKind::Release
                    || (matches!(key.code, KeyCode::Char(c) if c.is_ascii() && !c.is_ascii_control())
                        && (key.modifiers - KeyModifiers::SHIFT).is_empty())
            }
            RawInputEvent::Text(_) => true,
            RawInputEvent::HostDefaultColor { .. }
            | RawInputEvent::HostPaletteColors { .. }
            | RawInputEvent::HostCellSizeReport { .. }
            | RawInputEvent::HostColorSchemeChanged(_) => true,
            _ => false,
        };
        if !simple || !self.prediction_allowed() {
            outcome.repaint |= self.input_prediction.clear();
        }
    }

    pub(super) fn predict_pane_event(
        &mut self,
        target: &ClientInputTarget,
        event: &ClientPaneInputEvent,
        outcome: &mut ClientShellInput,
    ) {
        if !self.prediction_allowed() {
            outcome.repaint |= self.input_prediction.clear();
            return;
        }
        if let (ClientInputTarget::Pane(pane_id), Some(surface)) =
            (target, self.pane_surface.as_ref())
        {
            outcome.repaint |=
                self.input_prediction
                    .record_input(surface, pane_id, event, Instant::now());
        }
    }

    pub(super) fn reconcile_prediction(&mut self) {
        if !self.prediction_allowed() {
            self.input_prediction.clear();
        } else if let Some(surface) = self.pane_surface.as_ref() {
            self.input_prediction.observe(surface, Instant::now());
        }
    }

    pub(crate) fn tick_prediction(&mut self, now: Instant) -> bool {
        if !self.prediction_context_allowed() {
            self.input_prediction.clear()
        } else {
            self.input_prediction.expire(now)
        }
    }
}
