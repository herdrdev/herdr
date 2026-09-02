use super::*;

#[test]
fn endpoint_product_announcement_is_client_rendered_modal_and_dismissed_by_identity() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    let mut endpoint_snapshot = snapshot();
    endpoint_snapshot.product_announcement =
        Some(crate::protocol::ClientShellProductAnnouncement {
            version: "0.8.2".into(),
            id: "client-shell".into(),
            title: "A client-owned announcement".into(),
            body: (0..40)
                .map(|index| format!("- announcement line {index}"))
                .collect::<Vec<_>>()
                .join("\n"),
            preview: false,
        });
    state.set_snapshot(Box::new(endpoint_snapshot.clone()));
    state.set_pane_surface(surface_with_popup());

    let frame = state.compose(106, 30).expect("announcement frame");
    let text = frame
        .cells
        .chunks(frame.width as usize)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("A client-owned announcement"));
    assert!(text.contains("product announcement · v0.8.2"));
    assert!(!state.hits.product_announcement_scrollbar.is_empty());

    let popup_key = state.handle_input_bytes(b"x");
    assert!(popup_key.requests.is_empty());
    let popup_paste = state.handle_raw_events(vec![RawInputEvent::Paste("secret".into())]);
    assert!(popup_paste.requests.is_empty());

    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 0,
        row: 0,
        modifiers: KeyModifiers::NONE,
    })]);
    state.handle_input_bytes(b"\x1b[6~");
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::ProductAnnouncement(
            crate::app::state::ProductAnnouncementState { scroll: 11, .. }
        ))
    ));
    let repeated = state.handle_raw_events(vec![RawInputEvent::Key(
        crate::input::TerminalKey::new(KeyCode::PageDown, KeyModifiers::empty())
            .with_kind(crossterm::event::KeyEventKind::Repeat),
    )]);
    assert!(repeated.actions.is_empty());
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::ProductAnnouncement(
            crate::app::state::ProductAnnouncementState { scroll: 11, .. }
        ))
    ));

    let dismissed = state.handle_input_bytes(b"\r");
    assert!(state.overlay.is_none());
    assert!(matches!(
        &dismissed.actions[..],
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(
                &request.method,
                crate::api::schema::Method::ProductAnnouncementDismiss(params)
                    if params.version == "0.8.2" && params.id == "client-shell"
            )
    ));

    state.set_snapshot(Box::new(endpoint_snapshot.clone()));
    assert!(state.overlay.is_none(), "same announcement stays dismissed");
    endpoint_snapshot
        .product_announcement
        .as_mut()
        .expect("announcement")
        .id = "new-announcement".into();
    state.set_snapshot(Box::new(endpoint_snapshot.clone()));
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::ProductAnnouncement(_))
    ));
    state.chrome_drag = Some(ClientChromeDrag::ProductAnnouncementScrollbar { grab_row_offset: 0 });
    endpoint_snapshot.product_announcement = None;
    state.set_snapshot(Box::new(endpoint_snapshot));
    assert!(state.overlay.is_none());
    assert!(state.chrome_drag.is_none());
    assert!(state.dismissed_product_announcement.is_none());
}

#[test]
fn failed_product_announcement_dismiss_reopens_authoritative_snapshot() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    let mut endpoint_snapshot = snapshot();
    endpoint_snapshot.product_announcement =
        Some(crate::protocol::ClientShellProductAnnouncement {
            version: "0.8.2".into(),
            id: "client-shell".into(),
            title: "Client shell".into(),
            body: "announcement".into(),
            preview: false,
        });
    state.set_snapshot(Box::new(endpoint_snapshot));
    let dismissed = state.handle_input_bytes(b"\r");
    let request_id = match &dismissed.actions[..] {
        [ClientShellAction::Endpoint { request, .. }] => request.id.clone(),
        actions => panic!("unexpected actions: {actions:?}"),
    };

    let (repaint, actions) = state.handle_endpoint_result(
        "boot-1",
        &request_id,
        Err(ClientShellEndpointError {
            code: Some("stale_announcement".into()),
            message: "dismiss failed".into(),
        }),
    );
    assert!(repaint);
    assert!(actions.is_empty());
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::ProductAnnouncement(_))
    ));
    assert!(state.dismissed_product_announcement.is_none());
}

#[test]
fn release_notes_reconcile_and_failed_dismiss_reopens_authoritative_snapshot() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    let mut endpoint_snapshot = snapshot();
    endpoint_snapshot.latest_release_notes_available = true;
    endpoint_snapshot.release_notes = Some(crate::protocol::ClientShellReleaseNotes {
        version: "0.8.3".into(),
        body: "first notes".into(),
        preview: true,
    });
    state.set_snapshot(Box::new(endpoint_snapshot.clone()));
    state.open_release_notes();

    endpoint_snapshot.release_notes = Some(crate::protocol::ClientShellReleaseNotes {
        version: "0.8.4".into(),
        body: "second notes".into(),
        preview: true,
    });
    state.set_snapshot(Box::new(endpoint_snapshot.clone()));
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::ReleaseNotes(
            crate::app::state::ReleaseNotesState { ref version, .. }
        )) if version == "0.8.4"
    ));

    let dismissed = state.handle_input_bytes(b"\r");
    let request_id = match &dismissed.actions[..] {
        [ClientShellAction::Endpoint { request, .. }] => request.id.clone(),
        actions => panic!("unexpected actions: {actions:?}"),
    };
    endpoint_snapshot.release_notes = Some(crate::protocol::ClientShellReleaseNotes {
        version: "0.8.5".into(),
        body: "current notes".into(),
        preview: true,
    });
    state.set_snapshot(Box::new(endpoint_snapshot));

    let (repaint, actions) = state.handle_endpoint_result(
        "boot-1",
        &request_id,
        Err(ClientShellEndpointError {
            code: Some("stale_release_notes".into()),
            message: "dismiss failed".into(),
        }),
    );
    assert!(repaint);
    assert!(actions.is_empty());
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::ReleaseNotes(
            crate::app::state::ReleaseNotesState { ref version, .. }
        )) if version == "0.8.5"
    ));
}

#[test]
fn product_announcement_mouse_is_modal_and_closes_only_from_its_button() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    let mut endpoint_snapshot = snapshot();
    endpoint_snapshot.product_announcement =
        Some(crate::protocol::ClientShellProductAnnouncement {
            version: "0.8.2".into(),
            id: "client-shell".into(),
            title: "Client shell".into(),
            body: (0..40)
                .map(|index| format!("- line {index}"))
                .collect::<Vec<_>>()
                .join("\n"),
            preview: false,
        });
    state.set_snapshot(Box::new(endpoint_snapshot));
    state.set_pane_surface(surface_with_popup());
    state.compose(106, 30).expect("announcement frame");

    let outside =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        })]);
    assert!(outside.requests.is_empty());
    assert!(outside.actions.is_empty());
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::ProductAnnouncement(_))
    ));

    let metrics = state
        .hits
        .product_announcement_scroll_metrics
        .expect("scroll metrics");
    let track = state.hits.product_announcement_scrollbar;
    let thumb = crate::ui::scrollbar_thumb(metrics, track).expect("thumb");
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: track.x,
        row: thumb.top,
        modifiers: KeyModifiers::NONE,
    })]);
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: track.x,
        row: track.bottom().saturating_sub(1),
        modifiers: KeyModifiers::NONE,
    })]);
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: track.x,
        row: track.bottom().saturating_sub(1),
        modifiers: KeyModifiers::NONE,
    })]);
    assert!(state.chrome_drag.is_none());
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::ProductAnnouncement(
            crate::app::state::ProductAnnouncementState { scroll, .. }
        )) if usize::from(scroll) == state.hits.product_announcement_max_scroll
    ));

    let close = state.hits.overlay_primary;
    let closed =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: close.x,
            row: close.y,
            modifiers: KeyModifiers::NONE,
        })]);
    assert!(state.overlay.is_none());
    assert!(matches!(
        &closed.actions[..],
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(request.method, crate::api::schema::Method::ProductAnnouncementDismiss(_))
    ));
}

#[test]
fn onboarding_has_priority_over_endpoint_product_announcement() {
    let config = ClientShellConfig::from_config(&Config::default()).with_startup_onboarding(true);
    let mut state = ClientShellState::new(config);
    let mut endpoint_snapshot = snapshot();
    endpoint_snapshot.product_announcement =
        Some(crate::protocol::ClientShellProductAnnouncement {
            version: "0.8.2".into(),
            id: "client-shell".into(),
            title: "Hidden until a later launch".into(),
            body: "announcement body".into(),
            preview: false,
        });
    state.set_snapshot(Box::new(endpoint_snapshot));
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Onboarding)
    ));
}

#[test]
fn startup_onboarding_is_client_rendered_and_modal() {
    let config = ClientShellConfig::from_config(&Config::default()).with_startup_onboarding(true);
    let mut state = ClientShellState::new(config);
    let early = state.handle_input_bytes(b"\r");
    assert!(early.actions.is_empty());
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Onboarding)
    ));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());

    let frame = state.compose(106, 20).expect("onboarding frame");
    let text = frame
        .cells
        .chunks(frame.width as usize)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("terminal workspace manager for coding agents"));
    assert!(text.contains("this is a mouse-first terminal"));
    assert!(text.contains("ctrl+b enters prefix mode"));
    assert!(text.contains("install optional agent integrations"));
    assert_eq!(state.hits.overlay_primary.width, 12);

    let ignored = state.handle_input_bytes(b"x");
    assert!(ignored.requests.is_empty());
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Onboarding)
    ));
    let outside =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        })]);
    assert!(outside.actions.is_empty());
    assert!(outside.requests.is_empty());
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Onboarding)
    ));

    state.set_pane_surface(surface_with_popup());
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Onboarding)
    ));
    let popup_input = state.handle_input_bytes(b"hidden-popup-input");
    assert!(popup_input.requests.is_empty());
    let popup_paste = state.handle_raw_events(vec![RawInputEvent::Paste("secret".into())]);
    assert!(popup_paste.requests.is_empty());
}

#[test]
fn onboarding_completion_persists_and_opens_endpoint_integrations() {
    let path = std::env::temp_dir().join(format!(
        "herdr-client-onboarding-{}-{}.toml",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    std::fs::write(&path, "[terminal]\ndefault_shell = \"fish\"\n")
        .expect("write onboarding config");
    let onboarding_config = || {
        let mut config =
            ClientShellConfig::from_config(&Config::default()).with_startup_onboarding(true);
        config.local_config_path = path.clone();
        config
    };

    let config = onboarding_config();
    let mut state = ClientShellState::new(config);
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    let outcome = state.handle_input_bytes(b"\r");

    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Settings(ClientSettingsOverlay {
            section: ClientSettingsSection::Integrations,
            loading_integrations: true,
            ..
        }))
    ));
    assert!(matches!(
        &outcome.actions[..],
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(request.method, crate::api::schema::Method::IntegrationList(_))
    ));
    let persisted = std::fs::read_to_string(&path).expect("read onboarding config");
    assert!(persisted.contains("onboarding = false"));
    assert!(persisted.contains("default_shell = \"fish\""));

    for input in [b"\x1b[C".as_slice(), b"l".as_slice()] {
        let config = onboarding_config();
        let mut state = ClientShellState::new(config);
        state.set_snapshot(Box::new(snapshot()));
        state.set_pane_surface(surface());
        let outcome = state.handle_input_bytes(input);
        assert!(matches!(
            state.overlay,
            Some(ClientShellOverlay::Settings(ClientSettingsOverlay {
                section: ClientSettingsSection::Integrations,
                ..
            }))
        ));
        assert_eq!(outcome.actions.len(), 1);
    }

    let config = onboarding_config();
    let mut state = ClientShellState::new(config);
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.compose(106, 20).expect("onboarding mouse frame");
    let button = state.hits.overlay_primary;
    let click = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: button.x,
        row: button.y,
        modifiers: KeyModifiers::NONE,
    })]);
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Settings(ClientSettingsOverlay {
            section: ClientSettingsSection::Integrations,
            ..
        }))
    ));
    assert_eq!(click.actions.len(), 1);

    let unreadable_path = path.with_extension("dir");
    std::fs::create_dir(&unreadable_path).expect("create unreadable config path");
    let mut config = onboarding_config();
    config.local_config_path = unreadable_path.clone();
    let mut state = ClientShellState::new(config);
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    let failed_write = state.handle_input_bytes(b"\r");
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Settings(ClientSettingsOverlay {
            section: ClientSettingsSection::Integrations,
            ..
        }))
    ));
    assert_eq!(failed_write.actions.len(), 1);
    assert!(state
        .config_diagnostic
        .as_deref()
        .is_some_and(|diagnostic| diagnostic.contains("failed to read config")));
    assert!(unreadable_path.is_dir());
    std::fs::remove_dir(&unreadable_path).expect("remove unreadable config path");

    std::fs::remove_file(path).expect("remove onboarding config");
}

#[test]
fn unavailable_integration_list_does_not_wedge_settings() {
    let mut config =
        ClientShellConfig::from_config(&Config::default()).with_startup_onboarding(true);
    config.local_config_path = std::env::temp_dir().join(format!(
        "herdr-client-onboarding-unavailable-{}-{}.toml",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let mut state = ClientShellState::new(config);
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.set_endpoint_methods(Some(Vec::new()));

    let outcome = state.handle_input_bytes(b"\r");

    assert!(outcome.actions.is_empty());
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Settings(ClientSettingsOverlay {
            section: ClientSettingsSection::Integrations,
            loading_integrations: false,
            ..
        }))
    ));
    assert!(state
        .visible_endpoint_notice
        .as_ref()
        .is_some_and(|notice| notice.key.code == "integration.list"));
    let _ = std::fs::remove_file(&state.config.local_config_path);
}

#[test]
fn startup_config_diagnostics_are_client_rendered_and_persist_until_replaced() {
    let config = ClientShellConfig::from_config(&Config::default())
        .with_startup_config_diagnostic(Some("local config warning".into()));
    let mut state = ClientShellState::new(config);
    let mut shared_snapshot = snapshot();
    shared_snapshot.config_diagnostic = Some("local config warning".into());
    state.set_snapshot(Box::new(shared_snapshot));
    assert_eq!(
        state.config_diagnostic.as_deref(),
        Some("client + endpoint: local config warning")
    );

    let mut endpoint_snapshot = snapshot();
    endpoint_snapshot.config_diagnostic = Some("endpoint config warning".into());
    state.set_snapshot(Box::new(endpoint_snapshot));
    state.set_pane_surface(surface());

    let frame = state.compose(106, 20).expect("diagnostic frame");
    let text = frame
        .cells
        .chunks(frame.width as usize)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("client: local config warning"));
    assert!(text.contains("endpoint: endpoint config warning"));

    state.handle_input_bytes(b"x");
    assert!(state.config_diagnostic.is_some());

    state.set_snapshot(Box::new(snapshot()));
    assert_eq!(
        state.config_diagnostic.as_deref(),
        Some("local config warning")
    );
}

#[test]
fn config_diagnostic_offsets_only_the_pane_rows_it_overlaps() {
    let mut config = ClientShellConfig::from_config(&Config::default());
    config.toast_delay_seconds = 0;
    let mut state = ClientShellState::new(config);
    let mut endpoint_snapshot = snapshot();
    endpoint_snapshot.config_diagnostic = Some("one-line warning".into());
    state.set_snapshot(Box::new(endpoint_snapshot));
    state.set_pane_surface(surface());
    state.visible_notification = Some(ClientVisibleNotification {
        event: SemanticNotification {
            kind: SemanticNotificationKind::Custom,
            title: "notification".into(),
            body: None,
            sound: None,
            agent: None,
            workspace_id: None,
            tab_id: None,
            pane_id: None,
            position: Some(crate::config::ToastHerdrPosition::TopRight),
        },
        deadline: std::time::Instant::now(),
    });

    state.compose(106, 20).expect("one-line frame");
    let pane_area = state.layout(106, 20).pane_surface;
    assert_eq!(state.hits.notification_toast.y, pane_area.y);
    let targetless_hit = state.hits.notification_toast;
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: targetless_hit.x,
        row: targetless_hit.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(state.visible_notification.is_some());

    let mut endpoint_snapshot = snapshot();
    endpoint_snapshot.config_diagnostic = Some("first warning\nsecond warning".into());
    state.set_snapshot(Box::new(endpoint_snapshot));
    state.compose(106, 20).expect("two-line frame");
    assert_eq!(state.hits.notification_toast.y, pane_area.y);

    state
        .visible_notification
        .as_mut()
        .expect("visible notification")
        .event
        .position = Some(crate::config::ToastHerdrPosition::BottomRight);
    state.compose(106, 20).expect("bottom notification frame");
    assert_eq!(state.hits.notification_toast.bottom(), 19);
}

#[test]
fn endpoint_reload_result_does_not_override_snapshot_diagnostic_authority() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    let mut endpoint_snapshot = snapshot();
    endpoint_snapshot.config_diagnostic = Some("endpoint warning".into());
    state.set_snapshot(Box::new(endpoint_snapshot));
    state.pending_requests.insert(
        "reload-1".into(),
        PendingEndpointRequest {
            boot_id: "boot-1".into(),
            method_name: "server.reload_config".into(),
            confirmation_workspace_id: None,
            kind: PendingEndpointKind::ReloadConfig,
        },
    );

    state.handle_endpoint_result(
        "boot-1",
        "reload-1",
        Ok(crate::api::schema::ResponseResult::ConfigReload {
            status: crate::config::ConfigReloadStatus::Partial,
            diagnostics: vec!["keybinding warning".into()],
        }),
    );
    assert_eq!(state.config_diagnostic.as_deref(), Some("endpoint warning"));

    state.set_snapshot(Box::new(snapshot()));
    assert!(state.config_diagnostic.is_none());
}

#[test]
fn endpoint_keybindings_hide_only_local_keybinding_diagnostics() {
    let config = ClientShellConfig::from_config(&Config::default())
        .with_keybinding_source(ClientShellKeybindingSource::Endpoint);
    let diagnostics = vec![
        "unsafe direct keybinding: keys.close_pane would intercept typing".into(),
        "theme warning".into(),
    ];

    assert!(config.local_config_diagnostic(&diagnostics[..1]).is_none());
    assert!(config.local_config_diagnostic(&diagnostics).is_some());
}

#[test]
fn live_client_config_keeps_sound_diagnostics() {
    let mut shell_config = ClientShellConfig::from_config(&Config::default());
    let mut config = Config::default();
    config.ui.sound.path = Some(std::path::PathBuf::from("invalid.wav"));

    let diagnostics = shell_config.apply_live_config(&config, &[], &[]);
    assert!(diagnostics
        .iter()
        .any(|diagnostic| diagnostic.contains("expected an mp3 file")));
}

#[test]
fn update_ready_menu_opens_client_owned_release_notes_and_dismisses_by_version() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    let mut endpoint_snapshot = snapshot();
    endpoint_snapshot.update_available = Some("0.8.3".into());
    endpoint_snapshot.latest_release_notes_available = true;
    endpoint_snapshot.release_notes = Some(crate::protocol::ClientShellReleaseNotes {
        version: "0.8.3".into(),
        body: (0..40)
            .map(|index| format!("- release line {index}"))
            .collect::<Vec<_>>()
            .join("\n"),
        preview: true,
    });
    state.set_snapshot(Box::new(endpoint_snapshot.clone()));
    state.set_pane_surface(surface());
    let shell = state.compose(106, 30).expect("shell frame");
    assert_eq!(state.hits.global_launcher.width, 8);
    let launcher = state.hits.global_launcher;
    let shell_buffer = shell.to_ratatui_buffer().expect("shell buffer");
    let badge_x = launcher.right().saturating_sub(6);
    assert_eq!(
        shell_buffer[(badge_x, launcher.y)].fg,
        state.config.palette.accent
    );
    assert_eq!(
        shell_buffer[(badge_x + 2, launcher.y)].fg,
        state.config.palette.overlay0
    );

    state.sidebar_collapsed = true;
    let collapsed = state.compose(106, 30).expect("collapsed update shell");
    let collapsed_buffer = collapsed.to_ratatui_buffer().expect("collapsed buffer");
    assert_eq!(
        collapsed_buffer[(state.hits.sidebar_toggle.x, state.hits.sidebar_toggle.y)].fg,
        state.config.palette.accent
    );
    state.sidebar_collapsed = false;
    state.mode = ClientShellMode::Navigate;
    let navigate = state.compose(106, 30).expect("navigate update status");
    let navigate_text = navigate
        .cells
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect::<String>();
    assert!(navigate_text.contains("update ready"));
    state.mode = ClientShellMode::Prefix;
    let prefix = state
        .compose(106, 30)
        .expect("prefix without update status");
    let prefix_text = prefix
        .cells
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect::<String>();
    assert!(!prefix_text.contains("update ready"));
    state.mode = ClientShellMode::Navigate;

    state.toggle_global_menu();
    let menu = state.compose(106, 30).expect("update menu");
    let text = menu
        .cells
        .chunks(menu.width as usize)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("● update ready"));
    let update_row = state.hits.global_menu_rows[3].0;
    assert_eq!(update_row.width, 16);
    let menu_buffer = menu.to_ratatui_buffer().expect("menu buffer");
    assert_eq!(
        menu_buffer[(update_row.x + 1, update_row.y)].fg,
        state.config.palette.accent
    );
    assert_eq!(
        menu_buffer[(update_row.x + 3, update_row.y)].fg,
        state.config.palette.text
    );
    state.activate_global_menu_item(3, &mut ClientShellInput::default());
    let notes = state.compose(106, 30).expect("release notes");
    let bottom_row_start = usize::from(notes.width) * usize::from(notes.height - 1);
    let bottom_row = notes.cells[bottom_row_start..]
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect::<String>();
    assert!(!bottom_row.contains("NAVIGATE"));
    let text = notes
        .cells
        .chunks(notes.width as usize)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("v0.8.3"));
    assert!(text.contains("update ready"));
    assert!(text.contains("detach, run herdr update"));
    assert!(!state.hits.release_notes_scrollbar.is_empty());
    let outer = crate::ui::centered_popup_rect(
        Rect::new(0, 0, 106, 30),
        crate::ui::RELEASE_NOTES_MODAL_SIZE.0,
        crate::ui::RELEASE_NOTES_MODAL_SIZE.1,
    )
    .expect("release notes outer");
    let inner = Rect::new(outer.x + 1, outer.y + 1, outer.width - 2, outer.height - 2);
    let stack = crate::ui::modal_stack_areas(inner, 2, 1, 0, 1);
    assert_eq!(
        state.hits.overlay_primary,
        crate::ui::release_notes_close_button_rect(Rect::new(
            stack.header.x,
            stack.header.y,
            stack.header.width,
            1,
        ))
    );
    let notes_buffer = notes.to_ratatui_buffer().expect("release notes buffer");
    let title_cell = &notes_buffer[(stack.header.x + 1, stack.header.y)];
    assert_eq!(title_cell.fg, state.config.palette.text);
    assert!(title_cell.modifier.contains(Modifier::BOLD));
    assert_eq!(
        notes_buffer[(state.hits.overlay_primary.x, state.hits.overlay_primary.y)].bg,
        state.config.palette.accent
    );

    let outside =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        })]);
    assert!(outside.requests.is_empty());
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::ReleaseNotes(_))
    ));
    let pane_text = state.handle_raw_events(vec![
        RawInputEvent::Text(crate::input::TextCommit::new("ime")),
        RawInputEvent::Paste("secret".into()),
    ]);
    assert!(pane_text.requests.is_empty());

    let metrics = state
        .hits
        .release_notes_scroll_metrics
        .expect("release notes scroll metrics");
    let track = state.hits.release_notes_scrollbar;
    let thumb = crate::ui::scrollbar_thumb(metrics, track).expect("release notes thumb");
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: track.x,
        row: thumb.top,
        modifiers: KeyModifiers::NONE,
    })]);
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: track.x,
        row: track.bottom().saturating_sub(1),
        modifiers: KeyModifiers::NONE,
    })]);
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: track.x,
        row: track.bottom().saturating_sub(1),
        modifiers: KeyModifiers::NONE,
    })]);
    assert!(state.chrome_drag.is_none());
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::ReleaseNotes(
            crate::app::state::ReleaseNotesState { scroll, .. }
        )) if usize::from(scroll) == state.hits.release_notes_max_scroll
    ));
    if let Some(ClientShellOverlay::ReleaseNotes(notes)) = state.overlay.as_mut() {
        notes.scroll = 0;
    }
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 0,
        row: 0,
        modifiers: KeyModifiers::NONE,
    })]);
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::ReleaseNotes(
            crate::app::state::ReleaseNotesState { scroll: 3, .. }
        ))
    ));
    let repeated = state.handle_raw_events(vec![RawInputEvent::Key(
        crate::input::TerminalKey::new(KeyCode::PageDown, KeyModifiers::empty())
            .with_kind(crossterm::event::KeyEventKind::Repeat),
    )]);
    assert!(repeated.actions.is_empty());
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::ReleaseNotes(
            crate::app::state::ReleaseNotesState { scroll: 3, .. }
        ))
    ));
    let dismissed = state.handle_input_bytes(b"\r");
    assert!(state.overlay.is_none());
    assert_eq!(state.mode, ClientShellMode::Terminal);
    assert!(matches!(
        &dismissed.actions[..],
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(
                &request.method,
                crate::api::schema::Method::ReleaseNotesDismiss(params)
                    if params.version == "0.8.3"
            )
    ));

    endpoint_snapshot.boot_id = "boot-2".into();
    endpoint_snapshot.revision = 2;
    endpoint_snapshot.update_available = None;
    endpoint_snapshot
        .release_notes
        .as_mut()
        .expect("release notes")
        .preview = false;
    state.set_snapshot(Box::new(endpoint_snapshot));
    let mut installed_surface = surface();
    installed_surface.boot_id = "boot-2".into();
    installed_surface.projection_revision = 2;
    state.set_pane_surface(installed_surface);
    state.compose(106, 30).expect("installed shell");
    assert_eq!(state.hits.global_launcher.width, 6);
    state.toggle_global_menu();
    let installed = state.compose(106, 30).expect("installed menu");
    let installed_text = installed
        .cells
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect::<String>();
    assert!(installed_text.contains("what's new"));
    assert!(!installed_text.contains("● what's new"));
}

#[test]
fn coalesced_release_notes_open_and_scroll_uses_current_geometry() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    let mut endpoint_snapshot = snapshot();
    endpoint_snapshot.update_available = Some("0.8.3".into());
    endpoint_snapshot.latest_release_notes_available = true;
    endpoint_snapshot.release_notes = Some(crate::protocol::ClientShellReleaseNotes {
        version: "0.8.3".into(),
        body: (0..40)
            .map(|index| format!("- release line {index}"))
            .collect::<Vec<_>>()
            .join("\n"),
        preview: true,
    });
    state.set_snapshot(Box::new(endpoint_snapshot));
    state.set_pane_surface(surface());
    state.compose(106, 30).expect("initial shell");
    state.overlay = Some(ClientShellOverlay::GlobalMenu(ClientGlobalMenuOverlay {
        highlighted: 3,
    }));

    state.handle_raw_events(vec![
        RawInputEvent::Key(crate::input::TerminalKey::new(
            KeyCode::Enter,
            KeyModifiers::empty(),
        )),
        RawInputEvent::Key(crate::input::TerminalKey::new(
            KeyCode::PageDown,
            KeyModifiers::empty(),
        )),
    ]);

    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::ReleaseNotes(
            crate::app::state::ReleaseNotesState { scroll: 8, .. }
        ))
    ));
}

#[test]
fn coalesced_release_notes_open_and_mouse_uses_current_geometry() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    let body = (0..40)
        .map(|index| format!("- release line {index}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut endpoint_snapshot = snapshot();
    endpoint_snapshot.update_available = Some("0.8.3".into());
    endpoint_snapshot.latest_release_notes_available = true;
    endpoint_snapshot.release_notes = Some(crate::protocol::ClientShellReleaseNotes {
        version: "0.8.3".into(),
        body: body.clone(),
        preview: true,
    });
    state.set_snapshot(Box::new(endpoint_snapshot));
    state.set_pane_surface(surface());
    state.compose(106, 30).expect("initial shell");

    let outer = crate::ui::centered_popup_rect(
        Rect::new(0, 0, 106, 30),
        crate::ui::RELEASE_NOTES_MODAL_SIZE.0,
        crate::ui::RELEASE_NOTES_MODAL_SIZE.1,
    )
    .expect("release notes outer");
    let inner = Rect::new(outer.x + 1, outer.y + 1, outer.width - 2, outer.height - 2);
    let stack = crate::ui::modal_stack_areas(inner, 2, 1, 0, 1);
    let notes = crate::app::state::ReleaseNotesState {
        version: "0.8.3".into(),
        body,
        scroll: 0,
        preview: true,
    };
    let metrics = crate::ui::release_notes_scroll_metrics(
        &notes,
        "herdr update",
        stack.content,
        &state.config.palette,
    );
    let track = crate::ui::release_notes_scrollbar_rect(stack.content, metrics)
        .expect("release notes track");
    let close = crate::ui::release_notes_close_button_rect(Rect::new(
        stack.header.x,
        stack.header.y,
        stack.header.width,
        1,
    ));

    state.overlay = Some(ClientShellOverlay::GlobalMenu(ClientGlobalMenuOverlay {
        highlighted: 3,
    }));
    state.handle_raw_events(vec![
        RawInputEvent::Key(crate::input::TerminalKey::new(
            KeyCode::Enter,
            KeyModifiers::empty(),
        )),
        RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: track.x,
            row: track.bottom().saturating_sub(1),
            modifiers: KeyModifiers::NONE,
        }),
    ]);
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::ReleaseNotes(
            crate::app::state::ReleaseNotesState { scroll, .. }
        )) if scroll > 0
    ));

    state.overlay = Some(ClientShellOverlay::GlobalMenu(ClientGlobalMenuOverlay {
        highlighted: 3,
    }));
    let closed = state.handle_raw_events(vec![
        RawInputEvent::Key(crate::input::TerminalKey::new(
            KeyCode::Enter,
            KeyModifiers::empty(),
        )),
        RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: close.x,
            row: close.y,
            modifiers: KeyModifiers::NONE,
        }),
    ]);
    assert!(state.overlay.is_none());
    assert!(matches!(
        &closed.actions[..],
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(request.method, crate::api::schema::Method::ReleaseNotesDismiss(_))
    ));
}

#[test]
fn outdated_integration_badges_launcher_settings_and_settings_tab() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    let mut endpoint_snapshot = snapshot();
    endpoint_snapshot.integration_updates_available = true;
    state.set_snapshot(Box::new(endpoint_snapshot));
    state.set_pane_surface(surface());
    let shell = state.compose(106, 30).expect("integration attention shell");
    assert_eq!(state.hits.global_launcher.width, 8);
    let shell_text = shell
        .cells
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect::<String>();
    assert!(shell_text.contains("● menu"));

    state.toggle_global_menu();
    let menu = state.compose(106, 30).expect("integration attention menu");
    let menu_text = menu
        .cells
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect::<String>();
    assert!(menu_text.contains("● settings"));
    assert!(!menu_text.contains("update ready"));

    state.activate_global_menu_item(0, &mut ClientShellInput::default());
    let settings = state.compose(106, 30).expect("settings integration badge");
    let settings_text = settings
        .cells
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect::<String>();
    assert!(settings_text.contains("● integrations"));
    let integrations_tab = state
        .hits
        .settings_tabs
        .iter()
        .find(|(_, section)| *section == ClientSettingsSection::Integrations)
        .map(|(rect, _)| *rect)
        .expect("integrations tab");
    let settings_buffer = settings.to_ratatui_buffer().expect("settings buffer");
    assert_eq!(
        settings_buffer[(integrations_tab.x + 1, integrations_tab.y)].fg,
        state.config.palette.accent
    );
    assert_eq!(
        settings_buffer[(integrations_tab.x + 3, integrations_tab.y)].fg,
        state.config.palette.overlay1
    );
}

#[test]
fn combined_update_and_integration_attention_preserves_both_badges() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    let mut endpoint_snapshot = snapshot();
    endpoint_snapshot.update_available = Some("0.8.3".into());
    endpoint_snapshot.latest_release_notes_available = true;
    endpoint_snapshot.integration_updates_available = true;
    endpoint_snapshot.release_notes = Some(crate::protocol::ClientShellReleaseNotes {
        version: "0.8.3".into(),
        body: "### Changed\n- Both attention states".into(),
        preview: true,
    });
    state.set_snapshot(Box::new(endpoint_snapshot));
    state.set_pane_surface(surface());
    state.compose(106, 30).expect("combined attention shell");
    assert_eq!(state.hits.global_launcher.width, 8);

    state.toggle_global_menu();
    let menu = state.compose(106, 30).expect("combined attention menu");
    let text = menu
        .cells
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect::<String>();
    let settings = text.find("● settings").expect("settings badge");
    let update = text.find("● update ready").expect("update badge");
    assert!(settings < update);

    state.overlay = None;
    state.mode = ClientShellMode::Navigate;
    let navigate = state.compose(106, 30).expect("combined attention navigate");
    let navigate_text = navigate
        .cells
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect::<String>();
    assert!(navigate_text.contains("update ready"));
}

#[test]
fn current_release_notes_use_whats_new_without_attention_badge() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    let mut endpoint_snapshot = snapshot();
    endpoint_snapshot.latest_release_notes_available = true;
    endpoint_snapshot.release_notes = Some(crate::protocol::ClientShellReleaseNotes {
        version: "0.8.2".into(),
        body: "### Changed\n- Client shell".into(),
        preview: false,
    });
    state.set_snapshot(Box::new(endpoint_snapshot));
    state.set_pane_surface(surface());
    state.compose(106, 30).expect("shell frame");
    assert_eq!(state.hits.global_launcher.width, 6);
    state.toggle_global_menu();
    let menu = state.compose(106, 30).expect("what's new menu");
    let text = menu
        .cells
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect::<String>();
    assert!(text.contains("what's new"));
}

#[test]
fn client_settings_preview_restore_and_endpoint_integrations_are_owned_by_overlay() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.overlay = Some(ClientShellOverlay::GlobalMenu(ClientGlobalMenuOverlay {
        highlighted: 0,
    }));
    let open = state.handle_input_bytes(b"\r");
    assert!(open.actions.is_empty());
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Settings(ClientSettingsOverlay {
            section: ClientSettingsSection::Theme,
            ..
        }))
    ));
    let original_theme = state.config.theme_name.clone();
    let original_palette = state.config.palette.clone();
    state.handle_input_bytes(b"j");
    assert_ne!(state.config.theme_name, original_theme);
    assert_ne!(state.config.palette.accent, original_palette.accent);
    state.handle_input_bytes(b"\x1b");
    assert!(state.overlay.is_none());
    assert_eq!(state.config.theme_name, original_theme);
    assert_eq!(state.config.palette.accent, original_palette.accent);

    state.open_settings_overlay();
    state.handle_input_bytes(b"j");
    state.handle_input_bytes(b"\t");
    state
        .compose(106, 30)
        .expect("settings outside-click geometry");
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 0,
        row: 0,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(state.overlay.is_none());
    assert_eq!(state.config.theme_name, original_theme);
    assert_eq!(state.config.palette.accent, original_palette.accent);

    state.open_settings_overlay();
    state.compose(106, 30).expect("settings overlay");
    for _ in 0..3 {
        let next = state.handle_input_bytes(b"\t");
        assert!(next.actions.is_empty());
    }
    let integrations = state.handle_input_bytes(b"\t");
    let [ClientShellAction::Endpoint { request, .. }] = &integrations.actions[..] else {
        panic!("integration section should request endpoint status");
    };
    assert!(matches!(
        request.method,
        crate::api::schema::Method::IntegrationList(_)
    ));
    let request_id = request.id.clone();
    assert!(
        state
            .handle_endpoint_result(
                "boot-1",
                &request_id,
                Ok(crate::api::schema::ResponseResult::IntegrationList {
                    integrations: vec![
                        crate::api::schema::IntegrationInfo {
                            target: crate::api::schema::IntegrationTarget::Codex,
                            label: "codex".into(),
                            command: "codex".into(),
                            available: true,
                            state: crate::api::schema::IntegrationState::Outdated,
                        },
                        crate::api::schema::IntegrationInfo {
                            target: crate::api::schema::IntegrationTarget::Claude,
                            label: "claude".into(),
                            command: "claude".into(),
                            available: false,
                            state: crate::api::schema::IntegrationState::NotInstalled,
                        },
                    ],
                }),
            )
            .0
    );
    let frame = state.compose(106, 30).expect("loaded integrations");
    let text = frame
        .cells
        .chunks(frame.width as usize)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("update available"));
    assert!(text.contains("not found"));
    assert!(!text.contains("pane labels"));

    let popup = state.hits.settings_popup;
    let blank_click =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: popup.right().saturating_sub(2),
            row: popup.y + 3,
            modifiers: KeyModifiers::empty(),
        })]);
    assert!(!blank_click.repaint);
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Settings(_))
    ));

    let install = state.handle_input_bytes(b"\r");
    assert_eq!(install.actions.len(), 1);
    assert!(matches!(
        &install.actions[0],
        ClientShellAction::Endpoint { request, .. }
            if matches!(
                request.method,
                crate::api::schema::Method::IntegrationInstall(
                    crate::api::schema::IntegrationInstallParams {
                        target: crate::api::schema::IntegrationTarget::Codex
                    }
                )
            )
    ));
    let escape = state.handle_input_bytes(b"\x1b");
    assert!(!escape.repaint);
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Settings(_))
    ));
    let install_request_id = match &install.actions[0] {
        ClientShellAction::Endpoint { request, .. } => request.id.clone(),
        _ => unreachable!("integration install action"),
    };
    let (repaint, refresh_actions) = state.handle_endpoint_result(
        "boot-1",
        &install_request_id,
        Ok(crate::api::schema::ResponseResult::IntegrationInstall {
            target: crate::api::schema::IntegrationTarget::Codex,
            details: crate::api::schema::IntegrationInstallResult {
                messages: vec!["installed codex".into()],
            },
        }),
    );
    assert!(repaint);
    assert!(matches!(
        refresh_actions.as_slice(),
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(request.method, crate::api::schema::Method::IntegrationList(_))
    ));
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Settings(ClientSettingsOverlay {
            loading_integrations: true,
            installing_integrations: false,
            ref integration_messages,
            ..
        })) if integration_messages == &["installed codex"]
    ));
}
