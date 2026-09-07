use super::*;

fn endpoint() -> ClientEndpointId {
    ClientEndpointId::Ssh(
        super::super::ProfileId::parse("0123456789abcdef0123456789abcdef").unwrap(),
    )
}

fn lease(id: ClientEndpointId, generation: u64, boot: &str) -> EndpointLease {
    EndpointLease {
        endpoint_id: id,
        generation,
        boot_id: boot.into(),
        minimum_revision: 0,
    }
}

#[derive(Clone)]
struct FakeTransport {
    sent: std::sync::Arc<std::sync::Mutex<Vec<crate::protocol::ClientMessage>>>,
    fail_after_write: bool,
}

impl super::super::EndpointTransport for FakeTransport {
    fn send(&mut self, message: &crate::protocol::ClientMessage) -> std::io::Result<()> {
        self.sent.lock().unwrap().push(message.clone());
        if self.fail_after_write {
            Err(std::io::Error::other("simulated observed write failure"))
        } else {
            Ok(())
        }
    }
}

fn negotiation() -> super::super::EndpointNegotiation {
    super::super::EndpointNegotiation::new(
        vec!["client_shell.surface.set".into()],
        vec![
            crate::protocol::endpoint::SURFACE_INTEREST_CAPABILITY.into(),
            crate::protocol::endpoint::PRESENTATION_EFFECTS_FENCE_CAPABILITY.into(),
        ],
    )
}

fn test_snapshot(boot_id: &str, revision: u64) -> crate::protocol::ClientShellSnapshot {
    crate::protocol::ClientShellSnapshot {
        boot_id: boot_id.into(),
        revision,
        config_diagnostic: None,
        product_announcement: None,
        update_available: None,
        update_install_command: String::new(),
        server_keybindings_toml: None,
        latest_release_notes_available: false,
        integration_updates_available: false,
        worktree_directory: String::new(),
        release_notes: None,
        focused_workspace_id: None,
        focused_tab_id: None,
        focused_pane_id: None,
        tab_bar_right: Vec::new(),
        tab_bar_right_separator: String::new(),
        agent_view_label: None,
        agent_order: Vec::new(),
        workspaces: Vec::new(),
        tabs: Vec::new(),
        panes: Vec::new(),
        agents: Vec::new(),
        commands: Vec::new(),
    }
}

type SentMessages = std::sync::Arc<std::sync::Mutex<Vec<crate::protocol::ClientMessage>>>;
type TestFixture = (
    crate::client::ClientShellState,
    EndpointRegistry,
    SentMessages,
    SentMessages,
);

fn shell_and_registry() -> TestFixture {
    shell_and_registry_with_source_failure(false)
}

fn shell_and_registry_with_source_failure(source_fail_after_write: bool) -> TestFixture {
    let mut shell = crate::client::ClientShellState::new(
        crate::client::ClientShellConfig::from_config(&crate::config::Config::default()),
    );
    let profile = super::super::SavedSshEndpoint {
        id: super::super::ProfileId::parse("0123456789abcdef0123456789abcdef").unwrap(),
        label: "Remote".into(),
        target: "dev@example.com".into(),
        session: "main".into(),
        enabled: true,
        windows_desktop: false,
    };
    let target = ClientEndpointId::Ssh(profile.id.clone());
    shell.set_endpoint_catalog(&[profile]);
    shell.set_snapshot(Box::new(test_snapshot("local-boot", 1)));
    shell.set_endpoint_status(&target, ClientEndpointStatus::Online);
    shell.set_endpoint_snapshot(&target, Box::new(test_snapshot("remote-boot", 1)));

    let local_sent = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let remote_sent = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut endpoints = EndpointRegistry::new(
        FakeTransport {
            sent: local_sent.clone(),
            fail_after_write: source_fail_after_write,
        },
        1,
        negotiation(),
    );
    endpoints.insert(
        target,
        FakeTransport {
            sent: remote_sent.clone(),
            fail_after_write: false,
        },
        7,
        negotiation(),
        false,
    );
    (shell, endpoints, local_sent, remote_sent)
}

fn surface_success(id: &str, active: bool, projection_revision: u64) -> Vec<u8> {
    serde_json::to_vec(&crate::api::schema::SuccessResponse {
        id: id.into(),
        result: crate::api::schema::ResponseResult::ClientShellSurfaceSet {
            active,
            projection_revision,
        },
    })
    .unwrap()
}

fn workspace_focus_success(id: &str, workspace_id: &str) -> Vec<u8> {
    serde_json::to_vec(&crate::api::schema::SuccessResponse {
        id: id.into(),
        result: crate::api::schema::ResponseResult::WorkspaceInfo {
            workspace: crate::api::schema::WorkspaceInfo {
                workspace_id: workspace_id.into(),
                number: 1,
                label: workspace_id.into(),
                focused: true,
                pane_count: 1,
                tab_count: 1,
                active_tab_id: "tab".into(),
                agent_status: crate::api::schema::AgentStatus::Unknown,
                tokens: Default::default(),
                worktree: None,
            },
        },
    })
    .unwrap()
}

fn failure(id: &str, message: &str) -> Vec<u8> {
    serde_json::to_vec(&crate::api::schema::ErrorResponse {
        id: id.into(),
        error: crate::api::schema::ErrorBody {
            code: "surface_rejected".into(),
            message: message.into(),
        },
    })
    .unwrap()
}

fn surface_set_active(message: &crate::protocol::ClientMessage) -> Option<bool> {
    let crate::protocol::ClientMessage::ClientShellEndpointRequest { request, .. } = message else {
        return None;
    };
    let request: crate::api::schema::Request = serde_json::from_str(request).ok()?;
    match request.method {
        crate::api::schema::Method::ClientShellSurfaceSet(params) => Some(params.active),
        _ => None,
    }
}

fn resize() -> crate::protocol::ClientMessage {
    crate::protocol::ClientMessage::ClientShellResize {
        cell_width_px: 8,
        cell_height_px: 16,
        surface_size: crate::protocol::ClientSurfaceSize { cols: 80, rows: 24 },
        pixel_mouse: false,
    }
}

fn surface(boot_id: &str, revision: u64, pane: &str) -> crate::protocol::PaneSurfaceFrame {
    crate::protocol::PaneSurfaceFrame {
        boot_id: boot_id.into(),
        projection_revision: revision,
        surface_revision: revision,
        frame: crate::protocol::FrameData {
            cells: Vec::new(),
            width: 80,
            height: 24,
            cursor: None,
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        },
        panes: vec![crate::protocol::PaneSurfacePane {
            pane_id: pane.into(),
            content_revision: revision,
            rect: crate::protocol::SurfaceRect {
                x: 0,
                y: 0,
                width: 80,
                height: 24,
            },
            inner_rect: crate::protocol::SurfaceRect {
                x: 0,
                y: 0,
                width: 80,
                height: 24,
            },
            scrollbar_rect: None,
            scroll: None,
            focused: true,
            mouse_reporting: false,
            sgr_pixel_mouse: false,
            alternate_screen_active: false,
            pixel_width: 0,
            pixel_height: 0,
        }],
        splits: Vec::new(),
        popup: None,
        graphics: Default::default(),
    }
}

fn machine() -> PendingEndpointActivation {
    PendingEndpointActivation {
        source: lease(ClientEndpointId::Local, 1, "local-boot"),
        source_available: true,
        target: lease(endpoint(), 7, "remote-boot"),
        focus: None,
        host_focused: true,
        resize: resize(),
        phase: ActivationPhase::ActivatingTarget {
            request_id: "client-shell-surface:3:on".into(),
            acknowledged_revision: Some(1),
            focus_request_id: None,
            focus_request_target: None,
            focus_acknowledged: true,
            evidence: ActivationEvidence::default(),
        },
        deadline: Instant::now() + ACTIVATION_TIMEOUT,
        epoch: 3,
        next_focus_serial: 0,
        rollback_error: None,
        successor: None,
    }
}

#[test]
fn source_off_request_is_distinct_and_precedes_target_on_phase() {
    let activation = PendingEndpointActivation {
        source: lease(ClientEndpointId::Local, 1, "local-boot"),
        source_available: true,
        target: lease(endpoint(), 7, "remote-boot"),
        focus: None,
        host_focused: true,
        resize: resize(),
        phase: ActivationPhase::ReleasingSource {
            request_id: "client-shell-surface:9:off".into(),
        },
        deadline: Instant::now() + ACTIVATION_TIMEOUT,
        epoch: 9,
        next_focus_serial: 0,
        rollback_error: None,
        successor: None,
    };
    assert!(activation.accepts_response(
        &ClientEndpointId::Local,
        1,
        "local-boot",
        "client-shell-surface:9:off"
    ));
    assert!(!activation.accepts_response(
        &endpoint(),
        7,
        "remote-boot",
        "client-shell-surface:9:on"
    ));
}

#[test]
fn active_source_requires_metadata_from_its_current_connection_generation() {
    let (mut shell, mut endpoints, local_sent, remote_sent) = shell_and_registry();
    shell.set_endpoint_snapshot_for_generation(
        &ClientEndpointId::Local,
        99,
        Box::new(test_snapshot("stale-local-boot", 1)),
    );

    let result = PendingEndpointActivation::begin(
        &shell,
        &mut endpoints,
        endpoint(),
        None,
        resize(),
        26,
        Instant::now(),
    );

    assert!(
        matches!(result, Err(ActivationBeginError::Preflight(message)) if message.contains("this connection"))
    );
    assert!(local_sent.lock().unwrap().is_empty());
    assert!(remote_sent.lock().unwrap().is_empty());
    assert!(endpoints.active_surface_available());
}

#[test]
fn observed_begin_write_failure_returns_recoverable_partial_activation() {
    let (shell, mut endpoints, local_sent, remote_sent) =
        shell_and_registry_with_source_failure(true);
    let result = PendingEndpointActivation::begin(
        &shell,
        &mut endpoints,
        endpoint(),
        None,
        resize(),
        10,
        Instant::now(),
    );

    let ActivationBeginError::Partial {
        mut activation,
        error,
    } = (match result {
        Err(error) => error,
        Ok(_) => panic!("an observed source write must return partial lifecycle state"),
    })
    else {
        panic!("an observed source write must return partial lifecycle state");
    };
    assert!(error.contains("focus revoke"));
    assert_eq!(
        local_sent.lock().unwrap().first(),
        Some(&crate::protocol::ClientMessage::ClientShellFocus { focused: false })
    );
    assert!(remote_sent.lock().unwrap().is_empty());
    assert!(matches!(
        activation.rollback(&mut endpoints, error, false),
        ActivationRollback::Unavailable(_)
    ));
}

#[test]
fn source_release_is_sent_and_acknowledged_before_target_activation() {
    let (shell, mut endpoints, local_sent, remote_sent) = shell_and_registry();
    let target = endpoint();
    let mut activation = PendingEndpointActivation::begin(
        &shell,
        &mut endpoints,
        target.clone(),
        None,
        resize(),
        11,
        Instant::now(),
    )
    .unwrap();
    let local = local_sent.lock().unwrap();
    assert_eq!(
        local.first(),
        Some(&crate::protocol::ClientMessage::ClientShellFocus { focused: false }),
        "source focus is revoked before source-off"
    );
    assert_eq!(
        local
            .as_slice()
            .iter()
            .filter_map(surface_set_active)
            .collect::<Vec<_>>(),
        vec![false],
        "the source is released first"
    );
    drop(local);
    assert!(remote_sent.lock().unwrap().is_empty());
    assert!(
        !endpoints.active_surface_available(),
        "pane input is blocked while frozen"
    );

    assert_eq!(
        activation.receive_response(
            &ClientEndpointId::Local,
            1,
            "client-shell-surface:11:off",
            &surface_success("client-shell-surface:11:off", false, 1),
            &mut endpoints,
        ),
        SurfaceActivationProgress::Pending
    );
    let remote = remote_sent.lock().unwrap();
    assert!(matches!(
        remote[0],
        crate::protocol::ClientMessage::ClientShellResize { .. }
    ));
    assert_eq!(
        remote.get(2),
        Some(&crate::protocol::ClientMessage::ClientShellFocus { focused: true })
    );
    assert_eq!(remote.get(1).and_then(surface_set_active), Some(true));
}

#[test]
fn activation_requires_an_exact_snapshot_surface_revision_pair() {
    let mut activation = machine();
    let target = endpoint();
    let snapshot = crate::protocol::ClientShellSnapshot {
        boot_id: "remote-boot".into(),
        revision: 2,
        config_diagnostic: None,
        product_announcement: None,
        update_available: None,
        update_install_command: String::new(),
        server_keybindings_toml: None,
        latest_release_notes_available: false,
        integration_updates_available: false,
        worktree_directory: String::new(),
        release_notes: None,
        focused_workspace_id: None,
        focused_tab_id: None,
        focused_pane_id: None,
        tab_bar_right: Vec::new(),
        tab_bar_right_separator: String::new(),
        agent_view_label: None,
        agent_order: Vec::new(),
        workspaces: Vec::new(),
        tabs: Vec::new(),
        panes: Vec::new(),
        agents: Vec::new(),
        commands: Vec::new(),
    };
    assert_eq!(
        activation.receive_snapshot(&target, 7, &snapshot),
        SurfaceActivationProgress::Pending
    );
    assert_eq!(
        activation.receive_surface(&target, 7, surface("remote-boot", 1, "pane")),
        SurfaceActivationProgress::Pending
    );
    assert_eq!(
        activation.receive_surface(&target, 7, surface("remote-boot", 2, "pane")),
        SurfaceActivationProgress::Ready
    );
}

#[test]
fn typed_target_ack_sets_a_floor_for_same_boot_activation_evidence() {
    let (shell, mut endpoints, _local_sent, _remote_sent) = shell_and_registry();
    let target = endpoint();
    let mut activation = PendingEndpointActivation::begin(
        &shell,
        &mut endpoints,
        target.clone(),
        None,
        resize(),
        16,
        Instant::now(),
    )
    .unwrap();
    let _ = activation.receive_response(
        &ClientEndpointId::Local,
        1,
        "client-shell-surface:16:off",
        &surface_success("client-shell-surface:16:off", false, 1),
        &mut endpoints,
    );
    assert_eq!(
        activation.receive_response(
            &target,
            7,
            "client-shell-surface:16:on",
            &surface_success("client-shell-surface:16:on", true, 4),
            &mut endpoints,
        ),
        SurfaceActivationProgress::Pending
    );
    assert_eq!(
        activation.receive_snapshot(&target, 7, &test_snapshot("remote-boot", 3)),
        SurfaceActivationProgress::Pending
    );
    assert_eq!(
        activation.receive_surface(&target, 7, surface("remote-boot", 3, "pane")),
        SurfaceActivationProgress::Pending,
        "a delayed same-boot surface below the acknowledgement floor is not evidence"
    );
    assert_eq!(
        activation.receive_snapshot(&target, 7, &test_snapshot("remote-boot", 4)),
        SurfaceActivationProgress::Pending
    );
    assert_eq!(
        activation.receive_surface(&target, 7, surface("remote-boot", 4, "pane")),
        SurfaceActivationProgress::Ready
    );
}

#[test]
fn stale_generation_and_boot_are_not_activation_evidence() {
    let mut activation = machine();
    assert_eq!(
        activation.receive_surface(&endpoint(), 6, surface("remote-boot", 1, "pane")),
        SurfaceActivationProgress::Stale
    );
    assert_eq!(
        activation.receive_surface(&endpoint(), 7, surface("old-boot", 1, "pane")),
        SurfaceActivationProgress::Stale
    );
}

#[test]
fn stale_response_boot_is_not_consumed() {
    let mut activation = machine();
    let (_shell, mut endpoints, _local_sent, _remote_sent) = shell_and_registry();
    assert_eq!(
        activation.receive_response_for_boot(
            &endpoint(),
            7,
            "old-boot",
            "client-shell-surface:3:on",
            &surface_success("client-shell-surface:3:on", true, 2),
            &mut endpoints,
        ),
        SurfaceActivationProgress::Stale
    );
}

#[test]
fn same_target_retarget_is_latest_wins() {
    let (shell, mut endpoints, _local_sent, remote_sent) = shell_and_registry();
    let target = endpoint();
    let mut activation = PendingEndpointActivation::begin(
        &shell,
        &mut endpoints,
        target.clone(),
        Some(crate::client::shell::ClientEndpointFocusTarget::Workspace(
            "old".into(),
        )),
        resize(),
        12,
        Instant::now(),
    )
    .unwrap();
    let _ = activation.receive_response(
        &ClientEndpointId::Local,
        1,
        "client-shell-surface:12:off",
        &surface_success("client-shell-surface:12:off", false, 1),
        &mut endpoints,
    );
    let old_focus = remote_sent
        .lock()
        .unwrap()
        .iter()
        .find_map(|message| match message {
            crate::protocol::ClientMessage::ClientShellEndpointRequest { request, .. }
                if surface_set_active(message).is_none() =>
            {
                Some(
                    serde_json::from_str::<crate::api::schema::Request>(request)
                        .unwrap()
                        .id,
                )
            }
            _ => None,
        })
        .unwrap();
    let sent_before_retarget = remote_sent.lock().unwrap().len();
    activation
        .retarget(
            Some(crate::client::shell::ClientEndpointFocusTarget::Workspace(
                "new".into(),
            )),
            &mut endpoints,
        )
        .unwrap();
    assert_eq!(remote_sent.lock().unwrap().len(), sent_before_retarget);
    assert!(activation.accepts_response(&target, 7, "remote-boot", &old_focus));
    assert_eq!(
        activation.receive_response(
            &target,
            7,
            &old_focus,
            &workspace_focus_success(&old_focus, "old"),
            &mut endpoints,
        ),
        SurfaceActivationProgress::Pending
    );
    let latest_focus = remote_sent
        .lock()
        .unwrap()
        .last()
        .and_then(|message| match message {
            crate::protocol::ClientMessage::ClientShellEndpointRequest { request, .. } => Some(
                serde_json::from_str::<crate::api::schema::Request>(request)
                    .unwrap()
                    .id,
            ),
            _ => None,
        })
        .unwrap();
    assert_ne!(latest_focus, old_focus);
    assert!(activation.accepts_response(&target, 7, "remote-boot", &latest_focus));
}

#[test]
fn latest_host_focus_is_replayed_to_the_eventual_target() {
    let (shell, mut endpoints, _local_sent, remote_sent) = shell_and_registry();
    let mut activation = PendingEndpointActivation::begin(
        &shell,
        &mut endpoints,
        endpoint(),
        None,
        resize(),
        25,
        Instant::now(),
    )
    .unwrap();
    activation.update_host_focus(false, &mut endpoints).unwrap();
    assert!(remote_sent.lock().unwrap().is_empty());

    let _ = activation.receive_response(
        &ClientEndpointId::Local,
        1,
        "client-shell-surface:25:off",
        &surface_success("client-shell-surface:25:off", false, 1),
        &mut endpoints,
    );
    assert_eq!(
        remote_sent.lock().unwrap().get(2),
        Some(&crate::protocol::ClientMessage::ClientShellFocus { focused: false })
    );

    activation.update_host_focus(true, &mut endpoints).unwrap();
    assert_eq!(
        remote_sent.lock().unwrap().last(),
        Some(&crate::protocol::ClientMessage::ClientShellFocus { focused: true })
    );
}

#[test]
fn host_focus_change_restarts_an_issued_presentation_effects_fence() {
    let (_shell, mut endpoints, _local_sent, remote_sent) = shell_and_registry();
    let mut activation = machine();
    activation.phase = ActivationPhase::AwaitingPresentationEffects {
        lease: lease(endpoint(), 7, "remote-boot"),
        token: "old-token".into(),
        ready: false,
        completion: Box::new(ActivationCompletion::Activated),
    };

    activation.update_host_focus(false, &mut endpoints).unwrap();

    assert!(matches!(
        activation.phase,
        ActivationPhase::SynchronizingPresentation { .. }
    ));
    assert_eq!(
        activation.receive_presentation_effects_ready(&endpoint(), 7, "old-token"),
        SurfaceActivationProgress::Stale
    );
    assert_eq!(
        remote_sent
            .lock()
            .unwrap()
            .iter()
            .filter_map(surface_set_active)
            .collect::<Vec<_>>(),
        vec![true]
    );
}

#[test]
fn source_release_rejection_restores_the_source_coherently() {
    let (shell, mut endpoints, local_sent, _remote_sent) = shell_and_registry();
    let mut activation = PendingEndpointActivation::begin(
        &shell,
        &mut endpoints,
        endpoint(),
        None,
        resize(),
        13,
        Instant::now(),
    )
    .unwrap();
    assert_eq!(
        activation.receive_response(
            &ClientEndpointId::Local,
            1,
            "client-shell-surface:13:off",
            &failure("client-shell-surface:13:off", "source rejected release"),
            &mut endpoints,
        ),
        SurfaceActivationProgress::Rejected {
            message: "source rejected release".into(),
            source_release_rejected: true,
        }
    );
    assert_eq!(
        activation.rollback(&mut endpoints, "source rejected release".into(), true),
        ActivationRollback::Pending
    );
    assert_eq!(endpoints.active_id(), &ClientEndpointId::Local);
    assert!(!endpoints.active_surface_available());
    assert_eq!(
        local_sent
            .lock()
            .unwrap()
            .iter()
            .filter_map(surface_set_active)
            .collect::<Vec<_>>(),
        vec![false, true]
    );
}

#[test]
fn source_release_timeout_starts_an_acknowledged_source_restore() {
    let (shell, mut endpoints, local_sent, _remote_sent) = shell_and_registry();
    let mut activation = PendingEndpointActivation::begin(
        &shell,
        &mut endpoints,
        endpoint(),
        None,
        resize(),
        14,
        Instant::now(),
    )
    .unwrap();
    assert_eq!(
        activation.rollback(&mut endpoints, "source release timed out".into(), false),
        ActivationRollback::Pending
    );
    let sent = local_sent.lock().unwrap();
    assert_eq!(
        sent.iter()
            .filter_map(surface_set_active)
            .collect::<Vec<_>>(),
        vec![false, true]
    );
    assert!(activation.accepts_response(
        &ClientEndpointId::Local,
        1,
        "local-boot",
        "client-shell-surface:14:rollback-source-on"
    ));
}

#[test]
fn resize_invalidates_already_recorded_surface_evidence() {
    let (_shell, mut endpoints, _local_sent, _remote_sent) = shell_and_registry();
    let mut activation = machine();
    assert_eq!(
        activation.receive_snapshot(&endpoint(), 7, &test_snapshot("remote-boot", 1)),
        SurfaceActivationProgress::Pending
    );
    assert_eq!(
        activation.receive_surface(&endpoint(), 7, surface("remote-boot", 1, "pane")),
        SurfaceActivationProgress::Ready
    );
    let resize = crate::protocol::ClientMessage::ClientShellResize {
        cell_width_px: 9,
        cell_height_px: 17,
        surface_size: crate::protocol::ClientSurfaceSize {
            cols: 100,
            rows: 30,
        },
        pixel_mouse: true,
    };
    activation.update_resize(resize, &mut endpoints).unwrap();
    assert_eq!(activation.progress(), SurfaceActivationProgress::Pending);
}

#[test]
fn resize_during_activation_reaches_the_pending_target() {
    let (shell, mut endpoints, _local_sent, remote_sent) = shell_and_registry();
    let mut activation = PendingEndpointActivation::begin(
        &shell,
        &mut endpoints,
        endpoint(),
        None,
        resize(),
        15,
        Instant::now(),
    )
    .unwrap();
    let _ = activation.receive_response(
        &ClientEndpointId::Local,
        1,
        "client-shell-surface:15:off",
        &surface_success("client-shell-surface:15:off", false, 1),
        &mut endpoints,
    );
    let resized = crate::protocol::ClientMessage::ClientShellResize {
        cell_width_px: 9,
        cell_height_px: 17,
        surface_size: crate::protocol::ClientSurfaceSize {
            cols: 100,
            rows: 30,
        },
        pixel_mouse: true,
    };
    activation
        .update_resize(resized.clone(), &mut endpoints)
        .unwrap();
    assert_eq!(remote_sent.lock().unwrap().last(), Some(&resized));
    assert_eq!(
        activation.receive_surface(&endpoint(), 7, surface("remote-boot", 1, "pane")),
        SurfaceActivationProgress::Pending,
        "a surface for the prior geometry cannot commit"
    );
}

#[test]
fn rapid_a_to_b_to_a_restores_source_before_a_fresh_latest_epoch() {
    let (mut shell, mut endpoints, local_sent, remote_sent) = shell_and_registry();
    let mut activation = PendingEndpointActivation::begin(
        &shell,
        &mut endpoints,
        endpoint(),
        None,
        resize(),
        20,
        Instant::now(),
    )
    .unwrap();
    let _ = activation.receive_response(
        &ClientEndpointId::Local,
        1,
        "client-shell-surface:20:off",
        &surface_success("client-shell-surface:20:off", false, 1),
        &mut endpoints,
    );

    // The latest A-qualified target replaces B while B may already have accepted target-on.
    assert_eq!(
        activation.supersede(
            ClientEndpointId::Local,
            Some(crate::client::shell::ClientEndpointFocusTarget::Pane(
                "local-pane".into(),
            )),
            &mut endpoints,
        ),
        ActivationRollback::Pending
    );
    assert_eq!(
        remote_sent
            .lock()
            .unwrap()
            .iter()
            .filter_map(surface_set_active)
            .collect::<Vec<_>>(),
        vec![true, false],
        "B is released before A can be restored"
    );
    assert_eq!(
        activation.receive_surface(&endpoint(), 7, surface("remote-boot", 2, "pane")),
        SurfaceActivationProgress::Stale,
        "delayed B activation evidence cannot satisfy A restoration"
    );
    let _ = activation.receive_response(
        &endpoint(),
        7,
        "client-shell-surface:20:rollback-target-off",
        &surface_success("client-shell-surface:20:rollback-target-off", false, 1),
        &mut endpoints,
    );
    assert_eq!(
        local_sent
            .lock()
            .unwrap()
            .iter()
            .filter_map(surface_set_active)
            .collect::<Vec<_>>(),
        vec![false, true],
        "source restoration is acknowledged rather than racing target ownership"
    );
    let _ = activation.receive_response(
        &ClientEndpointId::Local,
        1,
        "client-shell-surface:20:rollback-source-on",
        &surface_success("client-shell-surface:20:rollback-source-on", true, 2),
        &mut endpoints,
    );
    let local_snapshot = test_snapshot("local-boot", 2);
    shell.set_snapshot(Box::new(local_snapshot.clone()));
    assert_eq!(
        activation.receive_snapshot(&ClientEndpointId::Local, 1, &local_snapshot),
        SurfaceActivationProgress::Pending
    );
    assert_eq!(
        activation.receive_surface(
            &ClientEndpointId::Local,
            1,
            surface("local-boot", 2, "pane")
        ),
        SurfaceActivationProgress::Ready
    );
    assert!(matches!(
        activation.complete(&mut shell, &mut endpoints),
        Ok(ActivationCompletion::AwaitingPresentationSync {
            endpoint: ClientEndpointId::Local,
            ..
        })
    ));
    let _ = activation.receive_response(
        &ClientEndpointId::Local,
        1,
        "client-shell-surface:20:presentation-sync",
        &surface_success("client-shell-surface:20:presentation-sync", true, 3),
        &mut endpoints,
    );
    let sync_snapshot = test_snapshot("local-boot", 3);
    shell.set_endpoint_snapshot_for_generation(
        &ClientEndpointId::Local,
        1,
        Box::new(sync_snapshot.clone()),
    );
    let _ = activation.receive_snapshot(&ClientEndpointId::Local, 1, &sync_snapshot);
    assert_eq!(
        activation.receive_surface(
            &ClientEndpointId::Local,
            1,
            surface("local-boot", 3, "pane")
        ),
        SurfaceActivationProgress::Ready
    );
    assert_eq!(
        activation.complete(&mut shell, &mut endpoints),
        Ok(ActivationCompletion::AwaitingPresentationEffects)
    );
    assert_eq!(
        activation.receive_presentation_effects_ready(
            &ClientEndpointId::Local,
            1,
            "20:1:local-boot"
        ),
        SurfaceActivationProgress::Ready
    );
    assert!(matches!(
        activation.complete(&mut shell, &mut endpoints),
        Ok(ActivationCompletion::RestoredSource {
            successor: Some(EndpointActivationIntent {
                endpoint_id: ClientEndpointId::Local,
                ..
            }),
            ..
        })
    ));
    assert_eq!(endpoints.active_id(), &ClientEndpointId::Local);

    // Runtime queues this successor with force=true, so even source==target receives a new
    // activation epoch only after restoration committed.
    let _fresh_epoch = PendingEndpointActivation::begin(
        &shell,
        &mut endpoints,
        ClientEndpointId::Local,
        Some(crate::client::shell::ClientEndpointFocusTarget::Pane(
            "local-pane".into(),
        )),
        resize(),
        21,
        Instant::now(),
    )
    .unwrap();
    assert!(local_sent.lock().unwrap().iter().any(|message| {
        matches!(
            message,
            crate::protocol::ClientMessage::ClientShellEndpointRequest { request, .. }
                if serde_json::from_str::<crate::api::schema::Request>(request)
                    .is_ok_and(|request| request.id == "client-shell-surface:21:on")
        )
    }));
}

#[test]
fn disconnected_committed_source_does_not_block_switching_to_a_live_endpoint() {
    let (mut shell, mut endpoints, local_sent, _remote_sent) = shell_and_registry();
    let disconnected = endpoint();
    endpoints.set_surface_active(&ClientEndpointId::Local, false);
    endpoints.set_surface_active(&disconnected, true);
    assert!(endpoints.set_active(&disconnected));
    assert!(shell.activate_endpoint_projection(&disconnected));
    endpoints.disconnect(&disconnected);

    let mut activation = PendingEndpointActivation::begin(
        &shell,
        &mut endpoints,
        ClientEndpointId::Local,
        None,
        resize(),
        22,
        Instant::now(),
    )
    .unwrap();
    assert_eq!(activation.source_command_lane(), None);
    assert_eq!(
        local_sent
            .lock()
            .unwrap()
            .iter()
            .filter_map(surface_set_active)
            .collect::<Vec<_>>(),
        vec![true],
        "there is no unavailable source release to await"
    );

    assert_eq!(
        activation.receive_response(
            &ClientEndpointId::Local,
            1,
            "client-shell-surface:22:on",
            &surface_success("client-shell-surface:22:on", true, 2),
            &mut endpoints,
        ),
        SurfaceActivationProgress::Pending
    );
    let snapshot = test_snapshot("local-boot", 2);
    shell.set_endpoint_snapshot_for_generation(
        &ClientEndpointId::Local,
        1,
        Box::new(snapshot.clone()),
    );
    assert_eq!(
        activation.receive_snapshot(&ClientEndpointId::Local, 1, &snapshot),
        SurfaceActivationProgress::Pending
    );
    assert_eq!(
        activation.receive_surface(
            &ClientEndpointId::Local,
            1,
            surface("local-boot", 2, "pane")
        ),
        SurfaceActivationProgress::Ready
    );
    assert!(matches!(
        activation.complete(&mut shell, &mut endpoints),
        Ok(ActivationCompletion::AwaitingPresentationSync {
            previous,
            endpoint: ClientEndpointId::Local,
        }) if previous == disconnected
    ));
    let _ = activation.receive_response(
        &ClientEndpointId::Local,
        1,
        "client-shell-surface:22:presentation-sync",
        &surface_success("client-shell-surface:22:presentation-sync", true, 3),
        &mut endpoints,
    );
    let sync_snapshot = test_snapshot("local-boot", 3);
    shell.set_endpoint_snapshot_for_generation(
        &ClientEndpointId::Local,
        1,
        Box::new(sync_snapshot.clone()),
    );
    let _ = activation.receive_snapshot(&ClientEndpointId::Local, 1, &sync_snapshot);
    assert_eq!(
        activation.receive_surface(
            &ClientEndpointId::Local,
            1,
            surface("local-boot", 3, "pane")
        ),
        SurfaceActivationProgress::Ready
    );
    assert_eq!(
        activation.complete(&mut shell, &mut endpoints),
        Ok(ActivationCompletion::AwaitingPresentationEffects)
    );
    assert_eq!(
        activation.receive_presentation_effects_ready(
            &ClientEndpointId::Local,
            1,
            "22:1:local-boot"
        ),
        SurfaceActivationProgress::Ready
    );
    assert_eq!(
        activation.complete(&mut shell, &mut endpoints),
        Ok(ActivationCompletion::Activated)
    );
    assert_eq!(endpoints.active_id(), &ClientEndpointId::Local);
    assert_ne!(endpoints.active_id(), &disconnected);
}

#[test]
fn rollback_keeps_the_latest_intent_even_when_it_returns_to_the_target() {
    let (shell, mut endpoints, _local_sent, _remote_sent) = shell_and_registry();
    let target = endpoint();
    let mut activation = PendingEndpointActivation::begin(
        &shell,
        &mut endpoints,
        target.clone(),
        None,
        resize(),
        23,
        Instant::now(),
    )
    .unwrap();
    let _ = activation.receive_response(
        &ClientEndpointId::Local,
        1,
        "client-shell-surface:23:off",
        &surface_success("client-shell-surface:23:off", false, 1),
        &mut endpoints,
    );
    assert_eq!(
        activation.supersede(
            ClientEndpointId::Local,
            Some(crate::client::shell::ClientEndpointFocusTarget::Pane(
                "local-pane".into()
            )),
            &mut endpoints,
        ),
        ActivationRollback::Pending
    );
    assert!(!activation.can_retarget(&target));
    assert_eq!(
        activation.supersede(
            target.clone(),
            Some(crate::client::shell::ClientEndpointFocusTarget::Pane(
                "remote-pane".into()
            )),
            &mut endpoints,
        ),
        ActivationRollback::Pending
    );
    assert_eq!(
        activation.successor,
        Some(EndpointActivationIntent {
            endpoint_id: target,
            target: Some(crate::client::shell::ClientEndpointFocusTarget::Pane(
                "remote-pane".into()
            )),
        })
    );
}

#[test]
fn unacknowledged_target_release_closes_target_before_restoring_source() {
    let (shell, mut endpoints, local_sent, _remote_sent) = shell_and_registry();
    let target = endpoint();
    let mut activation = PendingEndpointActivation::begin(
        &shell,
        &mut endpoints,
        target.clone(),
        None,
        resize(),
        24,
        Instant::now(),
    )
    .unwrap();
    let _ = activation.receive_response(
        &ClientEndpointId::Local,
        1,
        "client-shell-surface:24:off",
        &surface_success("client-shell-surface:24:off", false, 1),
        &mut endpoints,
    );
    assert_eq!(
        activation.rollback(&mut endpoints, "target activation timed out".into(), false),
        ActivationRollback::Pending
    );
    assert_eq!(
        activation.rollback(&mut endpoints, "target release timed out".into(), false),
        ActivationRollback::Pending
    );
    assert!(endpoints.connection(&target).is_none());
    let failures = endpoints.take_failures();
    assert_eq!(
        failures.len(),
        1,
        "rollback revocation must reach the reconnect owner"
    );
    assert_eq!(failures[0].endpoint_id, target);
    assert_eq!(failures[0].generation, 7);
    assert_eq!(failures[0].kind, std::io::ErrorKind::TimedOut);
    assert_eq!(
        local_sent
            .lock()
            .unwrap()
            .iter()
            .filter_map(surface_set_active)
            .collect::<Vec<_>>(),
        vec![false, true]
    );
}

#[test]
fn target_loss_at_activation_deadline_restores_source_before_timeout() {
    let (shell, mut endpoints, local_sent, _) = shell_and_registry();
    let target = endpoint();
    let mut activation = PendingEndpointActivation::begin(
        &shell,
        &mut endpoints,
        target.clone(),
        None,
        resize(),
        30,
        Instant::now(),
    )
    .unwrap();
    activation.receive_response(
        &ClientEndpointId::Local,
        1,
        "client-shell-surface:30:off",
        &surface_success("client-shell-surface:30:off", false, 1),
        &mut endpoints,
    );
    let now = Instant::now();
    activation.deadline = now;
    assert!(activation.expired(now));
    endpoints.fail(&target, std::io::ErrorKind::UnexpectedEof.into());
    // Match the client timer: apply transport failures before checking phase expiry.
    for failure in endpoints.take_failures() {
        assert_eq!(
            activation.endpoint_disconnected(&mut endpoints, &failure.endpoint_id, failure.message),
            ActivationRollback::Pending
        );
    }
    assert!(matches!(
        activation.phase,
        ActivationPhase::RestoringSource { .. }
    ));
    assert!(!activation.expired(now));
    assert_eq!(
        local_sent
            .lock()
            .unwrap()
            .iter()
            .filter_map(surface_set_active)
            .collect::<Vec<_>>(),
        vec![false, true]
    );
}

#[test]
fn losing_local_during_handoff_does_not_revoke_the_healthy_target() {
    for source_released in [false, true] {
        let (shell, mut endpoints, _local_sent, remote_sent) = shell_and_registry();
        let target = endpoint();
        let mut activation = PendingEndpointActivation::begin(
            &shell,
            &mut endpoints,
            target.clone(),
            None,
            resize(),
            29,
            Instant::now(),
        )
        .unwrap();
        if source_released {
            activation.receive_response(
                &ClientEndpointId::Local,
                1,
                "client-shell-surface:29:off",
                &surface_success("client-shell-surface:29:off", false, 1),
                &mut endpoints,
            );
        }
        if !source_released {
            activation.deadline = Instant::now() - Duration::from_millis(1);
        }
        endpoints.fail(
            &ClientEndpointId::Local,
            std::io::ErrorKind::BrokenPipe.into(),
        );
        assert_eq!(
            activation.endpoint_disconnected(
                &mut endpoints,
                &ClientEndpointId::Local,
                "Local stopped".into()
            ),
            ActivationRollback::Pending
        );
        assert!(!activation.source_available);
        assert!(
            !activation.expired(Instant::now()),
            "starting the healthy target must get a fresh deadline"
        );
        assert!(matches!(
            activation.phase,
            ActivationPhase::ActivatingTarget { .. }
        ));
        assert!(endpoints.connection(&target).is_some());
        assert_eq!(
            remote_sent
                .lock()
                .unwrap()
                .iter()
                .filter_map(surface_set_active)
                .collect::<Vec<_>>(),
            vec![true]
        );
    }
}

#[test]
fn resize_message_preserves_the_latest_surface_dimensions() {
    assert_eq!(
        resize_geometry(&resize()),
        Some(crate::protocol::ClientSurfaceSize { cols: 80, rows: 24 })
    );
}
