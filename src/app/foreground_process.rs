use std::time::{Duration, Instant};

use crate::layout::PaneId;
use crate::platform::{ForegroundJob, ForegroundProcess};

pub(crate) const FOREGROUND_PROCESS_REFRESH_INTERVAL: Duration = Duration::from_millis(1500);
pub(crate) const FOREGROUND_PROCESS_REFRESH_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ForegroundProcessRefreshInFlight {
    pub(crate) generation: u64,
    pub(crate) deadline: Instant,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ForegroundProcessTarget {
    pub(crate) pane_id: PaneId,
    pub(crate) shell_pid: Option<u32>,
    pub(crate) idle_agent_context: bool,
    /// Session log to read the model from when the agent argv names none.
    pub(crate) model_log: Option<crate::app::agent_model_log::ModelLogSource>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ForegroundProcessObservation {
    pub(crate) pane_id: PaneId,
    pub(crate) shell_pid: Option<u32>,
    pub(crate) process_name: Option<String>,
    pub(crate) process_active: bool,
    pub(crate) agent_model: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ForegroundProcessRefreshScope {
    AllPanes,
    IdleAgents,
}

pub(crate) fn process_name_for_job(shell_pid: u32, job: &ForegroundJob) -> Option<String> {
    if job.process_group_id == shell_pid
        || job.processes.iter().any(|process| process.pid == shell_pid)
    {
        return None;
    }

    let process = job
        .processes
        .iter()
        .find(|process| process.pid == job.process_group_id)
        .or_else(|| job.processes.first())?;
    process_name(process)
}

fn process_is_active(shell_pid: u32, job: &ForegroundJob, idle_agent_context: bool) -> bool {
    if job.process_group_id == shell_pid
        || job.processes.iter().any(|process| process.pid == shell_pid)
    {
        return false;
    }

    if idle_agent_context
        && job
            .processes
            .iter()
            .all(|process| is_idle_agent_runtime(&process.name))
    {
        return false;
    }

    let Some(selected_agent_pid) = agent_process_pid(job) else {
        return true;
    };

    job.processes.iter().any(|process| {
        match crate::detect::identify_agent_in_job(&single_process_job(process)) {
            Some(_) => process.pid != selected_agent_pid,
            None => {
                !crate::platform::is_pane_shell_process_name(&process.name)
                    && !is_idle_agent_runtime(&process.name)
            }
        }
    })
}

fn single_process_job(process: &ForegroundProcess) -> ForegroundJob {
    ForegroundJob {
        process_group_id: process.pid,
        processes: vec![process.clone()],
    }
}

/// The model the pane's agent process was launched with, from its argv.
pub(crate) fn agent_model_for_job(job: &ForegroundJob) -> Option<String> {
    let agent_pid = agent_process_pid(job)?;
    let process = job
        .processes
        .iter()
        .find(|process| process.pid == agent_pid)?;
    match process.argv.as_deref() {
        Some(argv) => crate::detect::agent_model_from_argv(argv),
        None => {
            let argv: Vec<String> = process
                .cmdline
                .as_deref()?
                .split_whitespace()
                .map(str::to_string)
                .collect();
            crate::detect::agent_model_from_argv(&argv)
        }
    }
}

/// Fallback when argv names no model: the model in the session's own log,
/// only when the log belongs to the same agent the job is running.
fn model_from_session_log(
    job: &ForegroundJob,
    source: &crate::app::agent_model_log::ModelLogSource,
) -> Option<String> {
    agent_process_pid(job)?;
    let (agent, _) = crate::detect::identify_agent_in_job(job)?;
    (agent == source.agent())
        .then(|| crate::app::agent_model_log::model_from_log(source))
        .flatten()
}

/// The pid of the agent process the pane's foreground job is named after.
fn agent_process_pid(job: &ForegroundJob) -> Option<u32> {
    let (agent, _) = crate::detect::identify_agent_in_job(job)?;
    job.processes.iter().find_map(|process| {
        crate::detect::identify_agent_in_job(&single_process_job(process))
            .is_some_and(|(process_agent, _)| process_agent == agent)
            .then_some(process.pid)
    })
}

/// True while the pane's agent still owns live sub-process work that has left
/// the pane's foreground process group.
///
/// A turn-end report describes the *agent*: the Stop hook fires and the prompt
/// box repaints the moment the agent stops talking. It says nothing about a
/// command the agent started in the background, which keeps running for
/// minutes afterwards. Those children are spawned into their own process group
/// with no controlling terminal, so `foreground_job` — which is exactly the
/// pane's foreground process group — cannot see them at all.
///
/// Evidence is the agent process's own live descendant tree, never screen
/// output, so this cannot resurface the retired-gate-by-output failure. Two
/// shapes count, both of which only exist while work is actually running:
///
/// - another agent process (`codex exec` under Claude, and the reverse),
/// - a command shell the agent spawned, which lives exactly as long as the
///   command it was given.
///
/// Everything else is ignored, which is what keeps a long-lived helper from
/// pinning the pane Working forever: MCP servers and other resting runtimes
/// (`node`, `bun`, `python`) never match, and a process that truly daemonizes
/// is reparented away from the agent and leaves this tree entirely.
fn agent_subprocess_active(job: &ForegroundJob, descendants: &[ForegroundProcess]) -> bool {
    let Some(agent_pid) = agent_process_pid(job) else {
        return false;
    };

    descendants.iter().any(|process| {
        process.pid != agent_pid
            && (crate::detect::identify_agent_in_job(&single_process_job(process)).is_some()
                || is_agent_command_shell(process))
    })
}

/// A shell the agent started to run one command, as opposed to an interactive
/// shell. A shell told to run one command exits with that command, which is why
/// its presence is evidence of live work.
fn is_agent_command_shell(process: &ForegroundProcess) -> bool {
    let Some(shell) = crate::platform::pane_shell_name(&process.name) else {
        return false;
    };

    let Some(argv) = process.argv.as_deref() else {
        return false;
    };
    let arguments = || argv.iter().skip(1);

    // The families do not share a syntax, so the flags are read against the
    // shell that was actually invoked rather than pattern-matched loosely: a
    // bare `-C` is `noclobber` to bash and an abbreviated `-Command` to
    // PowerShell.
    match shell.as_str() {
        // `-NoExit` runs the command and then stays interactive, so such a
        // shell outlives its work and is not evidence that any is running.
        // PowerShell stops reading switches at the command flag, so the search
        // stops there too: past it a bare `-NoExit` is a word in the command
        // being run, not a request to stay.
        "pwsh" | "powershell" => {
            let mut arguments = arguments();
            let mut runs_one_thing = false;
            while let Some(argument) = arguments.next() {
                if powershell_flag(argument, "noexit", 3) {
                    return false;
                }
                // `-EncodedCommand` takes exactly one value and PowerShell goes
                // on reading switches after it, so the search continues past it.
                // The other forms end switch parsing, and what follows is the
                // command text or the script's own arguments.
                if powershell_flag(argument, "encodedcommand", 1) {
                    arguments.next();
                    runs_one_thing = true;
                } else if powershell_command_flag(argument) {
                    return true;
                }
            }
            runs_one_thing
        }
        // `cmd /c`, and only `/c` — `/k` runs the command and then stays.
        // Whichever comes first takes the rest of the line as its command, so a
        // later `/c` there is a word in that command rather than a switch.
        "cmd" => arguments()
            .filter_map(|argument| argument.strip_prefix('/'))
            .find(|flag| flag.eq_ignore_ascii_case("c") || flag.eq_ignore_ascii_case("k"))
            .is_some_and(|flag| flag.eq_ignore_ascii_case("c")),
        // Every Unix shell takes `-c`, bundled with other short flags or not.
        _ => arguments().any(|argument| {
            argument.starts_with('-')
                && !argument.starts_with("--")
                && argument.contains('c')
                && argument
                    .chars()
                    .skip(1)
                    .all(|flag| flag.is_ascii_alphabetic())
        }),
    }
}

/// The switches that hand PowerShell the one thing it should run and end switch
/// parsing with it: `-Command`, `-File`, and `-CommandWithArgs` from PowerShell
/// 7.4 with its own `-cwa` alias. Everything after one of these belongs to the
/// work — the command text, or the script's own arguments — not to the shell.
///
/// `-File -` counts: the script arrives on the pipe the agent opened and the
/// shell is done at end of input.
fn powershell_command_flag(argument: &str) -> bool {
    ["command", "commandwithargs", "file"]
        .iter()
        .any(|name| powershell_flag(argument, name, 1))
        || argument
            .strip_prefix('-')
            .is_some_and(|flag| flag.eq_ignore_ascii_case("cwa"))
}

/// PowerShell flags are case-insensitive and accept any unambiguous prefix, so
/// `-Command`, `-command`, `-comm` and `-c` are one flag. `shortest` is where a
/// prefix stops being ambiguous with the other switches: `-c` can only be
/// `-Command`, but `-no` could still become `-NoLogo` or `-NoProfile`.
fn powershell_flag(argument: &str, name: &str, shortest: usize) -> bool {
    argument
        .strip_prefix('-')
        .is_some_and(|flag| flag.len() >= shortest && name.starts_with(&flag.to_ascii_lowercase()))
}

fn is_idle_agent_runtime(name: &str) -> bool {
    let name = name
        .rsplit(['/', '\\'])
        .find(|component| !component.is_empty())
        .unwrap_or(name)
        .trim_end_matches(".exe")
        .to_ascii_lowercase();
    name == "node"
        || name == "bun"
        || name == "sleep"
        || name == "python"
        || name.strip_prefix("python").is_some_and(|version| {
            !version.is_empty()
                && version
                    .split('.')
                    .all(|part| !part.is_empty() && part.chars().all(|ch| ch.is_ascii_digit()))
        })
}

fn process_name(process: &ForegroundProcess) -> Option<String> {
    let name = process.name.trim();
    if !name.is_empty() {
        return Some(name.to_string());
    }

    process.argv0.as_deref().and_then(|argv0| {
        argv0
            .rsplit(['/', '\\'])
            .find(|component| !component.is_empty())
            .filter(|name| !name.is_empty())
            .map(str::to_string)
    })
}

pub(crate) fn refresh_foreground_processes<F, D>(
    targets: &[ForegroundProcessTarget],
    deadline: Instant,
    lookup: F,
    descendants: D,
) -> Vec<ForegroundProcessObservation>
where
    F: Fn(u32) -> Option<ForegroundJob>,
    D: Fn(u32) -> Vec<ForegroundProcess>,
{
    let mut observations = Vec::with_capacity(targets.len());
    for target in targets {
        let (process_name, process_active, agent_model) = match target.shell_pid {
            None => (None, false, None),
            Some(_) if Instant::now() >= deadline => continue,
            Some(shell_pid) => lookup(shell_pid)
                .map(|job| {
                    let active = process_is_active(shell_pid, &job, target.idle_agent_context)
                        || agent_process_pid(&job).is_some_and(|agent_pid| {
                            agent_subprocess_active(&job, &descendants(agent_pid))
                        });
                    (
                        process_name_for_job(shell_pid, &job),
                        active,
                        agent_model_for_job(&job)
                            .or_else(|| model_from_session_log(&job, target.model_log.as_ref()?)),
                    )
                })
                .unwrap_or((None, false, None)),
        };
        observations.push(ForegroundProcessObservation {
            pane_id: target.pane_id,
            shell_pid: target.shell_pid,
            process_name,
            process_active,
            agent_model,
        });
    }
    observations
}

fn rotate_targets_for_generation(targets: &mut [ForegroundProcessTarget], generation: u64) {
    let target_count = targets.len();
    if target_count > 1 {
        targets.rotate_left(generation as usize % target_count);
    }
}

impl crate::app::App {
    pub(crate) fn foreground_process_refresh_deadline(&self) -> Option<Instant> {
        if let Some(refresh) = self.foreground_process_refresh_in_flight.as_ref() {
            return Some(refresh.deadline);
        }
        (!self.state.workspaces.is_empty()).then_some(self.next_foreground_process_refresh)
    }

    pub(crate) fn start_foreground_process_refresh_if_due(&mut self, now: Instant) {
        self.start_foreground_process_refresh_if_due_for_scope(
            now,
            ForegroundProcessRefreshScope::AllPanes,
        );
    }

    pub(crate) fn start_headless_foreground_process_refresh_if_due(&mut self, now: Instant) {
        self.start_foreground_process_refresh_if_due_for_scope(
            now,
            ForegroundProcessRefreshScope::IdleAgents,
        );
    }

    fn start_foreground_process_refresh_if_due_for_scope(
        &mut self,
        now: Instant,
        scope: ForegroundProcessRefreshScope,
    ) {
        if self
            .foreground_process_refresh_in_flight
            .as_ref()
            .is_some_and(|refresh| now >= refresh.deadline)
        {
            self.foreground_process_refresh_in_flight = None;
        }

        if self.foreground_process_refresh_in_flight.is_some()
            || now < self.next_foreground_process_refresh
        {
            return;
        }

        self.next_foreground_process_refresh = now + FOREGROUND_PROCESS_REFRESH_INTERVAL;
        let mut targets = self.foreground_process_targets(scope);
        if targets.is_empty() {
            return;
        }

        self.last_foreground_process_refresh_generation = self
            .last_foreground_process_refresh_generation
            .wrapping_add(1);
        let generation = self.last_foreground_process_refresh_generation;
        rotate_targets_for_generation(&mut targets, generation);
        let deadline = now + FOREGROUND_PROCESS_REFRESH_TIMEOUT;
        self.foreground_process_refresh_in_flight = Some(ForegroundProcessRefreshInFlight {
            generation,
            deadline,
        });

        let event_tx = self.event_tx.clone();
        let _ = std::thread::Builder::new()
            .name("herdr-foreground-process".into())
            .spawn(move || {
                let observations = refresh_foreground_processes(
                    &targets,
                    deadline,
                    crate::detect::foreground_process_job,
                    crate::platform::descendant_processes,
                );
                let _ =
                    event_tx.blocking_send(crate::events::AppEvent::ForegroundProcessesRefreshed {
                        generation,
                        observations,
                    });
            });
    }

    fn foreground_process_targets(
        &self,
        scope: ForegroundProcessRefreshScope,
    ) -> Vec<ForegroundProcessTarget> {
        let mut targets = Vec::new();
        for (ws_idx, workspace) in self.state.workspaces.iter().enumerate() {
            for tab in &workspace.tabs {
                for pane_id in tab.layout.pane_ids() {
                    let Some(terminal_id) = workspace.terminal_id(pane_id) else {
                        continue;
                    };
                    let idle_agent_context =
                        self.state
                            .terminals
                            .get(terminal_id)
                            .is_some_and(|terminal| {
                                terminal.effective_agent_label().is_some()
                                    && (terminal.raw_agent_state()
                                        == crate::detect::AgentState::Idle
                                        || terminal.foreground_process_active())
                            });
                    if scope == ForegroundProcessRefreshScope::IdleAgents && !idle_agent_context {
                        continue;
                    }
                    let runtime = self.state.runtime_for_pane_in_workspace(
                        &self.terminal_runtimes,
                        ws_idx,
                        pane_id,
                    );
                    if runtime.is_some_and(|runtime| runtime.is_suspended()) {
                        continue;
                    }
                    let shell_pid = runtime.and_then(|runtime| runtime.child_pid());
                    let model_log = self
                        .state
                        .terminals
                        .get(terminal_id)
                        .and_then(|terminal| terminal.agent_model_log_source());
                    targets.push(ForegroundProcessTarget {
                        pane_id,
                        shell_pid,
                        idle_agent_context,
                        model_log,
                    });
                }
            }
        }
        targets
    }

    pub(crate) fn handle_foreground_processes_refreshed(
        &mut self,
        generation: u64,
        observations: Vec<ForegroundProcessObservation>,
    ) -> bool {
        if generation <= self.last_applied_foreground_process_refresh_generation
            || generation != self.last_foreground_process_refresh_generation
        {
            return false;
        }

        let now = Instant::now();
        if self
            .foreground_process_refresh_in_flight
            .as_ref()
            .is_some_and(|refresh| refresh.generation == generation)
        {
            let overran_deadline = self
                .foreground_process_refresh_in_flight
                .as_ref()
                .is_some_and(|refresh| now >= refresh.deadline);
            self.foreground_process_refresh_in_flight = None;
            if overran_deadline {
                self.next_foreground_process_refresh = now + FOREGROUND_PROCESS_REFRESH_INTERVAL;
            }
        }
        self.last_applied_foreground_process_refresh_generation = generation;

        let mut changed = false;
        for observation in observations {
            let Some((ws_idx, terminal_id)) =
                self.state
                    .workspaces
                    .iter()
                    .enumerate()
                    .find_map(|(ws_idx, workspace)| {
                        workspace
                            .terminal_id(observation.pane_id)
                            .cloned()
                            .map(|terminal_id| (ws_idx, terminal_id))
                    })
            else {
                continue;
            };
            let runtime = self.state.runtime_for_pane_in_workspace(
                &self.terminal_runtimes,
                ws_idx,
                observation.pane_id,
            );
            // A settled pane's runtime is suspended on purpose. Its agent
            // disappearing is that suspension, not activity, and must not
            // unsettle and resume the pane.
            if runtime.is_some_and(|runtime| runtime.is_suspended())
                || runtime.and_then(|runtime| runtime.child_pid()) != observation.shell_pid
            {
                continue;
            }
            let model_changed = self
                .state
                .terminals
                .get(&terminal_id)
                .is_some_and(|terminal| terminal.agent_model != observation.agent_model);
            if model_changed {
                if let Some(terminal) = self.state.terminals.get_mut(&terminal_id) {
                    terminal.agent_model = observation.agent_model.clone();
                }
                changed = true;
            }
            let process_changed = self
                .state
                .terminals
                .get(&terminal_id)
                .is_some_and(|terminal| {
                    terminal.foreground_process_name != observation.process_name
                        || terminal.foreground_process_active() != observation.process_active
                });
            let update = self
                .state
                .update_terminal_state(observation.pane_id, |terminal| {
                    terminal.set_foreground_process(
                        observation.process_name,
                        observation.process_active,
                        now,
                    )
                });
            if process_changed {
                self.state
                    .note_automated_pane_activity_at(observation.pane_id, now);
            }
            if let Some(update) = update {
                self.emit_pane_state_update(&update);
            }
            changed |= process_changed;
        }

        if changed {
            self.render_dirty.request_generic();
            self.render_notify.notify_one();
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(pid: u32, name: &str) -> ForegroundProcess {
        ForegroundProcess {
            pid,
            name: name.into(),
            argv0: None,
            argv: None,
            cmdline: None,
        }
    }

    fn process_with_argv(pid: u32, name: &str, argv: &[&str]) -> ForegroundProcess {
        ForegroundProcess {
            pid,
            name: name.into(),
            argv0: argv.first().map(|value| (*value).to_string()),
            argv: Some(argv.iter().map(|value| (*value).to_string()).collect()),
            cmdline: None,
        }
    }

    fn job(process_group_id: u32, processes: Vec<ForegroundProcess>) -> ForegroundJob {
        ForegroundJob {
            process_group_id,
            processes,
        }
    }

    #[test]
    fn agent_model_is_read_from_the_agent_process_argv() {
        let job = job(
            40,
            vec![
                process_with_argv(40, "claude", &["claude", "--model", "claude-fable-5-1"]),
                process_with_argv(41, "node", &["node", "--model", "decoy"]),
            ],
        );
        assert_eq!(
            agent_model_for_job(&job).as_deref(),
            Some("claude-fable-5-1")
        );
        let plain = super::tests::job(50, vec![process_with_argv(50, "codex", &["codex"])]);
        assert_eq!(agent_model_for_job(&plain), None);
    }

    #[test]
    fn session_log_supplies_the_model_only_when_argv_has_none() {
        let dir = std::env::temp_dir().join(format!(
            "herdr-fg-model-log-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let transcript = dir.join("session.jsonl");
        std::fs::write(
            &transcript,
            "{\"type\":\"assistant\",\"message\":{\"model\":\"claude-fable-5-1\"}}\n",
        )
        .expect("write transcript");
        let log = Some(crate::app::agent_model_log::ModelLogSource::ClaudeTranscript(transcript));
        let target = |pane: u32, shell_pid: u32| ForegroundProcessTarget {
            pane_id: PaneId::from_raw(pane),
            shell_pid: Some(shell_pid),
            idle_agent_context: false,
            model_log: log.clone(),
        };
        let targets = [target(1, 10), target(2, 20), target(3, 30)];
        let observations = refresh_foreground_processes(
            &targets,
            Instant::now() + Duration::from_secs(1),
            |pid| match pid {
                10 => Some(job(11, vec![process_with_argv(11, "claude", &["claude"])])),
                20 => Some(job(
                    21,
                    vec![process_with_argv(
                        21,
                        "claude",
                        &["claude", "--model", "claude-opus-5-5"],
                    )],
                )),
                _ => Some(job(31, vec![process_with_argv(31, "codex", &["codex"])])),
            },
            |_| Vec::new(),
        );
        let models: Vec<_> = observations
            .iter()
            .map(|observation| observation.agent_model.as_deref())
            .collect();
        // Log fills a bare argv; argv wins; a Claude log never feeds Codex.
        assert_eq!(
            models,
            [Some("claude-fable-5-1"), Some("claude-opus-5-5"), None]
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn distinct_foreground_process_uses_group_leader_name() {
        let result = process_name_for_job(
            10,
            &job(20, vec![process(21, "child"), process(20, "cargo")]),
        );

        assert_eq!(result.as_deref(), Some("cargo"));
    }

    #[test]
    fn shell_foreground_group_is_suppressed() {
        assert_eq!(
            process_name_for_job(10, &job(10, vec![process(10, "zsh")])),
            None
        );
        assert_eq!(
            process_name_for_job(10, &job(20, vec![process(10, "zsh")])),
            None
        );
    }

    #[test]
    fn resting_agent_process_is_not_active() {
        assert!(!process_is_active(
            10,
            &job(20, vec![process(20, "claude")]),
            false,
        ));
    }

    #[test]
    fn agent_child_process_is_active() {
        assert!(process_is_active(
            10,
            &job(20, vec![process(20, "claude"), process(21, "codex")]),
            false,
        ));
    }

    #[test]
    fn same_agent_nested_child_is_active() {
        assert!(process_is_active(
            10,
            &job(20, vec![process(20, "claude"), process(21, "claude")]),
            false,
        ));
    }

    #[test]
    fn wrapped_resting_agent_processes_are_not_active() {
        assert!(!process_is_active(
            10,
            &job(
                20,
                vec![
                    process_with_argv(20, "node", &["node", "/path/to/bin/codex"]),
                    process(21, "bash"),
                ],
            ),
            false,
        ));
    }

    #[test]
    fn agent_sleep_helper_is_not_active() {
        assert!(!process_is_active(
            10,
            &job(20, vec![process(20, "pi"), process(21, "sleep")]),
            false,
        ));
        assert!(!process_is_active(
            10,
            &job(20, vec![process(20, "sleep")]),
            true,
        ));
        assert!(process_is_active(
            10,
            &job(20, vec![process(20, "sleep")]),
            false,
        ));
    }

    #[test]
    fn agent_with_plain_running_command_is_active() {
        assert!(process_is_active(
            10,
            &job(20, vec![process(20, "claude"), process(21, "cargo")]),
            false,
        ));
    }

    #[test]
    fn plain_foreground_command_is_active() {
        assert!(process_is_active(
            10,
            &job(20, vec![process(20, "cargo")]),
            false,
        ));
    }

    /// The reproduced bug: Claude ends its turn while a `codex exec` it started
    /// with `run_in_background` keeps running. The background command is spawned
    /// into its own process group, so the pane's foreground job holds nothing
    /// but the resting agent.
    #[test]
    fn backgrounded_agent_subprocess_outside_the_foreground_group_is_active() {
        let claude = process(20, "claude");
        let wrapper = process_with_argv(30, "zsh", &["/bin/zsh", "-c", "codex exec ..."]);
        let codex = process(31, "codex");

        assert!(!process_is_active(10, &job(20, vec![claude.clone()]), true));
        assert!(agent_subprocess_active(
            &job(20, vec![claude]),
            &[wrapper, codex],
        ));
    }

    #[test]
    fn a_command_shell_alone_holds_the_pane_until_its_command_exits() {
        let wrapper = process_with_argv(30, "zsh", &["/bin/zsh", "-c", "sleep 240 && echo ok"]);

        assert!(agent_subprocess_active(
            &job(20, vec![process(20, "claude")]),
            &[wrapper.clone(), process(31, "sleep")],
        ));
        assert!(!agent_subprocess_active(
            &job(20, vec![process(20, "claude")]),
            &[],
        ));
    }

    /// The pinned-forever failure this must not cause: an idle agent keeps its
    /// MCP servers and other resting runtimes alive for its whole lifetime.
    #[test]
    fn resting_agent_helpers_do_not_hold_the_pane_working() {
        assert!(!agent_subprocess_active(
            &job(20, vec![process(20, "claude")]),
            &[
                process(30, "node"),
                process(31, "python3"),
                process(32, "bun"),
                process(33, "sleep"),
                process_with_argv(34, "zsh", &["-zsh"]),
            ],
        ));
    }

    /// The Windows shells spell "run one command" their own way: PowerShell
    /// takes `-Command` and any unambiguous prefix of it, `cmd` takes `/c`.
    /// Reading only the Unix `-c` bundle left those panes reading done.
    #[test]
    fn windows_command_shells_hold_the_pane_too() {
        for argv in [
            vec!["pwsh", "-NoProfile", "-Command", "Start-Sleep 300"],
            vec!["pwsh", "-nologo", "-command", "Start-Sleep 300"],
            vec!["powershell.exe", "-Comm", "Start-Sleep 300"],
            vec!["pwsh", "-c", "Start-Sleep 300"],
            // PowerShell 7.4's `-CommandWithArgs`, spelled out and aliased.
            vec!["pwsh", "-CommandWithArgs", "Start-Sleep 300"],
            vec!["pwsh", "-cwa", "Start-Sleep 300"],
            // The other two finite forms: an encoded command and a script file.
            vec![
                "pwsh",
                "-EncodedCommand",
                "UwB0AGEAcgB0AC0AUwBsAGUAZQBwAA==",
            ],
            vec!["pwsh", "-File", "C:\\work\\build.ps1"],
            // Past the command flag every word belongs to the command being
            // run, so this `-NoExit` is an argument to `ping`, not a switch.
            vec!["pwsh", "-Command", "ping", "--%", "-n", "300", "-NoExit"],
        ] {
            assert!(
                agent_subprocess_active(
                    &job(20, vec![process(20, "claude")]),
                    &[process_with_argv(30, argv[0], &argv)],
                ),
                "{argv:?} should read as a command shell"
            );
        }

        assert!(agent_subprocess_active(
            &job(20, vec![process(20, "claude")]),
            &[process_with_argv(
                30,
                "cmd.exe",
                &["cmd.exe", "/c", "timeout /t 300"],
            )],
        ));
    }

    /// An interactive PowerShell or `cmd` is not work: `-NoExit` and `/k` both
    /// leave a shell sitting there, and a bare `-C` is bash's `noclobber`.
    #[test]
    fn interactive_windows_shells_do_not_hold_the_pane() {
        for argv in [
            vec!["pwsh", "-NoProfile", "-NoExit"],
            // -NoExit with a command: it runs the command and then stays, so
            // the shell outlives the work and proves nothing about it.
            vec!["pwsh", "-NoExit", "-Command", "Start-Sleep 1"],
            vec!["pwsh", "-noex", "-c", "Start-Sleep 1"],
            vec!["pwsh", "-NoExit", "-File", "C:\\work\\build.ps1"],
            // `-EncodedCommand` takes one value and switch parsing goes on, so
            // this `-NoExit` is a switch and the shell stays.
            vec![
                "pwsh",
                "-EncodedCommand",
                "UwB0AGEAcgB0AC0AUwBsAGUAZQBwAA==",
                "-NoExit",
            ],
            vec!["cmd.exe", "/k", "prompt"],
            // `/k` takes the rest of the line, so this `/c` is a word in the
            // command it runs and the shell still stays.
            vec!["cmd.exe", "/k", "echo", "/c"],
            vec!["bash", "-C"],
        ] {
            assert!(
                !agent_subprocess_active(
                    &job(20, vec![process(20, "claude")]),
                    &[process_with_argv(30, argv[0], &argv)],
                ),
                "{argv:?} should not read as a command shell"
            );
        }
    }

    /// Windows has no foreground process group, so `foreground_process_job`
    /// reports the agent together with its descendants. The command shell is
    /// then in both lists, and skipping it as "already in the foreground job"
    /// left the pane reading done while its command ran.
    #[test]
    fn a_command_shell_inside_the_foreground_job_still_holds_the_pane() {
        let claude = process(20, "claude");
        let shell = process_with_argv(30, "cmd.exe", &["cmd.exe", "/c", "timeout /t 300"]);

        assert!(agent_subprocess_active(
            &job(20, vec![claude, shell.clone()]),
            &[shell],
        ));
    }

    #[test]
    fn subprocesses_are_only_read_for_a_pane_running_an_agent() {
        assert!(!agent_subprocess_active(
            &job(20, vec![process(20, "cargo")]),
            &[process(30, "codex")],
        ));
    }

    #[test]
    fn the_agents_own_foreground_processes_are_not_counted_twice() {
        let claude = process(20, "claude");

        assert!(!agent_subprocess_active(
            &job(20, vec![claude.clone()]),
            &[claude],
        ));
    }

    #[test]
    fn a_refresh_promotes_a_resting_agent_with_a_live_background_command() {
        let targets = [ForegroundProcessTarget {
            pane_id: PaneId::from_raw(1),
            shell_pid: Some(10),
            idle_agent_context: true,
            model_log: None,
        }];

        let observations = refresh_foreground_processes(
            &targets,
            Instant::now() + Duration::from_secs(1),
            |_pid| Some(job(20, vec![process(20, "claude")])),
            |agent_pid| {
                assert_eq!(agent_pid, 20);
                vec![
                    process_with_argv(30, "zsh", &["/bin/zsh", "-c", "codex exec ..."]),
                    process(31, "codex"),
                ]
            },
        );

        assert!(observations[0].process_active);
    }

    #[test]
    fn a_refresh_leaves_a_quiet_agent_idle() {
        let targets = [ForegroundProcessTarget {
            pane_id: PaneId::from_raw(1),
            shell_pid: Some(10),
            idle_agent_context: true,
            model_log: None,
        }];

        let observations = refresh_foreground_processes(
            &targets,
            Instant::now() + Duration::from_secs(1),
            |_pid| Some(job(20, vec![process(20, "claude")])),
            |_pid| vec![process(30, "node")],
        );

        assert!(!observations[0].process_active);
    }

    #[test]
    fn missing_shell_or_lookup_result_is_silent() {
        let targets = [
            ForegroundProcessTarget {
                pane_id: PaneId::from_raw(1),
                shell_pid: None,
                idle_agent_context: false,
                model_log: None,
            },
            ForegroundProcessTarget {
                pane_id: PaneId::from_raw(2),
                shell_pid: Some(10),
                idle_agent_context: false,
                model_log: None,
            },
        ];
        let observations = refresh_foreground_processes(
            &targets,
            Instant::now() + Duration::from_secs(1),
            |_pid| None,
            |_pid| Vec::new(),
        );

        assert_eq!(observations.len(), 2);
        assert!(observations
            .iter()
            .all(|observation| observation.process_name.is_none()));
    }

    #[test]
    fn timed_out_refresh_omits_unobserved_targets() {
        let targets = [
            ForegroundProcessTarget {
                pane_id: PaneId::from_raw(1),
                shell_pid: Some(10),
                idle_agent_context: false,
                model_log: None,
            },
            ForegroundProcessTarget {
                pane_id: PaneId::from_raw(2),
                shell_pid: Some(20),
                idle_agent_context: false,
                model_log: None,
            },
        ];

        for _ in 0..2 {
            let observations = refresh_foreground_processes(
                &targets,
                Instant::now() - Duration::from_millis(1),
                |_pid| panic!("timed-out targets must not be inspected"),
                |_pid| panic!("timed-out targets must not be inspected"),
            );
            assert!(observations.is_empty());
        }
    }

    #[test]
    fn refresh_target_order_rotates_between_generations() {
        let targets = vec![
            ForegroundProcessTarget {
                pane_id: PaneId::from_raw(1),
                shell_pid: Some(10),
                idle_agent_context: false,
                model_log: None,
            },
            ForegroundProcessTarget {
                pane_id: PaneId::from_raw(2),
                shell_pid: Some(20),
                idle_agent_context: false,
                model_log: None,
            },
            ForegroundProcessTarget {
                pane_id: PaneId::from_raw(3),
                shell_pid: Some(30),
                idle_agent_context: false,
                model_log: None,
            },
        ];

        let mut first_generation = targets.clone();
        rotate_targets_for_generation(&mut first_generation, 1);
        assert_eq!(first_generation[0].pane_id, PaneId::from_raw(2));

        let mut second_generation = targets;
        rotate_targets_for_generation(&mut second_generation, 2);
        assert_eq!(second_generation[0].pane_id, PaneId::from_raw(3));
    }

    fn app_with_test_pane(name: &str) -> (crate::app::App, PaneId, crate::terminal::TerminalId) {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = crate::app::App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let workspace = crate::workspace::Workspace::test_new(name);
        let pane_id = workspace.tabs[0].root_pane;
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        let terminal_id = app.state.workspaces[0].tabs[0]
            .terminal_id(pane_id)
            .cloned()
            .expect("test pane terminal");
        (app, pane_id, terminal_id)
    }

    #[test]
    fn unobserved_refresh_leaves_process_state_and_events_unchanged() {
        let (mut app, pane_id, terminal_id) = app_with_test_pane("unobserved-foreground");
        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("test pane terminal")
            .set_foreground_process(Some("cargo".into()), true, Instant::now());
        let event_count = app.event_hub.events_after(0).len();
        app.last_foreground_process_refresh_generation = 1;

        assert!(!app.handle_foreground_processes_refreshed(1, Vec::new()));

        let terminal = &app.state.terminals[&terminal_id];
        assert_eq!(terminal.foreground_process_name.as_deref(), Some("cargo"));
        assert!(terminal.foreground_process_active());
        assert_eq!(app.event_hub.events_after(0).len(), event_count);
        assert_eq!(app.state.workspaces[0].tabs[0].root_pane, pane_id);
    }

    #[test]
    fn late_foreground_process_refresh_applies_process_name() {
        let (mut app, pane_id, terminal_id) = app_with_test_pane("late-foreground");
        app.last_foreground_process_refresh_generation = 1;
        app.foreground_process_refresh_in_flight = Some(ForegroundProcessRefreshInFlight {
            generation: 1,
            deadline: Instant::now() - Duration::from_millis(1),
        });

        app.handle_foreground_processes_refreshed(
            1,
            vec![ForegroundProcessObservation {
                pane_id,
                shell_pid: None,
                process_name: Some("cargo".into()),
                process_active: true,
                agent_model: None,
            }],
        );

        assert_eq!(
            app.state.terminals[&terminal_id]
                .foreground_process_name
                .as_deref(),
            Some("cargo")
        );
        assert!(app.foreground_process_refresh_in_flight.is_none());
        assert!(app.next_foreground_process_refresh > Instant::now());
    }

    #[test]
    fn foreground_process_changes_leave_settled_guarded_pane_untouched() {
        let (mut app, pane_id, terminal_id) = app_with_test_pane("settled-foreground");
        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("test pane terminal")
            .set_foreground_process(Some("claude".into()), true, Instant::now());
        assert!(app.state.settle_pane_at(0, pane_id, 1_725_000_000));
        app.state.workspaces[0]
            .pane_state_mut(pane_id)
            .expect("test pane")
            .settle_resume_guard = true;
        app.last_foreground_process_refresh_generation = 1;

        assert!(app.handle_foreground_processes_refreshed(
            1,
            vec![ForegroundProcessObservation {
                pane_id,
                shell_pid: None,
                process_name: None,
                process_active: false,
                agent_model: None,
            }],
        ));

        assert!(app.state.pane_is_settled(0, pane_id));
        assert!(
            app.state.workspaces[0]
                .pane_state(pane_id)
                .unwrap()
                .settle_resume_guard
        );

        app.last_foreground_process_refresh_generation = 2;
        assert!(app.handle_foreground_processes_refreshed(
            2,
            vec![ForegroundProcessObservation {
                pane_id,
                shell_pid: None,
                process_name: Some("claude".into()),
                process_active: true,
                agent_model: None,
            }],
        ));
        let pane = app.state.workspaces[0].pane_state(pane_id).unwrap();
        assert!(pane.settled_at.is_some());
        assert!(pane.settle_resume_guard);
    }

    #[test]
    fn foreground_process_refresh_promotes_idle_agent_until_process_clears() {
        let (mut app, pane_id, terminal_id) = app_with_test_pane("active-agent-child");
        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("test pane terminal")
            .set_detected_state(
                Some(crate::detect::Agent::Claude),
                crate::detect::AgentState::Idle,
            );

        app.last_foreground_process_refresh_generation = 1;
        assert!(app.handle_foreground_processes_refreshed(
            1,
            vec![ForegroundProcessObservation {
                pane_id,
                shell_pid: None,
                process_name: Some("codex".into()),
                process_active: true,
                agent_model: None,
            }],
        ));
        assert_eq!(
            app.state.terminals[&terminal_id].raw_agent_state(),
            crate::detect::AgentState::Working
        );

        app.last_foreground_process_refresh_generation = 2;
        assert!(app.handle_foreground_processes_refreshed(
            2,
            vec![ForegroundProcessObservation {
                pane_id,
                shell_pid: None,
                process_name: None,
                process_active: false,
                agent_model: None,
            }],
        ));
        assert_eq!(
            app.state.terminals[&terminal_id].raw_agent_state(),
            crate::detect::AgentState::Idle
        );
    }

    #[test]
    fn same_process_name_activity_flip_requests_render_and_emits_status_event() {
        let (mut app, pane_id, terminal_id) = app_with_test_pane("same-name-activity");
        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("test pane terminal")
            .set_detected_state(
                Some(crate::detect::Agent::Claude),
                crate::detect::AgentState::Idle,
            );
        app.last_foreground_process_refresh_generation = 1;
        app.handle_foreground_processes_refreshed(
            1,
            vec![ForegroundProcessObservation {
                pane_id,
                shell_pid: None,
                process_name: Some("claude".into()),
                process_active: false,
                agent_model: None,
            }],
        );
        let _ = app.render_dirty.take();
        let event_count = app.event_hub.events_after(0).len();

        app.last_foreground_process_refresh_generation = 2;
        assert!(app.handle_foreground_processes_refreshed(
            2,
            vec![ForegroundProcessObservation {
                pane_id,
                shell_pid: None,
                process_name: Some("claude".into()),
                process_active: true,
                agent_model: None,
            }],
        ));

        assert!(app.render_dirty.is_pending());
        assert!(
            app.event_hub.events_after(0)[event_count..]
                .iter()
                .any(|(_, event)| event.event
                    == crate::api::schema::EventKind::PaneAgentStatusChanged)
        );
    }

    #[test]
    fn scheduler_expired_foreground_refresh_accepts_late_result_and_rejects_older_generation() {
        let (mut app, pane_id, terminal_id) = app_with_test_pane("scheduled-late-foreground");
        let first_now = Instant::now();
        app.next_foreground_process_refresh = first_now;
        app.start_foreground_process_refresh_if_due(first_now);
        let first_refresh = app
            .foreground_process_refresh_in_flight
            .clone()
            .expect("first foreground refresh in flight");

        // Push the schedule out so the deadline sweep clears the in-flight record
        // without issuing a successor. That is the interleaving the late-result
        // repair exists for: the record is gone, but nothing has superseded the
        // generation, so its results are still the freshest data available.
        app.next_foreground_process_refresh = first_refresh.deadline + Duration::from_secs(60);
        app.start_foreground_process_refresh_if_due(
            first_refresh.deadline + Duration::from_millis(1),
        );
        assert!(app.foreground_process_refresh_in_flight.is_none());
        assert_eq!(
            app.last_foreground_process_refresh_generation,
            first_refresh.generation
        );

        assert!(app.handle_foreground_processes_refreshed(
            first_refresh.generation,
            vec![ForegroundProcessObservation {
                pane_id,
                shell_pid: None,
                process_name: Some("cargo".into()),
                process_active: true,
                agent_model: None,
            }],
        ));
        assert_eq!(
            app.state.terminals[&terminal_id]
                .foreground_process_name
                .as_deref(),
            Some("cargo")
        );

        assert!(!app.handle_foreground_processes_refreshed(
            first_refresh.generation,
            vec![ForegroundProcessObservation {
                pane_id,
                shell_pid: None,
                process_name: Some("stale-process".into()),
                process_active: true,
                agent_model: None,
            }],
        ));
        assert_eq!(
            app.state.terminals[&terminal_id]
                .foreground_process_name
                .as_deref(),
            Some("cargo")
        );
    }

    #[test]
    fn superseded_foreground_refresh_drops_result_before_successor() {
        let (mut app, pane_id, terminal_id) = app_with_test_pane("superseded-foreground");
        let first_now = Instant::now();
        app.next_foreground_process_refresh = first_now;
        app.start_foreground_process_refresh_if_due(first_now);
        let first_refresh = app
            .foreground_process_refresh_in_flight
            .clone()
            .expect("first foreground refresh in flight");

        let successor_now = first_refresh.deadline + Duration::from_millis(1);
        app.next_foreground_process_refresh = successor_now;
        app.start_foreground_process_refresh_if_due(successor_now);
        let successor_generation = app
            .foreground_process_refresh_in_flight
            .as_ref()
            .expect("successor foreground refresh in flight")
            .generation;
        assert_eq!(successor_generation, first_refresh.generation + 1);

        assert!(!app.handle_foreground_processes_refreshed(
            first_refresh.generation,
            vec![ForegroundProcessObservation {
                pane_id,
                shell_pid: None,
                process_name: Some("stale-process".into()),
                process_active: true,
                agent_model: None,
            }],
        ));
        assert_eq!(
            app.state.terminals[&terminal_id].foreground_process_name,
            None
        );
        assert_eq!(
            app.foreground_process_refresh_in_flight
                .as_ref()
                .map(|refresh| refresh.generation),
            Some(successor_generation)
        );

        assert!(!app.handle_foreground_processes_refreshed(successor_generation, Vec::new(),));
        assert_eq!(
            app.last_applied_foreground_process_refresh_generation,
            successor_generation
        );
        assert!(app.foreground_process_refresh_in_flight.is_none());
    }

    #[test]
    fn foreground_process_generation_mismatch_drops_observations() {
        let (mut app, pane_id, terminal_id) = app_with_test_pane("stale-foreground");
        app.foreground_process_refresh_in_flight = Some(ForegroundProcessRefreshInFlight {
            generation: 2,
            deadline: Instant::now() - Duration::from_millis(1),
        });
        app.last_applied_foreground_process_refresh_generation = 1;

        app.handle_foreground_processes_refreshed(
            1,
            vec![ForegroundProcessObservation {
                pane_id,
                shell_pid: None,
                process_name: Some("stale-process".into()),
                process_active: true,
                agent_model: None,
            }],
        );

        assert_eq!(
            app.state.terminals[&terminal_id].foreground_process_name,
            None
        );
        assert_eq!(
            app.foreground_process_refresh_in_flight
                .map(|refresh| refresh.generation),
            Some(2)
        );
    }
}
