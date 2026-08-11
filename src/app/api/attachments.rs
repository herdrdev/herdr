use std::collections::HashSet;

use crate::api::schema::conversations::{
    AgentAttachmentAbortParams, AgentAttachmentBeginParams, AgentAttachmentChunkParams,
    AgentAttachmentFinishParams, AttachmentHandle,
};
use crate::api::schema::ResponseResult;
use crate::app::attachments::{AttachmentError, StagedAttachment};
use crate::app::App;
use crate::layout::PaneId;

use super::responses::{encode_error, encode_success};

impl App {
    pub(super) fn handle_agent_attachment_begin(
        &mut self,
        id: String,
        params: AgentAttachmentBeginParams,
    ) -> String {
        let (pane_id, session) = match self.attachment_binding(&params.target) {
            Ok(binding) => binding,
            Err((code, message)) => return encode_error(id, &code, message),
        };
        let (upload, chunk_size) = match self.attachment_store.begin(pane_id, &session, &params) {
            Ok(result) => result,
            Err(error) => return attachment_error(id, error),
        };
        encode_success(
            id,
            ResponseResult::AgentAttachmentBegin { upload, chunk_size },
        )
    }

    pub(super) fn handle_agent_attachment_chunk(
        &mut self,
        id: String,
        params: AgentAttachmentChunkParams,
    ) -> String {
        let index = params.index;
        match self.attachment_store.chunk(params) {
            Ok(()) => encode_success(id, ResponseResult::AgentAttachmentChunk { index }),
            Err(error) => attachment_error(id, error),
        }
    }

    pub(super) fn handle_agent_attachment_finish(
        &mut self,
        id: String,
        params: AgentAttachmentFinishParams,
    ) -> String {
        match self.attachment_store.finish(params.upload) {
            Ok(attachment) => {
                encode_success(id, ResponseResult::AgentAttachmentFinished { attachment })
            }
            Err(error) => attachment_error(id, error),
        }
    }

    pub(super) fn handle_agent_attachment_abort(
        &mut self,
        id: String,
        params: AgentAttachmentAbortParams,
    ) -> String {
        match self.attachment_store.abort_upload(params.upload) {
            Ok(()) => encode_success(id, ResponseResult::AgentAttachmentAborted {}),
            Err(error) => attachment_error(id, error),
        }
    }

    pub(super) fn resolve_prompt_attachments(
        &mut self,
        target: &str,
        handles: &[AttachmentHandle],
    ) -> Result<Vec<StagedAttachment>, (String, String)> {
        let (pane_id, session) = self.attachment_binding(target)?;
        handles
            .iter()
            .map(|handle| {
                self.attachment_store
                    .resolve(pane_id, &session, handle)
                    .map_err(|error| ("attachment_invalid".to_string(), error.to_string()))
            })
            .collect()
    }

    pub(super) fn take_prompt_attachments(
        &mut self,
        target: &str,
        handles: &[AttachmentHandle],
    ) -> Result<Vec<StagedAttachment>, (String, String)> {
        let mut seen = HashSet::new();
        for handle in handles {
            if !seen.insert(&handle.handle) {
                return Err((
                    "attachment_invalid".into(),
                    "attachment handle is repeated".into(),
                ));
            }
        }
        let attachments = self.resolve_prompt_attachments(target, handles)?;
        for (index, handle) in handles.iter().enumerate() {
            if let Err(error) = self.attachment_store.take_for_prompt(handle) {
                self.attachment_store
                    .discard_prompt_attachments(&attachments[..index]);
                return Err(("attachment_invalid".into(), error.to_string()));
            }
        }
        Ok(attachments)
    }

    pub(super) fn attachment_binding(
        &self,
        target: &str,
    ) -> Result<(PaneId, String), (String, String)> {
        let resolved = self
            .resolve_agent_target(target)
            .map_err(|_| ("agent_not_found".into(), "agent target not found".into()))?;
        let Some(workspace) = self.state.workspaces.get(resolved.ws_idx) else {
            return Err(("agent_not_found".into(), "agent target not found".into()));
        };
        let Some(pane_state) = workspace.pane_state(resolved.pane_id) else {
            return Err(("agent_not_found".into(), "agent target not found".into()));
        };
        let Some(terminal) = self.state.terminals.get(&pane_state.attached_terminal_id) else {
            return Err(("agent_not_found".into(), "agent target not found".into()));
        };
        if terminal.effective_known_agent().is_none() {
            return Err((
                "agent_not_ready".into(),
                "the target is not an active named agent".into(),
            ));
        }
        let Some(parts) = super::super::creation::terminal_effective_session_parts(terminal) else {
            return Err((
                "conversation_no_session".into(),
                "structured Chat requires an active provider session".into(),
            ));
        };
        let session = crate::app::conversation_sources::session_identity_key(
            &parts.0,
            &parts.1,
            parts.2.kind_name(),
            &parts.3,
        );
        Ok((resolved.pane_id, session))
    }
}

fn attachment_error(id: String, error: AttachmentError) -> String {
    encode_error(id, "attachment_invalid", error.to_string())
}
