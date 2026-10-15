use super::*;

#[test]
fn automatic_worktree_collision_keeps_focused_response_pending_until_final_completion() {
    // The response proxy uses a blocking receive. A Tokio timeout on the same
    // event loop cannot catch accidentally proxying an intermediate collision.
    let (finished, completion) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(check_focused_automatic_collision());
        finished.send(()).unwrap();
    });
    let outcome = completion.recv_timeout(Duration::from_secs(20));
    assert!(
        !matches!(outcome, Err(std::sync::mpsc::RecvTimeoutError::Timeout)),
        "intermediate worktree collision blocked the server response proxy"
    );
    worker.join().unwrap();
    outcome.unwrap();
}

async fn check_focused_automatic_collision() {
    use api::schema::{EventData, Method, ResponseResult, WorktreeCreateParams};

    let event_hub = api::EventHub::default();
    let mut server = test_headless_server_with_event_hub(event_hub.clone());
    let mut source = crate::workspace::Workspace::test_new("automatic-focus-source");
    let root = std::env::temp_dir().join(format!("herdr-auto-focus-{}", source.id));
    let repo = root.join("repo");
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "--quiet", repo.to_str().unwrap()]);
    git(&[
        "-C",
        repo.to_str().unwrap(),
        "-c",
        "user.name=Herdr Test",
        "-c",
        "user.email=herdr@example.invalid",
        "commit",
        "--quiet",
        "--allow-empty",
        "-m",
        "initial",
    ]);
    source.identity_cwd = repo.clone();
    let source_id = source.id.clone();
    server.app.state.workspaces = vec![source];
    server.app.state.ensure_test_terminals();
    server.app.state.active = Some(0);
    server.app.state.selected = 0;
    server.app.state.worktree_directory = root.join("checkouts");
    let (_control, _render) = connect_matching_test_shell(&mut server, 71);
    let original_tab = server.shell_tab_id_for_client(71);

    let (entered, release) = crate::worktree::test_list_gate::block(&repo);
    let (respond_to, response_rx) = std::sync::mpsc::channel();
    server.handle_api_request_with_shutdown_check(api::ApiRequestMessage {
        request: api::schema::Request {
            id: "automatic-focus".into(),
            method: Method::WorktreeCreate(WorktreeCreateParams {
                workspace_id: Some(source_id),
                focus: true,
                ..Default::default()
            }),
        },
        respond_to,
        response_write_complete: None,
    });
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    let first_path_key = server
        .app
        .pending_api_worktree_creates
        .keys()
        .next()
        .unwrap()
        .clone();
    // Reservation keys may use a Windows verbatim prefix that Git cannot
    // consume. Reconstruct the checkout path from the configured directory.
    let first_path = server
        .app
        .state
        .worktree_directory
        .join(repo.file_name().unwrap())
        .join(first_path_key.file_name().unwrap());
    assert_eq!(
        crate::worktree::canonical_or_original(&first_path),
        first_path_key
    );
    // Occupy the candidate while discovery is blocked. The automatic request
    // must allocate another path without adopting or deleting this checkout.
    git(&[
        "-C",
        repo.to_str().unwrap(),
        "worktree",
        "add",
        "--quiet",
        "--detach",
        first_path.to_str().unwrap(),
        "HEAD",
    ]);
    release.send(()).unwrap();
    let collision = tokio::time::timeout(Duration::from_secs(5), server.app.event_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(&collision, AppEvent::WorktreeAddFinished(result)
        if matches!(result.result, Err(crate::worktree::WorktreeCreateError::Collision(_)))),
        "expected intermediate collision, got {collision:?}"
    );
    server.handle_internal_event_with_forwarding(collision);
    assert!(matches!(
        response_rx.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));
    assert_eq!(server.shell_tab_id_for_client(71), original_tab);
    assert!(!event_hub
        .events_after(0)
        .iter()
        .any(|(_, event)| { matches!(event.data, EventData::WorktreeCreated { .. }) }));

    let response = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let event = server.app.event_rx.recv().await.unwrap();
            server.handle_internal_event_with_forwarding(event);
            if let Ok(response) = response_rx.try_recv() {
                break response;
            }
        }
    })
    .await
    .unwrap();
    let response: api::schema::SuccessResponse = serde_json::from_str(&response).unwrap();
    let ResponseResult::WorktreeCreated { worktree, tab, .. } = response.result else {
        panic!("expected final worktree creation response");
    };
    assert_ne!(std::path::Path::new(&worktree.path), first_path);
    assert_eq!(server.shell_tab_id_for_client(71), Some(tab.tab_id));
    assert!(server.app.pending_api_worktree_creates.is_empty());
    let created = event_hub
        .events_after(0)
        .into_iter()
        .filter_map(|(_, event)| match event.data {
            EventData::WorktreeCreated { worktree, .. } => Some(worktree),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(created.len(), 1);
    assert_eq!(created[0].path, worktree.path);
    assert_eq!(created[0].branch, worktree.branch);
    assert!(first_path.join(".git").is_file());
    shutdown_test_runtimes(&mut server);
    let _ = std::fs::remove_dir_all(root);
}
