//! Agent state detection via terminal tail pattern matching.
//!
//! Each pane's live bottom-of-buffer text is read periodically and matched
//! against known agent output patterns to determine state.

use crate::agents::AgentRegistry;

pub mod manifest;
pub mod manifest_update;

/// The detected state of a terminal pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    /// Agent finished, prompt visible, nothing happening.
    Idle,
    /// Agent is actively working/processing.
    Working,
    /// Agent needs human input and is blocked on a response.
    Blocked,
    /// Plain shell or unrecognized program.
    Unknown,
}

/// Screen-derived agent state plus confidence metadata used for source arbitration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentDetection {
    pub state: AgentState,
    /// True when the current screen is an agent-owned viewer that shows
    /// transcript/history instead of the live prompt state.
    pub skip_state_update: bool,
    /// True when the current screen visibly shows live idle chrome.
    pub visible_idle: bool,
    /// Idle evidence from terminal cells, never an OSC title or progress report.
    pub screen_visible_idle: bool,
    /// True when the current screen visibly shows live UI chrome that needs
    /// human input. This is stronger than arbitrary prompt-like text in the
    /// scrollback and may override a non-blocked integration state.
    pub visible_blocker: bool,
    /// True when the current screen visibly shows live working chrome. PTY
    /// activity is the normal working authority; this remains diagnostic
    /// metadata and for non-PTY fallback paths.
    pub visible_working: bool,
}

/// Which agent we detected running in a pane.
pub use crate::agents::id::AgentId as Agent;

pub fn agent_label(agent: &Agent) -> &str {
    agent.as_str()
}

#[cfg(test)]
pub fn interactive_agent_executable(agent: Agent) -> String {
    crate::agents::registry()
        .profile_by_agent(agent)
        .map(|profile| profile.launch().executable().to_owned())
        .unwrap_or_default()
}

pub fn parse_agent_label(agent: &str) -> Option<Agent> {
    let registry = crate::agents::registry();
    let name = normalized_agent_lookup_name(agent);
    let name = path_basename(&name);
    registry
        .profile_by_normalized_alias(name)
        .or_else(|| registry.profile_by_versioned_process_name(name))
        .map(|profile| profile.legacy_agent())
}

#[cfg(test)]
pub(crate) fn parse_canonical_agent_label(label: &str) -> Option<Agent> {
    crate::agents::registry()
        .profile_by_id(label)
        .map(|profile| profile.legacy_agent())
}

/// Identify which agent is running from the process name.
/// Returns `None` for plain shells or unrecognized programs.
#[cfg(test)]
pub fn identify_agent(process_name: &str) -> Option<Agent> {
    let registry = crate::agents::registry();
    identify_agent_with_registry(&registry, process_name)
}

/// Identify a process using only the caller's pinned registry.
pub fn identify_agent_with_registry(registry: &AgentRegistry, process_name: &str) -> Option<Agent> {
    let name = normalized_agent_lookup_name(process_name);
    let name = path_basename(&name);
    registry
        .profile_by_normalized_process_name(name)
        .map(|profile| profile.legacy_agent())
}

pub fn identify_agent_in_job(job: &crate::platform::ForegroundJob) -> Option<(Agent, String)> {
    let registry = crate::agents::registry();
    identify_agent_in_job_with_registry(&registry, job)
}

/// Recognize every candidate in a foreground job against the same snapshot.
pub fn identify_agent_in_job_with_registry(
    registry: &AgentRegistry,
    job: &crate::platform::ForegroundJob,
) -> Option<(Agent, String)> {
    let recognizer = ProcessRecognizer { registry };
    if let Some(process) = job
        .processes
        .iter()
        .find(|process| process.pid == job.process_group_id)
    {
        if let Some(candidate) = recognizer.normalized_process_name(process) {
            return Some(candidate);
        }
    }

    let mut best: Option<(u8, Agent, String)> = None;

    // Preserve the leader-first, single best-candidate pass without collecting
    // profiles or allocating registry state inside the process/path loops.
    for process in &job.processes {
        let Some((agent, candidate)) = recognizer.normalized_process_name(process) else {
            continue;
        };
        let score = process_priority(process, &candidate);

        match &best {
            Some((best_score, _, _)) if *best_score >= score => {}
            _ => best = Some((score, agent, candidate)),
        }
    }

    best.map(|(_, agent, name)| (agent, name))
}

/// Detect the state of an agent from the live terminal tail snapshot.
/// If `agent` is `None`, returns `Unknown`.
#[cfg(test)]
pub fn detect_state(agent: Option<Agent>, screen_content: &str) -> AgentState {
    detect_agent(agent, screen_content).state
}

/// Detect state and whether a visible blocker is present on the current screen.
#[allow(dead_code)] // shim for existing callers; detect_agent_with_osc is the real path
pub fn detect_agent(agent: Option<Agent>, screen_content: &str) -> AgentDetection {
    detect_agent_with_osc(agent, screen_content, "", "")
}

/// Detect state using screen content plus OSC title/progress strings.
pub fn detect_agent_with_osc(
    agent: Option<Agent>,
    screen_content: &str,
    osc_title: &str,
    osc_progress: &str,
) -> AgentDetection {
    let Some(agent) = agent else {
        return AgentDetection {
            state: AgentState::Unknown,
            skip_state_update: false,
            visible_idle: false,
            screen_visible_idle: false,
            visible_blocker: false,
            visible_working: false,
        };
    };
    manifest::detect_with_osc(
        agent,
        manifest::DetectionInput {
            screen: screen_content,
            osc_title,
            osc_progress,
        },
    )
}

pub(crate) fn full_lifecycle_hook_authority(source: &str, agent_label: &str) -> bool {
    crate::agents::registry().has_full_lifecycle_report_authority(source, agent_label)
}

pub(crate) fn session_identity_only_integration(source: &str, agent_label: &str) -> bool {
    crate::agents::registry().is_session_identity_only_integration(source, agent_label)
}

// ---------------------------------------------------------------------------
// Process identification (platform-specific)
// ---------------------------------------------------------------------------

/// Get the foreground job for a given child PID.
/// Delegates to platform-specific implementation.
pub fn foreground_job(child_pid: u32) -> Option<crate::platform::ForegroundJob> {
    crate::platform::foreground_job(child_pid)
}

/// Get the foreground process group leader as a one-process job.
/// This is cheaper than collecting every process in the foreground job.
pub fn foreground_group_leader_job(
    process_group_id: u32,
) -> Option<crate::platform::ForegroundJob> {
    crate::platform::foreground_group_leader_job(process_group_id)
}

struct ProcessRecognizer<'a> {
    registry: &'a AgentRegistry,
}

impl ProcessRecognizer<'_> {
    fn normalized_process_name(
        &self,
        process: &crate::platform::ForegroundProcess,
    ) -> Option<(Agent, String)> {
        let effective = process.argv0.as_deref().unwrap_or(&process.name);
        let lower_effective = effective.to_lowercase();

        if is_generic_runtime_or_shell(&lower_effective) {
            if let Some(wrapped_agent) =
                self.wrapped_agent_name_from_runtime_argv(&lower_effective, process.argv.as_deref())
            {
                return self.canonical_candidate(wrapped_agent);
            }
        }

        if let Some(agent) = identify_agent_with_registry(self.registry, effective) {
            return Some((agent, effective.to_string()));
        }

        if let Some(runtime) = process.argv.as_deref().and_then(|argv| argv.first()) {
            let runtime_name = normalized_agent_lookup_name(path_basename(runtime));
            if matches!(runtime_name.as_str(), "node" | "bun") {
                if let Some(wrapped_agent) =
                    self.wrapped_agent_name_from_runtime_argv(runtime, process.argv.as_deref())
                {
                    if self
                        .registry
                        .profile_by_id(&wrapped_agent)
                        .and_then(|profile| profile.process())
                        .is_some_and(|profile| profile.uses_secondary_runtime_argv_fallback())
                    {
                        return self.canonical_candidate(wrapped_agent);
                    }
                }
            }
        }

        self.argv0_agent_name(process.argv.as_deref())
            .or_else(|| {
                self.cmdline_argv0_agent_name(process.cmdline.as_deref().unwrap_or_default())
            })
            .and_then(|name| self.canonical_candidate(name))
    }

    // Path/runtime matchers return canonical IDs, not process names. Carry the
    // resolved identity forward so a novel ID need not also be a process alias.
    fn canonical_candidate(&self, name: String) -> Option<(Agent, String)> {
        let agent = self.registry.profile_by_id(&name)?.legacy_agent();
        Some((agent, name))
    }

    fn wrapped_agent_name_from_runtime_argv(
        &self,
        runtime: &str,
        argv: Option<&[String]>,
    ) -> Option<String> {
        let argv = argv?;
        let runtime_name = normalized_agent_lookup_name(path_basename(runtime));

        match runtime_name.as_str() {
            "node" => self.bundled_node_agent_name_from_argv(argv).or_else(|| {
                self.script_arg_agent_name(argv, &["-e", "--eval", "-p", "--print"], &[])
            }),
            "bun" => self.script_arg_agent_name(argv, &["-e", "--eval", "-p", "--print"], &[]),
            name if is_python_runtime(name) => self.script_arg_agent_name(argv, &["-c"], &["-m"]),
            "sh" | "bash" | "zsh" | "fish" => self.script_arg_agent_name(argv, &["-c"], &[]),
            "cmd" => self.windows_cmd_arg_agent_name(argv),
            "powershell" | "pwsh" => self.powershell_arg_agent_name(argv),
            "tmux" => None,
            _ => None,
        }
    }

    fn bundled_node_agent_name_from_argv(&self, argv: &[String]) -> Option<String> {
        let (runtime_parent, runtime_name) = path_parent_and_basename(argv.first()?)?;
        let (script_parent, script_name) = path_parent_and_basename(argv.get(1)?)?;
        if !runtime_parent.eq_ignore_ascii_case(script_parent) {
            return None;
        }

        for profile in self.registry.process_profiles_with_bundled_node_layout() {
            let layout = profile.process()?.bundled_node_layout()?;
            if !runtime_name.eq_ignore_ascii_case(layout.runtime_basename())
                || !script_name.eq_ignore_ascii_case(layout.entrypoint_basename())
            {
                continue;
            }

            let mut tail = runtime_parent
                .rsplit(['/', '\\'])
                .filter(|component| !component.is_empty());
            let (Some(version), Some(versions), Some(package)) =
                (tail.next(), tail.next(), tail.next())
            else {
                continue;
            };
            if package.eq_ignore_ascii_case(layout.package_directory())
                && versions.eq_ignore_ascii_case(layout.versions_directory())
                && !version.trim().is_empty()
            {
                return Some(profile.canonical_id().to_string());
            }
        }

        None
    }

    fn windows_cmd_arg_agent_name(&self, argv: &[String]) -> Option<String> {
        let mut args = argv.iter().skip(1);
        while let Some(arg) = args.next() {
            let flag = arg.trim_matches('"').to_lowercase();
            match flag.as_str() {
                "/c" | "/k" => {
                    return args
                        .next()
                        .and_then(|command| self.command_text_agent_name(command))
                }
                "/d" | "/s" | "/q" | "/a" | "/u" | "/e:on" | "/e:off" | "/f:on" | "/f:off"
                | "/v:on" | "/v:off" => continue,
                _ => {}
            }
        }
        None
    }

    fn powershell_arg_agent_name(&self, argv: &[String]) -> Option<String> {
        let mut args = argv.iter().skip(1);
        while let Some(arg) = args.next() {
            let flag = arg.trim_matches('"').to_lowercase();
            match flag.as_str() {
                "-file" | "-f" | "/file" => {
                    return args
                        .next()
                        .and_then(|path| self.agent_name_from_path_token(path));
                }
                "-command" | "-c" | "/command" | "/c" => {
                    return args
                        .next()
                        .and_then(|command| self.command_text_agent_name(command));
                }
                "-encodedcommand" | "-enc" | "/encodedcommand" | "/enc" => return None,
                "-configurationname" | "-executionpolicy" | "-outputformat" | "-psconsolefile"
                | "-version" | "-windowstyle" | "-workingdirectory" => {
                    let _ = args.next();
                }
                _ if flag.starts_with('-') || flag.starts_with('/') => {}
                _ => return self.agent_name_from_path_token(arg),
            }
        }
        None
    }

    fn command_text_agent_name(&self, command: &str) -> Option<String> {
        let mut rest = command;
        while let Some((token, next)) = command_text_token(rest) {
            let token = token.trim();
            if token.eq_ignore_ascii_case("&")
                || token.eq_ignore_ascii_case(".")
                || token.eq_ignore_ascii_case("call")
            {
                rest = next;
                continue;
            }
            return self.agent_name_from_path_token(token);
        }
        None
    }

    fn script_arg_agent_name(
        &self,
        argv: &[String],
        eval_flags: &[&str],
        module_flags: &[&str],
    ) -> Option<String> {
        let mut args = argv.iter().skip(1);
        while let Some(arg) = args.next() {
            if arg == "--" {
                return args
                    .next()
                    .and_then(|token| self.agent_name_from_path_token(token));
            }

            if flag_matches(arg, eval_flags) || flag_matches(arg, module_flags) {
                return None;
            }

            if arg.starts_with('-') {
                if option_takes_value(arg) {
                    let _ = args.next();
                }
                continue;
            }

            return self.agent_name_from_path_token(arg);
        }

        None
    }

    fn argv0_agent_name(&self, argv: Option<&[String]>) -> Option<String> {
        self.agent_name_from_path_token(argv?.first()?)
    }

    fn cmdline_argv0_agent_name(&self, cmdline: &str) -> Option<String> {
        self.agent_name_from_path_token(cmdline.split_whitespace().next()?)
    }

    fn agent_name_from_path_token(&self, token: &str) -> Option<String> {
        let trimmed = token.trim_matches(|c| matches!(c, '"' | '\''));
        if trimmed.is_empty() || trimmed.starts_with('-') {
            return None;
        }

        self.agent_name_from_basename(path_basename(trimmed))
            .or_else(|| self.agent_name_from_known_package_path(trimmed))
            .or_else(|| self.resolved_agent_name_from_path_token(trimmed))
    }

    fn agent_name_from_known_package_path(&self, path: &str) -> Option<String> {
        use crate::agents::process::KnownPackageMatch;

        let raw_components: Vec<&str> = path
            .split(['/', '\\'])
            .filter(|component| !component.is_empty())
            .collect();
        let ends_with = |suffix: &[String]| {
            raw_components.len() >= suffix.len()
                && raw_components[raw_components.len() - suffix.len()..]
                    .iter()
                    .zip(suffix)
                    .all(|(actual, expected)| actual.eq_ignore_ascii_case(expected))
        };
        // Exact suffixes take precedence over the legacy normalized layout search.
        for profile in self.registry.process_profiles_with_package_layouts() {
            for layout in profile.process()?.known_package_layouts() {
                if layout.match_kind() == KnownPackageMatch::ExactSuffix
                    && ends_with(layout.components())
                {
                    return Some(profile.canonical_id().to_string());
                }
            }
        }

        let components: Vec<String> = raw_components
            .into_iter()
            .map(normalized_agent_lookup_name)
            .collect();

        let mut best_match: Option<((usize, usize, usize), &crate::agents::AgentProfile)> = None;
        for (profile_priority, profile) in self
            .registry
            .process_profiles_with_package_layouts()
            .enumerate()
        {
            let process_profile = profile.process()?;
            for layout in process_profile.known_package_layouts() {
                if layout.match_kind() != KnownPackageMatch::NormalizedComponents {
                    continue;
                }
                let expected = layout.components();
                let Some(component_position) = components
                    .windows(expected.len())
                    .position(|window| window == expected)
                else {
                    continue;
                };
                let rank = (expected.len(), component_position, profile_priority);
                let replace = match &best_match {
                    None => true,
                    Some((best_rank, _)) => {
                        rank.0 > best_rank.0
                            || (rank.0 == best_rank.0 && rank.1 < best_rank.1)
                            || (rank.0 == best_rank.0
                                && rank.1 == best_rank.1
                                && rank.2 < best_rank.2)
                    }
                };
                if replace {
                    best_match = Some((rank, profile));
                }
            }
        }

        best_match.map(|(_, profile)| profile.canonical_id().to_string())
    }

    fn resolved_agent_name_from_path_token(&self, token: &str) -> Option<String> {
        let path = std::path::Path::new(token);
        if path.components().count() < 2 {
            return None;
        }

        let resolved = std::fs::canonicalize(path).ok()?;
        let basename = resolved.file_name()?.to_str()?;
        self.agent_name_from_basename(basename)
    }

    fn agent_name_from_basename(&self, basename: &str) -> Option<String> {
        let agent = identify_agent_with_registry(self.registry, basename)?;
        Some(agent_label(&agent).to_string())
    }
}

fn path_parent_and_basename(path: &str) -> Option<(&str, &str)> {
    let split = path.rfind(['/', '\\'])?;
    let parent = path[..split].trim_end_matches(['/', '\\']);
    let basename = &path[split + 1..];
    (!parent.is_empty() && !basename.is_empty()).then_some((parent, basename))
}

fn command_text_token(input: &str) -> Option<(&str, &str)> {
    let input = input.trim_start();
    let first = input.chars().next()?;
    if first == '"' || first == '\'' {
        let start = first.len_utf8();
        if let Some(end) = input[start..].find(first) {
            let end = start + end;
            return Some((&input[start..end], &input[end + first.len_utf8()..]));
        }
        return Some((&input[start..], ""));
    }

    let end = input.find(char::is_whitespace).unwrap_or(input.len());
    Some((&input[..end], &input[end..]))
}

fn flag_matches(arg: &str, flags: &[&str]) -> bool {
    flags
        .iter()
        .any(|flag| arg == *flag || short_flag_payload(arg, flag) || long_flag_value(arg, flag))
}

fn short_flag_payload(arg: &str, flag: &str) -> bool {
    flag.starts_with('-')
        && !flag.starts_with("--")
        && arg.starts_with(flag)
        && arg.len() > flag.len()
}

fn long_flag_value(arg: &str, flag: &str) -> bool {
    flag.starts_with("--")
        && arg
            .strip_prefix(flag)
            .is_some_and(|rest| rest.starts_with('='))
}

fn option_takes_value(arg: &str) -> bool {
    matches!(
        arg,
        "-r" | "--require"
            | "--loader"
            | "--import"
            | "--experimental-loader"
            | "--inspect-port"
            | "-W"
            | "-X"
            | "-S"
            | "-L"
            | "-o"
    )
}

fn normalized_agent_lookup_name(name: &str) -> String {
    let mut name = name.trim().to_lowercase();
    for suffix in [".exe", ".cmd", ".bat", ".ps1", ".js"] {
        if name.ends_with(suffix) {
            name.truncate(name.len() - suffix.len());
            break;
        }
    }
    name
}

fn path_basename(path: &str) -> &str {
    path.rsplit(['/', '\\'])
        .find(|component| !component.is_empty())
        .unwrap_or(path)
}

fn process_priority(process: &crate::platform::ForegroundProcess, normalized_name: &str) -> u8 {
    let lower_name = normalized_name.to_lowercase();
    if lower_name != process.name.to_lowercase() {
        return 3;
    }
    if !is_generic_runtime_or_shell(&lower_name) {
        return 2;
    }
    1
}

fn is_generic_runtime_or_shell(name: &str) -> bool {
    let name = normalized_agent_lookup_name(path_basename(name));
    is_python_runtime(&name)
        || matches!(
            name.as_str(),
            "sh" | "bash"
                | "zsh"
                | "fish"
                | "tmux"
                | "node"
                | "bun"
                | "cmd"
                | "powershell"
                | "pwsh"
        )
}

fn is_python_runtime(name: &str) -> bool {
    name == "python"
        || name.strip_prefix("python").is_some_and(|version| {
            !version.is_empty()
                && version
                    .split('.')
                    .all(|part| !part.is_empty() && part.chars().all(|ch| ch.is_ascii_digit()))
        })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn foreground_process(
        pid: u32,
        name: &str,
        argv: &[&str],
    ) -> crate::platform::ForegroundProcess {
        crate::platform::ForegroundProcess {
            pid,
            name: name.to_string(),
            argv0: None,
            argv: Some(argv.iter().map(|arg| (*arg).to_string()).collect()),
            cmdline: Some(argv.join(" ")),
        }
    }

    #[cfg(unix)]
    fn temp_detection_path(name: &str) -> std::path::PathBuf {
        let unique = format!(
            "herdr-detect-tests-{}-{}-{}",
            name,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time should be after unix epoch")
                .as_nanos()
        );
        std::env::temp_dir().join(unique)
    }

    #[test]
    fn moved_agent_detection_routes_through_production_dispatch() {
        let detection = detect_agent(Some(Agent::Pi), "Working...");

        assert_eq!(detection.state, AgentState::Working);
        assert!(detection.visible_working);
    }

    // ---- Agent identification ----

    fn novel_process_registry() -> AgentRegistry {
        let packages = crate::agents::source::load_packages(&[
            (
                "agents/opencode-lab/agent.toml",
                r#"schema = 1
id = "opencode-lab"
name = "OpenCode Lab"
aliases = ["lab-alias"]
startable = true
[launch]
unix = "opencode"
windows = "opencode.exe"
"#,
            ),
            (
                "agents/opencode-lab/process.toml",
                r#"names = ["opencode"]
secondary_runtime_argv_fallback = true
[[package_paths]]
kind = "exact_suffix"
components = ["node_modules", "opencode-lab", "dist", "cli.js"]
[[package_paths]]
kind = "normalized_components"
components = ["node_modules", "opencode-lab", "dist", "main"]
[bundled_node]
runtime_basename = "node.exe"
entrypoint_basename = "index.js"
package_directory = "opencode-lab"
versions_directory = "versions"
"#,
            ),
        ])
        .expect("validated novel package");
        AgentRegistry::from_packages(packages).expect("owned registry")
    }

    #[test]
    fn pinned_process_recognition_uses_novel_process_names_not_identity_aliases() {
        let registry = novel_process_registry();
        let agent = Agent::parse("opencode-lab").unwrap();
        for name in ["opencode", "/usr/bin/OpenCode", r"C:\bin\opencode.exe"] {
            assert_eq!(identify_agent_with_registry(&registry, name), Some(agent));
        }
        for name in ["opencode-lab", "lab-alias", "claude", "opencode-helper"] {
            assert_eq!(identify_agent_with_registry(&registry, name), None);
            let job = crate::platform::ForegroundJob {
                process_group_id: 123,
                processes: vec![foreground_process(123, name, &[name])],
            };
            assert_eq!(identify_agent_in_job_with_registry(&registry, &job), None);
        }
        let empty = AgentRegistry::from_packages(Vec::new()).unwrap();
        assert_eq!(identify_agent_with_registry(&empty, "opencode"), None);
        assert_eq!(
            identify_agent_with_registry(&registry, "opencode"),
            Some(agent)
        );
        assert!(!registry.has_full_lifecycle_report_authority("herdr:opencode-lab", "opencode-lab"));
        assert!(
            !registry.is_session_identity_only_integration("herdr:opencode-lab", "opencode-lab")
        );
        assert!(registry
            .profile_by_id("opencode-lab")
            .unwrap()
            .integration()
            .is_none());
    }

    #[test]
    fn pinned_job_recognition_preserves_novel_identity_through_runtime_and_paths() {
        let registry = novel_process_registry();
        let recognizer = ProcessRecognizer {
            registry: &registry,
        };
        let agent = Agent::parse("opencode-lab").unwrap();
        assert_eq!(
            recognizer.agent_name_from_basename("opencode.exe"),
            Some("opencode-lab".into())
        );
        for path in [
            "/opt/node_modules/opencode-lab/dist/cli.js",
            r"C:\opt\node_modules\opencode-lab\dist\main.js",
        ] {
            assert_eq!(
                recognizer.agent_name_from_known_package_path(path),
                Some("opencode-lab".into())
            );
        }
        for (name, argv) in [
            ("opencode", vec!["opencode"]),
            ("node", vec!["node", "/bin/opencode"]),
            ("bun", vec!["bun", "/bin/opencode"]),
            ("python3", vec!["python3", "/bin/opencode"]),
            ("bash", vec!["bash", "/bin/opencode"]),
            ("cmd.exe", vec!["cmd.exe", "/c", "opencode"]),
            ("pwsh", vec!["pwsh", "-file", "opencode.ps1"]),
            ("MainThread", vec!["node", "/bin/opencode"]),
            (
                "node",
                vec!["node", "/opt/node_modules/opencode-lab/dist/cli.js"],
            ),
            (
                "node",
                vec!["node", "/opt/node_modules/opencode-lab/dist/main.js"],
            ),
            (
                "node.exe",
                vec![
                    "/opt/opencode-lab/versions/1/node.exe",
                    "/opt/opencode-lab/versions/1/index.js",
                ],
            ),
        ] {
            for pid in [123, 124] {
                let job = crate::platform::ForegroundJob {
                    process_group_id: 123,
                    processes: vec![foreground_process(pid, name, &argv)],
                };
                let expected_name = if name == "opencode" {
                    "opencode"
                } else {
                    "opencode-lab"
                };
                assert_eq!(
                    identify_agent_in_job_with_registry(&registry, &job),
                    Some((agent, expected_name.to_string())),
                    "{name} {argv:?} pid={pid}",
                );
            }
        }
    }

    #[test]
    fn identify_known_agents() {
        assert_eq!(identify_agent("pi"), Some(Agent::Pi));
        assert_eq!(identify_agent("claude"), Some(Agent::Claude));
        assert_eq!(identify_agent("claude-code"), Some(Agent::Claude));
        assert_eq!(identify_agent("codex"), Some(Agent::Codex));
        assert_eq!(identify_agent("gemini"), Some(Agent::Gemini));
        assert_eq!(identify_agent("cursor"), Some(Agent::Cursor));
        assert_eq!(identify_agent("cursor-agent"), Some(Agent::Cursor));
        assert_eq!(identify_agent("devin"), Some(Agent::Devin));
        assert_eq!(identify_agent("devin-cli"), Some(Agent::Devin));
        assert_eq!(identify_agent("agy"), Some(Agent::Antigravity));
        assert_eq!(identify_agent("antigravity-cli"), Some(Agent::Antigravity));
        assert_eq!(identify_agent("cline"), Some(Agent::Cline));
        assert_eq!(identify_agent("omp"), Some(Agent::Omp));
        assert_eq!(identify_agent("mastracode"), Some(Agent::Mastracode));
        assert_eq!(identify_agent("mastra-code"), Some(Agent::Mastracode));
        assert_eq!(identify_agent("opencode"), Some(Agent::OpenCode));
        assert_eq!(identify_agent("opencode.exe"), Some(Agent::OpenCode));
        assert_eq!(identify_agent("opencode2"), Some(Agent::OpenCode));
        assert_eq!(identify_agent("opencode2.exe"), Some(Agent::OpenCode));
        assert_eq!(identify_agent("kimi"), Some(Agent::Kimi));
        assert_eq!(identify_agent("Kimi Code"), Some(Agent::Kimi));
        assert_eq!(identify_agent("kiro"), Some(Agent::Kiro));
        assert_eq!(identify_agent("kiro-cli"), Some(Agent::Kiro));
        assert_eq!(identify_agent("copilot"), Some(Agent::GithubCopilot));
        assert_eq!(identify_agent("ghcs"), Some(Agent::GithubCopilot));
        assert_eq!(identify_agent("grok"), Some(Agent::Grok));
        assert_eq!(identify_agent("grok-build"), Some(Agent::Grok));
        assert_eq!(identify_agent("hermes"), Some(Agent::Hermes));
        assert_eq!(identify_agent("hermes-agent"), Some(Agent::Hermes));
        assert_eq!(identify_agent("kilo"), Some(Agent::Kilo));
        assert_eq!(identify_agent("kilo-code"), Some(Agent::Kilo));
        assert_eq!(identify_agent("qwen"), Some(Agent::Qwen));
        assert_eq!(identify_agent("Qwen Code"), Some(Agent::Qwen));
        assert_eq!(identify_agent("maki"), Some(Agent::Maki));
        assert_eq!(identify_agent("muse"), Some(Agent::Muse));
        assert_eq!(identify_agent("muse-code"), Some(Agent::Muse));
        assert_eq!(identify_agent("muse-cli"), Some(Agent::Muse));
        assert_eq!(identify_agent("muse-bin-0.1.0-R708.1"), Some(Agent::Muse));
        assert_eq!(identify_agent("muse-bin-1.2.3"), Some(Agent::Muse));
        assert_eq!(
            identify_agent("/home/user/.local/bin/muse-bin-0.2.1-R1215.1"),
            Some(Agent::Muse)
        );
        assert_eq!(
            identify_agent(r"C:\Users\user\muse-bin-0.2.1-R1215.1.exe"),
            Some(Agent::Muse)
        );
    }

    #[test]
    fn parse_known_agent_labels() {
        assert_eq!(parse_agent_label("pi"), Some(Agent::Pi));
        assert_eq!(parse_agent_label("claude"), Some(Agent::Claude));
        assert_eq!(parse_agent_label("cursor-agent"), Some(Agent::Cursor));
        assert_eq!(parse_agent_label("devin-cli"), Some(Agent::Devin));
        assert_eq!(parse_agent_label("agy"), Some(Agent::Antigravity));
        assert_eq!(parse_agent_label("antigravity"), Some(Agent::Antigravity));
        assert_eq!(parse_agent_label("omp"), Some(Agent::Omp));
        assert_eq!(parse_agent_label("mastracode"), Some(Agent::Mastracode));
        assert_eq!(parse_agent_label("mastra code"), Some(Agent::Mastracode));
        assert_eq!(parse_agent_label("opencode.exe"), Some(Agent::OpenCode));
        assert_eq!(parse_agent_label("copilot"), Some(Agent::GithubCopilot));
        assert_eq!(parse_agent_label("kimi-code"), Some(Agent::Kimi));
        assert_eq!(
            parse_agent_label("github-copilot"),
            Some(Agent::GithubCopilot)
        );
        assert_eq!(parse_agent_label("amp-local"), Some(Agent::Amp));
        assert_eq!(parse_agent_label("kiro-cli"), Some(Agent::Kiro));
        assert_eq!(parse_agent_label("grok-build"), Some(Agent::Grok));
        assert_eq!(parse_agent_label("hermes-agent"), Some(Agent::Hermes));
        assert_eq!(parse_agent_label("qwen-code"), Some(Agent::Qwen));
        assert_eq!(parse_agent_label("maki"), Some(Agent::Maki));
        assert_eq!(parse_agent_label("kilo-code"), Some(Agent::Kilo));
    }

    #[test]
    fn every_agent_label_round_trips_through_canonical_and_alias_parsers() {
        for agent in Agent::ALL {
            let label = agent_label(&agent);
            assert_eq!(parse_canonical_agent_label(label), Some(agent));
            assert_eq!(parse_agent_label(label), Some(agent));
        }
    }

    #[test]
    fn every_agent_has_a_canonical_interactive_executable() {
        let expected = [
            (Agent::Pi, "pi"),
            (Agent::Claude, "claude"),
            (Agent::Codex, "codex"),
            (Agent::Gemini, "gemini"),
            (
                Agent::Cursor,
                if cfg!(windows) {
                    "cursor-agent.cmd"
                } else {
                    "cursor-agent"
                },
            ),
            (Agent::Devin, "devin"),
            (Agent::Antigravity, "agy"),
            (Agent::Cline, "cline"),
            (Agent::Omp, "omp"),
            (Agent::Mastracode, "mastracode"),
            (Agent::OpenCode, "opencode"),
            (Agent::GithubCopilot, "copilot"),
            (Agent::Kimi, "kimi"),
            (Agent::Kiro, "kiro-cli"),
            (Agent::Droid, "droid"),
            (Agent::Amp, "amp"),
            (Agent::Grok, "grok"),
            (Agent::Hermes, "hermes"),
            (Agent::Kilo, "kilo"),
            (Agent::Qodercli, "qodercli"),
            (Agent::Qwen, "qwen"),
            (Agent::Maki, "maki"),
            (Agent::Muse, "muse"),
        ];
        assert_eq!(expected.len(), Agent::ALL.len());
        for (agent, executable) in expected {
            assert_eq!(interactive_agent_executable(agent), executable);
        }
    }

    #[test]
    fn canonical_agent_labels_are_strict() {
        assert_eq!(parse_canonical_agent_label("claude-code"), None);
        assert_eq!(parse_canonical_agent_label("Pi"), None);
        assert_eq!(parse_canonical_agent_label(" pi "), None);
        assert_eq!(parse_canonical_agent_label("opencode.exe"), None);
    }

    #[test]
    fn mastracode_is_hook_authority_without_screen_manifest() {
        assert!(full_lifecycle_hook_authority(
            "herdr:mastracode",
            "mastracode"
        ));
        assert!(!crate::agents::registry()
            .screen_detectable_profiles()
            .any(|profile| profile.legacy_agent() == Agent::Mastracode));
    }

    #[test]
    fn session_identity_integrations_leave_state_to_screen_detection() {
        for (source, label, agent) in [
            ("herdr:hermes", "hermes", Agent::Hermes),
            ("herdr:qwen", "qwen", Agent::Qwen),
            ("herdr:antigravity_cli", "agy", Agent::Antigravity),
        ] {
            assert!(!full_lifecycle_hook_authority(source, label));
            assert!(session_identity_only_integration(source, label));
            assert!(crate::agents::registry()
                .screen_detectable_profiles()
                .any(|profile| profile.legacy_agent() == agent));
        }
    }

    #[test]
    fn identify_unknown_processes() {
        assert_eq!(identify_agent("bash"), None);
        assert_eq!(identify_agent("zsh"), None);
        assert_eq!(identify_agent("vim"), None);
        assert_eq!(identify_agent("node"), None);
        assert_eq!(identify_agent("museum"), None);
        assert_eq!(identify_agent("muse-helper"), None);
        assert_eq!(identify_agent("muser"), None);
        assert_eq!(identify_agent("musescore"), None);
        assert_eq!(identify_agent("muse-bin"), None);
        assert_eq!(identify_agent("muse-bin-"), None);
        assert_eq!(identify_agent("muse-binary"), None);
    }

    #[test]
    fn identify_case_insensitive() {
        assert_eq!(identify_agent("Pi"), Some(Agent::Pi));
        assert_eq!(identify_agent("CLAUDE"), Some(Agent::Claude));
        assert_eq!(identify_agent("Codex"), Some(Agent::Codex));
        assert_eq!(identify_agent("Devin"), Some(Agent::Devin));
    }

    #[test]
    fn identify_agent_in_job_preserves_absolute_argv0_recognition() {
        for (path, expected) in [
            ("/usr/bin/claude", Agent::Claude),
            (r"C:\Users\user\bin\claude.exe", Agent::Claude),
            ("/home/user/.local/bin/muse-bin-1.2.3", Agent::Muse),
            (r"C:\Users\user\bin\muse-bin-1.2.3.exe", Agent::Muse),
        ] {
            for pid in [123, 124] {
                let mut process = foreground_process(pid, "worker", &[path]);
                process.argv0 = Some(path.to_string());
                let job = crate::platform::ForegroundJob {
                    process_group_id: 123,
                    processes: vec![process],
                };
                assert_eq!(
                    identify_agent_in_job(&job),
                    Some((expected, path.to_string())),
                    "{path} pid={pid}"
                );
            }
        }
    }

    #[test]
    fn identify_agent_in_job_prefers_wrapped_codex() {
        let job = crate::platform::ForegroundJob {
            process_group_id: 123,
            processes: vec![
                foreground_process(1, "node", &["node", "/path/to/bin/codex"]),
                foreground_process(2, "bash", &["bash"]),
            ],
        };

        assert_eq!(
            identify_agent_in_job(&job),
            Some((Agent::Codex, "codex".to_string()))
        );
    }

    #[test]
    fn identify_agent_in_job_detects_node_wrapped_qwen() {
        for argv in [
            vec!["node", "/home/user/.fnm/bin/qwen"],
            vec![
                "node.exe",
                r"C:\Users\user\AppData\Roaming\npm\node_modules\@qwen-code\qwen-code\dist\index.js",
            ],
        ] {
            let job = crate::platform::ForegroundJob {
                process_group_id: 123,
                processes: vec![foreground_process(123, "MainThread", &argv)],
            };

            assert_eq!(
                identify_agent_in_job(&job),
                Some((Agent::Qwen, "qwen".to_string()))
            );
        }
    }

    #[test]
    fn identify_agent_in_job_detects_cline_native_binaries() {
        for (name, executable) in [
            (
                ".cline",
                "/home/user/.npm/lib/node_modules/cline/bin/.cline",
            ),
            (
                "cline",
                "/usr/local/lib/node_modules/@cline/cli-darwin-arm64/bin/cline",
            ),
            (
                "cline.exe",
                r"C:\Users\user\AppData\Roaming\npm\node_modules\@cline\cli-windows-x64\bin\cline.exe",
            ),
        ] {
            let job = crate::platform::ForegroundJob {
                process_group_id: 123,
                processes: vec![foreground_process(123, name, &[executable, "--tui"])],
            };

            assert_eq!(
                identify_agent_in_job(&job),
                Some((Agent::Cline, name.to_string()))
            );
        }
    }

    #[test]
    fn identify_agent_in_job_detects_cline_node_wrapper() {
        for (name, argv) in [
            (
                "MainThread",
                vec!["node", "/home/user/.fnm/bin/cline", "--tui"],
            ),
            (
                "node",
                vec!["node", "/usr/local/lib/node_modules/cline/bin/cline"],
            ),
            (
                "node.exe",
                vec![
                    r"C:\Program Files\nodejs\node.exe",
                    r"C:\Users\user\AppData\Roaming\npm\node_modules\cline\bin\cline",
                ],
            ),
        ] {
            let job = crate::platform::ForegroundJob {
                process_group_id: 123,
                processes: vec![foreground_process(123, name, &argv)],
            };

            assert_eq!(
                identify_agent_in_job(&job),
                Some((Agent::Cline, "cline".to_string()))
            );
        }
    }

    #[test]
    fn identify_agent_in_job_rejects_unrelated_cline_mentions() {
        for argv in [
            vec!["node"],
            vec!["node", "/path/to/other.js", "cline"],
            vec!["node", "-e", "cline"],
            vec!["node", "/path/to/cline-helper"],
            vec!["/path/to/.cline-helper"],
            vec!["/path/to/other", "/path/to/cline"],
        ] {
            let job = crate::platform::ForegroundJob {
                process_group_id: 123,
                processes: vec![foreground_process(123, "MainThread", &argv)],
            };

            assert_eq!(identify_agent_in_job(&job), None);
        }
        assert_eq!(identify_agent("MainThread"), None);
    }

    #[test]
    fn identify_agent_in_job_detects_windows_cursor_install() {
        let job = crate::platform::ForegroundJob {
            process_group_id: 123,
            processes: vec![foreground_process(
                123,
                "node.exe",
                &[
                    r"C:\Users\user\AppData\Local\cursor-agent\versions\2026.08.11-e8db854\node.exe",
                    r"C:\Users\user\AppData\Local\cursor-agent\versions\2026.08.11-e8db854\index.js",
                ],
            )],
        };

        assert_eq!(
            identify_agent_in_job(&job),
            Some((Agent::Cursor, "cursor".to_string()))
        );
    }

    #[test]
    fn identify_agent_in_job_ignores_invalid_windows_cursor_install_paths() {
        for script in [
            r"C:\Users\user\AppData\Local\cursor-agent\versions\2026.08.11-e8db854\scripts\postinstall.js",
            r"C:\Users\user\AppData\Local\cursor-agent\versions\2026.08.11-e8db854\index",
            r"C:\Users\user\AppData\Local\cursor-agent\versions\2026.08.11-e8db854\index.exe",
        ] {
            let job = crate::platform::ForegroundJob {
                process_group_id: 123,
                processes: vec![foreground_process(
                    123,
                    "node.exe",
                    &[
                        r"C:\Users\user\AppData\Local\cursor-agent\versions\2026.08.11-e8db854\node.exe",
                        script,
                    ],
                )],
            };

            assert_eq!(identify_agent_in_job(&job), None, "script: {script}");
        }

        let lookalike = crate::platform::ForegroundJob {
            process_group_id: 123,
            processes: vec![foreground_process(
                123,
                "node.exe",
                &[
                    r"C:\Program Files\nodejs\node.exe",
                    r"C:\workspace\cursor-agent\versions\test\index.js",
                ],
            )],
        };
        assert_eq!(identify_agent_in_job(&lookalike), None);
    }

    #[test]
    fn identify_agent_in_job_prefers_recognized_process_group_leader() {
        let job = crate::platform::ForegroundJob {
            process_group_id: 42,
            processes: vec![
                foreground_process(42, "claude", &["claude"]),
                foreground_process(43, "node", &["node", "/tmp/mcp/bin/codex"]),
            ],
        };

        assert_eq!(
            identify_agent_in_job(&job),
            Some((Agent::Claude, "claude".to_string()))
        );
    }

    #[test]
    fn identify_agent_in_job_falls_back_when_process_group_leader_is_unrecognized() {
        let job = crate::platform::ForegroundJob {
            process_group_id: 42,
            processes: vec![
                foreground_process(42, "bash", &["bash"]),
                foreground_process(43, "node", &["node", "/tmp/mcp/bin/codex"]),
            ],
        };

        assert_eq!(
            identify_agent_in_job(&job),
            Some((Agent::Codex, "codex".to_string()))
        );
    }

    #[test]
    fn identify_agent_in_job_detects_python_version_wrapped_hermes() {
        let job = crate::platform::ForegroundJob {
            process_group_id: 123,
            processes: vec![foreground_process(
                123,
                "python3.12",
                &[
                    "/nix/store/example/bin/python3.12",
                    "/nix/store/example/bin/hermes",
                    "--resume",
                    "session-id",
                ],
            )],
        };

        assert_eq!(
            identify_agent_in_job(&job),
            Some((Agent::Hermes, "hermes".to_string()))
        );
    }

    #[test]
    fn identify_agent_in_job_detects_nix_wrapped_codex_from_cmdline_argv0() {
        let job = crate::platform::ForegroundJob {
            process_group_id: 123,
            processes: vec![foreground_process(
                1,
                ".codex-wrapped",
                &["/etc/profiles/per-user/user/bin/codex", "--model", "gpt-5"],
            )],
        };

        assert_eq!(
            identify_agent_in_job(&job),
            Some((Agent::Codex, "codex".to_string()))
        );
    }

    #[test]
    fn identify_agent_in_job_canonicalizes_nix_wrapped_aliases_from_cmdline_argv0() {
        let job = crate::platform::ForegroundJob {
            process_group_id: 123,
            processes: vec![foreground_process(
                1,
                ".claude-code-wrapped",
                &["/nix/store/example/bin/claude-code"],
            )],
        };

        assert_eq!(
            identify_agent_in_job(&job),
            Some((Agent::Claude, "claude".to_string()))
        );
    }

    #[test]
    fn identify_agent_in_job_detects_shell_wrapped_pi() {
        let job = crate::platform::ForegroundJob {
            process_group_id: 123,
            processes: vec![foreground_process(
                1,
                "sh",
                &["/bin/sh", "/tmp/test-bin/pi"],
            )],
        };

        assert_eq!(
            identify_agent_in_job(&job),
            Some((Agent::Pi, "pi".to_string()))
        );
    }

    #[test]
    fn identify_agent_in_job_detects_bun_wrapped_omp() {
        let job = crate::platform::ForegroundJob {
            process_group_id: 123,
            processes: vec![foreground_process(
                123,
                "bun",
                &["bun", "/home/can/.bun/bin/omp"],
            )],
        };

        assert_eq!(
            identify_agent_in_job(&job),
            Some((Agent::Omp, "omp".to_string()))
        );
    }

    #[test]
    fn identify_agent_in_job_detects_node_wrapped_pi_package_cli() {
        let job = crate::platform::ForegroundJob {
            process_group_id: 123,
            processes: vec![foreground_process(
                123,
                "node.exe",
                &[
                    "node.exe",
                    "C:\\Users\\herdr\\AppData\\Roaming\\npm\\node_modules\\@earendil-works\\pi-coding-agent\\dist\\cli.js",
                ],
            )],
        };

        assert_eq!(
            identify_agent_in_job(&job),
            Some((Agent::Pi, "pi".to_string()))
        );
    }

    #[test]
    fn identify_agent_in_job_detects_node_wrapped_pi_bundled_cli() {
        let job = crate::platform::ForegroundJob {
            process_group_id: 123,
            processes: vec![foreground_process(
                123,
                "node.exe",
                &[
                    r"C:\Users\herdr\AppData\Local\pi-node\current\node.exe",
                    r"C:\Users\herdr\AppData\Local\pi-node\current/node_modules/@earendil-works/pi-coding-agent/dist/bundle/cli.js",
                ],
            )],
        };

        assert_eq!(
            identify_agent_in_job(&job),
            Some((Agent::Pi, "pi".to_string()))
        );
    }

    #[test]
    fn identify_agent_in_job_detects_node_wrapped_mastracode_package_cli() {
        let job = crate::platform::ForegroundJob {
            process_group_id: 123,
            processes: vec![foreground_process(
                123,
                "node.exe",
                &[
                    "node.exe",
                    "C:\\Users\\herdr\\AppData\\Roaming\\npm\\node_modules\\mastracode\\dist\\cli.js",
                ],
            )],
        };

        assert_eq!(
            identify_agent_in_job(&job),
            Some((Agent::Mastracode, "mastracode".to_string()))
        );
    }

    #[test]
    fn identify_agent_in_job_ignores_non_cli_pi_package_scripts() {
        for script in [
            r"C:\Users\herdr\AppData\Roaming\npm\node_modules\@earendil-works\pi-coding-agent\scripts\build.js",
            r"C:\Users\herdr\AppData\Local\pi-node\current\node_modules\@earendil-works\pi-coding-agent\dist\bundle\update.js",
            r"C:\workspace\dist\bundle\cli.js",
            r"C:\workspace\node_modules\other-package\dist\bundle\cli.js",
            r"C:\workspace\node_modules\@earendil-works\pi-coding-agent\dist\cli.exe",
            r"C:\workspace\node_modules\@earendil-works\pi-coding-agent\dist\cli.js\other.js",
            r"C:\workspace\node_modules\@earendil-works\pi-coding-agent\dist\bundle\cli.exe",
            r"C:\workspace\node_modules\@earendil-works\pi-coding-agent\dist\bundle\cli.js\other.js",
        ] {
            let job = crate::platform::ForegroundJob {
                process_group_id: 123,
                processes: vec![foreground_process(123, "node.exe", &["node.exe", script])],
            };

            assert_eq!(identify_agent_in_job(&job), None, "script: {script}");
        }
    }

    #[test]
    fn identify_agent_in_job_detects_windows_cmd_wrapped_codex() {
        let job = crate::platform::ForegroundJob {
            process_group_id: 123,
            processes: vec![foreground_process(
                1,
                "cmd.exe",
                &[
                    "cmd.exe",
                    "/D",
                    "/S",
                    "/C",
                    "C:\\Users\\herdr\\AppData\\Roaming\\npm\\codex.cmd --model gpt-5",
                ],
            )],
        };

        assert_eq!(
            identify_agent_in_job(&job),
            Some((Agent::Codex, "codex".to_string()))
        );
    }

    #[test]
    fn identify_agent_in_job_detects_powershell_file_wrapped_claude() {
        let job = crate::platform::ForegroundJob {
            process_group_id: 123,
            processes: vec![foreground_process(
                1,
                "powershell.exe",
                &[
                    "powershell.exe",
                    "-NoProfile",
                    "-File",
                    "C:\\Users\\herdr\\Documents\\PowerShell\\Scripts\\claude.ps1",
                ],
            )],
        };

        assert_eq!(
            identify_agent_in_job(&job),
            Some((Agent::Claude, "claude".to_string()))
        );
    }

    // A plain shell pane launched with herdr's injected prompt integration
    // must still classify as a shell, not an agent, even though its argv now
    // carries a -Command payload.
    #[test]
    fn identify_agent_in_job_ignores_herdr_powershell_shell_integration_argv() {
        let job = crate::platform::ForegroundJob {
            process_group_id: 123,
            processes: vec![foreground_process(
                1,
                "powershell.exe",
                &[
                    "powershell.exe",
                    "-NoExit",
                    "-Command",
                    crate::pane::WINDOWS_POWERSHELL_SHELL_INTEGRATION_COMMAND,
                ],
            )],
        };

        assert_eq!(identify_agent_in_job(&job), None);
    }

    #[test]
    fn identify_agent_in_job_detects_opencode2_as_opencode() {
        let job = crate::platform::ForegroundJob {
            process_group_id: 123,
            processes: vec![foreground_process(
                123,
                "opencode2",
                &["opencode2", "--standalone"],
            )],
        };

        assert_eq!(
            identify_agent_in_job(&job),
            Some((Agent::OpenCode, "opencode2".to_string()))
        );
    }

    #[test]
    fn identify_agent_in_job_detects_opencode_exe_from_pnpm_package() {
        let job = crate::platform::ForegroundJob {
            process_group_id: 123,
            processes: vec![foreground_process(
                123,
                "opencode.exe",
                &["/home/user/.local/share/pnpm/global/node_modules/opencode-ai/bin/opencode.exe"],
            )],
        };

        assert_eq!(
            identify_agent_in_job(&job),
            Some((Agent::OpenCode, "opencode.exe".to_string()))
        );
    }

    #[test]
    fn identify_agent_in_job_detects_opencode_exe_from_argv0_path() {
        let job = crate::platform::ForegroundJob {
            process_group_id: 123,
            processes: vec![foreground_process(
                123,
                "MainThread",
                &["/home/user/.local/share/pnpm/global/node_modules/opencode-ai/bin/opencode.exe"],
            )],
        };

        assert_eq!(
            identify_agent_in_job(&job),
            Some((Agent::OpenCode, "opencode".to_string()))
        );
    }

    #[test]
    fn wrapped_agent_name_from_runtime_argv_ignores_plain_shell_flags() {
        let registry = crate::agents::registry();
        let recognizer = ProcessRecognizer {
            registry: &registry,
        };
        assert_eq!(
            recognizer
                .wrapped_agent_name_from_runtime_argv("bash", Some(&["bash".into(), "-lc".into()])),
            None
        );
    }

    #[test]
    fn identify_agent_in_job_ignores_python_c_argument_named_codex() {
        let job = crate::platform::ForegroundJob {
            process_group_id: 123,
            processes: vec![foreground_process(
                1,
                "python3",
                &["python3", "-c", "import time; time.sleep(60)", "/tmp/codex"],
            )],
        };

        assert_eq!(identify_agent_in_job(&job), None);
    }

    #[test]
    fn identify_agent_in_job_ignores_node_eval_argument_named_codex() {
        let job = crate::platform::ForegroundJob {
            process_group_id: 123,
            processes: vec![foreground_process(
                1,
                "node",
                &["node", "-e", "setTimeout(() => {}, 60000)", "/tmp/codex"],
            )],
        };

        assert_eq!(identify_agent_in_job(&job), None);
    }

    #[test]
    fn identify_agent_in_job_ignores_shell_c_argument_named_codex() {
        let job = crate::platform::ForegroundJob {
            process_group_id: 123,
            processes: vec![foreground_process(
                1,
                "bash",
                &["bash", "-c", "sleep 60", "/tmp/codex"],
            )],
        };

        assert_eq!(identify_agent_in_job(&job), None);
    }

    #[test]
    fn identify_agent_in_job_detects_python_script_named_codex() {
        let job = crate::platform::ForegroundJob {
            process_group_id: 123,
            processes: vec![foreground_process(
                1,
                "python3",
                &["python3", "/tmp/codex", "--model", "gpt-5"],
            )],
        };

        assert_eq!(
            identify_agent_in_job(&job),
            Some((Agent::Codex, "codex".to_string()))
        );
    }

    #[test]
    fn cmdline_argv0_agent_name_canonicalizes_known_aliases() {
        let registry = crate::agents::registry();
        let recognizer = ProcessRecognizer {
            registry: &registry,
        };
        assert_eq!(
            recognizer.cmdline_argv0_agent_name("/nix/store/example/bin/ghcs"),
            Some("copilot".to_string())
        );
    }

    #[test]
    fn cmdline_argv0_agent_name_requires_exact_agent_basename() {
        let registry = crate::agents::registry();
        let recognizer = ProcessRecognizer {
            registry: &registry,
        };
        assert_eq!(
            recognizer.cmdline_argv0_agent_name("/tmp/my-codex-helper"),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn identify_agent_in_job_resolves_cursor_agent_symlink_argv0() {
        let dir = temp_detection_path("cursor-agent-symlink");
        std::fs::create_dir_all(&dir).expect("test directory should be created");
        let target = dir.join("cursor-agent");
        let link = dir.join("agent");
        std::fs::write(&target, b"#!/bin/sh\n").expect("target should be written");
        std::os::unix::fs::symlink(&target, &link).expect("symlink should be created");

        let argv0 = link.to_string_lossy().into_owned();
        let job = crate::platform::ForegroundJob {
            process_group_id: 42,
            processes: vec![foreground_process(
                42,
                "MainThread",
                &[&argv0, "--use-system-ca", "/tmp/index.js"],
            )],
        };

        assert_eq!(
            identify_agent_in_job(&job),
            Some((Agent::Cursor, "cursor".to_string()))
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    // ---- Screen detection routing ----

    #[test]
    fn no_agent_returns_unknown() {
        assert_eq!(detect_state(None, "anything"), AgentState::Unknown);
    }

    // ---- Process identification (real PTY) ----

    #[cfg(target_os = "linux")]
    fn open_test_pty() -> portable_pty::PtyPair {
        portable_pty::native_pty_system()
            .openpty(portable_pty::PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("failed to open pty")
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn foreground_job_detects_sleep() {
        use portable_pty::CommandBuilder;

        let pair = open_test_pty();

        // Spawn "sleep 999" — a known, deterministic process
        let mut cmd = CommandBuilder::new("sleep");
        cmd.arg("999");
        let mut child = pair.slave.spawn_command(cmd).expect("failed to spawn");
        let pid = child.process_id().expect("no pid");

        // Give the process a moment to become the foreground group
        std::thread::sleep(std::time::Duration::from_millis(50));

        let job = foreground_job(pid).expect("expected foreground job");
        assert!(
            job.processes.iter().any(|p| p.name == "sleep"),
            "expected sleep in {job:?}"
        );
        assert_eq!(
            identify_agent_in_job(&job),
            None,
            "sleep should not map to an agent"
        );

        // Clean up
        child.kill().ok();
        child.wait().ok();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn foreground_job_detects_shell_running_command() {
        use portable_pty::CommandBuilder;
        use std::io::Write;

        let pair = open_test_pty();

        // Spawn a shell, then run a command inside it
        let cmd = CommandBuilder::new("sh");
        let mut child = pair.slave.spawn_command(cmd).expect("failed to spawn");
        let pid = child.process_id().expect("no pid");

        // Write a command to the shell
        let mut writer = pair.master.take_writer().expect("no writer");
        // Use exec so sleep replaces sh as the foreground process
        writer.write_all(b"exec sleep 999\n").ok();
        drop(writer);

        std::thread::sleep(std::time::Duration::from_millis(100));

        let job = foreground_job(pid).expect("expected foreground job");
        assert!(
            job.processes.iter().any(|p| p.name == "sleep"),
            "expected sleep in {job:?}"
        );
        assert_eq!(
            identify_agent_in_job(&job),
            None,
            "sleep should not map to an agent"
        );

        child.kill().ok();
        child.wait().ok();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn foreground_job_detects_agent_behind_shell_wrapper() {
        use portable_pty::CommandBuilder;

        let pair = open_test_pty();

        let mut cmd = CommandBuilder::new("bash");
        cmd.arg("-c");
        cmd.arg("bash -c 'exec -a codex sleep 999' & wait");
        let mut child = pair.slave.spawn_command(cmd).expect("failed to spawn");
        let pid = child.process_id().expect("no pid");
        std::thread::sleep(std::time::Duration::from_millis(100));

        let job = foreground_job(pid);
        let process_group_id = job.as_ref().map(|job| job.process_group_id).unwrap_or(pid);
        unsafe {
            libc::kill(-(process_group_id as i32), libc::SIGKILL);
        }
        child.wait().ok();

        let job = job.expect("expected foreground job");
        assert!(
            job.processes.iter().any(|process| process.name == "bash")
                && job.processes.iter().any(|process| {
                    process.name == "sleep"
                        && process
                            .argv
                            .as_deref()
                            .and_then(|argv| argv.first())
                            .is_some_and(|argv0| argv0 == "codex")
                }),
            "expected wrapper and agent child in {job:?}"
        );
        assert_eq!(
            identify_agent_in_job(&job),
            Some((Agent::Codex, "codex".to_string()))
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn proc_stat_parsing_handles_spaces_in_comm() {
        // Verify our /proc/pid/stat parser correctly extracts fields
        // even when (comm) could contain spaces.
        let pid = std::process::id();
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();

        // Our parsing: find last ')' then split the rest
        let close_paren = stat.rfind(')').expect("should have closing paren");
        let rest = &stat[close_paren + 2..];
        let fields: Vec<&str> = rest.split_whitespace().collect();

        // We should have enough fields (at least 6 for tpgid)
        assert!(
            fields.len() >= 6,
            "not enough fields in stat: {}",
            fields.len()
        );

        // Field 0 should be a valid state char (S, R, D, etc.)
        let state = fields[0];
        assert!(
            ["S", "R", "D", "Z", "T", "t", "W", "X", "I"].contains(&state),
            "unexpected state: {state}"
        );

        // Field 5 (tpgid) should parse as i32 (can be -1 if no controlling terminal)
        let tpgid: i32 = fields[5].parse().expect("tpgid should be a number");
        // In CI/test environments without a terminal, tpgid is typically -1
        let _ = tpgid;
    }
}
