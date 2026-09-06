use super::*;
use crate::client::endpoint::{ClientEndpointId, ClientEndpointStatus};

fn pending_popup() -> (ClientShellState, Vec<ClientShellAction>) {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    let binding = crate::config::CustomCommandKeybind {
        bindings: crate::config::ActionKeybinds::prefix("t"),
        label: "prefix+t".into(),
        command: "popup-command".into(),
        action: crate::config::CustomCommandAction::Popup,
        description: None,
        width: None,
        height: None,
    };
    let mut projection = snapshot();
    projection
        .commands
        .push(crate::protocol::ClientShellCommand {
            command_id: "cmd_popup".into(),
            binding_label: binding.label.clone(),
            binding_labels: binding.bindings.labels(),
            action: crate::protocol::ClientShellCommandAction::Popup,
            description: None,
        });
    state.set_snapshot(Box::new(projection));
    state.set_pane_surface(surface());
    let mut outcome = ClientShellInput::default();
    state.record_binding(crate::input::KeybindMatch::Command(binding), &mut outcome);
    assert!(state.popup_pending);
    (state, outcome.actions)
}

fn pending_worktree() -> (ClientShellState, Vec<ClientShellAction>) {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    let mut outcome = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::NewWorktree),
        &mut outcome,
    );
    let [ClientShellAction::Endpoint { request, .. }] = &outcome.actions[..] else {
        panic!("expected worktree preparation");
    };
    state.handle_endpoint_result("boot-1", &request.id, Ok(worktree_list_result(None)));
    state.handle_input_bytes(b"feature/reconnect");
    let outcome = state.handle_input_bytes(b"\r");
    assert!(matches!(
        &state.overlay,
        Some(ClientShellOverlay::WorktreeCreate(create)) if create.creating
    ));
    (state, outcome.actions)
}

fn request_id(actions: &[ClientShellAction]) -> &str {
    let [ClientShellAction::Endpoint { request, .. }] = actions else {
        panic!("expected one endpoint request");
    };
    &request.id
}

#[test]
fn cancelling_popup_request_unblocks_input_and_ignores_late_success() {
    let (mut state, actions) = pending_popup();
    let id = request_id(&actions);
    assert!(state.cancel_endpoint_request(id));
    assert!(!state.popup_pending);
    assert!(state.pending_requests.is_empty());
    assert!(state
        .handle_endpoint_result("boot-1", id, Ok(crate::api::schema::ResponseResult::Ok {}))
        .1
        .is_empty());
    assert!(!state.popup_pending);
    assert!(!state.handle_input_bytes(b"x").requests.is_empty());
}

#[test]
fn disconnect_cancels_worktree_dialog_before_same_server_reconnect() {
    let (mut state, _) = pending_worktree();
    state.mark_endpoint_disconnected(&ClientEndpointId::Local);
    assert!(state.pending_requests.is_empty());
    assert!(matches!(
        &state.overlay,
        Some(ClientShellOverlay::WorktreeCreate(create)) if !create.creating
    ));
    state.set_endpoint_status(&ClientEndpointId::Local, ClientEndpointStatus::Online);
    state.set_endpoint_snapshot(&ClientEndpointId::Local, Box::new(snapshot()));
    assert!(state.activate_endpoint_projection(&ClientEndpointId::Local));
    state.set_pane_surface(surface());
    state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        KeyCode::Esc,
        KeyModifiers::NONE,
    ))]);
    assert!(state.overlay.is_none());
    assert!(!state.handle_input_bytes(b"x").requests.is_empty());
}

struct TestTransport {
    fail: bool,
}

impl crate::client::endpoint::EndpointTransport for TestTransport {
    fn send(&mut self, _: &ClientMessage) -> std::io::Result<()> {
        if self.fail {
            Err(std::io::ErrorKind::BrokenPipe.into())
        } else {
            Ok(())
        }
    }
}

#[test]
fn dispatcher_cancels_worktree_requests_on_frozen_surface_or_failed_send() {
    use crate::client::endpoint::{EndpointNegotiation, EndpointRegistry};
    use crate::client::endpoint_commands::EndpointCommands;

    for fail_send in [false, true] {
        let (mut state, actions) = pending_worktree();
        let mut endpoints = EndpointRegistry::new(
            TestTransport { fail: fail_send },
            1,
            EndpointNegotiation::default(),
        );
        endpoints.set_surface_active(&ClientEndpointId::Local, fail_send);
        let mut commands = EndpointCommands::default();
        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        let (replay, repaint) = crate::client::shell_runtime::dispatch_client_shell_actions(
            actions,
            &mut commands,
            &mut endpoints,
            Some(&mut state),
            &mut Vec::new(),
            &tx,
        )
        .unwrap();
        assert!(repaint);
        assert!(replay.is_empty());
        assert!(state.pending_requests.is_empty());
        assert!(matches!(
            &state.overlay,
            Some(ClientShellOverlay::WorktreeCreate(create)) if !create.creating
        ));
        assert!(state
            .visible_endpoint_notice
            .as_ref()
            .is_some_and(|notice| { notice.title == "Action interrupted" }));
        assert!(commands.disconnect(&ClientEndpointId::Local).is_empty());
    }
}

#[test]
fn stale_queued_request_is_cancelled_without_blocking_the_current_generation() {
    use crate::client::endpoint::{EndpointNegotiation, EndpointRegistry};
    use crate::client::endpoint_commands::EndpointCommands;

    let (mut state, actions) = pending_popup();
    let stale_id = request_id(&actions).to_owned();
    let current = state.focus_endpoint_target(ClientEndpointFocusTarget::Workspace("ws_1".into()));
    let current_id = request_id(&current).to_owned();
    let mut commands = EndpointCommands::default();
    for (generation, actions) in [(1, actions), (2, current)] {
        for action in actions {
            let ClientShellAction::Endpoint {
                endpoint_id,
                boot_id,
                request,
            } = action
            else {
                panic!("expected endpoint request");
            };
            commands.enqueue(endpoint_id, generation, boot_id, request);
        }
    }
    let mut endpoints = EndpointRegistry::new(
        TestTransport { fail: false },
        2,
        EndpointNegotiation::default(),
    );
    let cancelled = commands.send_next(&ClientEndpointId::Local, &mut endpoints);
    assert_eq!(cancelled, vec![stale_id.clone()]);
    state.cancel_endpoint_request(&stale_id);
    assert!(!state.popup_pending);
    assert!(!commands.accepts_response(&ClientEndpointId::Local, 1, "boot-1", &stale_id));
    assert!(commands.accepts_response(&ClientEndpointId::Local, 2, "boot-1", &current_id));
    assert!(state.pending_requests.contains_key(&current_id));
}

#[test]
fn cancelled_integration_install_does_not_queue_a_refresh() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.open_settings_overlay();
    let Some(ClientShellOverlay::Settings(settings)) = state.overlay.as_mut() else {
        panic!("settings overlay");
    };
    settings.installing_integrations = true;
    state.pending_integration_installs = 1;
    let mut outcome = ClientShellInput::default();
    assert!(state.push_endpoint_method_with_kind(
        crate::api::schema::Method::IntegrationInstall(
            crate::api::schema::IntegrationInstallParams {
                target: crate::api::schema::IntegrationTarget::Pi,
            }
        ),
        PendingEndpointKind::IntegrationInstall,
        &mut outcome,
    ));
    assert!(state.cancel_endpoint_request(request_id(&outcome.actions)));
    assert!(state.pending_requests.is_empty());
    assert_eq!(state.pending_integration_installs, 0);
    assert!(matches!(
        &state.overlay,
        Some(ClientShellOverlay::Settings(settings))
            if !settings.installing_integrations && !settings.loading_integrations
    ));
}

#[test]
fn cancelled_selection_does_not_send_terminal_input() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.selection = Some(crate::selection::Selection::absolute_range(
        "pane_1".into(),
        (0, 0),
        (0, 2),
    ));
    let mut outcome = ClientShellInput::default();
    state.request_selection_copy(&mut outcome);
    let (_, actions) = state.handle_endpoint_result(
        "boot-1",
        request_id(&outcome.actions),
        Err(ClientShellEndpointError {
            code: Some("endpoint_cancelled".into()),
            message: "cancelled".into(),
        }),
    );
    assert!(actions.is_empty());
    assert!(state.pending_requests.is_empty());
}

#[test]
fn cancelled_link_activation_does_not_replay_mouse_input() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.compose(100, 28).unwrap();
    let pane_id = state.hits.panes[0].pane_id.clone();
    let inner_rect = state.hits.panes[0].inner_rect;
    let mut outcome = ClientShellInput::default();
    state.push_endpoint_method_with_kind(
        crate::api::schema::Method::PaneLinkActivate(crate::api::schema::PaneLinkActivateParams {
            pane_id: pane_id.clone(),
            viewport_row: 0,
            col: 0,
            content_revision: None,
            offset_from_bottom: None,
        }),
        PendingEndpointKind::PaneLinkActivate {
            pane_id,
            inner_rect,
            fallback_events: vec![MouseEvent {
                kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column: inner_rect.x,
                row: inner_rect.y,
                modifiers: KeyModifiers::NONE,
            }],
        },
        &mut outcome,
    );
    let (_, actions) = state.handle_endpoint_result(
        "boot-1",
        request_id(&outcome.actions),
        Err(ClientShellEndpointError {
            code: Some("endpoint_cancelled".into()),
            message: "cancelled".into(),
        }),
    );
    assert!(actions.is_empty());
    assert!(state.url_click_consumes_until_up);
}

#[test]
fn another_machine_disconnect_does_not_cancel_active_popup() {
    let (mut state, _) = pending_popup();
    let remote = ClientEndpointId::Ssh(
        crate::client::endpoint::ProfileId::parse("0123456789abcdef0123456789abcdef").unwrap(),
    );
    state.mark_endpoint_disconnected(&remote);
    assert!(state.popup_pending);
    assert_eq!(state.pending_requests.len(), 1);
}
