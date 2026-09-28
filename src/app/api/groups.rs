use crate::api::schema::{
    AuthorityMutation, AuthorityMutationParams, GroupCreateParams, GroupDeleteParams,
    GroupRenameParams, PaneGroupSetParams, Request, ResponseResult,
};
use crate::app::App;
use crate::groups::{
    GroupAuthoritySnapshot, GroupState, MutationError, OwnedPaneMembership, PaneGroupMembership,
    RuntimeError,
};

use super::responses::{encode_error, encode_success};

impl App {
    pub(super) fn handle_group_host_snapshot(&self, id: String) -> String {
        let authority = match self.group_runtime.authority() {
            Ok(authority) => authority,
            Err(error) => return encode_runtime_error(id, error),
        };
        let memberships = self.group_membership_projection.values().cloned().collect();
        encode_success(
            id,
            ResponseResult::GroupHostSnapshot {
                snapshot: GroupAuthoritySnapshot {
                    authority_id: authority.authority_id().clone(),
                    revision: authority.revision(),
                    groups: authority.records(),
                    memberships,
                },
            },
        )
    }

    pub(super) fn handle_group_create(&mut self, id: String, params: GroupCreateParams) -> String {
        if let Some(message) = self.local_authority_conflict_message() {
            return encode_error(id, "authority_not_fresh", message);
        }
        match self
            .group_runtime
            .create(&params.name, params.expected_revision)
        {
            Ok((record, revision)) => {
                self.refresh_local_group_snapshot();
                encode_success(id, ResponseResult::GroupMutation { record, revision })
            }
            Err(error) => encode_runtime_error(id, error),
        }
    }

    pub(super) fn handle_group_rename(&mut self, id: String, params: GroupRenameParams) -> String {
        if !self.local_group_owner(&params.group_id.owner) {
            let authority = params.group_id.owner.clone();
            return self.route_authority_mutation(id, authority, AuthorityMutation::Rename(params));
        }
        self.apply_group_rename(id, params)
    }

    fn apply_group_rename(&mut self, id: String, params: GroupRenameParams) -> String {
        if let Some(message) = self.local_authority_conflict_message() {
            return encode_error(id, "authority_not_fresh", message);
        }
        match self
            .group_runtime
            .rename(&params.group_id, &params.name, params.expected_revision)
        {
            Ok((record, revision)) => {
                self.refresh_local_group_snapshot();
                encode_success(id, ResponseResult::GroupMutation { record, revision })
            }
            Err(error) => encode_runtime_error(id, error),
        }
    }

    pub(super) fn handle_group_delete(&mut self, id: String, params: GroupDeleteParams) -> String {
        if !self.local_group_owner(&params.group_id.owner) {
            let authority = params.group_id.owner.clone();
            return self.route_authority_mutation(id, authority, AuthorityMutation::Delete(params));
        }
        self.apply_group_delete(id, params)
    }

    fn apply_group_delete(&mut self, id: String, params: GroupDeleteParams) -> String {
        if let Some(message) = self.local_authority_conflict_message() {
            return encode_error(id, "authority_not_fresh", message);
        }
        match self
            .group_runtime
            .delete(&params.group_id, params.expected_revision)
        {
            Ok((record, revision)) => {
                self.refresh_local_group_snapshot();
                encode_success(id, ResponseResult::GroupMutation { record, revision })
            }
            Err(error) => encode_runtime_error(id, error),
        }
    }

    pub(super) fn handle_pane_group_set(
        &mut self,
        id: String,
        mut params: PaneGroupSetParams,
    ) -> String {
        let expected_owner = params.expected_pane_authority.clone();
        match expected_owner {
            None => {
                // A caller that omits the pane authority still gets refusals
                // named after the pane's own authority, matching the supplied
                // and forwarded paths.
                params.expected_pane_authority = self
                    .group_runtime
                    .authority()
                    .ok()
                    .map(|local| local.authority_id().clone());
                self.apply_local_pane_group_set(id, params)
            }
            Some(owner) if self.local_group_owner(&owner) => {
                self.apply_local_pane_group_set(id, params)
            }
            Some(owner) => {
                let catalog = match self.state.fleet_snapshot.fresh_group_catalog(&owner) {
                    Ok(catalog) => catalog,
                    Err(message) => return encode_error(id, "authority_not_fresh", message),
                };
                let reported_pane = catalog.snapshot.as_ref().and_then(|snapshot| {
                    snapshot
                        .memberships
                        .iter()
                        .find(|membership| membership.pane_id == params.pane_id)
                });
                let Some(reported_pane) = reported_pane else {
                    return encode_error(
                        id,
                        "pane_not_found",
                        format!("authority {owner} does not report pane {}", params.pane_id),
                    );
                };
                if reported_pane.pane_incarnation.is_empty() {
                    return encode_error(
                        id,
                        "authority_not_fresh",
                        format!("authority {owner} did not report a pane incarnation"),
                    );
                }
                params.expected_pane_incarnation = Some(reported_pane.pane_incarnation.clone());
                if let Some(group_id) = params.group_id.as_ref() {
                    if let Err(message) = self.validate_group_target(group_id) {
                        return pane_group_set_error(
                            id,
                            group_target_error_code(&message),
                            message,
                            Some(&owner),
                        );
                    }
                }
                self.route_authority_mutation(id, owner, AuthorityMutation::PaneGroupSet(params))
            }
        }
    }

    fn apply_local_pane_group_set(&mut self, id: String, params: PaneGroupSetParams) -> String {
        if self.local_pane(&params.pane_id).is_none() {
            let owner = params
                .expected_pane_authority
                .as_ref()
                .map_or_else(|| "local authority".to_string(), ToString::to_string);
            return encode_error(
                id,
                "pane_not_found",
                format!("{owner} does not own pane {}", params.pane_id),
            );
        }
        if let Some(group_id) = params.group_id.as_ref() {
            if let Err(message) = self.validate_group_target(group_id) {
                return pane_group_set_error(
                    id,
                    group_target_error_code(&message),
                    message,
                    params.expected_pane_authority.as_ref(),
                );
            }
        }
        self.apply_pane_group_set(id, params)
    }

    fn local_pane(&self, public_id: &str) -> Option<(usize, crate::layout::PaneId)> {
        self.parse_pane_id(public_id)
            .and_then(|(workspace_index, pane_id)| {
                self.state.workspaces[workspace_index]
                    .pane_state(pane_id)
                    .filter(|pane| {
                        !self
                            .terminal_runtimes
                            .get(&pane.attached_terminal_id)
                            .is_some_and(crate::terminal::TerminalRuntime::is_remote_proxy)
                    })
                    .map(|_| (workspace_index, pane_id))
            })
    }

    fn apply_pane_group_set(&mut self, id: String, params: PaneGroupSetParams) -> String {
        let pane_authority = params.expected_pane_authority.clone();
        if params.group_id.is_some() {
            if let Some(message) = self.local_authority_conflict_message() {
                return pane_group_set_error(
                    id,
                    "authority_not_fresh",
                    message,
                    pane_authority.as_ref(),
                );
            }
        }
        if self.no_session {
            return pane_group_set_error(
                id,
                "group_persistence_disabled",
                "pane membership requires session persistence",
                pane_authority.as_ref(),
            );
        }
        let Some((workspace_index, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_group_set_error(
                id,
                "pane_not_found",
                "pane not found",
                pane_authority.as_ref(),
            );
        };
        let Some(pane) = self.state.workspaces[workspace_index].pane_state(pane_id) else {
            return pane_group_set_error(
                id,
                "pane_not_found",
                "pane not found",
                pane_authority.as_ref(),
            );
        };
        if self
            .terminal_runtimes
            .get(&pane.attached_terminal_id)
            .is_some_and(crate::terminal::TerminalRuntime::is_remote_proxy)
        {
            return pane_group_set_error(
                id,
                "pane_not_owned",
                "pane is not owned by this server",
                pane_authority.as_ref(),
            );
        }
        if pane.group_membership.revision != params.expected_revision {
            return pane_group_set_error(
                id,
                "revision_conflict",
                format!(
                    "expected revision {}, current revision is {}",
                    params.expected_revision, pane.group_membership.revision
                ),
                pane_authority.as_ref(),
            );
        }
        let Some(revision) = pane.group_membership.revision.checked_add(1) else {
            return pane_group_set_error(
                id,
                "revision_exhausted",
                "membership revision is exhausted",
                pane_authority.as_ref(),
            );
        };
        let membership = PaneGroupMembership {
            group_id: params.group_id,
            revision,
        };

        let mut snapshot = crate::persist::capture(
            &self.state.workspaces,
            &self.state.terminals,
            &self.terminal_runtimes,
            self.state.active,
            self.state.selected,
            self.state.sidebar_width,
            self.state.sidebar_section_split,
            self.state.collapsed_space_keys.clone(),
            self.state.prio_panel_collapsed,
            self.state.window_cycle_mode,
            self.state.skip_collapsed_cycle,
        );
        let Some(saved_pane) = snapshot
            .workspaces
            .iter_mut()
            .flat_map(|workspace| workspace.tabs.iter_mut())
            .find_map(|tab| tab.panes.get_mut(&pane_id.raw()))
        else {
            return pane_group_set_error(
                id,
                "pane_not_persistable",
                "pane is not in the session snapshot",
                pane_authority.as_ref(),
            );
        };
        saved_pane.group_membership = membership.clone();
        let history = self.persist_pane_history.then(|| {
            crate::persist::capture_history(
                &snapshot,
                &self.state.workspaces,
                &self.state.terminals,
                &self.terminal_runtimes,
            )
        });
        if let Err(error) = self.persist_session_candidate(snapshot, history) {
            return pane_group_set_error(
                id,
                "persistence_failed",
                error.to_string(),
                pane_authority.as_ref(),
            );
        }
        let Some(pane) = self.state.workspaces[workspace_index].pane_state_mut(pane_id) else {
            return pane_group_set_error(
                id,
                "pane_not_found",
                "pane disappeared after persistence",
                pane_authority.as_ref(),
            );
        };
        pane.group_membership = membership.clone();
        self.refresh_group_membership_projection_for_pane(&params.pane_id);
        encode_success(
            id,
            ResponseResult::PaneGroupSet {
                pane_id: params.pane_id,
                membership,
            },
        )
    }

    pub(super) fn handle_group_authority_mutate(
        &mut self,
        id: String,
        params: AuthorityMutationParams,
    ) -> String {
        if !params.forwarded {
            return encode_error(
                id,
                "invalid_authority_route",
                "authority mutation is receiver-only",
            );
        }
        let actual = match self.group_runtime.authority() {
            Ok(authority) => authority.authority_id().clone(),
            Err(error) => {
                let pane_authority = match &params.mutation {
                    AuthorityMutation::PaneGroupSet(params) => {
                        params.expected_pane_authority.as_ref()
                    }
                    _ => None,
                };
                return encode_runtime_error_for_pane(id, error, pane_authority);
            }
        };
        if actual != params.expected_authority {
            return encode_error(
                id,
                "authority_mismatch",
                format!(
                    "expected authority {}, receiver owns {}",
                    params.expected_authority, actual
                ),
            );
        }
        match params.mutation {
            AuthorityMutation::Rename(params) => self.apply_group_rename(id, params),
            AuthorityMutation::Delete(params) => self.apply_group_delete(id, params),
            AuthorityMutation::PaneGroupSet(params) => {
                if params.expected_pane_authority.as_ref() != Some(&actual) {
                    let expected = params
                        .expected_pane_authority
                        .as_ref()
                        .map_or_else(|| "no authority".to_string(), ToString::to_string);
                    return encode_error(
                        id,
                        "authority_mismatch",
                        format!("pane mutation expected {expected}, receiver owns {actual}"),
                    );
                }
                let expected_incarnation = match params.expected_pane_incarnation.as_deref() {
                    Some(incarnation) => incarnation,
                    None => {
                        return encode_error(
                            id,
                            "authority_not_fresh",
                            format!("authority {actual} received a pane mutation without an incarnation"),
                        );
                    }
                };
                let actual_incarnation =
                    self.local_pane(&params.pane_id)
                        .and_then(|(workspace_index, pane_id)| {
                            self.state.workspaces[workspace_index]
                                .pane_state(pane_id)
                                .map(|pane| pane.attached_terminal_id.as_str())
                        });
                if actual_incarnation != Some(expected_incarnation) {
                    return encode_error(
                        id,
                        "authority_not_fresh",
                        format!(
                            "authority {actual} no longer owns pane {} with the expected incarnation",
                            params.pane_id
                        ),
                    );
                }
                if let Some(group_id) = params.group_id.as_ref() {
                    if let Err(message) = self.validate_group_target(group_id) {
                        return pane_group_set_error(
                            id,
                            group_target_error_code(&message),
                            message,
                            params.expected_pane_authority.as_ref(),
                        );
                    }
                }
                self.apply_pane_group_set(id, params)
            }
        }
    }

    fn local_group_owner(&self, authority: &crate::groups::AuthorityId) -> bool {
        self.group_runtime
            .authority()
            .is_ok_and(|local| local.authority_id() == authority)
    }

    pub(crate) fn rebuild_group_membership_projection(&mut self) {
        let mut projection = std::collections::BTreeMap::new();
        for (workspace_index, workspace) in self.state.workspaces.iter().enumerate() {
            for tab in &workspace.tabs {
                for (pane_id, pane) in &tab.panes {
                    if self
                        .terminal_runtimes
                        .get(&pane.attached_terminal_id)
                        .is_some_and(crate::terminal::TerminalRuntime::is_remote_proxy)
                    {
                        continue;
                    }
                    if let Some(public_id) = self.public_pane_id(workspace_index, *pane_id) {
                        projection.insert(
                            public_id.clone(),
                            OwnedPaneMembership {
                                pane_id: public_id,
                                pane_incarnation: pane.attached_terminal_id.to_string(),
                                membership: pane.group_membership.clone(),
                            },
                        );
                    }
                }
            }
        }
        self.group_membership_projection = projection;
        self.refresh_local_group_snapshot();
    }

    pub(crate) fn refresh_group_membership_projection_for_pane(&mut self, public_id: &str) {
        let membership = self
            .local_pane(public_id)
            .and_then(|(workspace_index, pane_id)| {
                self.state.workspaces[workspace_index]
                    .pane_state(pane_id)
                    .map(|pane| OwnedPaneMembership {
                        pane_id: public_id.to_string(),
                        pane_incarnation: pane.attached_terminal_id.to_string(),
                        membership: pane.group_membership.clone(),
                    })
            });
        match membership {
            Some(membership) => {
                self.group_membership_projection
                    .insert(public_id.to_string(), membership);
            }
            None => {
                self.group_membership_projection.remove(public_id);
            }
        }
        self.refresh_local_group_snapshot();
    }

    fn refresh_local_group_snapshot(&mut self) {
        self.state.local_group_snapshot =
            self.group_runtime
                .authority()
                .ok()
                .map(|authority| GroupAuthoritySnapshot {
                    authority_id: authority.authority_id().clone(),
                    revision: authority.revision(),
                    groups: authority.records(),
                    memberships: self.group_membership_projection.values().cloned().collect(),
                });
    }

    pub(crate) fn observe_group_membership_lifecycle_event(
        &mut self,
        data: &crate::api::schema::EventData,
    ) {
        let changed = match data {
            crate::api::schema::EventData::PaneCreated { pane } => {
                self.refresh_group_membership_projection_for_pane(&pane.pane_id);
                false
            }
            crate::api::schema::EventData::PaneClosed { pane_id, .. } => {
                self.group_membership_projection.remove(pane_id);
                true
            }
            crate::api::schema::EventData::PaneMoved {
                previous_pane_id,
                pane,
                ..
            } => {
                self.group_membership_projection.remove(previous_pane_id);
                self.refresh_group_membership_projection_for_pane(&pane.pane_id);
                false
            }
            crate::api::schema::EventData::WorkspaceClosed { workspace_id, .. } => {
                let prefix = format!("{workspace_id}:");
                self.group_membership_projection
                    .retain(|pane_id, _| !pane_id.starts_with(&prefix));
                true
            }
            crate::api::schema::EventData::TabClosed { .. } => {
                let pane_ids = self
                    .group_membership_projection
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>();
                for pane_id in pane_ids {
                    if self.local_pane(&pane_id).is_none() {
                        self.group_membership_projection.remove(&pane_id);
                    }
                }
                true
            }
            _ => false,
        };
        if changed {
            self.refresh_local_group_snapshot();
        }
    }

    fn local_authority_conflict_message(&self) -> Option<String> {
        let authority = self.group_runtime.authority().ok()?.authority_id();
        self.state
            .fleet_snapshot
            .authority_has_identity_conflict(authority)
            .then(|| format!("authority {authority} has an identity conflict"))
    }

    fn validate_group_target(&self, group_id: &crate::groups::GroupId) -> Result<(), String> {
        if self.local_group_owner(&group_id.owner) {
            if let Some(message) = self.local_authority_conflict_message() {
                return Err(message);
            }
            let authority = self.group_runtime.authority().map_err(|error| {
                format!(
                    "local authority {} is unavailable: {error:?}",
                    group_id.owner
                )
            })?;
            let record = authority
                .record(group_id)
                .map_err(|_| format!("group {group_id:?} is unavailable"))?;
            return if matches!(record.state, GroupState::Deleted) {
                Err(format!(
                    "group authority {} reports a deleted group",
                    group_id.owner
                ))
            } else {
                Ok(())
            };
        }

        let catalog = self
            .state
            .fleet_snapshot
            .fresh_group_catalog(&group_id.owner)?;
        let record = catalog
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.groups.iter().find(|record| record.id == *group_id))
            .ok_or_else(|| {
                format!(
                    "authority {} does not report the target group",
                    group_id.owner
                )
            })?;
        if matches!(record.state, GroupState::Deleted) {
            return Err(format!(
                "authority {} reports a deleted group",
                group_id.owner
            ));
        }
        Ok(())
    }

    fn route_authority_mutation(
        &self,
        id: String,
        authority: crate::groups::AuthorityId,
        mutation: AuthorityMutation,
    ) -> String {
        match self.prepare_authority_mutation(id.clone(), authority, mutation) {
            Ok(_) => encode_error(
                id,
                "deferred_response_required",
                "remote authority mutations require the deferred API transport",
            ),
            Err(response) => response,
        }
    }

    fn prepare_authority_mutation(
        &self,
        id: String,
        authority: crate::groups::AuthorityId,
        mutation: AuthorityMutation,
    ) -> Result<(crate::fleet::GroupCatalog, Request), String> {
        let catalog = match self.state.fleet_snapshot.fresh_group_catalog(&authority) {
            Ok(catalog) => catalog.clone(),
            Err(message) => return Err(encode_error(id, "authority_not_fresh", message)),
        };
        let request = Request {
            id,
            method: crate::api::schema::Method::GroupAuthorityMutate(AuthorityMutationParams {
                expected_authority: authority,
                forwarded: true,
                mutation,
            }),
        };
        Ok((catalog, request))
    }

    pub(crate) fn should_defer_group_api_request(&self, request: &Request) -> bool {
        match &request.method {
            crate::api::schema::Method::GroupRename(params) => {
                !self.local_group_owner(&params.group_id.owner)
            }
            crate::api::schema::Method::GroupDelete(params) => {
                !self.local_group_owner(&params.group_id.owner)
            }
            crate::api::schema::Method::PaneGroupSet(params) => params
                .expected_pane_authority
                .as_ref()
                .is_some_and(|owner| !self.local_group_owner(owner)),
            _ => false,
        }
    }

    pub(crate) fn start_deferred_group_api_request(
        &mut self,
        request: Request,
        respond_to: std::sync::mpsc::Sender<String>,
    ) {
        let id = request.id;
        let prepared = match request.method {
            crate::api::schema::Method::GroupRename(params) => {
                let authority = params.group_id.owner.clone();
                self.prepare_authority_mutation(id, authority, AuthorityMutation::Rename(params))
            }
            crate::api::schema::Method::GroupDelete(params) => {
                let authority = params.group_id.owner.clone();
                self.prepare_authority_mutation(id, authority, AuthorityMutation::Delete(params))
            }
            crate::api::schema::Method::PaneGroupSet(mut params) => {
                let Some(owner) = params.expected_pane_authority.clone() else {
                    let _ = respond_to.send(encode_error(
                        id,
                        "invalid_authority_route",
                        "remote pane mutation has no expected authority",
                    ));
                    return;
                };
                let catalog = match self.state.fleet_snapshot.fresh_group_catalog(&owner) {
                    Ok(catalog) => catalog,
                    Err(message) => {
                        let _ = respond_to.send(encode_error(id, "authority_not_fresh", message));
                        return;
                    }
                };
                let reported_pane = catalog.snapshot.as_ref().and_then(|snapshot| {
                    snapshot
                        .memberships
                        .iter()
                        .find(|membership| membership.pane_id == params.pane_id)
                });
                let Some(reported_pane) = reported_pane else {
                    let _ = respond_to.send(encode_error(
                        id,
                        "pane_not_found",
                        format!("authority {owner} does not report pane {}", params.pane_id),
                    ));
                    return;
                };
                if reported_pane.pane_incarnation.is_empty() {
                    let _ = respond_to.send(encode_error(
                        id,
                        "authority_not_fresh",
                        format!("authority {owner} did not report a pane incarnation"),
                    ));
                    return;
                }
                params.expected_pane_incarnation = Some(reported_pane.pane_incarnation.clone());
                if let Some(group_id) = params.group_id.as_ref() {
                    if let Err(message) = self.validate_group_target(group_id) {
                        let _ = respond_to.send(pane_group_set_error(
                            id,
                            group_target_error_code(&message),
                            message,
                            Some(&owner),
                        ));
                        return;
                    }
                }
                self.prepare_authority_mutation(id, owner, AuthorityMutation::PaneGroupSet(params))
            }
            _ => {
                let _ = respond_to.send(encode_error(
                    id,
                    "invalid_authority_route",
                    "request is not a remote authority mutation",
                ));
                return;
            }
        };
        let (catalog, request) = match prepared {
            Ok(prepared) => prepared,
            Err(response) => {
                let _ = respond_to.send(response);
                return;
            }
        };
        let pane_authority = match &request.method {
            crate::api::schema::Method::GroupAuthorityMutate(params) => match &params.mutation {
                AuthorityMutation::PaneGroupSet(params) => params.expected_pane_authority.clone(),
                _ => None,
            },
            _ => None,
        };
        if let Err(error) = self.authority_mutation_router.enqueue(
            catalog,
            self.fleet_poller_config.generation(),
            request.clone(),
            respond_to.clone(),
        ) {
            let _ = respond_to.send(pane_group_set_error(
                request.id,
                "authority_unreachable",
                error,
                pane_authority.as_ref(),
            ));
        }
    }
}

fn encode_runtime_error(id: String, error: RuntimeError) -> String {
    encode_runtime_error_for_pane(id, error, None)
}

fn encode_runtime_error_for_pane(
    id: String,
    error: RuntimeError,
    pane_authority: Option<&crate::groups::AuthorityId>,
) -> String {
    match error {
        RuntimeError::Unavailable(message) => {
            pane_group_set_error(id, "group_authority_unavailable", message, pane_authority)
        }
        RuntimeError::Persistence(message) => {
            pane_group_set_error(id, "persistence_failed", message, pane_authority)
        }
        RuntimeError::Mutation(MutationError::Deleted) => {
            pane_group_set_error(id, "group_deleted", "group is deleted", pane_authority)
        }
        RuntimeError::Mutation(MutationError::ForeignOwner) => pane_group_set_error(
            id,
            "group_not_owned",
            "group is owned by another server",
            pane_authority,
        ),
        RuntimeError::Mutation(MutationError::InvalidName) => pane_group_set_error(
            id,
            "invalid_group_name",
            "group name must not be empty",
            pane_authority,
        ),
        RuntimeError::Mutation(MutationError::NotFound) => {
            pane_group_set_error(id, "group_not_found", "group not found", pane_authority)
        }
        RuntimeError::Mutation(MutationError::RevisionConflict { expected, actual }) => {
            pane_group_set_error(
                id,
                "revision_conflict",
                format!("expected revision {expected}, current revision is {actual}"),
                pane_authority,
            )
        }
        RuntimeError::Mutation(MutationError::RevisionExhausted) => pane_group_set_error(
            id,
            "revision_exhausted",
            "group revision is exhausted",
            pane_authority,
        ),
    }
}

fn pane_group_set_error(
    id: String,
    code: &str,
    message: impl Into<String>,
    pane_authority: Option<&crate::groups::AuthorityId>,
) -> String {
    let mut message = message.into();
    if let Some(authority) = pane_authority {
        if !message.contains(authority.as_str()) {
            message = format!("authority {authority}: {message}");
        }
    }
    encode_error(id, code, message)
}

fn group_target_error_code(message: &str) -> &'static str {
    if message.contains("does not report the target group")
        || message.contains("reports a deleted group")
        || message.contains("is unavailable") && message.contains("group")
    {
        "group_not_found"
    } else {
        "authority_not_fresh"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{
        GroupDeleteParams, GroupRenameParams, Method, PaneMoveDestination, PaneMoveParams,
        SplitDirection, SuccessResponse,
    };
    use crate::groups::{GroupId, GroupRecord};
    use crate::workspace::Workspace;
    use std::path::PathBuf;

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(name: &str) -> Self {
            let unique = format!(
                "herdr-group-api-{name}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            );
            Self(std::env::temp_dir().join(unique))
        }

        fn session_paths(&self) -> (PathBuf, PathBuf) {
            (
                self.0.join("session.json"),
                self.0.join("session-history.json"),
            )
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn app_with_groups(name: &str) -> (App, TestDir, String) {
        let dir = TestDir::new(name);
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.no_session = false;
        app.group_runtime = crate::groups::Runtime::load(&dir.0.join("groups"));
        app.group_session_paths_override = Some(dir.session_paths());
        app.state.workspaces = vec![Workspace::test_new("source")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.ensure_test_terminals();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let public_id = app.public_pane_id(0, pane_id).expect("public pane id");
        app.rebuild_group_membership_projection();
        (app, dir, public_id)
    }

    fn created_group(response: &str) -> GroupRecord {
        let response: SuccessResponse = serde_json::from_str(response).expect("success response");
        let ResponseResult::GroupMutation { record, .. } = response.result else {
            panic!("expected group mutation response");
        };
        record
    }

    #[cfg(unix)]
    fn write_executable(path: &std::path::Path, contents: &str) {
        use std::os::unix::fs::PermissionsExt;

        std::fs::write(path, contents).expect("write executable fixture");
        let mut permissions = std::fs::metadata(path)
            .expect("executable fixture metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).expect("mark fixture executable");
    }

    fn remote_pane_move_catalogs(
        pane_owner: &crate::groups::AuthorityId,
        group_owner: &crate::groups::AuthorityId,
    ) -> (GroupId, Vec<crate::fleet::GroupCatalog>) {
        let group_id = GroupId {
            owner: group_owner.clone(),
            local: 1,
        };
        (
            group_id.clone(),
            vec![
                crate::fleet::GroupCatalog {
                    host: "pane-owner".into(),
                    target: "pane-host".into(),
                    local: false,
                    session: None,
                    socket: None,
                    state: crate::fleet::GroupCatalogState::Fresh,
                    observed_authority_id: Some(pane_owner.clone()),
                    snapshot: Some(GroupAuthoritySnapshot {
                        authority_id: pane_owner.clone(),
                        revision: 1,
                        groups: Vec::new(),
                        memberships: vec![OwnedPaneMembership {
                            pane_id: "remote-pane".into(),
                            pane_incarnation: "pane-incarnation".into(),
                            membership: PaneGroupMembership::default(),
                        }],
                    }),
                    error: None,
                },
                crate::fleet::GroupCatalog {
                    host: "group-owner".into(),
                    target: "group-host".into(),
                    local: false,
                    session: None,
                    socket: None,
                    state: crate::fleet::GroupCatalogState::Fresh,
                    observed_authority_id: Some(group_owner.clone()),
                    snapshot: Some(GroupAuthoritySnapshot {
                        authority_id: group_owner.clone(),
                        revision: 1,
                        groups: vec![GroupRecord {
                            id: group_id.clone(),
                            revision: 1,
                            state: GroupState::Active {
                                name: "Remote".into(),
                            },
                        }],
                        memberships: Vec::new(),
                    }),
                    error: None,
                },
            ],
        )
    }

    #[test]
    fn create_rename_delete_and_snapshot_keep_one_group_identity() {
        let (mut app, dir, _) = app_with_groups("lifecycle");
        let created = created_group(&app.dispatch_api_request(
            "create",
            Method::GroupCreate(GroupCreateParams {
                name: " Work ".into(),
                expected_revision: 0,
            }),
        ));
        let renamed = created_group(&app.dispatch_api_request(
            "rename",
            Method::GroupRename(GroupRenameParams {
                group_id: created.id.clone(),
                name: "Focus".into(),
                expected_revision: created.revision,
            }),
        ));
        assert_eq!(renamed.id, created.id);
        let deleted = created_group(&app.dispatch_api_request(
            "delete",
            Method::GroupDelete(GroupDeleteParams {
                group_id: created.id.clone(),
                expected_revision: renamed.revision,
            }),
        ));
        assert!(matches!(deleted.state, GroupState::Deleted));

        app.group_runtime = crate::groups::Runtime::load(&dir.0.join("groups"));
        let response: SuccessResponse = serde_json::from_str(
            &app.dispatch_api_request("snapshot", Method::GroupHostSnapshot(Default::default())),
        )
        .expect("snapshot response");
        let ResponseResult::GroupHostSnapshot { snapshot } = response.result else {
            panic!("expected host snapshot");
        };
        assert_eq!(snapshot.groups, vec![deleted]);
    }

    #[test]
    fn local_group_created_without_fleet_catalog_renders_its_name() {
        let (mut app, _dir, _) = app_with_groups("local-render");
        assert!(app.state.fleet_snapshot.group_catalogs.is_empty());
        let _ = created_group(&app.dispatch_api_request(
            "create",
            Method::GroupCreate(GroupCreateParams {
                name: "Solo".into(),
                expected_revision: 0,
            }),
        ));

        assert!(crate::ui::sidebar_rows(&app.state).iter().any(|row| {
            matches!(row, crate::ui::SidebarRow::PodHeader { title, .. } if title == "Solo")
        }));
    }

    #[test]
    fn local_group_snapshot_tracks_record_and_membership_mutations() {
        let (mut app, _dir, pane_id) = app_with_groups("local-projection");
        let created = created_group(&app.handle_group_create(
            "create".into(),
            GroupCreateParams {
                name: "One".into(),
                expected_revision: 0,
            },
        ));
        assert_eq!(
            app.state.local_group_snapshot.as_ref().map(|s| s.revision),
            Some(1)
        );
        let renamed = created_group(&app.handle_group_rename(
            "rename".into(),
            GroupRenameParams {
                group_id: created.id.clone(),
                name: "Two".into(),
                expected_revision: created.revision,
            },
        ));
        let authority = created.id.owner.clone();
        let assigned: SuccessResponse = serde_json::from_str(&app.handle_pane_group_set(
            "assign".into(),
            PaneGroupSetParams {
                pane_id: pane_id.clone(),
                group_id: Some(created.id.clone()),
                expected_revision: 0,
                expected_pane_authority: Some(authority.clone()),
                expected_pane_incarnation: None,
            },
        ))
        .expect("assignment response");
        assert!(matches!(
            assigned.result,
            ResponseResult::PaneGroupSet { .. }
        ));
        let snapshot = app
            .state
            .local_group_snapshot
            .as_ref()
            .expect("local projection");
        assert_eq!(
            snapshot.memberships[0].membership.group_id,
            Some(created.id.clone())
        );
        let _ = app.handle_pane_group_set(
            "clear".into(),
            PaneGroupSetParams {
                pane_id,
                group_id: None,
                expected_revision: 1,
                expected_pane_authority: Some(authority),
                expected_pane_incarnation: None,
            },
        );
        assert_eq!(
            app.state
                .local_group_snapshot
                .as_ref()
                .expect("projection")
                .memberships[0]
                .membership
                .group_id,
            None
        );
        let _ = app.handle_group_delete(
            "delete".into(),
            GroupDeleteParams {
                group_id: created.id,
                expected_revision: renamed.revision,
            },
        );
        assert!(matches!(
            app.state
                .local_group_snapshot
                .as_ref()
                .expect("projection")
                .groups[0]
                .state,
            GroupState::Deleted
        ));
    }

    #[test]
    fn api_group_create_reports_ui_change() {
        let (mut app, _dir, _) = app_with_groups("api-redraw");
        let (tx, rx) = std::sync::mpsc::channel();
        let changed = app.handle_api_request_message(crate::api::ApiRequestMessage {
            request: Request {
                id: "create".into(),
                method: Method::GroupCreate(GroupCreateParams {
                    name: "Visible".into(),
                    expected_revision: 0,
                }),
            },
            respond_to: tx,
            response_write_complete: None,
            stream_active: None,
        });
        assert!(changed);
        assert!(serde_json::from_str::<SuccessResponse>(&rx.recv().expect("response")).is_ok());
    }

    #[test]
    fn pod_assignment_revision_conflict_toasts_without_retry() {
        let (mut app, _dir, pane_id) = app_with_groups("pod-conflict-toast");
        let created = created_group(&app.handle_group_create(
            "create".into(),
            GroupCreateParams {
                name: "Target".into(),
                expected_revision: 0,
            },
        ));
        let authority = created.id.owner.clone();
        let first = PaneGroupSetParams {
            pane_id: pane_id.clone(),
            group_id: Some(created.id.clone()),
            expected_revision: 0,
            expected_pane_authority: Some(authority.clone()),
            expected_pane_incarnation: None,
        };
        assert!(serde_json::from_str::<SuccessResponse>(
            &app.handle_pane_group_set("first".into(), first)
        )
        .is_ok());
        app.runtime_pane_group_set(PaneGroupSetParams {
            pane_id: pane_id.clone(),
            group_id: None,
            expected_revision: 0,
            expected_pane_authority: Some(authority),
            expected_pane_incarnation: None,
        });
        let pane = app
            .local_pane(&pane_id)
            .and_then(|(ws, pane)| app.state.workspaces[ws].pane_state(pane))
            .expect("pane");
        assert_eq!(
            pane.group_membership.revision, 1,
            "no retry advanced membership"
        );
        let toast = app.state.toast.as_ref().expect("visible conflict toast");
        assert_eq!(toast.context, "membership changed elsewhere — not moved");
    }

    #[test]
    fn pod_assignment_stale_owner_toasts_and_does_not_move() {
        let (mut app, _dir, pane_id) = app_with_groups("pod-stale-owner-toast");
        let owner = crate::groups::AuthorityId::from_random_bytes([41; 16]);

        app.runtime_pane_group_set(PaneGroupSetParams {
            pane_id: pane_id.clone(),
            group_id: Some(GroupId {
                owner: owner.clone(),
                local: 1,
            }),
            expected_revision: 0,
            expected_pane_authority: app
                .state
                .local_group_snapshot
                .as_ref()
                .map(|snapshot| snapshot.authority_id.clone()),
            expected_pane_incarnation: None,
        });

        let pane = app
            .local_pane(&pane_id)
            .and_then(|(ws, pane)| app.state.workspaces[ws].pane_state(pane))
            .expect("pane");
        assert_eq!(pane.group_membership, PaneGroupMembership::default());
        let toast = app.state.toast.as_ref().expect("visible stale-owner toast");
        assert!(toast.context.contains("pod owner unreachable"));
        assert!(toast.context.contains(owner.as_str()));
    }

    #[test]
    fn pod_deleted_after_last_motion_toasts_group_not_found_without_moving() {
        let (mut app, _dir, pane_id) = app_with_groups("pod-deleted-race-toast");
        let created = created_group(&app.handle_group_create(
            "create".into(),
            GroupCreateParams {
                name: "Transient".into(),
                expected_revision: 0,
            },
        ));
        assert!(
            serde_json::from_str::<SuccessResponse>(&app.handle_group_delete(
                "delete".into(),
                GroupDeleteParams {
                    group_id: created.id.clone(),
                    expected_revision: created.revision,
                },
            ))
            .is_ok()
        );

        app.runtime_pane_group_set(PaneGroupSetParams {
            pane_id: pane_id.clone(),
            group_id: Some(created.id),
            expected_revision: 0,
            expected_pane_authority: app
                .state
                .local_group_snapshot
                .as_ref()
                .map(|snapshot| snapshot.authority_id.clone()),
            expected_pane_incarnation: None,
        });

        let pane = app
            .local_pane(&pane_id)
            .and_then(|(ws, pane)| app.state.workspaces[ws].pane_state(pane))
            .expect("pane");
        assert_eq!(pane.group_membership, PaneGroupMembership::default());
        let toast = app.state.toast.as_ref().expect("visible missing-pod toast");
        assert!(toast.context.contains("pod no longer exists"));
    }

    #[test]
    fn two_creates_from_the_same_base_revision_have_one_winner() {
        let (mut app, _dir, _) = app_with_groups("create-conflict");
        let first = app.handle_group_create(
            "first".into(),
            GroupCreateParams {
                name: "One".into(),
                expected_revision: 0,
            },
        );
        let second = app.handle_group_create(
            "second".into(),
            GroupCreateParams {
                name: "Two".into(),
                expected_revision: 0,
            },
        );

        assert!(serde_json::from_str::<SuccessResponse>(&first).is_ok());
        let second: serde_json::Value = serde_json::from_str(&second).unwrap();
        assert_eq!(second["error"]["code"], "revision_conflict");
    }

    #[test]
    fn delete_rejects_a_stale_expected_revision() {
        let (mut app, _dir, _) = app_with_groups("delete-conflict");
        let created = created_group(&app.handle_group_create(
            "create".into(),
            GroupCreateParams {
                name: "Work".into(),
                expected_revision: 0,
            },
        ));
        let renamed = created_group(&app.handle_group_rename(
            "rename".into(),
            GroupRenameParams {
                group_id: created.id.clone(),
                name: "Focus".into(),
                expected_revision: created.revision,
            },
        ));

        let response: serde_json::Value = serde_json::from_str(&app.handle_group_delete(
            "delete".into(),
            GroupDeleteParams {
                group_id: created.id,
                expected_revision: created.revision,
            },
        ))
        .unwrap();

        assert_eq!(response["error"]["code"], "revision_conflict");
        assert!(response["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains(&renamed.revision.to_string())));
    }

    #[test]
    fn membership_compare_and_set_is_durable_before_live_state_changes() {
        let (mut app, dir, pane_public_id) = app_with_groups("membership");
        let group = created_group(&app.handle_group_create(
            "create".into(),
            GroupCreateParams {
                name: "Work".into(),
                expected_revision: 0,
            },
        ));

        let first = app.handle_pane_group_set(
            "first".into(),
            PaneGroupSetParams {
                pane_id: pane_public_id.clone(),
                group_id: Some(group.id.clone()),
                expected_revision: 0,
                expected_pane_authority: None,
                expected_pane_incarnation: None,
            },
        );
        let first: SuccessResponse = serde_json::from_str(&first).expect("first assignment");
        let ResponseResult::PaneGroupSet { membership, .. } = first.result else {
            panic!("expected pane group response");
        };
        assert_eq!(membership.revision, 1);
        assert_eq!(membership.group_id.as_ref(), Some(&group.id));

        let loser: serde_json::Value = serde_json::from_str(&app.handle_pane_group_set(
            "loser".into(),
            PaneGroupSetParams {
                pane_id: pane_public_id,
                group_id: None,
                expected_revision: 0,
                expected_pane_authority: None,
                expected_pane_incarnation: None,
            },
        ))
        .unwrap();
        assert_eq!(loser["error"]["code"], "revision_conflict");

        let session_json = std::fs::read_to_string(dir.session_paths().0).unwrap();
        let snapshot: crate::persist::SessionSnapshot =
            serde_json::from_str(&session_json).expect("persisted session");
        let saved = snapshot.workspaces[0].tabs[0]
            .panes
            .values()
            .next()
            .expect("saved pane");
        assert_eq!(saved.group_membership, membership);
    }

    #[test]
    fn membership_persists_for_a_local_tab_behind_an_omitted_proxy_tab() {
        let (mut app, dir, _) = app_with_groups("membership-after-proxy");
        let local_tab = app.state.workspaces[0].test_add_tab(Some("later local"));
        app.state.ensure_test_terminals();
        let local_pane = app.state.workspaces[0].tabs[local_tab].root_pane;
        let local_public_id = app.public_pane_id(0, local_pane).expect("local pane id");

        let proxy_pane = crate::layout::PaneId::alloc();
        let proxy_terminal = crate::terminal::TerminalId::alloc();
        let (proxy_runtime, _channels) = crate::terminal::TerminalRuntime::spawn_remote_proxy(
            proxy_pane,
            24,
            80,
            0,
            std::sync::Arc::new(tokio::sync::Notify::new()),
            std::sync::Arc::new(crate::render_signal::RenderSignal::default()),
            crate::remote::RemoteFocusOperationState::new(),
        )
        .expect("proxy runtime");
        app.state.terminals.insert(
            proxy_terminal.clone(),
            crate::terminal::TerminalState::new(proxy_terminal.clone(), "/remote".into()),
        );
        app.terminal_runtimes
            .insert(proxy_terminal.clone(), proxy_runtime);
        let events = app.state.workspaces[0].tabs[0].events.clone();
        let proxy_tab = app.state.workspaces[0].create_tab_from_existing_pane(
            crate::workspace::MovedPane {
                pane_id: proxy_pane,
                pane_state: crate::pane::PaneState::new(proxy_terminal),
            },
            Some("proxy only".into()),
            events,
            std::sync::Arc::new(tokio::sync::Notify::new()),
            std::sync::Arc::new(crate::render_signal::RenderSignal::default()),
        );
        assert!(app.state.workspaces[0].move_tab(proxy_tab, 1));
        assert_eq!(
            app.state.workspaces[0].find_tab_index_for_pane(local_pane),
            Some(2)
        );

        let response = app.handle_pane_group_set(
            "clear".into(),
            PaneGroupSetParams {
                pane_id: local_public_id,
                group_id: None,
                expected_revision: 0,
                expected_pane_authority: None,
                expected_pane_incarnation: None,
            },
        );
        assert!(
            serde_json::from_str::<SuccessResponse>(&response).is_ok(),
            "pane membership should persist by identity: {response}"
        );

        let session_json = std::fs::read_to_string(dir.session_paths().0).unwrap();
        let snapshot: crate::persist::SessionSnapshot =
            serde_json::from_str(&session_json).expect("persisted session");
        let saved = snapshot.workspaces[0]
            .tabs
            .iter()
            .find_map(|tab| tab.panes.get(&local_pane.raw()))
            .expect("local pane persisted after compacting proxy-only tab");
        assert_eq!(saved.group_membership.revision, 1);
        assert!(saved.group_membership.group_id.is_none());
    }

    #[test]
    fn failed_membership_write_leaves_the_live_pane_unchanged() {
        let (mut app, dir, pane_public_id) = app_with_groups("membership-failure");
        let group = created_group(&app.handle_group_create(
            "create".into(),
            GroupCreateParams {
                name: "Work".into(),
                expected_revision: 0,
            },
        ));
        let blocker = dir.0.join("not-a-directory");
        std::fs::write(&blocker, "block").unwrap();
        app.group_session_paths_override = Some((
            blocker.join("session.json"),
            blocker.join("session-history.json"),
        ));

        let response: serde_json::Value = serde_json::from_str(&app.handle_pane_group_set(
            "assign".into(),
            PaneGroupSetParams {
                pane_id: pane_public_id,
                group_id: Some(group.id),
                expected_revision: 0,
                expected_pane_authority: None,
                expected_pane_incarnation: None,
            },
        ))
        .unwrap();

        assert_eq!(response["error"]["code"], "persistence_failed");
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        assert_eq!(
            app.state.workspaces[0]
                .pane_state(pane_id)
                .unwrap()
                .group_membership,
            PaneGroupMembership::default()
        );
    }

    #[test]
    fn membership_follows_a_pane_when_its_public_locator_changes() {
        let (mut app, _dir, source_public_id) = app_with_groups("move");
        let group = created_group(&app.handle_group_create(
            "create".into(),
            GroupCreateParams {
                name: "Work".into(),
                expected_revision: 0,
            },
        ));
        let response = app.handle_pane_group_set(
            "assign".into(),
            PaneGroupSetParams {
                pane_id: source_public_id.clone(),
                group_id: Some(group.id.clone()),
                expected_revision: 0,
                expected_pane_authority: None,
                expected_pane_incarnation: None,
            },
        );
        assert!(serde_json::from_str::<SuccessResponse>(&response).is_ok());
        let source_pane = app.state.workspaces[0].tabs[0].root_pane;
        app.state.workspaces.push(Workspace::test_new("target"));
        let target_tab_index = app.state.workspaces[1].test_add_tab(Some("destination"));
        app.state.ensure_test_terminals();
        let target_pane = app.state.workspaces[1].tabs[target_tab_index].root_pane;
        let target_tab_id = app.public_tab_id(1, target_tab_index).unwrap();
        let target_pane_id = app.public_pane_id(1, target_pane).unwrap();

        let response = app.handle_pane_move(
            "move".into(),
            PaneMoveParams {
                pane_id: source_public_id.clone(),
                destination: PaneMoveDestination::Tab {
                    tab_id: target_tab_id,
                    target_pane_id: Some(target_pane_id),
                    split: SplitDirection::Down,
                    ratio: None,
                },
                focus: false,
            },
        );
        assert!(serde_json::from_str::<SuccessResponse>(&response).is_ok());
        let moved = app.state.workspaces[0]
            .pane_state(source_pane)
            .expect("moved pane");
        assert_ne!(
            app.public_pane_id(0, source_pane).as_deref(),
            Some(source_public_id.as_str())
        );
        assert_eq!(moved.group_membership.group_id.as_ref(), Some(&group.id));

        let before_alias_change = moved.group_membership.clone();
        app.state.agent_host_name = "renamed-alias".into();
        assert_eq!(
            app.state.workspaces[0]
                .pane_state(source_pane)
                .unwrap()
                .group_membership,
            before_alias_change
        );
    }

    #[test]
    fn host_snapshot_never_serves_a_foreign_group_record() {
        let (mut app, foreign_dir, _) = app_with_groups("source-only");
        let local = created_group(&app.handle_group_create(
            "create".into(),
            GroupCreateParams {
                name: "Local".into(),
                expected_revision: 0,
            },
        ));
        let other_dir = TestDir::new("other-authority");
        let other = crate::groups::Runtime::load(&other_dir.0);
        let foreign_id = GroupId {
            owner: other.authority().unwrap().authority_id().clone(),
            local: 99,
        };
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        app.state.workspaces[0]
            .pane_state_mut(pane_id)
            .unwrap()
            .group_membership = PaneGroupMembership {
            group_id: Some(foreign_id.clone()),
            revision: 4,
        };
        let local_public_id = app.public_pane_id(0, pane_id).unwrap();
        app.refresh_group_membership_projection_for_pane(&local_public_id);
        let proxy_pane = crate::layout::PaneId::alloc();
        let proxy_terminal = crate::terminal::TerminalId::alloc();
        let (proxy_runtime, _channels) = crate::terminal::TerminalRuntime::spawn_remote_proxy(
            proxy_pane,
            24,
            80,
            0,
            std::sync::Arc::new(tokio::sync::Notify::new()),
            std::sync::Arc::new(crate::render_signal::RenderSignal::default()),
            crate::remote::RemoteFocusOperationState::new(),
        )
        .expect("proxy runtime");
        app.state.terminals.insert(
            proxy_terminal.clone(),
            crate::terminal::TerminalState::new(proxy_terminal.clone(), "/remote".into()),
        );
        app.terminal_runtimes
            .insert(proxy_terminal.clone(), proxy_runtime);
        let events = app.state.workspaces[0].tabs[0].events.clone();
        let mut proxy_state = crate::pane::PaneState::new(proxy_terminal);
        proxy_state.group_membership = PaneGroupMembership {
            group_id: Some(foreign_id.clone()),
            revision: 5,
        };
        app.state.workspaces[0].create_tab_from_existing_pane(
            crate::workspace::MovedPane {
                pane_id: proxy_pane,
                pane_state: proxy_state,
            },
            Some("remote proxy".into()),
            events,
            std::sync::Arc::new(tokio::sync::Notify::new()),
            std::sync::Arc::new(crate::render_signal::RenderSignal::default()),
        );
        let proxy_public_id = app.public_pane_id(0, proxy_pane).unwrap();

        let response: SuccessResponse =
            serde_json::from_str(&app.handle_group_host_snapshot("snapshot".into())).unwrap();
        let ResponseResult::GroupHostSnapshot { snapshot } = response.result else {
            panic!("expected host snapshot");
        };
        assert_eq!(snapshot.groups, vec![local]);
        assert!(snapshot
            .groups
            .iter()
            .all(|record| record.id.owner == snapshot.authority_id));
        assert_eq!(
            snapshot.memberships[0].membership.group_id.as_ref(),
            Some(&foreign_id)
        );
        assert_eq!(snapshot.memberships.len(), 1);
        assert_ne!(snapshot.memberships[0].pane_id, proxy_public_id);
        drop(foreign_dir);
    }

    #[test]
    fn foreign_mutations_name_the_authority_when_no_fresh_route_exists() {
        let (mut app, _dir, pane_public_id) = app_with_groups("foreign-mutation");
        let local_authority = app
            .group_runtime
            .authority()
            .expect("local authority")
            .authority_id()
            .clone();
        let other_dir = TestDir::new("foreign-owner");
        let other = crate::groups::Runtime::load(&other_dir.0);
        let foreign_id = GroupId {
            owner: other.authority().unwrap().authority_id().clone(),
            local: 1,
        };

        let foreign_authority_responses = [
            app.handle_group_rename(
                "rename".into(),
                GroupRenameParams {
                    group_id: foreign_id.clone(),
                    name: "Foreign".into(),
                    expected_revision: 1,
                },
            ),
            app.handle_group_delete(
                "delete".into(),
                GroupDeleteParams {
                    group_id: foreign_id.clone(),
                    expected_revision: 1,
                },
            ),
        ];

        for response in foreign_authority_responses {
            let response: serde_json::Value = serde_json::from_str(&response).unwrap();
            assert_eq!(response["error"]["code"], "authority_not_fresh");
            assert!(response["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains(foreign_id.owner.as_str())));
        }

        let pane_response: serde_json::Value = serde_json::from_str(&app.handle_pane_group_set(
            "assign".into(),
            PaneGroupSetParams {
                pane_id: pane_public_id,
                group_id: Some(foreign_id),
                expected_revision: 0,
                expected_pane_authority: Some(local_authority.clone()),
                expected_pane_incarnation: None,
            },
        ))
        .expect("pane mutation response");
        assert_eq!(pane_response["error"]["code"], "authority_not_fresh");
        assert!(pane_response["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains(local_authority.as_str())));
    }

    #[test]
    fn receiver_only_mutation_rejects_a_second_forward() {
        let (mut app, _dir, _) = app_with_groups("receiver-only");
        let authority = app
            .group_runtime
            .authority()
            .expect("local authority")
            .authority_id()
            .clone();
        let response: serde_json::Value = serde_json::from_str(&app.handle_group_authority_mutate(
            "forward".into(),
            AuthorityMutationParams {
                expected_authority: authority,
                forwarded: false,
                mutation: AuthorityMutation::Delete(GroupDeleteParams {
                    group_id: GroupId {
                        owner: crate::groups::AuthorityId::from_random_bytes([3; 16]),
                        local: 1,
                    },
                    expected_revision: 1,
                }),
            },
        ))
        .unwrap();

        assert_eq!(response["error"]["code"], "invalid_authority_route");
    }

    #[test]
    fn clearing_a_reachable_local_pane_ignores_foreign_catalog_freshness() {
        let (mut app, _dir, pane_public_id) = app_with_groups("clear-stale-foreign");
        let foreign = GroupId {
            owner: crate::groups::AuthorityId::from_random_bytes([4; 16]),
            local: 1,
        };
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        app.state.workspaces[0]
            .pane_state_mut(pane_id)
            .expect("local pane")
            .group_membership = PaneGroupMembership {
            group_id: Some(foreign),
            revision: 7,
        };

        let response = app.handle_pane_group_set(
            "clear".into(),
            PaneGroupSetParams {
                pane_id: pane_public_id,
                group_id: None,
                expected_revision: 7,
                expected_pane_authority: None,
                expected_pane_incarnation: None,
            },
        );

        assert!(
            serde_json::from_str::<SuccessResponse>(&response).is_ok(),
            "clearing a reachable pane must not need the old group owner: {response}"
        );
    }

    #[test]
    fn clearing_a_remote_pane_names_its_stale_owner() {
        let (mut app, _dir, _) = app_with_groups("clear-stale-pane-owner");
        let pane_owner = crate::groups::AuthorityId::from_random_bytes([5; 16]);
        app.state.fleet_snapshot.group_catalogs = vec![crate::fleet::GroupCatalog {
            host: "remote".into(),
            target: "remote".into(),
            local: false,
            session: None,
            socket: None,
            state: crate::fleet::GroupCatalogState::Stale,
            observed_authority_id: Some(pane_owner.clone()),
            snapshot: Some(GroupAuthoritySnapshot {
                authority_id: pane_owner.clone(),
                revision: 0,
                groups: Vec::new(),
                memberships: vec![OwnedPaneMembership {
                    pane_id: "remote-pane".into(),
                    pane_incarnation: "remote-incarnation".into(),
                    membership: PaneGroupMembership::default(),
                }],
            }),
            error: Some("offline".into()),
        }];

        let response: serde_json::Value = serde_json::from_str(&app.handle_pane_group_set(
            "clear".into(),
            PaneGroupSetParams {
                pane_id: "remote-pane".into(),
                group_id: None,
                expected_revision: 0,
                expected_pane_authority: Some(pane_owner.clone()),
                expected_pane_incarnation: None,
            },
        ))
        .unwrap();

        assert_eq!(response["error"]["code"], "authority_not_fresh");
        assert!(response["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains(pane_owner.as_str())));
    }

    #[test]
    fn forwarded_pane_mutation_requires_the_receivers_authority() {
        let (mut app, _dir, pane_id) = app_with_groups("qualified-receiver");
        let actual = app
            .group_runtime
            .authority()
            .expect("local authority")
            .authority_id()
            .clone();
        let other = crate::groups::AuthorityId::from_random_bytes([8; 16]);

        let response: serde_json::Value = serde_json::from_str(&app.handle_group_authority_mutate(
            "forward".into(),
            AuthorityMutationParams {
                expected_authority: actual,
                forwarded: true,
                mutation: AuthorityMutation::PaneGroupSet(PaneGroupSetParams {
                    pane_id,
                    group_id: None,
                    expected_revision: 0,
                    expected_pane_authority: Some(other.clone()),
                    expected_pane_incarnation: None,
                }),
            },
        ))
        .unwrap();

        assert_eq!(response["error"]["code"], "authority_mismatch");
        assert!(response["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains(other.as_str())));
    }

    #[test]
    fn observed_local_authority_collision_blocks_group_and_assignment_mutations() {
        let (mut app, _dir, pane_id) = app_with_groups("local-authority-collision");
        let created = created_group(&app.handle_group_create(
            "before_conflict".into(),
            GroupCreateParams {
                name: "Work".into(),
                expected_revision: 0,
            },
        ));
        let authority = created.id.owner.clone();
        let catalog = |host: &str| crate::fleet::GroupCatalog {
            host: host.into(),
            target: host.into(),
            local: true,
            session: None,
            socket: Some(format!("/tmp/{host}.sock")),
            state: crate::fleet::GroupCatalogState::IdentityConflict,
            observed_authority_id: Some(authority.clone()),
            snapshot: Some(GroupAuthoritySnapshot {
                authority_id: authority.clone(),
                revision: 1,
                groups: vec![created.clone()],
                memberships: Vec::new(),
            }),
            error: Some("identity conflict".into()),
        };
        app.state.fleet_snapshot.group_catalogs = vec![catalog("one"), catalog("two")];

        let responses = [
            app.handle_group_create(
                "create".into(),
                GroupCreateParams {
                    name: "Other".into(),
                    expected_revision: 1,
                },
            ),
            app.handle_group_rename(
                "rename".into(),
                GroupRenameParams {
                    group_id: created.id.clone(),
                    name: "Renamed".into(),
                    expected_revision: 1,
                },
            ),
            app.handle_group_delete(
                "delete".into(),
                GroupDeleteParams {
                    group_id: created.id.clone(),
                    expected_revision: 1,
                },
            ),
            app.handle_pane_group_set(
                "membership".into(),
                PaneGroupSetParams {
                    pane_id,
                    group_id: Some(created.id.clone()),
                    expected_revision: 0,
                    expected_pane_authority: Some(authority.clone()),
                    expected_pane_incarnation: None,
                },
            ),
        ];

        for response in responses {
            let response: serde_json::Value = serde_json::from_str(&response).unwrap();
            assert_eq!(response["error"]["code"], "authority_not_fresh");
            assert!(response["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains(authority.as_str())));
        }
        assert!(app.validate_group_target(&created.id).is_err());
    }

    #[test]
    fn observed_local_authority_collision_does_not_block_clearing_a_local_pane() {
        let (mut app, _dir, pane_id) = app_with_groups("clear-local-authority-collision");
        let created = created_group(&app.handle_group_create(
            "create".into(),
            GroupCreateParams {
                name: "Work".into(),
                expected_revision: 0,
            },
        ));
        let authority = created.id.owner.clone();
        let local_pane_id = app.state.workspaces[0].tabs[0].root_pane;
        app.state.workspaces[0]
            .pane_state_mut(local_pane_id)
            .expect("local pane")
            .group_membership = PaneGroupMembership {
            group_id: Some(created.id.clone()),
            revision: 4,
        };
        app.state.fleet_snapshot.group_catalogs = ["one", "two"]
            .into_iter()
            .map(|host| crate::fleet::GroupCatalog {
                host: host.into(),
                target: host.into(),
                local: true,
                session: None,
                socket: Some(format!("/tmp/{host}.sock")),
                state: crate::fleet::GroupCatalogState::IdentityConflict,
                observed_authority_id: Some(authority.clone()),
                snapshot: Some(GroupAuthoritySnapshot {
                    authority_id: authority.clone(),
                    revision: 1,
                    groups: vec![created.clone()],
                    memberships: Vec::new(),
                }),
                error: Some("identity conflict".into()),
            })
            .collect();

        let response = app.handle_pane_group_set(
            "clear".into(),
            PaneGroupSetParams {
                pane_id,
                group_id: None,
                expected_revision: 4,
                expected_pane_authority: Some(authority),
                expected_pane_incarnation: None,
            },
        );

        assert!(
            serde_json::from_str::<SuccessResponse>(&response).is_ok(),
            "clearing a reachable pane must ignore group authority conflicts: {response}"
        );
    }

    #[test]
    fn observed_local_authority_collision_blocks_assignment_to_a_foreign_group() {
        let (mut app, _dir, pane_id) = app_with_groups("foreign-assignment-local-collision");
        let local = app
            .group_runtime
            .authority()
            .expect("local authority")
            .authority_id()
            .clone();
        let foreign = crate::groups::AuthorityId::from_random_bytes([12; 16]);
        let foreign_group = GroupRecord {
            id: GroupId {
                owner: foreign.clone(),
                local: 1,
            },
            revision: 1,
            state: GroupState::Active {
                name: "Foreign".into(),
            },
        };
        let local_collision = |host: &str| crate::fleet::GroupCatalog {
            host: host.into(),
            target: host.into(),
            local: true,
            session: None,
            socket: Some(format!("/tmp/{host}.sock")),
            state: crate::fleet::GroupCatalogState::IdentityConflict,
            observed_authority_id: Some(local.clone()),
            snapshot: Some(GroupAuthoritySnapshot {
                authority_id: local.clone(),
                revision: 0,
                groups: Vec::new(),
                memberships: Vec::new(),
            }),
            error: Some("identity conflict".into()),
        };
        app.state.fleet_snapshot.group_catalogs = vec![
            local_collision("one"),
            local_collision("two"),
            crate::fleet::GroupCatalog {
                host: "foreign".into(),
                target: "foreign".into(),
                local: false,
                session: None,
                socket: None,
                state: crate::fleet::GroupCatalogState::Fresh,
                observed_authority_id: Some(foreign.clone()),
                snapshot: Some(GroupAuthoritySnapshot {
                    authority_id: foreign,
                    revision: 1,
                    groups: vec![foreign_group.clone()],
                    memberships: Vec::new(),
                }),
                error: None,
            },
        ];

        let response: serde_json::Value = serde_json::from_str(&app.handle_pane_group_set(
            "assign".into(),
            PaneGroupSetParams {
                pane_id,
                group_id: Some(foreign_group.id),
                expected_revision: 0,
                expected_pane_authority: Some(local.clone()),
                expected_pane_incarnation: None,
            },
        ))
        .expect("assignment response");

        assert_eq!(response["error"]["code"], "authority_not_fresh");
        assert!(response["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains(local.as_str())));
    }

    #[test]
    fn remote_pane_move_target_refusals_name_the_pane_authority() {
        let pane_owner = crate::groups::AuthorityId::from_random_bytes([33; 16]);
        let group_owner = crate::groups::AuthorityId::from_random_bytes([34; 16]);
        let (group_id, base_catalogs) = remote_pane_move_catalogs(&pane_owner, &group_owner);

        let catalogs = ["missing", "stale", "deleted", "conflicted"]
            .into_iter()
            .map(|case| {
                let mut catalogs = base_catalogs.clone();
                match case {
                    "missing" => catalogs[1]
                        .snapshot
                        .as_mut()
                        .expect("group catalog snapshot")
                        .groups
                        .clear(),
                    "stale" => catalogs[1].state = crate::fleet::GroupCatalogState::Stale,
                    "deleted" => {
                        catalogs[1]
                            .snapshot
                            .as_mut()
                            .expect("group catalog snapshot")
                            .groups[0]
                            .state = GroupState::Deleted;
                    }
                    "conflicted" => {
                        let mut duplicate = catalogs[1].clone();
                        duplicate.host = "group-owner-copy".into();
                        duplicate.target = "group-host-copy".into();
                        duplicate.state = crate::fleet::GroupCatalogState::IdentityConflict;
                        catalogs[1].state = crate::fleet::GroupCatalogState::IdentityConflict;
                        catalogs.push(duplicate);
                    }
                    _ => unreachable!(),
                }
                (case, catalogs)
            })
            .collect::<Vec<_>>();

        for (case, catalogs) in catalogs {
            let (mut app, _dir, _) = app_with_groups(case);
            app.state.fleet_snapshot.group_catalogs = catalogs;
            let response: crate::api::schema::ErrorResponse =
                serde_json::from_str(&app.handle_pane_group_set(
                    case.into(),
                    PaneGroupSetParams {
                        pane_id: "remote-pane".into(),
                        group_id: Some(group_id.clone()),
                        expected_revision: 0,
                        expected_pane_authority: Some(pane_owner.clone()),
                        expected_pane_incarnation: None,
                    },
                ))
                .expect("target preflight refusal");
            assert_eq!(
                response.error.code,
                if matches!(case, "missing" | "deleted") {
                    "group_not_found"
                } else {
                    "authority_not_fresh"
                },
                "{case}"
            );
            assert!(
                response.error.message.contains(pane_owner.as_str()),
                "{case} refusal named the group authority instead: {}",
                response.error.message
            );
        }
    }

    #[test]
    fn local_pane_move_target_refusals_name_the_pane_authority() {
        let group_owner = crate::groups::AuthorityId::from_random_bytes([37; 16]);
        let group_id = GroupId {
            owner: group_owner.clone(),
            local: 1,
        };
        let base_catalog = crate::fleet::GroupCatalog {
            host: "group-owner".into(),
            target: "group-host".into(),
            local: false,
            session: None,
            socket: None,
            state: crate::fleet::GroupCatalogState::Fresh,
            observed_authority_id: Some(group_owner.clone()),
            snapshot: Some(GroupAuthoritySnapshot {
                authority_id: group_owner,
                revision: 1,
                groups: vec![GroupRecord {
                    id: group_id.clone(),
                    revision: 1,
                    state: GroupState::Active {
                        name: "Remote".into(),
                    },
                }],
                memberships: Vec::new(),
            }),
            error: None,
        };

        for (kind, supplied) in [
            ("missing", true),
            ("stale", true),
            ("deleted", true),
            ("conflicted", true),
            ("missing", false),
            ("stale", false),
            ("deleted", false),
            ("conflicted", false),
        ] {
            let case = if supplied {
                format!("{kind}-supplied")
            } else {
                format!("{kind}-omitted")
            };
            let (mut app, _dir, pane_id) = app_with_groups(&case);
            let pane_owner = app
                .group_runtime
                .authority()
                .expect("pane authority")
                .authority_id()
                .clone();
            let mut catalog = base_catalog.clone();
            match kind {
                "missing" => catalog
                    .snapshot
                    .as_mut()
                    .expect("group catalog snapshot")
                    .groups
                    .clear(),
                "stale" => catalog.state = crate::fleet::GroupCatalogState::Stale,
                "deleted" => {
                    catalog
                        .snapshot
                        .as_mut()
                        .expect("group catalog snapshot")
                        .groups[0]
                        .state = GroupState::Deleted;
                }
                "conflicted" => catalog.state = crate::fleet::GroupCatalogState::IdentityConflict,
                _ => unreachable!(),
            }
            app.state.fleet_snapshot.group_catalogs = vec![catalog];

            let response: crate::api::schema::ErrorResponse =
                serde_json::from_str(&app.handle_pane_group_set(
                    case.clone(),
                    PaneGroupSetParams {
                        pane_id,
                        group_id: Some(group_id.clone()),
                        expected_revision: 0,
                        expected_pane_authority: supplied.then(|| pane_owner.clone()),
                        expected_pane_incarnation: None,
                    },
                ))
                .expect("local target refusal");
            assert_eq!(
                response.error.code,
                if matches!(
                    case.as_str(),
                    "missing-supplied" | "missing-omitted" | "deleted-supplied" | "deleted-omitted"
                ) {
                    "group_not_found"
                } else {
                    "authority_not_fresh"
                },
                "{case}"
            );
            assert!(
                response.error.message.contains(pane_owner.as_str()),
                "{case} refusal omitted the local pane authority: {}",
                response.error.message
            );
        }
    }

    #[test]
    fn local_pane_move_revision_refusal_names_the_pane_authority_without_one_supplied() {
        let (mut app, _dir, pane_id) = app_with_groups("omitted-revision");
        let pane_owner = app
            .group_runtime
            .authority()
            .expect("pane authority")
            .authority_id()
            .clone();

        let response: crate::api::schema::ErrorResponse =
            serde_json::from_str(&app.handle_pane_group_set(
                "clear".into(),
                PaneGroupSetParams {
                    pane_id,
                    group_id: None,
                    expected_revision: 99,
                    expected_pane_authority: None,
                    expected_pane_incarnation: None,
                },
            ))
            .expect("local revision refusal");
        assert_eq!(response.error.code, "revision_conflict");
        assert!(
            response.error.message.contains(pane_owner.as_str()),
            "refusal omitted the local pane authority: {}",
            response.error.message
        );
    }

    #[test]
    fn forwarded_pane_move_runtime_refusal_names_the_pane_authority() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let pane_owner = crate::groups::AuthorityId::from_random_bytes([38; 16]);
        let response: crate::api::schema::ErrorResponse =
            serde_json::from_str(&app.handle_group_authority_mutate(
                "runtime-unavailable".into(),
                AuthorityMutationParams {
                    expected_authority: pane_owner.clone(),
                    forwarded: true,
                    mutation: AuthorityMutation::PaneGroupSet(PaneGroupSetParams {
                        pane_id: "missing-pane".into(),
                        group_id: None,
                        expected_revision: 0,
                        expected_pane_authority: Some(pane_owner.clone()),
                        expected_pane_incarnation: Some("missing-incarnation".into()),
                    }),
                },
            ))
            .expect("receiver runtime refusal");

        assert_eq!(response.error.code, "group_authority_unavailable");
        assert!(response.error.message.contains(pane_owner.as_str()));
    }

    #[test]
    fn forwarded_pane_move_target_refusal_names_the_pane_authority() {
        let (mut app, _dir, pane_id) = app_with_groups("forwarded-target-refusal");
        let pane_owner = app
            .group_runtime
            .authority()
            .expect("pane authority")
            .authority_id()
            .clone();
        let pane = app.state.workspaces[0].tabs[0].root_pane;
        let pane_incarnation = app.state.workspaces[0]
            .pane_state(pane)
            .expect("local pane")
            .attached_terminal_id
            .to_string();
        let group_owner = crate::groups::AuthorityId::from_random_bytes([35; 16]);

        let response: crate::api::schema::ErrorResponse =
            serde_json::from_str(&app.handle_group_authority_mutate(
                "forwarded-target".into(),
                AuthorityMutationParams {
                    expected_authority: pane_owner.clone(),
                    forwarded: true,
                    mutation: AuthorityMutation::PaneGroupSet(PaneGroupSetParams {
                        pane_id,
                        group_id: Some(GroupId {
                            owner: group_owner,
                            local: 1,
                        }),
                        expected_revision: 0,
                        expected_pane_authority: Some(pane_owner.clone()),
                        expected_pane_incarnation: Some(pane_incarnation),
                    }),
                },
            ))
            .expect("receiver target refusal");

        assert_eq!(response.error.code, "authority_not_fresh");
        assert!(response.error.message.contains(pane_owner.as_str()));
    }

    #[test]
    fn remote_pane_move_lease_refusal_names_the_pane_authority() {
        let (mut app, _dir, _) = app_with_groups("remote-move-stale-lease");
        let pane_owner = crate::groups::AuthorityId::from_random_bytes([22; 16]);
        let group_owner = crate::groups::AuthorityId::from_random_bytes([23; 16]);
        let (group_id, catalogs) = remote_pane_move_catalogs(&pane_owner, &group_owner);
        app.state.fleet_snapshot.group_catalogs = catalogs;
        app.authority_mutation_router
            .observe_snapshot(&app.state.fleet_snapshot);
        app.authority_mutation_router.poison_route_leases_for_test();
        let (respond_to, response_rx) = std::sync::mpsc::channel();

        app.handle_api_request_message(crate::api::ApiRequestMessage {
            request: Request {
                id: "move".into(),
                method: Method::PaneGroupSet(PaneGroupSetParams {
                    pane_id: "remote-pane".into(),
                    group_id: Some(group_id),
                    expected_revision: 0,
                    expected_pane_authority: Some(pane_owner.clone()),
                    expected_pane_incarnation: None,
                }),
            },
            respond_to,
            response_write_complete: None,
            stream_active: None,
        });

        let response = response_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("immediate lease refusal");
        let response: crate::api::schema::ErrorResponse =
            serde_json::from_str(&response).expect("lease refusal response");
        assert_eq!(response.error.code, "authority_unreachable");
        assert!(response.error.message.contains(pane_owner.as_str()));
    }

    #[test]
    fn remote_pane_move_receiver_refusals_name_the_pane_authority() {
        fn params(
            pane_id: String,
            authority: &crate::groups::AuthorityId,
            expected_revision: u64,
        ) -> PaneGroupSetParams {
            PaneGroupSetParams {
                pane_id,
                group_id: None,
                expected_revision,
                expected_pane_authority: Some(authority.clone()),
                expected_pane_incarnation: Some("receiver-fixture".into()),
            }
        }

        fn refusal(response: String) -> crate::api::schema::ErrorResponse {
            serde_json::from_str(&response).expect("receiver refusal response")
        }

        let mut refusals = Vec::new();

        let (mut disabled, _dir, pane_id) = app_with_groups("receiver-disabled");
        let authority = disabled
            .group_runtime
            .authority()
            .expect("receiver authority")
            .authority_id()
            .clone();
        disabled.no_session = true;
        refusals.push((
            authority.clone(),
            refusal(
                disabled.apply_pane_group_set("disabled".into(), params(pane_id, &authority, 0)),
            ),
        ));

        let (mut missing, _dir, _) = app_with_groups("receiver-missing");
        let authority = missing
            .group_runtime
            .authority()
            .expect("receiver authority")
            .authority_id()
            .clone();
        refusals.push((
            authority.clone(),
            refusal(missing.apply_pane_group_set(
                "missing".into(),
                params("missing-pane".into(), &authority, 0),
            )),
        ));

        let (mut foreign, _dir, pane_id) = app_with_groups("receiver-not-owned");
        let authority = foreign
            .group_runtime
            .authority()
            .expect("receiver authority")
            .authority_id()
            .clone();
        let pane = foreign.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = foreign.state.workspaces[0]
            .pane_state(pane)
            .expect("receiver pane")
            .attached_terminal_id
            .clone();
        let (proxy_runtime, _channels) = crate::terminal::TerminalRuntime::spawn_remote_proxy(
            pane,
            24,
            80,
            0,
            std::sync::Arc::new(tokio::sync::Notify::new()),
            std::sync::Arc::new(crate::render_signal::RenderSignal::default()),
            crate::remote::RemoteFocusOperationState::new(),
        )
        .expect("proxy runtime");
        foreign.terminal_runtimes.insert(terminal_id, proxy_runtime);
        refusals.push((
            authority.clone(),
            refusal(
                foreign.apply_pane_group_set("not-owned".into(), params(pane_id, &authority, 0)),
            ),
        ));

        let (mut conflict, _dir, pane_id) = app_with_groups("receiver-conflict");
        let authority = conflict
            .group_runtime
            .authority()
            .expect("receiver authority")
            .authority_id()
            .clone();
        refusals.push((
            authority.clone(),
            refusal(
                conflict.apply_pane_group_set("conflict".into(), params(pane_id, &authority, 1)),
            ),
        ));

        let (mut failed, dir, pane_id) = app_with_groups("receiver-persist-failure");
        let authority = failed
            .group_runtime
            .authority()
            .expect("receiver authority")
            .authority_id()
            .clone();
        let blocker = dir.0.join("not-a-directory");
        std::fs::write(&blocker, "block").expect("create persistence blocker");
        failed.group_session_paths_override = Some((
            blocker.join("session.json"),
            blocker.join("session-history.json"),
        ));
        refusals.push((
            authority.clone(),
            refusal(failed.apply_pane_group_set("persist".into(), params(pane_id, &authority, 0))),
        ));

        assert_eq!(
            refusals
                .iter()
                .map(|(_, response)| response.error.code.as_str())
                .collect::<Vec<_>>(),
            [
                "group_persistence_disabled",
                "pane_not_found",
                "pane_not_owned",
                "revision_conflict",
                "persistence_failed",
            ]
        );
        let unnamed = refusals
            .iter()
            .filter(|(authority, response)| !response.error.message.contains(authority.as_str()))
            .map(|(_, response)| response.error.code.as_str())
            .collect::<Vec<_>>();
        assert!(unnamed.is_empty(), "unnamed receiver refusals: {unnamed:?}");
    }

    #[cfg(unix)]
    #[test]
    fn remote_pane_move_transport_refusal_names_the_pane_authority() {
        let (mut app, dir, _) = app_with_groups("remote-move-transport-refusal");
        let fake_ssh = dir.0.join("refusing-ssh");
        write_executable(
            &fake_ssh,
            "#!/bin/sh\ncat >/dev/null\nprintf 'transport refused by fixture\\n' >&2\nexit 17\n",
        );
        app.authority_mutation_router = crate::fleet::AuthorityMutationRouter::with_ssh_program(
            fake_ssh,
            std::time::Duration::from_secs(1),
        );
        let pane_owner = crate::groups::AuthorityId::from_random_bytes([24; 16]);
        let group_owner = crate::groups::AuthorityId::from_random_bytes([25; 16]);
        let (group_id, catalogs) = remote_pane_move_catalogs(&pane_owner, &group_owner);
        app.state.fleet_snapshot.group_catalogs = catalogs;
        app.authority_mutation_router
            .observe_snapshot(&app.state.fleet_snapshot);
        let (respond_to, response_rx) = std::sync::mpsc::channel();

        app.handle_api_request_message(crate::api::ApiRequestMessage {
            request: Request {
                id: "move".into(),
                method: Method::PaneGroupSet(PaneGroupSetParams {
                    pane_id: "remote-pane".into(),
                    group_id: Some(group_id),
                    expected_revision: 0,
                    expected_pane_authority: Some(pane_owner.clone()),
                    expected_pane_incarnation: None,
                }),
            },
            respond_to,
            response_write_complete: None,
            stream_active: None,
        });

        let response = response_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("transport refusal");
        let response: crate::api::schema::ErrorResponse =
            serde_json::from_str(&response).expect("transport refusal response");
        assert_eq!(response.error.code, "authority_unreachable");
        assert!(response.error.message.contains(pane_owner.as_str()));
    }

    #[cfg(unix)]
    #[test]
    fn remote_pane_move_forwarded_refusal_names_the_pane_authority() {
        let (mut app, dir, _) = app_with_groups("remote-move-forwarded-refusal");
        let fake_ssh = dir.0.join("receiver-refusal-ssh");
        write_executable(
            &fake_ssh,
            "#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' '{\"id\":\"move\",\"error\":{\"code\":\"group_persistence_disabled\",\"message\":\"pane membership requires session persistence\"}}'\n",
        );
        app.authority_mutation_router = crate::fleet::AuthorityMutationRouter::with_ssh_program(
            fake_ssh,
            std::time::Duration::from_secs(1),
        );
        let pane_owner = crate::groups::AuthorityId::from_random_bytes([26; 16]);
        let group_owner = crate::groups::AuthorityId::from_random_bytes([27; 16]);
        let (group_id, catalogs) = remote_pane_move_catalogs(&pane_owner, &group_owner);
        app.state.fleet_snapshot.group_catalogs = catalogs;
        app.authority_mutation_router
            .observe_snapshot(&app.state.fleet_snapshot);
        let (respond_to, response_rx) = std::sync::mpsc::channel();

        app.handle_api_request_message(crate::api::ApiRequestMessage {
            request: Request {
                id: "move".into(),
                method: Method::PaneGroupSet(PaneGroupSetParams {
                    pane_id: "remote-pane".into(),
                    group_id: Some(group_id),
                    expected_revision: 0,
                    expected_pane_authority: Some(pane_owner.clone()),
                    expected_pane_incarnation: None,
                }),
            },
            respond_to,
            response_write_complete: None,
            stream_active: None,
        });

        let response = response_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("forwarded receiver refusal");
        let response: crate::api::schema::ErrorResponse =
            serde_json::from_str(&response).expect("forwarded refusal response");
        assert_eq!(response.error.code, "group_persistence_disabled");
        assert!(response.error.message.contains(pane_owner.as_str()));
    }

    #[cfg(unix)]
    #[test]
    fn unreachable_remote_mutation_does_not_block_the_app_event_loop() {
        let (mut app, dir, _) = app_with_groups("deferred-remote-mutation");
        let fake_ssh = dir.0.join("blocking-ssh");
        let started_marker = dir.0.join("ssh-started");
        let captured_request = dir.0.join("ssh-request");
        write_executable(
            &fake_ssh,
            &format!(
                "#!/bin/sh\ncat > '{}'\ntouch '{}'\nsleep 30\n",
                captured_request.display(),
                started_marker.display(),
            ),
        );
        app.authority_mutation_router = crate::fleet::AuthorityMutationRouter::with_ssh_program(
            fake_ssh,
            std::time::Duration::from_secs(2),
        );
        let authority = crate::groups::AuthorityId::from_random_bytes([14; 16]);
        let group_id = GroupId {
            owner: authority.clone(),
            local: 1,
        };
        app.state.fleet_snapshot.group_catalogs = vec![crate::fleet::GroupCatalog {
            host: "remote".into(),
            target: "unreachable".into(),
            local: false,
            session: None,
            socket: None,
            state: crate::fleet::GroupCatalogState::Fresh,
            observed_authority_id: Some(authority.clone()),
            snapshot: Some(GroupAuthoritySnapshot {
                authority_id: authority,
                revision: 1,
                groups: vec![GroupRecord {
                    id: group_id.clone(),
                    revision: 1,
                    state: GroupState::Active {
                        name: "Remote".into(),
                    },
                }],
                memberships: Vec::new(),
            }),
            error: None,
        }];
        app.authority_mutation_router
            .observe_snapshot(&app.state.fleet_snapshot);
        let (mutation_tx, mutation_rx) = std::sync::mpsc::channel();
        app.handle_api_request_message(crate::api::ApiRequestMessage {
            request: Request {
                id: "mutation".into(),
                method: Method::GroupDelete(GroupDeleteParams {
                    group_id,
                    expected_revision: 1,
                }),
            },
            respond_to: mutation_tx,
            response_write_complete: None,
            stream_active: None,
        });

        assert!(mutation_rx.try_recv().is_err());
        let transport_deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while !started_marker.exists() && std::time::Instant::now() < transport_deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(started_marker.exists(), "fake SSH transport did not start");
        let captured_request =
            std::fs::read_to_string(captured_request).expect("captured remote mutation framing");
        assert!(captured_request.contains("herdr api relay"));
        assert!(captured_request.contains("expected_revision"));

        let (status_tx, status_rx) = std::sync::mpsc::channel();
        app.handle_api_request_message(crate::api::ApiRequestMessage {
            request: Request {
                id: "status".into(),
                method: Method::ThemeStatus(Default::default()),
            },
            respond_to: status_tx,
            response_write_complete: None,
            stream_active: None,
        });
        let status = status_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("event loop answers while remote mutation is blocked");
        assert!(
            serde_json::from_str::<SuccessResponse>(&status).is_ok(),
            "unexpected status response: {status}"
        );

        let response = mutation_rx
            .recv_timeout(std::time::Duration::from_secs(4))
            .expect("timed-out mutation response");
        let response: crate::api::schema::ErrorResponse =
            serde_json::from_str(&response).expect("mutation error response");
        assert_eq!(response.error.code, "authority_unreachable");
    }

    #[cfg(unix)]
    #[test]
    fn reload_cancels_a_queued_mutation_for_a_removed_connection() {
        let mut config = crate::config::Config::default();
        config.remote.fleet.hosts = vec![crate::config::FleetHostConfig {
            name: "remote".into(),
            target: "fixture".into(),
            ..Default::default()
        }];
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(&config, true, None, api_rx, crate::api::EventHub::default());
        let dir = TestDir::new("reload-cancels-queued-mutation");
        std::fs::create_dir_all(&dir.0).expect("create mutation fixture directory");
        let fake_ssh = dir.0.join("blocking-ssh");
        let first_started = dir.0.join("first-started");
        let release_first = dir.0.join("release-first");
        let second_started = dir.0.join("second-started");
        write_executable(
            &fake_ssh,
            &format!(
                "#!/bin/sh\ncat >/dev/null\nif [ ! -e '{}' ]; then\n  touch '{}'\n  while [ ! -e '{}' ]; do sleep 0.01; done\nelse\n  touch '{}'\nfi\nprintf '%s\\n' '{{\"id\":\"ok\",\"result\":{{\"type\":\"ok\"}}}}'\n",
                first_started.display(),
                first_started.display(),
                release_first.display(),
                second_started.display(),
            ),
        );
        app.authority_mutation_router = crate::fleet::AuthorityMutationRouter::with_ssh_program(
            fake_ssh,
            std::time::Duration::from_secs(2),
        );
        let authority = crate::groups::AuthorityId::from_random_bytes([16; 16]);
        let group_id = GroupId {
            owner: authority.clone(),
            local: 1,
        };
        app.state.fleet_snapshot.group_catalogs = vec![crate::fleet::GroupCatalog {
            host: "remote".into(),
            target: "fixture".into(),
            local: false,
            session: None,
            socket: None,
            state: crate::fleet::GroupCatalogState::Fresh,
            observed_authority_id: Some(authority),
            snapshot: Some(GroupAuthoritySnapshot {
                authority_id: group_id.owner.clone(),
                revision: 1,
                groups: vec![GroupRecord {
                    id: group_id.clone(),
                    revision: 1,
                    state: GroupState::Active {
                        name: "Remote".into(),
                    },
                }],
                memberships: Vec::new(),
            }),
            error: None,
        }];
        app.authority_mutation_router
            .observe_snapshot(&app.state.fleet_snapshot);
        let enqueue = |app: &mut App, id: &str| {
            let (respond_to, response_rx) = std::sync::mpsc::channel();
            app.handle_api_request_message(crate::api::ApiRequestMessage {
                request: Request {
                    id: id.into(),
                    method: Method::GroupDelete(GroupDeleteParams {
                        group_id: group_id.clone(),
                        expected_revision: 1,
                    }),
                },
                respond_to,
                response_write_complete: None,
                stream_active: None,
            });
            response_rx
        };
        let first_rx = enqueue(&mut app, "first");
        let second_rx = enqueue(&mut app, "second");
        let transport_deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while !first_started.exists() && std::time::Instant::now() < transport_deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            first_started.exists(),
            "first mutation did not reach transport"
        );

        app.apply_live_config(&crate::config::Config::default(), &[], &[], false);
        std::fs::write(&release_first, b"").expect("release first mutation");

        first_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("in-flight mutation response");
        let second = second_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("queued mutation cancellation");
        let second: crate::api::schema::ErrorResponse =
            serde_json::from_str(&second).expect("queued mutation error response");
        assert_eq!(second.error.code, "authority_not_fresh");
        assert!(!second_started.exists(), "removed connection was contacted");
    }

    #[cfg(unix)]
    fn queued_pane_assignment_after_poll(
        fixture: &str,
        poll_catalog: impl FnOnce(
            &crate::groups::AuthorityId,
            &GroupAuthoritySnapshot,
        ) -> crate::fleet::GroupCatalog,
    ) -> (
        crate::api::schema::ErrorResponse,
        crate::groups::AuthorityId,
    ) {
        let mut config = crate::config::Config::default();
        config.remote.fleet.hosts = vec![crate::config::FleetHostConfig {
            name: "remote".into(),
            target: "fixture".into(),
            ..Default::default()
        }];
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(&config, true, None, api_rx, crate::api::EventHub::default());
        let dir = TestDir::new(fixture);
        std::fs::create_dir_all(&dir.0).expect("create pane assignment fixture directory");
        let fake_ssh = dir.0.join("blocking-ssh");
        let first_started = dir.0.join("first-started");
        let release_first = dir.0.join("release-first");
        let second_started = dir.0.join("second-started");
        write_executable(
            &fake_ssh,
            &format!(
                "#!/bin/sh\ncat >/dev/null\nif [ ! -e '{}' ]; then\n  touch '{}'\n  while [ ! -e '{}' ]; do sleep 0.01; done\nelse\n  touch '{}'\nfi\nprintf '%s\\n' '{{\"id\":\"ok\",\"result\":{{\"type\":\"ok\"}}}}'\n",
                first_started.display(),
                first_started.display(),
                release_first.display(),
                second_started.display(),
            ),
        );
        app.authority_mutation_router = crate::fleet::AuthorityMutationRouter::with_ssh_program(
            fake_ssh,
            std::time::Duration::from_secs(2),
        );
        let authority = crate::groups::AuthorityId::from_random_bytes([20; 16]);
        let group_id = GroupId {
            owner: authority.clone(),
            local: 1,
        };
        let accepted = GroupAuthoritySnapshot {
            authority_id: authority.clone(),
            revision: 1,
            groups: vec![GroupRecord {
                id: group_id.clone(),
                revision: 1,
                state: GroupState::Active {
                    name: "Remote".into(),
                },
            }],
            memberships: vec![OwnedPaneMembership {
                pane_id: "remote-pane".into(),
                pane_incarnation: "incarnation-a".into(),
                membership: PaneGroupMembership::default(),
            }],
        };
        app.state.fleet_snapshot.group_catalogs = vec![crate::fleet::GroupCatalog {
            host: "remote".into(),
            target: "fixture".into(),
            local: false,
            session: None,
            socket: None,
            state: crate::fleet::GroupCatalogState::Fresh,
            observed_authority_id: Some(authority.clone()),
            snapshot: Some(accepted.clone()),
            error: None,
        }];
        app.authority_mutation_router
            .observe_snapshot(&app.state.fleet_snapshot);

        let (first_tx, first_rx) = std::sync::mpsc::channel();
        app.handle_api_request_message(crate::api::ApiRequestMessage {
            request: Request {
                id: "blocker".into(),
                method: Method::GroupDelete(GroupDeleteParams {
                    group_id,
                    expected_revision: 1,
                }),
            },
            respond_to: first_tx,
            response_write_complete: None,
            stream_active: None,
        });
        let (second_tx, second_rx) = std::sync::mpsc::channel();
        app.handle_api_request_message(crate::api::ApiRequestMessage {
            request: Request {
                id: "pane-assignment".into(),
                method: Method::PaneGroupSet(PaneGroupSetParams {
                    pane_id: "remote-pane".into(),
                    group_id: None,
                    expected_revision: 0,
                    expected_pane_authority: Some(authority.clone()),
                    expected_pane_incarnation: None,
                }),
            },
            respond_to: second_tx,
            response_write_complete: None,
            stream_active: None,
        });
        let transport_deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while !first_started.exists() && std::time::Instant::now() < transport_deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(first_started.exists(), "blocking mutation did not start");

        app.handle_internal_event_with_render_impact(crate::events::AppEvent::FleetRefreshed {
            snapshot: crate::fleet::Snapshot {
                polled: true,
                configured_hosts: vec!["remote".into()],
                group_catalogs: vec![poll_catalog(&authority, &accepted)],
                ..crate::fleet::Snapshot::default()
            },
        });
        std::fs::write(&release_first, b"").expect("release blocking mutation");
        first_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("blocking mutation response");
        let response = second_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("queued pane assignment response");
        let response = serde_json::from_str(&response).expect("pane assignment refusal");
        assert!(!second_started.exists(), "replacement pane was contacted");
        (response, authority)
    }

    #[cfg(unix)]
    #[test]
    fn queued_pane_assignment_is_refused_after_same_address_pane_replacement() {
        let (response, _) = queued_pane_assignment_after_poll(
            "pane-incarnation-cancels-assignment",
            |authority, accepted| {
                let mut replacement = accepted.clone();
                replacement.memberships[0].pane_incarnation = "incarnation-b".into();
                crate::fleet::GroupCatalog {
                    host: "remote".into(),
                    target: "fixture".into(),
                    local: false,
                    session: None,
                    socket: None,
                    state: crate::fleet::GroupCatalogState::Fresh,
                    observed_authority_id: Some(authority.clone()),
                    snapshot: Some(replacement),
                    error: None,
                }
            },
        );
        assert_eq!(response.error.code, "authority_not_fresh");
    }

    #[cfg(unix)]
    #[test]
    fn queued_remote_pane_assignment_names_failed_authority_when_route_turns_stale() {
        let (response, authority) =
            queued_pane_assignment_after_poll("stale-pane-route-names-authority", |_, _| {
                crate::fleet::GroupCatalog {
                    host: "remote".into(),
                    target: "fixture".into(),
                    local: false,
                    session: None,
                    socket: None,
                    state: crate::fleet::GroupCatalogState::Unavailable,
                    observed_authority_id: None,
                    snapshot: None,
                    error: Some("offline".into()),
                }
            });
        assert_eq!(response.error.code, "authority_not_fresh");
        assert!(response.error.message.contains(authority.as_str()));
    }

    #[cfg(unix)]
    fn assert_poll_cancels_queued_mutation(
        fixture: &str,
        recover_route_before_send: bool,
        poll_catalogs: impl FnOnce(
            &crate::groups::AuthorityId,
            &GroupAuthoritySnapshot,
        ) -> Vec<crate::fleet::GroupCatalog>,
    ) {
        let mut config = crate::config::Config::default();
        config.remote.fleet.hosts = vec![crate::config::FleetHostConfig {
            name: "remote".into(),
            target: "fixture".into(),
            ..Default::default()
        }];
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(&config, true, None, api_rx, crate::api::EventHub::default());
        let dir = TestDir::new(fixture);
        std::fs::create_dir_all(&dir.0).expect("create mutation fixture directory");
        let fake_ssh = dir.0.join("blocking-ssh");
        let first_started = dir.0.join("first-started");
        let release_first = dir.0.join("release-first");
        let second_started = dir.0.join("second-started");
        write_executable(
            &fake_ssh,
            &format!(
                "#!/bin/sh\ncat >/dev/null\nif [ ! -e '{}' ]; then\n  touch '{}'\n  while [ ! -e '{}' ]; do sleep 0.01; done\nelse\n  touch '{}'\nfi\nprintf '%s\\n' '{{\"id\":\"ok\",\"result\":{{\"type\":\"ok\"}}}}'\n",
                first_started.display(),
                first_started.display(),
                release_first.display(),
                second_started.display(),
            ),
        );
        app.authority_mutation_router = crate::fleet::AuthorityMutationRouter::with_ssh_program(
            fake_ssh,
            std::time::Duration::from_secs(2),
        );
        let authority = crate::groups::AuthorityId::from_random_bytes([17; 16]);
        let group_id = GroupId {
            owner: authority.clone(),
            local: 1,
        };
        let accepted = GroupAuthoritySnapshot {
            authority_id: authority.clone(),
            revision: 1,
            groups: vec![GroupRecord {
                id: group_id.clone(),
                revision: 1,
                state: GroupState::Active {
                    name: "Remote".into(),
                },
            }],
            memberships: Vec::new(),
        };
        app.state.fleet_snapshot.group_catalogs = vec![crate::fleet::GroupCatalog {
            host: "remote".into(),
            target: "fixture".into(),
            local: false,
            session: None,
            socket: None,
            state: crate::fleet::GroupCatalogState::Fresh,
            observed_authority_id: Some(authority.clone()),
            snapshot: Some(accepted.clone()),
            error: None,
        }];
        app.authority_mutation_router
            .observe_snapshot(&app.state.fleet_snapshot);
        let enqueue = |app: &mut App, id: &str| {
            let (respond_to, response_rx) = std::sync::mpsc::channel();
            app.handle_api_request_message(crate::api::ApiRequestMessage {
                request: Request {
                    id: id.into(),
                    method: Method::GroupDelete(GroupDeleteParams {
                        group_id: group_id.clone(),
                        expected_revision: 1,
                    }),
                },
                respond_to,
                response_write_complete: None,
                stream_active: None,
            });
            response_rx
        };
        let first_rx = enqueue(&mut app, "first");
        let second_rx = enqueue(&mut app, "second");
        let transport_deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while !first_started.exists() && std::time::Instant::now() < transport_deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            first_started.exists(),
            "first mutation did not reach transport"
        );

        let snapshot = crate::fleet::Snapshot {
            polled: true,
            configured_hosts: vec!["remote".into()],
            group_catalogs: poll_catalogs(&authority, &accepted),
            ..crate::fleet::Snapshot::default()
        };
        app.handle_internal_event_with_render_impact(crate::events::AppEvent::FleetRefreshed {
            snapshot,
        });
        if recover_route_before_send {
            let recovered = crate::fleet::Snapshot {
                polled: true,
                configured_hosts: vec!["remote".into()],
                group_catalogs: vec![crate::fleet::GroupCatalog {
                    host: "remote".into(),
                    target: "fixture".into(),
                    local: false,
                    session: None,
                    socket: None,
                    state: crate::fleet::GroupCatalogState::Fresh,
                    observed_authority_id: Some(authority.clone()),
                    snapshot: Some(accepted),
                    error: None,
                }],
                ..crate::fleet::Snapshot::default()
            };
            app.handle_internal_event_with_render_impact(crate::events::AppEvent::FleetRefreshed {
                snapshot: recovered,
            });
        }
        std::fs::write(&release_first, b"").expect("release first mutation");

        first_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("in-flight mutation response");
        let second = second_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("queued mutation cancellation");
        let second: crate::api::schema::ErrorResponse =
            serde_json::from_str(&second).expect("queued mutation error response");
        assert_eq!(second.error.code, "authority_not_fresh");
        assert!(!second_started.exists(), "invalid route was contacted");
    }

    #[cfg(unix)]
    #[test]
    fn queued_mutation_is_cancelled_when_poll_makes_route_stale() {
        assert_poll_cancels_queued_mutation("poll-stale-cancels-mutation", false, |_, _| {
            vec![crate::fleet::GroupCatalog {
                host: "remote".into(),
                target: "fixture".into(),
                local: false,
                session: None,
                socket: None,
                state: crate::fleet::GroupCatalogState::Unavailable,
                observed_authority_id: None,
                snapshot: None,
                error: Some("offline".into()),
            }]
        });
    }

    #[cfg(unix)]
    #[test]
    fn queued_mutation_is_cancelled_when_poll_makes_authority_conflicted() {
        assert_poll_cancels_queued_mutation(
            "poll-conflict-cancels-mutation",
            false,
            |authority, accepted| {
                [("remote", "fixture"), ("duplicate", "other-fixture")]
                    .into_iter()
                    .map(|(host, target)| crate::fleet::GroupCatalog {
                        host: host.into(),
                        target: target.into(),
                        local: false,
                        session: None,
                        socket: None,
                        state: crate::fleet::GroupCatalogState::Fresh,
                        observed_authority_id: Some(authority.clone()),
                        snapshot: Some(accepted.clone()),
                        error: None,
                    })
                    .collect()
            },
        );
    }

    #[cfg(unix)]
    #[test]
    fn queued_mutation_stays_cancelled_after_a_stale_route_recovers() {
        assert_poll_cancels_queued_mutation("poll-stale-route-recovers", true, |_, _| {
            vec![crate::fleet::GroupCatalog {
                host: "remote".into(),
                target: "fixture".into(),
                local: false,
                session: None,
                socket: None,
                state: crate::fleet::GroupCatalogState::Unavailable,
                observed_authority_id: None,
                snapshot: None,
                error: Some("offline".into()),
            }]
        });
    }

    #[test]
    fn host_snapshot_reads_the_maintained_membership_projection() {
        let (mut app, _dir, pane_public_id) = app_with_groups("membership-projection");
        app.rebuild_group_membership_projection();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        app.state.workspaces[0]
            .pane_state_mut(pane_id)
            .expect("local pane")
            .group_membership
            .revision = 7;

        let stale: SuccessResponse =
            serde_json::from_str(&app.handle_group_host_snapshot("stale".into())).unwrap();
        let ResponseResult::GroupHostSnapshot { snapshot } = stale.result else {
            panic!("expected group snapshot");
        };
        assert_eq!(snapshot.memberships[0].membership.revision, 0);

        app.refresh_group_membership_projection_for_pane(&pane_public_id);
        let refreshed: SuccessResponse =
            serde_json::from_str(&app.handle_group_host_snapshot("refreshed".into())).unwrap();
        let ResponseResult::GroupHostSnapshot { snapshot } = refreshed.result else {
            panic!("expected group snapshot");
        };
        assert_eq!(snapshot.memberships[0].membership.revision, 7);
    }
}
