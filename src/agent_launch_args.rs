//! Remembering the options an agent CLI was started with.
//!
//! Herdr resumes a native agent session by running the agent again with its own
//! session reference. The rest of the original command line matters too: a pane
//! started with `claude --permission-mode bypassPermissions` must come back with
//! that permission mode, not with plain `claude --resume <id>`.
//!
//! The captured argument list comes from the live agent process, so it can also
//! contain the session reference Herdr appended on a previous resume, a
//! subcommand, or the prompt the user typed at launch. Replaying those would
//! either point the agent at a stale conversation or restart work the user never
//! asked for, so two rules decide what survives a resume:
//!
//! - Options that select a conversation or a one-shot run are dropped, per
//!   agent. Herdr supplies its own session reference.
//! - A bare word is kept only when it directly follows a kept option, where it
//!   is that option's value. Bare words elsewhere are prompts, subcommands, or
//!   positional paths, and none of those belong in a resume command.
//!
//! Together those drop the value of a dropped option without Herdr having to
//! know which options take values: `--resume <id>` loses the id because the id
//! follows an option that was dropped.
//!
//! Two deliberate limits: an option that takes several separate values keeps
//! only the first (`--add-dir /a /b` resumes as `--add-dir /a`), and a prompt
//! written directly after a flag is indistinguishable from that flag's value, so
//! it is replayed.

/// Return the arguments worth replaying when resuming `agent`.
///
/// `args` is the agent process command line with the executable removed.
pub fn replayable(agent: &str, args: &[String]) -> Vec<String> {
    let dropped = dropped_options(agent);
    let mut kept: Vec<String> = Vec::new();
    // Whether the previous argument was a kept option still waiting for a value.
    let mut kept_option_wants_value = false;

    for arg in args {
        if arg == "--" {
            // Everything after the separator is positional.
            break;
        }
        match option_name(arg) {
            Some(name) => {
                let inline_value = name.len() < arg.len();
                if dropped.contains(&name) {
                    kept_option_wants_value = false;
                    continue;
                }
                kept.push(arg.clone());
                kept_option_wants_value = !inline_value;
            }
            None => {
                if kept_option_wants_value {
                    kept.push(arg.clone());
                }
                kept_option_wants_value = false;
            }
        }
    }

    kept
}

fn option_name(arg: &str) -> Option<&str> {
    if !arg.starts_with('-') || arg == "-" || arg == "--" {
        return None;
    }
    Some(arg.split('=').next().unwrap_or(arg))
}

/// Options that select or continue a conversation, or switch the agent to a
/// one-shot run. Values are dropped with them, because a value following a
/// dropped option is a bare word that follows no kept option.
///
/// Agents that select a session through a subcommand and a positional id, such
/// as `codex resume <id>`, need no entry: those are bare words too.
fn dropped_options(agent: &str) -> &'static [&'static str] {
    match agent {
        "claude" => &[
            "-r",
            "--resume",
            "-c",
            "--continue",
            "-p",
            "--print",
            "--session-id",
            "--fork-session",
            "--cloud",
            "--teleport",
            "--from-pr",
            "--bg",
            "--background",
        ],
        "codex" => &["--last"],
        "copilot" => &[
            "-r",
            "--resume",
            "--continue",
            "-p",
            "--prompt",
            "-i",
            "--interactive",
            "--session-id",
            "--acp",
            "--connect",
        ],
        "grok" => &[
            "-r",
            "--resume",
            "-c",
            "--continue",
            "-p",
            "--single",
            "-s",
            "--session-id",
            "--fork-session",
            "--prompt-file",
            "--prompt-json",
        ],
        "opencode" => &["-s", "--session", "-c", "--continue", "--fork", "--prompt"],
        "pi" => &[
            "-r",
            "--resume",
            "-c",
            "--continue",
            "-p",
            "--print",
            "--session",
            "--session-id",
            "--fork",
            "--no-session",
            "--export",
            "--list-models",
        ],
        "omp" => &[
            "-r",
            "--resume",
            "-c",
            "--continue",
            "-p",
            "--print",
            "--from-claude",
            "--from-codex",
            "--no-session",
            "--alias",
            "--export",
        ],
        "cursor" => &["--resume", "-p", "--print"],
        "droid" => &["--resume", "-p", "--print"],
        "devin" | "hermes" | "qodercli" => &["--resume"],
        "kimi" | "kilo" => &["--session"],
        "mastracode" => &["--thread"],
        "agy" => &["--conversation"],
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn keeps_options_and_their_values() {
        assert_eq!(
            replayable(
                "claude",
                &args(&["--permission-mode", "bypassPermissions", "--verbose"])
            ),
            args(&["--permission-mode", "bypassPermissions", "--verbose"])
        );
        assert_eq!(
            replayable("codex", &args(&["-s", "danger-full-access", "-a", "never"])),
            args(&["-s", "danger-full-access", "-a", "never"])
        );
        assert_eq!(
            replayable("claude", &args(&["--dangerously-skip-permissions"])),
            args(&["--dangerously-skip-permissions"])
        );
        assert_eq!(
            replayable("omp", &args(&["--model=opus", "--auto-approve"])),
            args(&["--model=opus", "--auto-approve"])
        );
    }

    #[test]
    fn drops_session_selection_from_an_earlier_resume() {
        assert_eq!(
            replayable(
                "claude",
                &args(&[
                    "--permission-mode",
                    "bypassPermissions",
                    "--resume",
                    "c1893fd1-3b1a-46d0-9b4f-a09cd2a42c8b",
                ])
            ),
            args(&["--permission-mode", "bypassPermissions"])
        );
        assert_eq!(
            replayable("copilot", &args(&["--allow-all-tools", "--resume=abc123"])),
            args(&["--allow-all-tools"])
        );
        assert_eq!(
            replayable("omp", &args(&["--model=opus", "--resume=abc123"])),
            args(&["--model=opus"])
        );
    }

    #[test]
    fn drops_session_subcommands_and_their_ids() {
        assert_eq!(
            replayable(
                "codex",
                &args(&[
                    "resume",
                    "01997f1e-4b4a-7c31-9c0e-2f1f0a3f0f11",
                    "--full-auto"
                ])
            ),
            args(&["--full-auto"])
        );
        // Global options can precede the subcommand; the id still follows a bare
        // word rather than an option, so both fall away.
        assert_eq!(
            replayable(
                "codex",
                &args(&[
                    "-m",
                    "gpt-5.6",
                    "resume",
                    "01997f1e-4b4a-7c31-9c0e-2f1f0a3f0f11"
                ])
            ),
            args(&["-m", "gpt-5.6"])
        );
    }

    #[test]
    fn drops_prompts_and_one_shot_options() {
        assert_eq!(
            replayable("claude", &args(&["fix the failing test"])),
            Vec::<String>::new()
        );
        assert_eq!(
            replayable("claude", &args(&["-p", "summarize this repo"])),
            Vec::<String>::new()
        );
        assert_eq!(
            replayable(
                "grok",
                &args(&["--always-approve", "--", "trailing prompt"])
            ),
            args(&["--always-approve"])
        );
    }

    #[test]
    fn keeps_options_herdr_has_never_heard_of() {
        assert_eq!(
            replayable(
                "claude",
                &args(&["--future-option", "value", "--future-flag"])
            ),
            args(&["--future-option", "value", "--future-flag"])
        );
        assert_eq!(
            replayable("something-else", &args(&["--flag", "value"])),
            args(&["--flag", "value"])
        );
    }

    #[test]
    fn keeps_only_the_first_value_of_a_repeated_option() {
        assert_eq!(
            replayable(
                "claude",
                &args(&["--add-dir", "/one", "/two", "--permission-mode", "plan"])
            ),
            args(&["--add-dir", "/one", "--permission-mode", "plan"])
        );
    }
}
