use super::harness::*;
use super::hooks::FakeHookSocket;

use std::os::unix::fs::PermissionsExt;

const JCODE_HOOK_ASSET: &str =
    include_str!("../../src/integration/assets/jcode/herdr-agent-state.sh");
const USER_SCALAR: &str = "user-session-observer";
const USER_FIRST: &str = "user-first";
const USER_SECOND: &str = "user-second";
const REPLACEMENT_FIRST: &str = "replacement-first";
const REPLACEMENT_SECOND: &str = "replacement-second";
const SCALAR_CONFIG: &str = "# keep user metadata\n[display]\nemoji = false\n\n[hooks]\nturn_end = \"notify\"\nsession_start = \"user-session-observer\"\n";
const ARRAY_CONFIG: &str = "# keep user metadata\n[display]\nemoji = false\n\n[hooks]\nturn_end = \"notify\"\nsession_start = [\"user-first\", \"user-second\"]\n";
const MALFORMED_CONFIG: &str = "[hooks]\nsession_start = [\"user-session-observer\", 42]\n";
const REPLACEMENT_CONFIG: &str = "# user replaced the registered hook list\n[hooks]\nturn_end = \"notify\"\nsession_start = [\"replacement-first\", \"replacement-second\"]\n";

/*
The table below is the executable form of this graph. Every edge gets a fresh
Jcode home, materializes and checks its source state, executes the real CLI or
installed hook, then checks the target-state invariants.

```mermaid
stateDiagram-v2
    AbsentConfig --> ManagedOnly: install
    ScalarUserHook --> ManagedPlusScalar: install
    ArrayUserHooks --> ManagedPlusArray: install
    MalformedConfig --> MalformedConfig: rejected install
    ManagedOnly --> ManagedOnly: idempotent reinstall / hook probes
    ManagedPlusScalar --> ManagedPlusScalar: idempotent reinstall
    ManagedPlusArray --> ManagedPlusArray: idempotent reinstall
    ManagedPlusScalar --> UserReplacedManaged: user replaces registration
    ManagedOnly --> UninstalledEmpty: uninstall
    ManagedPlusScalar --> UninstalledScalar: uninstall
    ManagedPlusArray --> UninstalledArray: uninstall
    UserReplacedManaged --> UninstalledReplacement: uninstall
    UninstalledEmpty --> ManagedOnly: reinstall
    UninstalledScalar --> ManagedPlusScalar: reinstall
    UninstalledArray --> ManagedPlusArray: reinstall
```
*/
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum State {
    AbsentConfig,
    ScalarUserHook,
    ArrayUserHooks,
    MalformedConfig,
    ManagedOnly,
    ManagedPlusScalar,
    ManagedPlusArray,
    UserReplacedManaged,
    UninstalledEmpty,
    UninstalledScalar,
    UninstalledArray,
    UninstalledReplacement,
}

impl State {
    fn user_hooks(self) -> &'static [&'static str] {
        match self {
            Self::ScalarUserHook | Self::ManagedPlusScalar | Self::UninstalledScalar => {
                &[USER_SCALAR]
            }
            Self::ArrayUserHooks | Self::ManagedPlusArray | Self::UninstalledArray => {
                &[USER_FIRST, USER_SECOND]
            }
            Self::UserReplacedManaged | Self::UninstalledReplacement => {
                &[REPLACEMENT_FIRST, REPLACEMENT_SECOND]
            }
            Self::AbsentConfig
            | Self::MalformedConfig
            | Self::ManagedOnly
            | Self::UninstalledEmpty => &[],
        }
    }

    fn has_config(self) -> bool {
        self != Self::AbsentConfig
    }

    fn has_managed_hook_file(self) -> bool {
        matches!(
            self,
            Self::ManagedOnly
                | Self::ManagedPlusScalar
                | Self::ManagedPlusArray
                | Self::UserReplacedManaged
        )
    }

    fn has_managed_registration(self) -> bool {
        matches!(
            self,
            Self::ManagedOnly | Self::ManagedPlusScalar | Self::ManagedPlusArray
        )
    }

    fn preserves_seed_metadata(self) -> bool {
        matches!(
            self,
            Self::ScalarUserHook
                | Self::ArrayUserHooks
                | Self::ManagedPlusScalar
                | Self::ManagedPlusArray
                | Self::UninstalledScalar
                | Self::UninstalledArray
        )
    }
}

#[derive(Clone, Copy, Debug)]
enum Action {
    Install,
    IdempotentReinstall,
    RejectMalformedInstall,
    ReplaceManagedRegistration,
    Uninstall,
    ReinstallAfterUninstall,
    RunOutsideHerdr,
    RunCreateAndResume,
    RunTwoPanesOnOneSocket,
    RunWithoutPython,
    RunWithUnavailableSocket,
}

impl Action {
    fn must_not_change_files(self) -> bool {
        matches!(
            self,
            Self::IdempotentReinstall
                | Self::RejectMalformedInstall
                | Self::RunOutsideHerdr
                | Self::RunCreateAndResume
                | Self::RunTwoPanesOnOneSocket
                | Self::RunWithoutPython
                | Self::RunWithUnavailableSocket
        )
    }
}

#[derive(Clone, Copy, Debug)]
struct Edge {
    name: &'static str,
    from: State,
    action: Action,
    to: State,
}

const GRAPH: &[Edge] = &[
    Edge {
        name: "install with absent config",
        from: State::AbsentConfig,
        action: Action::Install,
        to: State::ManagedOnly,
    },
    Edge {
        name: "install with scalar user hook",
        from: State::ScalarUserHook,
        action: Action::Install,
        to: State::ManagedPlusScalar,
    },
    Edge {
        name: "install with user hook array",
        from: State::ArrayUserHooks,
        action: Action::Install,
        to: State::ManagedPlusArray,
    },
    Edge {
        name: "reject malformed hook array",
        from: State::MalformedConfig,
        action: Action::RejectMalformedInstall,
        to: State::MalformedConfig,
    },
    Edge {
        name: "idempotent reinstall without user hooks",
        from: State::ManagedOnly,
        action: Action::IdempotentReinstall,
        to: State::ManagedOnly,
    },
    Edge {
        name: "idempotent reinstall with scalar user hook",
        from: State::ManagedPlusScalar,
        action: Action::IdempotentReinstall,
        to: State::ManagedPlusScalar,
    },
    Edge {
        name: "idempotent reinstall with user hook array",
        from: State::ManagedPlusArray,
        action: Action::IdempotentReinstall,
        to: State::ManagedPlusArray,
    },
    Edge {
        name: "execution outside Herdr reports nothing",
        from: State::ManagedOnly,
        action: Action::RunOutsideHerdr,
        to: State::ManagedOnly,
    },
    Edge {
        name: "create and resume source mapping",
        from: State::ManagedOnly,
        action: Action::RunCreateAndResume,
        to: State::ManagedOnly,
    },
    Edge {
        name: "two panes share a socket without sharing session identity",
        from: State::ManagedOnly,
        action: Action::RunTwoPanesOnOneSocket,
        to: State::ManagedOnly,
    },
    Edge {
        name: "missing Python fails open",
        from: State::ManagedOnly,
        action: Action::RunWithoutPython,
        to: State::ManagedOnly,
    },
    Edge {
        name: "unavailable socket fails open",
        from: State::ManagedOnly,
        action: Action::RunWithUnavailableSocket,
        to: State::ManagedOnly,
    },
    Edge {
        name: "user replaces managed registration",
        from: State::ManagedPlusScalar,
        action: Action::ReplaceManagedRegistration,
        to: State::UserReplacedManaged,
    },
    Edge {
        name: "uninstall after user replacement preserves replacement",
        from: State::UserReplacedManaged,
        action: Action::Uninstall,
        to: State::UninstalledReplacement,
    },
    Edge {
        name: "uninstall removes only managed hook",
        from: State::ManagedOnly,
        action: Action::Uninstall,
        to: State::UninstalledEmpty,
    },
    Edge {
        name: "uninstall preserves scalar user hook",
        from: State::ManagedPlusScalar,
        action: Action::Uninstall,
        to: State::UninstalledScalar,
    },
    Edge {
        name: "uninstall preserves user hook array",
        from: State::ManagedPlusArray,
        action: Action::Uninstall,
        to: State::UninstalledArray,
    },
    Edge {
        name: "reinstall after empty uninstall",
        from: State::UninstalledEmpty,
        action: Action::ReinstallAfterUninstall,
        to: State::ManagedOnly,
    },
    Edge {
        name: "reinstall after scalar-hook uninstall",
        from: State::UninstalledScalar,
        action: Action::ReinstallAfterUninstall,
        to: State::ManagedPlusScalar,
    },
    Edge {
        name: "reinstall after array-hook uninstall",
        from: State::UninstalledArray,
        action: Action::ReinstallAfterUninstall,
        to: State::ManagedPlusArray,
    },
];

#[derive(Debug, Eq, PartialEq)]
struct FileSnapshot {
    config: Option<Vec<u8>>,
    hook: Option<Vec<u8>>,
}

struct LifecycleHarness {
    base: PathBuf,
    jcode_dir: PathBuf,
    config_path: PathBuf,
    hooks_dir: PathBuf,
    hook_path: PathBuf,
}

impl LifecycleHarness {
    fn new(edge_index: usize) -> Self {
        let base = unique_test_dir().join(format!("jcode-lifecycle-edge-{edge_index}"));
        let jcode_dir = base.join("jcode-home");
        fs::create_dir_all(&jcode_dir).unwrap();
        let hooks_dir = jcode_dir.join("hooks");
        Self {
            config_path: jcode_dir.join("config.toml"),
            hook_path: hooks_dir.join("herdr-agent-state.sh"),
            base,
            jcode_dir,
            hooks_dir,
        }
    }

    fn materialize(&self, state: State, edge: &Edge) {
        match state {
            State::AbsentConfig => {}
            State::ScalarUserHook => self.write_config(SCALAR_CONFIG),
            State::ArrayUserHooks => self.write_config(ARRAY_CONFIG),
            State::MalformedConfig => self.write_config(MALFORMED_CONFIG),
            State::ManagedOnly => self.install(edge),
            State::ManagedPlusScalar => {
                self.write_config(SCALAR_CONFIG);
                self.install(edge);
            }
            State::ManagedPlusArray => {
                self.write_config(ARRAY_CONFIG);
                self.install(edge);
            }
            State::UserReplacedManaged => {
                self.write_config(SCALAR_CONFIG);
                self.install(edge);
                self.write_config(REPLACEMENT_CONFIG);
            }
            State::UninstalledEmpty => {
                self.install(edge);
                self.uninstall(edge);
            }
            State::UninstalledScalar => {
                self.write_config(SCALAR_CONFIG);
                self.install(edge);
                self.uninstall(edge);
            }
            State::UninstalledArray => {
                self.write_config(ARRAY_CONFIG);
                self.install(edge);
                self.uninstall(edge);
            }
            State::UninstalledReplacement => {
                self.write_config(SCALAR_CONFIG);
                self.install(edge);
                self.write_config(REPLACEMENT_CONFIG);
                self.uninstall(edge);
            }
        }
    }

    fn apply(&self, edge: &Edge) {
        let before = self.snapshot();
        match edge.action {
            Action::Install | Action::IdempotentReinstall | Action::ReinstallAfterUninstall => {
                self.install(edge)
            }
            Action::RejectMalformedInstall => {
                let output = self.integration("install");
                assert!(
                    !output.status.success(),
                    "{}: malformed config was accepted",
                    edge.name
                );
                assert!(
                    String::from_utf8_lossy(&output.stderr)
                        .contains("must be a string or array of strings"),
                    "{}: unexpected rejection: {}",
                    edge.name,
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            Action::ReplaceManagedRegistration => self.write_config(REPLACEMENT_CONFIG),
            Action::Uninstall => self.uninstall(edge),
            Action::RunOutsideHerdr => self.run_outside_herdr(edge),
            Action::RunCreateAndResume => self.run_create_and_resume(edge),
            Action::RunTwoPanesOnOneSocket => self.run_two_panes(edge),
            Action::RunWithoutPython => self.run_without_python(edge),
            Action::RunWithUnavailableSocket => self.run_with_unavailable_socket(edge),
        }
        if edge.action.must_not_change_files() {
            assert_eq!(
                self.snapshot(),
                before,
                "{} changed managed files",
                edge.name
            );
        }
        if matches!(
            (edge.from, edge.action),
            (State::UserReplacedManaged, Action::Uninstall)
        ) {
            assert_eq!(
                fs::read_to_string(&self.config_path).unwrap(),
                REPLACEMENT_CONFIG,
                "{} rewrote the user's replacement config",
                edge.name
            );
        }
    }

    fn assert_state(&self, state: State, edge: &Edge, side: &str) {
        assert_eq!(
            self.config_path.is_file(),
            state.has_config(),
            "{} {side}: config existence does not match {state:?}",
            edge.name
        );
        assert_eq!(
            self.hook_path.is_file(),
            state.has_managed_hook_file(),
            "{} {side}: hook existence does not match {state:?}",
            edge.name
        );

        if state == State::MalformedConfig {
            assert_eq!(
                fs::read_to_string(&self.config_path).unwrap(),
                MALFORMED_CONFIG,
                "{} {side}: malformed config was changed",
                edge.name
            );
            assert!(
                !self.hooks_dir.exists(),
                "{} {side}: rejected config left managed filesystem state",
                edge.name
            );
            return;
        }

        if state.has_config() {
            let content = fs::read_to_string(&self.config_path).unwrap();
            let mut expected: Vec<_> = state
                .user_hooks()
                .iter()
                .map(|hook| (*hook).to_string())
                .collect();
            if state.has_managed_registration() {
                expected.push(self.managed_command());
            }
            assert_eq!(
                session_start_commands(&content),
                expected,
                "{} {side}: session_start commands do not match {state:?}",
                edge.name
            );
            if state.preserves_seed_metadata() {
                assert!(
                    content.contains("# keep user metadata"),
                    "{} {side}",
                    edge.name
                );
                assert!(
                    content.contains("turn_end = \"notify\""),
                    "{} {side}",
                    edge.name
                );
                assert!(content.contains("emoji = false"), "{} {side}", edge.name);
            }
        }

        if state.has_managed_hook_file() {
            assert_eq!(
                fs::read_to_string(&self.hook_path).unwrap(),
                JCODE_HOOK_ASSET,
                "{} {side}: installed hook differs from the bundled asset",
                edge.name
            );
            let mode = fs::metadata(&self.hook_path).unwrap().permissions().mode();
            assert_ne!(
                mode & 0o111,
                0,
                "{} {side}: hook is not executable",
                edge.name
            );
        }
    }

    fn integration(&self, action: &str) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_herdr"))
            .args(["integration", action, "jcode"])
            .env("JCODE_HOME", &self.jcode_dir)
            .env_remove("HERDR_ENV")
            .env_remove("HERDR_SOCKET_PATH")
            .env_remove("HERDR_CLIENT_SOCKET_PATH")
            .output()
            .unwrap()
    }

    fn install(&self, edge: &Edge) {
        self.assert_cli_success(edge, "install", self.integration("install"));
    }

    fn uninstall(&self, edge: &Edge) {
        self.assert_cli_success(edge, "uninstall", self.integration("uninstall"));
    }

    fn assert_cli_success(&self, edge: &Edge, action: &str, output: std::process::Output) {
        assert!(
            output.status.success(),
            "{}: {action} failed: stdout={} stderr={}",
            edge.name,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn write_config(&self, content: &str) {
        fs::write(&self.config_path, content).unwrap();
    }

    fn managed_command(&self) -> String {
        format!("bash '{}'", self.hook_path.display())
    }

    fn snapshot(&self) -> FileSnapshot {
        FileSnapshot {
            config: fs::read(&self.config_path).ok(),
            hook: fs::read(&self.hook_path).ok(),
        }
    }

    fn invoke_hook(
        &self,
        socket_path: &Path,
        pane_id: &str,
        session_id: &str,
        source: Option<&str>,
        inside_herdr: bool,
        path: Option<&Path>,
    ) -> std::process::Output {
        let mut command = Command::new(&self.hook_path);
        command
            .env_remove("HERDR_ENV")
            .env_remove("HERDR_SOCKET_PATH")
            .env_remove("HERDR_PANE_ID")
            .env_remove("JCODE_HOOK_SESSION_ID")
            .env_remove("JCODE_HOOK_SOURCE")
            .env("HERDR_SOCKET_PATH", socket_path)
            .env("HERDR_PANE_ID", pane_id)
            .env("JCODE_HOOK_SESSION_ID", session_id);
        if inside_herdr {
            command.env("HERDR_ENV", "1");
        }
        if let Some(source) = source {
            command.env("JCODE_HOOK_SOURCE", source);
        }
        if let Some(path) = path {
            command.env("PATH", path);
        }
        command.output().unwrap()
    }

    fn assert_hook_success(&self, edge: &Edge, output: &std::process::Output) {
        assert!(
            output.status.success(),
            "{}: hook failed open contract: stdout={} stderr={}",
            edge.name,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn run_outside_herdr(&self, edge: &Edge) {
        let socket = FakeHookSocket::start(1);
        let socket_path = socket.path().to_path_buf();
        let output = self.invoke_hook(
            &socket_path,
            "p_outside",
            "outside-session",
            Some("create"),
            false,
            None,
        );
        self.assert_hook_success(edge, &output);
        assert!(
            socket.finish().is_empty(),
            "{}: hook reported outside Herdr",
            edge.name
        );
    }

    fn run_create_and_resume(&self, edge: &Edge) {
        let socket = FakeHookSocket::start(2);
        let socket_path = socket.path().to_path_buf();
        for (pane, session, source) in [
            ("p_create", "create-session", "create"),
            ("p_resume", "resume-session", "resume"),
        ] {
            let output = self.invoke_hook(&socket_path, pane, session, Some(source), true, None);
            self.assert_hook_success(edge, &output);
        }
        let requests = socket.finish();
        assert_eq!(requests.len(), 2, "{}: missing reports", edge.name);
        assert_report(
            edge,
            &requests[0],
            "p_create",
            "create-session",
            Some("startup"),
        );
        assert_report(
            edge,
            &requests[1],
            "p_resume",
            "resume-session",
            Some("resume"),
        );
    }

    fn run_two_panes(&self, edge: &Edge) {
        let socket = FakeHookSocket::start(2);
        let socket_path = socket.path().to_path_buf();
        for (pane, session) in [
            ("p_shared_one", "session-one"),
            ("p_shared_two", "session-two"),
        ] {
            let output = self.invoke_hook(&socket_path, pane, session, None, true, None);
            self.assert_hook_success(edge, &output);
        }
        let requests = socket.finish();
        assert_eq!(requests.len(), 2, "{}: missing reports", edge.name);
        assert_report(edge, &requests[0], "p_shared_one", "session-one", None);
        assert_report(edge, &requests[1], "p_shared_two", "session-two", None);
    }

    fn run_without_python(&self, edge: &Edge) {
        let empty_path = self.base.join("empty-path");
        fs::create_dir_all(&empty_path).unwrap();
        let socket = FakeHookSocket::start(1);
        let socket_path = socket.path().to_path_buf();
        let output = self.invoke_hook(
            &socket_path,
            "p_no_python",
            "no-python-session",
            Some("create"),
            true,
            Some(&empty_path),
        );
        self.assert_hook_success(edge, &output);
        assert!(
            socket.finish().is_empty(),
            "{}: hook reported without Python",
            edge.name
        );
    }

    fn run_with_unavailable_socket(&self, edge: &Edge) {
        let socket_path = self.base.join("unavailable.sock");
        let output = self.invoke_hook(
            &socket_path,
            "p_no_socket",
            "no-socket-session",
            Some("resume"),
            true,
            None,
        );
        self.assert_hook_success(edge, &output);
    }
}

impl Drop for LifecycleHarness {
    fn drop(&mut self) {
        cleanup_test_base(&self.base);
    }
}

fn session_start_commands(content: &str) -> Vec<String> {
    let document: toml::Value = toml::from_str(content).unwrap();
    let Some(value) = document
        .get("hooks")
        .and_then(|hooks| hooks.get("session_start"))
    else {
        return Vec::new();
    };
    match value {
        toml::Value::String(command) => vec![command.clone()],
        toml::Value::Array(commands) => commands
            .iter()
            .map(|command| command.as_str().unwrap().to_string())
            .collect(),
        other => panic!("unexpected session_start value: {other:?}"),
    }
}

fn assert_report(
    edge: &Edge,
    request: &serde_json::Value,
    pane_id: &str,
    session_id: &str,
    start_source: Option<&str>,
) {
    assert_eq!(
        request["method"], "pane.report_agent_session",
        "{}",
        edge.name
    );
    assert_eq!(request["params"]["source"], "herdr:jcode", "{}", edge.name);
    assert_eq!(request["params"]["agent"], "jcode", "{}", edge.name);
    assert_eq!(request["params"]["pane_id"], pane_id, "{}", edge.name);
    assert_eq!(
        request["params"]["agent_session_id"], session_id,
        "{}",
        edge.name
    );
    match start_source {
        Some(source) => assert_eq!(
            request["params"]["session_start_source"], source,
            "{}",
            edge.name
        ),
        None => assert!(
            request["params"].get("session_start_source").is_none(),
            "{}",
            edge.name
        ),
    }
    assert!(request["params"].get("state").is_none(), "{}", edge.name);
}

#[test]
fn jcode_lifecycle_state_space_graph() {
    for (edge_index, edge) in GRAPH.iter().enumerate() {
        let harness = LifecycleHarness::new(edge_index);
        harness.materialize(edge.from, edge);
        harness.assert_state(edge.from, edge, "source");
        harness.apply(edge);
        harness.assert_state(edge.to, edge, "target");
    }
}
