use super::harness::*;

#[test]
fn agent_explain_missing_file_reports_json_error() {
    let base = unique_test_dir();
    let missing = base.join("missing-screen.txt");
    let output = run_named_cli(
        &base.join("config"),
        &base.join("runtime"),
        &[
            "agent",
            "explain",
            "--file",
            missing.to_str().unwrap(),
            "--agent",
            "claude",
            "--json",
        ],
    );

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["id"], "cli:agent:explain");
    assert_eq!(error["error"]["code"], "agent_explain_file_read_failed");
    assert!(error["error"]["message"]
        .as_str()
        .unwrap()
        .contains(missing.to_str().unwrap()));
}

fn write_delayed_shell_and_fake_pi(
    base: &Path,
    shell_delay_seconds: &str,
) -> (PathBuf, PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt;

    let bin = base.join("bin");
    let delayed_shell = bin.join("delayed-shell");
    let fake_pi = bin.join("pi");
    let invocations = base.join("pi-invocations");
    fs::create_dir_all(&bin).unwrap();
    fs::write(
        &delayed_shell,
        format!("#!/bin/sh\n/bin/sleep {shell_delay_seconds}\nexec /bin/sh\n"),
    )
    .unwrap();
    fs::write(
        &fake_pi,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >> '{}'\nexport HERDR_AGENT=pi\n'{}' pane report-agent \"$HERDR_PANE_ID\" --source custom:delayed-shell-pi --agent pi --state idle >/dev/null\nwhile IFS= read -r _prompt; do :; done\n",
            invocations.display(),
            env!("CARGO_BIN_EXE_herdr"),
        ),
    )
    .unwrap();
    fs::set_permissions(&delayed_shell, fs::Permissions::from_mode(0o755)).unwrap();
    fs::set_permissions(&fake_pi, fs::Permissions::from_mode(0o755)).unwrap();
    (bin, delayed_shell, invocations)
}

#[test]
fn agent_start_waits_for_a_new_pane_shell_to_finish_initializing() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");
    let (bin, delayed_shell, invocations) = write_delayed_shell_and_fake_pi(&base, "0.4");
    let config = format!(
        "onboarding = false\n[terminal]\ndefault_shell = {:?}\nshell_mode = \"non_login\"\n",
        delayed_shell.to_str().unwrap()
    );
    let herdr = spawn_herdr_with_config(
        &config_home,
        &runtime_dir,
        &socket_path,
        Some(&bin),
        &config,
    );
    wait_for_socket(&socket_path, Duration::from_secs(5));
    let seed = run_cli_json(
        &socket_path,
        &["workspace", "create", "--cwd", base.to_str().unwrap()],
    );
    let seed_workspace = seed["result"]["workspace"]["workspace_id"]
        .as_str()
        .unwrap();
    let created = run_cli_json(
        &socket_path,
        &[
            "workspace",
            "create",
            "--cwd",
            base.to_str().unwrap(),
            "--no-focus",
        ],
    );
    let pane_id = created["result"]["root_pane"]["pane_id"].as_str().unwrap();
    let terminal_id = created["result"]["root_pane"]["terminal_id"]
        .as_str()
        .unwrap();
    assert!(!created["result"]["root_pane"]["focused"].as_bool().unwrap());

    let started = run_cli_json(
        &socket_path,
        &[
            "agent",
            "start",
            "worker",
            "--kind",
            "pi",
            "--pane",
            pane_id,
            "--timeout",
            "8000",
            "--",
            "--no-context-files",
            "--no-skills",
            "--no-extensions",
        ],
    );

    assert_eq!(started["result"]["agent"]["terminal_id"], terminal_id);
    assert_eq!(started["result"]["agent"]["pane_id"], pane_id);
    assert_eq!(started["result"]["agent"]["interactive_ready"], true);
    assert_eq!(
        fs::read_to_string(&invocations).unwrap(),
        "--no-context-files\n--no-skills\n--no-extensions\n"
    );
    assert_eq!(
        run_cli_json(&socket_path, &["workspace", "list"])["result"]["workspaces"]
            .as_array()
            .unwrap()
            .iter()
            .find(|workspace| workspace["workspace_id"] == seed_workspace)
            .unwrap()["focused"],
        true
    );

    cleanup_spawned_herdr(herdr, base);
}

#[test]
fn agent_start_stops_retrying_when_the_pane_shell_stays_busy() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");
    let (bin, delayed_shell, invocations) = write_delayed_shell_and_fake_pi(&base, "2.3");
    let config = format!(
        "onboarding = false\n[terminal]\ndefault_shell = {:?}\nshell_mode = \"non_login\"\n",
        delayed_shell.to_str().unwrap()
    );
    let herdr = spawn_herdr_with_config(
        &config_home,
        &runtime_dir,
        &socket_path,
        Some(&bin),
        &config,
    );
    wait_for_socket(&socket_path, Duration::from_secs(5));
    let created = run_cli_json(
        &socket_path,
        &["workspace", "create", "--cwd", base.to_str().unwrap()],
    );
    let pane_id = created["result"]["root_pane"]["pane_id"].as_str().unwrap();

    let started_at = Instant::now();
    let unavailable = run_cli(
        &socket_path,
        &[
            "agent",
            "start",
            "worker",
            "--kind",
            "pi",
            "--pane",
            pane_id,
            "--timeout",
            "8000",
        ],
    );
    assert_eq!(unavailable.status.code(), Some(1));
    let error: serde_json::Value = serde_json::from_slice(&unavailable.stderr).unwrap();
    assert_eq!(error["error"]["code"], "agent_pane_busy");
    assert!(started_at.elapsed() >= Duration::from_secs(2));
    assert!(started_at.elapsed() < Duration::from_secs(4));
    assert!(!invocations.exists());

    let retried = run_cli_json(
        &socket_path,
        &[
            "agent",
            "start",
            "worker",
            "--kind",
            "pi",
            "--pane",
            pane_id,
            "--timeout",
            "8000",
        ],
    );
    assert_eq!(retried["result"]["type"], "agent_started");
    assert_eq!(fs::read_to_string(&invocations).unwrap(), "\n");

    cleanup_spawned_herdr(herdr, base);
}

#[test]
fn agent_start_command_works() {
    use std::os::unix::fs::PermissionsExt;

    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");
    let bin = base.join("bin");
    let captured_args = base.join("pi-args");
    let captured_prompts = base.join("pi-prompts");
    fs::create_dir_all(&bin).unwrap();
    let fake_pi = bin.join("pi");
    fs::write(
        &fake_pi,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nexport HERDR_AGENT=pi\n'{}' pane report-agent \"$HERDR_PANE_ID\" --source custom:fake-pi --agent pi --state idle >/dev/null\nwhile IFS= read -r prompt; do\n  case \"$prompt\" in \"do not transition\"|\"stall\") continue ;; esac\n  '{}' pane report-agent \"$HERDR_PANE_ID\" --source custom:fake-pi --agent pi --state working >/dev/null\n  '{}' pane report-agent \"$HERDR_PANE_ID\" --source custom:fake-pi --agent pi --state idle >/dev/null\n  printf '%s\\n' \"$prompt\" >> '{}'\ndone\n",
            captured_args.display(),
            env!("CARGO_BIN_EXE_herdr"),
            env!("CARGO_BIN_EXE_herdr"),
            env!("CARGO_BIN_EXE_herdr"),
            captured_prompts.display(),
        ),
    )
    .unwrap();
    fs::set_permissions(&fake_pi, fs::Permissions::from_mode(0o755)).unwrap();

    let herdr = spawn_herdr_with_path(&config_home, &runtime_dir, &socket_path, Some(&bin));
    wait_for_socket(&socket_path, Duration::from_secs(5));
    let created = run_cli_json(
        &socket_path,
        &["workspace", "create", "--cwd", base.to_str().unwrap()],
    );
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    run_cli_json(&socket_path, &["pane", "rename", &pane_id, "shell-pane"]);
    let before = run_cli_json(&socket_path, &["pane", "list"]);
    let before_topology = pane_topology_snapshot(&before);

    let missing = run_cli(
        &socket_path,
        &[
            "agent", "start", "missing", "--kind", "pi", "--pane", "w999:p1",
        ],
    );
    assert_eq!(missing.status.code(), Some(1));
    let missing: serde_json::Value = serde_json::from_slice(&missing.stderr).unwrap();
    assert_eq!(missing["error"]["code"], "agent_pane_not_found");
    assert_eq!(
        pane_topology_snapshot(&run_cli_json(&socket_path, &["pane", "list"])),
        before_topology
    );

    for unsafe_arg in ["tab\tcompletion", "escape\x1b[201~"] {
        let rejected = run_cli(
            &socket_path,
            &[
                "agent",
                "start",
                "invalid-argument",
                "--kind",
                "pi",
                "--pane",
                &pane_id,
                "--",
                unsafe_arg,
            ],
        );
        assert_eq!(rejected.status.code(), Some(1));
        let error: serde_json::Value = serde_json::from_slice(&rejected.stderr).unwrap();
        assert_eq!(error["error"]["code"], "invalid_agent_argument");
    }

    for invalid_timeout in ["3000", "300001", "18446744073709551615"] {
        let rejected = run_cli(
            &socket_path,
            &[
                "agent",
                "start",
                "invalid-timeout",
                "--kind",
                "pi",
                "--pane",
                &pane_id,
                "--timeout",
                invalid_timeout,
            ],
        );
        assert_eq!(rejected.status.code(), Some(1));
        let error: serde_json::Value = serde_json::from_slice(&rejected.stderr).unwrap();
        assert_eq!(error["error"]["code"], "invalid_agent_timeout");
    }

    let started = run_cli_json(
        &socket_path,
        &[
            "agent",
            "start",
            "main",
            "--kind",
            "pi",
            "--pane",
            &pane_id,
            "--timeout",
            "8000",
            "--",
            "--name",
            "scratch",
            "--no-session",
        ],
    );
    assert_eq!(started["result"]["type"], "agent_started");
    assert_eq!(started["result"]["agent"]["name"], "main");
    assert_eq!(started["result"]["agent"]["agent"], "pi");
    assert_eq!(started["result"]["agent"]["pane_id"], pane_id);
    assert_eq!(
        run_cli_json(&socket_path, &["pane", "get", &pane_id])["result"]["pane"]["label"],
        "shell-pane"
    );
    assert_eq!(started["result"]["argv"][0], "pi");
    assert_eq!(started["result"]["argv"][1], "--name");
    assert_eq!(started["result"]["argv"][2], "scratch");
    assert_eq!(started["result"]["argv"][3], "--no-session");
    assert_eq!(
        fs::read_to_string(&captured_args).unwrap(),
        "--name\nscratch\n--no-session\n"
    );

    let literal_flag_prompt = run_cli(&socket_path, &["agent", "prompt", "main", "--wait"]);
    assert!(
        literal_flag_prompt.status.success(),
        "flag-shaped prompt was not treated literally: {}",
        String::from_utf8_lossy(&literal_flag_prompt.stderr)
    );
    let literal_flag_prompt: serde_json::Value =
        serde_json::from_slice(&literal_flag_prompt.stdout).unwrap();
    assert_eq!(literal_flag_prompt["result"]["type"], "agent_prompted");
    assert!(wait_until(
        Duration::from_secs(2),
        Duration::from_millis(25),
        || captured_prompts.exists()
    ));

    let after = run_cli_json(&socket_path, &["pane", "list"]);
    assert_eq!(pane_topology_snapshot(&after), before_topology);

    let prompts_before_blocked = fs::read(&captured_prompts).unwrap();
    let blocked_report = run_cli(
        &socket_path,
        &[
            "pane",
            "report-agent",
            &pane_id,
            "--source",
            "custom:fake-pi",
            "--agent",
            "pi",
            "--state",
            "blocked",
        ],
    );
    assert!(blocked_report.status.success());
    assert_eq!(
        run_cli_json(&socket_path, &["agent", "get", "main"])["result"]["agent"]["agent_status"],
        "blocked"
    );

    let blocked_prompt = run_cli(
        &socket_path,
        &[
            "agent",
            "prompt",
            "main",
            "must not be submitted",
            "--wait",
            "--timeout",
            "2000",
        ],
    );
    assert_eq!(blocked_prompt.status.code(), Some(1));
    let blocked_prompt: serde_json::Value = serde_json::from_slice(&blocked_prompt.stderr).unwrap();
    assert_eq!(blocked_prompt["error"]["code"], "agent_blocked");
    thread::sleep(Duration::from_millis(400));
    assert_eq!(fs::read(&captured_prompts).unwrap(), prompts_before_blocked);

    let idle_report = run_cli(
        &socket_path,
        &[
            "pane",
            "report-agent",
            &pane_id,
            "--source",
            "custom:fake-pi",
            "--agent",
            "pi",
            "--state",
            "idle",
        ],
    );
    assert!(idle_report.status.success());

    let stale_idle = run_cli(
        &socket_path,
        &[
            "agent",
            "prompt",
            "main",
            "do not transition",
            "--wait",
            "--timeout",
            "500",
        ],
    );
    assert_eq!(stale_idle.status.code(), Some(1));
    let stale_idle: serde_json::Value = serde_json::from_slice(&stale_idle.stderr).unwrap();
    assert_eq!(stale_idle["error"]["code"], "timeout");

    let stalled = run_cli(
        &socket_path,
        &[
            "agent",
            "prompt",
            "main",
            "stall",
            "--wait",
            "--timeout",
            "6000",
        ],
    );
    assert_eq!(stalled.status.code(), Some(1));
    let stalled: serde_json::Value = serde_json::from_slice(&stalled.stderr).unwrap();
    assert_eq!(stalled["error"]["code"], "agent_prompt_stalled");
    assert!(stalled["error"]["message"]
        .as_str()
        .is_some_and(|message| message.contains("state_change_seq remained")));

    let prompted = run_cli(
        &socket_path,
        &[
            "agent",
            "prompt",
            "main",
            "Review this diff",
            "--wait",
            "--timeout",
            "2000",
        ],
    );
    assert!(
        prompted.status.success(),
        "prompt failed: {}",
        String::from_utf8_lossy(&prompted.stderr)
    );
    let prompted: serde_json::Value = serde_json::from_slice(&prompted.stdout).unwrap();
    assert_eq!(prompted["result"]["type"], "agent_prompted");

    let duplicate = run_cli(
        &socket_path,
        &["agent", "start", "main", "--kind", "pi", "--pane", &pane_id],
    );
    assert!(!duplicate.status.success());
    let duplicate_json: serde_json::Value = serde_json::from_slice(&duplicate.stderr).unwrap();
    assert_eq!(duplicate_json["error"]["code"], "agent_name_taken");

    let busy = run_cli(
        &socket_path,
        &[
            "agent", "start", "second", "--kind", "pi", "--pane", &pane_id,
        ],
    );
    assert!(!busy.status.success());
    let busy_json: serde_json::Value = serde_json::from_slice(&busy.stderr).unwrap();
    assert_eq!(busy_json["error"]["code"], "agent_pane_busy");

    cleanup_spawned_herdr(herdr, base);
}

#[test]
fn agent_start_rejects_a_shell_replaced_by_a_foreground_program() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");
    let herdr = spawn_herdr(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));
    let created = run_cli_json(
        &socket_path,
        &["workspace", "create", "--cwd", base.to_str().unwrap()],
    );
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    let topology = pane_topology_snapshot(&run_cli_json(&socket_path, &["pane", "list"]));
    assert!(
        run_cli(&socket_path, &["pane", "run", &pane_id, "exec sleep 5"])
            .status
            .success()
    );
    thread::sleep(Duration::from_millis(150));

    let started = run_cli(
        &socket_path,
        &[
            "agent",
            "start",
            "worker",
            "--kind",
            "pi",
            "--pane",
            &pane_id,
            "--timeout",
            "4000",
        ],
    );
    assert_eq!(started.status.code(), Some(1));
    let error: serde_json::Value = serde_json::from_slice(&started.stderr).unwrap();
    assert_eq!(error["error"]["code"], "agent_pane_busy");
    assert_eq!(
        pane_topology_snapshot(&run_cli_json(&socket_path, &["pane", "list"])),
        topology
    );

    cleanup_spawned_herdr(herdr, base);
}

#[test]
fn agent_start_timeout_releases_the_name_for_reuse() {
    use std::os::unix::fs::PermissionsExt;

    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");
    let bin = base.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let fake_pi = bin.join("pi");
    fs::write(
        &fake_pi,
        "#!/bin/sh\nunset HERDR_AGENT\nexec /bin/sleep 20\n",
    )
    .unwrap();
    fs::set_permissions(&fake_pi, fs::Permissions::from_mode(0o755)).unwrap();

    let herdr = spawn_herdr_with_path(&config_home, &runtime_dir, &socket_path, Some(&bin));
    wait_for_socket(&socket_path, Duration::from_secs(5));
    let created = run_cli_json(
        &socket_path,
        &["workspace", "create", "--cwd", base.to_str().unwrap()],
    );
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    let split = run_cli_json(
        &socket_path,
        &["pane", "split", &pane_id, "--direction", "right"],
    );
    let reuse_pane_id = split["result"]["pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();

    assert!(run_cli(
        &socket_path,
        &[
            "pane",
            "report-agent",
            &reuse_pane_id,
            "--source",
            "custom:reuse",
            "--agent",
            "pi",
            "--state",
            "idle",
        ],
    )
    .status
    .success());

    let started = run_cli(
        &socket_path,
        &[
            "agent",
            "start",
            "worker",
            "--kind",
            "pi",
            "--pane",
            &pane_id,
            "--timeout",
            "3100",
        ],
    );
    assert_eq!(started.status.code(), Some(1));
    let error: serde_json::Value = serde_json::from_slice(&started.stderr).unwrap();
    assert_eq!(error["error"]["code"], "timeout");

    let reused = run_cli(&socket_path, &["agent", "rename", &reuse_pane_id, "worker"]);
    assert!(
        reused.status.success(),
        "name was not released: {}",
        String::from_utf8_lossy(&reused.stderr)
    );

    cleanup_spawned_herdr(herdr, base);
}

#[test]
fn agent_start_reports_detected_kind_mismatch_before_released_name() {
    use std::os::unix::fs::PermissionsExt;

    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");
    let bin = base.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let fake_pi = bin.join("pi");
    fs::write(
        &fake_pi,
        "#!/bin/sh\nHERDR_AGENT=codex exec /bin/sleep 10\n",
    )
    .unwrap();
    fs::set_permissions(&fake_pi, fs::Permissions::from_mode(0o755)).unwrap();

    let herdr = spawn_herdr_with_path(&config_home, &runtime_dir, &socket_path, Some(&bin));
    wait_for_socket(&socket_path, Duration::from_secs(5));
    let created = run_cli_json(
        &socket_path,
        &["workspace", "create", "--cwd", base.to_str().unwrap()],
    );
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    let split = run_cli_json(
        &socket_path,
        &["pane", "split", &pane_id, "--direction", "right"],
    );
    let reuse_pane_id = split["result"]["pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();

    let started = run_cli(
        &socket_path,
        &[
            "agent",
            "start",
            "worker",
            "--kind",
            "pi",
            "--pane",
            &pane_id,
            "--timeout",
            "5000",
        ],
    );
    assert_eq!(started.status.code(), Some(1));
    let error: serde_json::Value = serde_json::from_slice(&started.stderr).unwrap();
    assert_eq!(error["error"]["code"], "agent_kind_mismatch");

    assert!(run_cli(
        &socket_path,
        &[
            "pane",
            "report-agent",
            &reuse_pane_id,
            "--source",
            "custom:reuse",
            "--agent",
            "pi",
            "--state",
            "idle",
        ],
    )
    .status
    .success());
    let reused = run_cli(&socket_path, &["agent", "rename", &reuse_pane_id, "worker"]);
    assert!(reused.status.success());

    cleanup_spawned_herdr(herdr, base);
}

#[test]
fn agent_start_follows_its_named_terminal_when_the_pane_moves() {
    use std::os::unix::fs::PermissionsExt;

    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");
    let bin = base.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let fake_pi = bin.join("pi");
    fs::write(&fake_pi, "#!/bin/sh\nHERDR_AGENT=pi exec /bin/sleep 10\n").unwrap();
    fs::set_permissions(&fake_pi, fs::Permissions::from_mode(0o755)).unwrap();

    let herdr = spawn_herdr_with_path(&config_home, &runtime_dir, &socket_path, Some(&bin));
    wait_for_socket(&socket_path, Duration::from_secs(5));
    let created = run_cli_json(
        &socket_path,
        &["workspace", "create", "--cwd", base.to_str().unwrap()],
    );
    let first = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    let start_socket = socket_path.clone();
    let start_pane = first.clone();
    let starter = thread::spawn(move || {
        run_cli(
            &start_socket,
            &[
                "agent",
                "start",
                "worker",
                "--kind",
                "pi",
                "--pane",
                &start_pane,
                "--timeout",
                "8000",
            ],
        )
    });
    assert!(wait_until(
        Duration::from_secs(2),
        Duration::from_millis(25),
        || run_cli(&socket_path, &["agent", "get", "worker"])
            .status
            .success()
    ));

    let moved = run_cli(
        &socket_path,
        &[
            "pane",
            "move",
            &first,
            "--new-workspace",
            "--label",
            "moved",
            "--no-focus",
        ],
    );
    assert!(
        moved.status.success(),
        "move failed: {}",
        String::from_utf8_lossy(&moved.stderr)
    );

    let started = starter.join().unwrap();
    assert!(
        started.status.success(),
        "start failed after swap: {}",
        String::from_utf8_lossy(&started.stderr)
    );
    let started: serde_json::Value = serde_json::from_slice(&started.stdout).unwrap();
    assert_ne!(started["result"]["agent"]["pane_id"], first);

    cleanup_spawned_herdr(herdr, base);
}

#[test]
fn agent_start_and_rename_reject_invalid_names() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");
    let herdr = spawn_herdr(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));
    let created = run_cli_json(
        &socket_path,
        &["workspace", "create", "--cwd", base.to_str().unwrap()],
    );
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    let expected_message = "agent name must start with a lowercase letter and contain only lowercase letters, digits, '-' or '_' (1-32 characters)";

    let started = run_cli(
        &socket_path,
        &[
            "agent",
            "start",
            "reviewer one",
            "--kind",
            "pi",
            "--pane",
            &pane_id,
        ],
    );
    assert_eq!(started.status.code(), Some(1));
    let error: serde_json::Value = serde_json::from_slice(&started.stderr).unwrap();
    assert_eq!(error["error"]["code"], "invalid_agent_name");
    assert_eq!(error["error"]["message"], expected_message);

    assert!(run_cli(
        &socket_path,
        &[
            "pane",
            "report-agent",
            &pane_id,
            "--source",
            "custom:name",
            "--agent",
            "pi",
            "--state",
            "idle",
        ],
    )
    .status
    .success());
    let renamed = run_cli(&socket_path, &["agent", "rename", &pane_id, "reviewer one"]);
    assert_eq!(renamed.status.code(), Some(1));
    let error: serde_json::Value = serde_json::from_slice(&renamed.stderr).unwrap();
    assert_eq!(error["error"]["code"], "invalid_agent_name");
    assert_eq!(error["error"]["message"], expected_message);

    cleanup_spawned_herdr(herdr, base);
}

#[test]
fn agent_commands_work() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");

    let herdr = spawn_herdr(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let created = run_cli(
        &socket_path,
        &["workspace", "create", "--cwd", base.to_str().unwrap()],
    );
    assert!(created.status.success());
    let created_json: serde_json::Value = serde_json::from_slice(&created.stdout).unwrap();
    let root_pane_id = created_json["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    let terminal_id = created_json["result"]["root_pane"]["terminal_id"]
        .as_str()
        .unwrap()
        .to_string();

    let reported = run_cli(
        &socket_path,
        &[
            "pane",
            "report-agent",
            &root_pane_id,
            "--source",
            "custom:test",
            "--agent",
            "pi",
            "--state",
            "idle",
        ],
    );
    assert!(reported.status.success());
    let renamed = run_cli(&socket_path, &["agent", "rename", &root_pane_id, "worker"]);
    assert!(renamed.status.success());

    let listed = run_cli_json(&socket_path, &["agent", "list"]);
    assert_eq!(listed["result"]["type"], "agent_list");
    assert_eq!(listed["result"]["agents"][0]["terminal_id"], terminal_id);
    assert_eq!(listed["result"]["agents"][0]["name"], "worker");

    let fetched = run_cli_json(&socket_path, &["agent", "get", "worker"]);
    assert_eq!(fetched["result"]["agent"]["pane_id"], root_pane_id);
    let waited = run_cli_json(
        &socket_path,
        &["agent", "wait", "worker", "--timeout", "100"],
    );
    assert_eq!(waited["result"]["agent"]["pane_id"], root_pane_id);

    // A stale semantic report must not allow prompt text into the resumed shell.
    let prompted = run_cli(
        &socket_path,
        &["agent", "prompt", "worker", "echo prompt-must-not-run"],
    );
    assert_eq!(prompted.status.code(), Some(1));
    let prompted_json: serde_json::Value = serde_json::from_slice(&prompted.stderr).unwrap();
    assert_eq!(prompted_json["error"]["code"], "agent_not_ready");

    let working = run_cli(
        &socket_path,
        &[
            "pane",
            "report-agent",
            &root_pane_id,
            "--source",
            "custom:wait",
            "--agent",
            "pi",
            "--state",
            "working",
        ],
    );
    assert!(working.status.success());
    let blocked_socket = socket_path.clone();
    let blocked_pane = root_pane_id.clone();
    let blocked_transition = thread::spawn(move || {
        thread::sleep(Duration::from_millis(100));
        let blocked = run_cli(
            &blocked_socket,
            &[
                "pane",
                "report-agent",
                &blocked_pane,
                "--source",
                "custom:wait",
                "--agent",
                "pi",
                "--state",
                "blocked",
            ],
        );
        assert!(blocked.status.success());
    });
    let waited = run_cli_json(
        &socket_path,
        &["agent", "wait", "worker", "--timeout", "2000"],
    );
    blocked_transition.join().unwrap();
    assert_eq!(waited["result"]["agent"]["agent_status"], "blocked");
    let immediate_blocked =
        run_cli_json(&socket_path, &["agent", "wait", "worker", "--timeout", "1"]);
    assert_eq!(
        immediate_blocked["result"]["agent"]["agent_status"],
        "blocked"
    );

    assert!(run_cli(
        &socket_path,
        &[
            "pane",
            "report-agent",
            &root_pane_id,
            "--source",
            "custom:wait",
            "--agent",
            "pi",
            "--state",
            "working",
        ],
    )
    .status
    .success());
    let idle_socket = socket_path.clone();
    let idle_pane = root_pane_id.clone();
    let idle_transition = thread::spawn(move || {
        thread::sleep(Duration::from_millis(100));
        assert!(run_cli(
            &idle_socket,
            &[
                "pane",
                "report-agent",
                &idle_pane,
                "--source",
                "custom:wait",
                "--agent",
                "pi",
                "--state",
                "idle",
            ],
        )
        .status
        .success());
    });
    let idle_wait = run_cli_json(
        &socket_path,
        &["agent", "wait", "worker", "--timeout", "2000"],
    );
    idle_transition.join().unwrap();
    assert!(matches!(
        idle_wait["result"]["agent"]["agent_status"].as_str(),
        Some("idle" | "done")
    ));

    assert!(run_cli(
        &socket_path,
        &[
            "pane",
            "report-agent",
            &root_pane_id,
            "--source",
            "custom:wait",
            "--agent",
            "pi",
            "--state",
            "working",
        ],
    )
    .status
    .success());
    let timed_out = run_cli(
        &socket_path,
        &["agent", "wait", "worker", "--timeout", "100"],
    );
    assert_eq!(timed_out.status.code(), Some(1));
    let timeout: serde_json::Value = serde_json::from_slice(&timed_out.stderr).unwrap();
    assert_eq!(timeout["error"]["code"], "timeout");

    assert!(run_cli(
        &socket_path,
        &[
            "pane",
            "report-agent",
            &root_pane_id,
            "--source",
            "custom:wait",
            "--agent",
            "pi",
            "--state",
            "unknown",
        ],
    )
    .status
    .success());
    let unknown = run_cli_json(
        &socket_path,
        &[
            "agent",
            "wait",
            "worker",
            "--until",
            "unknown",
            "--timeout",
            "1000",
        ],
    );
    assert_eq!(unknown["result"]["agent"]["agent_status"], "unknown");

    let pane_read = run_cli(
        &socket_path,
        &["pane", "read", &root_pane_id, "--source", "visible"],
    );
    let agent_read = run_cli(
        &socket_path,
        &["agent", "read", &root_pane_id, "--source", "visible"],
    );
    assert!(pane_read.status.success());
    assert!(agent_read.status.success());
    assert_eq!(agent_read.stdout, pane_read.stdout);

    let missing_read = run_cli(&socket_path, &["agent", "read", "missing"]);
    assert_eq!(missing_read.status.code(), Some(1));
    let missing_read: serde_json::Value = serde_json::from_slice(&missing_read.stderr).unwrap();
    assert_eq!(missing_read["error"]["code"], "agent_not_found");

    let sent = run_cli(&socket_path, &["agent", "send-keys", "worker", "enter"]);
    assert_eq!(sent.status.code(), Some(1));
    let sent: serde_json::Value = serde_json::from_slice(&sent.stderr).unwrap();
    assert_eq!(sent["error"]["code"], "agent_not_ready");

    let agent_renamed = run_cli_json(&socket_path, &["agent", "rename", "worker", "reviewer"]);
    assert_eq!(agent_renamed["result"]["agent"]["name"], "reviewer");

    let focused = run_cli_json(&socket_path, &["agent", "focus", "reviewer"]);
    assert_eq!(focused["result"]["agent"]["focused"], true);

    cleanup_spawned_herdr(herdr, base);
}

#[test]
fn agent_wait_returns_immediately_for_unseen_done_agent() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");
    let herdr = spawn_herdr(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let created = run_cli_json(
        &socket_path,
        &["workspace", "create", "--cwd", base.to_str().unwrap()],
    );
    let first = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    let workspace_id = created["result"]["workspace"]["workspace_id"]
        .as_str()
        .unwrap();
    let second_tab = run_cli_json(
        &socket_path,
        &["tab", "create", "--workspace", workspace_id],
    );
    let second_tab_id = second_tab["result"]["tab"]["tab_id"].as_str().unwrap();
    assert_ne!(second_tab_id, "w1:t1");
    assert!(run_cli(&socket_path, &["tab", "focus", second_tab_id])
        .status
        .success());
    assert!(run_cli(
        &socket_path,
        &[
            "pane",
            "report-agent",
            &first,
            "--source",
            "custom:done",
            "--agent",
            "pi",
            "--state",
            "working",
        ],
    )
    .status
    .success());
    assert!(
        run_cli(&socket_path, &["agent", "rename", &first, "worker"])
            .status
            .success()
    );
    assert!(run_cli(
        &socket_path,
        &[
            "pane",
            "report-agent",
            &first,
            "--source",
            "custom:done",
            "--agent",
            "pi",
            "--state",
            "idle",
        ],
    )
    .status
    .success());

    let waited = run_cli_json(&socket_path, &["agent", "wait", "worker", "--timeout", "1"]);
    assert_eq!(waited["result"]["agent"]["agent_status"], "done");

    cleanup_spawned_herdr(herdr, base);
}

#[test]
fn agent_wait_tolerates_detection_uncertainty_and_pane_target_rename() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");
    let herdr = spawn_herdr(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));
    let created = run_cli_json(
        &socket_path,
        &["workspace", "create", "--cwd", base.to_str().unwrap()],
    );
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(run_cli(
        &socket_path,
        &[
            "pane",
            "report-agent",
            &pane_id,
            "--source",
            "custom:uncertain",
            "--agent",
            "pi",
            "--state",
            "working",
        ],
    )
    .status
    .success());
    assert!(
        run_cli(&socket_path, &["agent", "rename", &pane_id, "worker"])
            .status
            .success()
    );

    let wait_socket = socket_path.clone();
    let waiter = thread::spawn(move || {
        run_cli(
            &wait_socket,
            &[
                "agent",
                "wait",
                "worker",
                "--until",
                "unknown",
                "--timeout",
                "2000",
            ],
        )
    });
    thread::sleep(Duration::from_millis(150));
    let cleared = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"agent_wait_uncertain","method":"pane.clear_agent_authority","params":{{"pane_id":"{}","source":"custom:uncertain"}}}}"#,
            pane_id
        ),
    );
    assert_eq!(cleared["result"]["type"], "ok");
    let waited = waiter.join().unwrap();
    assert!(
        waited.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&waited.stderr)
    );
    let waited: serde_json::Value = serde_json::from_slice(&waited.stdout).unwrap();
    assert_eq!(waited["result"]["agent"]["agent_status"], "unknown");
    assert_eq!(waited["result"]["agent"]["name"], "worker");

    assert!(run_cli(
        &socket_path,
        &[
            "pane",
            "report-agent",
            &pane_id,
            "--source",
            "custom:return",
            "--agent",
            "pi",
            "--state",
            "working",
        ],
    )
    .status
    .success());
    let wait_socket = socket_path.clone();
    let wait_pane = pane_id.clone();
    let waiter = thread::spawn(move || {
        run_cli(
            &wait_socket,
            &[
                "agent",
                "wait",
                &wait_pane,
                "--until",
                "idle",
                "--timeout",
                "2000",
            ],
        )
    });
    thread::sleep(Duration::from_millis(150));
    assert!(
        run_cli(&socket_path, &["agent", "rename", "worker", "reviewer"])
            .status
            .success()
    );
    assert!(run_cli(
        &socket_path,
        &[
            "pane",
            "report-agent",
            &pane_id,
            "--source",
            "custom:return",
            "--agent",
            "pi",
            "--state",
            "idle",
        ],
    )
    .status
    .success());
    let waited = waiter.join().unwrap();
    assert!(
        waited.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&waited.stderr)
    );
    let waited: serde_json::Value = serde_json::from_slice(&waited.stdout).unwrap();
    assert_eq!(waited["result"]["agent"]["agent_status"], "idle");
    assert_eq!(waited["result"]["agent"]["name"], "reviewer");

    cleanup_spawned_herdr(herdr, base);
}

#[test]
fn agent_wait_pins_the_original_terminal_when_name_is_reused() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");
    let herdr = spawn_herdr(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let created = run_cli_json(
        &socket_path,
        &["workspace", "create", "--cwd", base.to_str().unwrap()],
    );
    let first = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    let split = run_cli_json(
        &socket_path,
        &["pane", "split", &first, "--direction", "right"],
    );
    let second = split["result"]["pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(run_cli(
        &socket_path,
        &[
            "pane",
            "report-agent",
            &first,
            "--source",
            "custom:race",
            "--agent",
            "pi",
            "--state",
            "working",
        ],
    )
    .status
    .success());
    assert!(
        run_cli(&socket_path, &["agent", "rename", &first, "worker"])
            .status
            .success()
    );

    let wait_socket = socket_path.clone();
    let waiter = thread::spawn(move || {
        run_cli(
            &wait_socket,
            &["agent", "wait", "worker", "--timeout", "2000"],
        )
    });
    thread::sleep(Duration::from_millis(250));
    assert!(
        run_cli(&socket_path, &["agent", "rename", "worker", "--clear"])
            .status
            .success()
    );
    assert!(run_cli(
        &socket_path,
        &[
            "pane",
            "report-agent",
            &second,
            "--source",
            "custom:race",
            "--agent",
            "pi",
            "--state",
            "idle",
        ],
    )
    .status
    .success());
    assert!(
        run_cli(&socket_path, &["agent", "rename", &second, "worker"])
            .status
            .success()
    );

    let waited = waiter.join().unwrap();
    assert_eq!(waited.status.code(), Some(1));
    let error: serde_json::Value = serde_json::from_slice(&waited.stderr).unwrap();
    assert_eq!(error["error"]["code"], "agent_not_running");

    cleanup_spawned_herdr(herdr, base);
}

#[test]
fn agent_wait_ignores_other_panes_and_errors_when_its_pane_closes() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");
    let herdr = spawn_herdr(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let created = run_cli_json(
        &socket_path,
        &["workspace", "create", "--cwd", base.to_str().unwrap()],
    );
    let first = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    let split = run_cli_json(
        &socket_path,
        &["pane", "split", &first, "--direction", "right"],
    );
    let second = split["result"]["pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(run_cli(
        &socket_path,
        &[
            "pane",
            "report-agent",
            &first,
            "--source",
            "custom:close",
            "--agent",
            "pi",
            "--state",
            "working",
        ],
    )
    .status
    .success());
    assert!(
        run_cli(&socket_path, &["agent", "rename", &first, "worker"])
            .status
            .success()
    );

    let wait_socket = socket_path.clone();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let _ = done_tx.send(run_cli(
            &wait_socket,
            &["agent", "wait", "worker", "--timeout", "3000"],
        ));
    });
    thread::sleep(Duration::from_millis(150));
    assert!(run_cli(
        &socket_path,
        &[
            "pane",
            "report-agent",
            &second,
            "--source",
            "custom:close",
            "--agent",
            "pi",
            "--state",
            "idle",
        ],
    )
    .status
    .success());
    thread::sleep(Duration::from_millis(150));
    assert!(matches!(
        done_rx.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));

    assert!(run_cli(&socket_path, &["pane", "close", &first])
        .status
        .success());
    let waited = done_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(waited.status.code(), Some(1));
    let error: serde_json::Value = serde_json::from_slice(&waited.stderr).unwrap();
    assert_eq!(error["error"]["code"], "agent_not_running");

    cleanup_spawned_herdr(herdr, base);
}
