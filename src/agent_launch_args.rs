//! Safe replayable argument filtering for agent session restore.
//!
//! When Herdr restores a native agent session (e.g. Claude Code), it resumes
//! the session with the agent's native resume flag (such as `claude --resume <id>`).
//! However, any flags the agent was originally launched with (such as
//! `--dangerously-skip-permissions` or `--permission-mode bypassPermissions`)
//! must be preserved across restore so unattended agents do not freeze at approval prompts.
//!
//! At the same time, we must carefully drop:
//! - Session selectors (`--resume`, `-r`, `--continue`, `-c`, `--session`, `--session-id`)
//! - One-shot options and prompts (`--print`, `-p`, `--prompt`)
//! - Trailing positional arguments (e.g. initial prompt strings)

/// Returns the filtered list of arguments that are safe to replay when resuming `agent`.
pub fn replayable_args(agent: &str, raw_args: &[String]) -> Vec<String> {
    let dropped = dropped_options_for_agent(agent);
    let mut kept: Vec<String> = Vec::new();
    let mut expect_value_for_kept_opt = false;

    for arg in raw_args {
        if arg == "--" {
            // Positional arguments after double-dash should not be replayed
            break;
        }

        if let Some(opt_name) = extract_option_name(arg) {
            let has_inline_val = opt_name.len() < arg.len(); // e.g. --opt=val
            if dropped.contains(&opt_name) {
                expect_value_for_kept_opt = false;
                continue;
            }
            kept.push(arg.clone());
            expect_value_for_kept_opt = !has_inline_val;
        } else {
            // Bare positional argument
            if expect_value_for_kept_opt {
                kept.push(arg.clone());
            }
            expect_value_for_kept_opt = false;
        }
    }

    kept
}

fn extract_option_name(arg: &str) -> Option<&str> {
    if !arg.starts_with('-') || arg == "-" || arg == "--" {
        return None;
    }
    Some(arg.split('=').next().unwrap_or(arg))
}

fn dropped_options_for_agent(agent: &str) -> &'static [&'static str] {
    match agent {
        "claude" => &[
            "-r",
            "--resume",
            "-c",
            "--continue",
            "-p",
            "--print",
            "--prompt",
            "--session-id",
        ],
        "codex" => &["--prompt", "-p", "--resume", "-r"],
        "copilot" => &["--resume", "-r", "--prompt", "-p"],
        "droid" => &["--resume", "-r", "--prompt", "-p"],
        "devin" => &["--resume", "-r", "--prompt", "-p"],
        "kimi" => &["--session", "-s", "--prompt", "-p"],
        "mastracode" => &["--thread", "-t", "--prompt", "-p"],
        "pi" => &["--session", "-s", "--prompt", "-p"],
        "omp" => &["--resume", "-r", "--prompt", "-p"],
        "hermes" => &["--resume", "-r", "--prompt", "-p"],
        "opencode" => &["--session", "-s", "--prompt", "-p"],
        "qodercli" => &["--resume", "-r", "--prompt", "-p"],
        "qwen" => &["--resume", "-r", "--prompt", "-p"],
        "kilo" => &["--session", "-s", "--prompt", "-p"],
        "cursor" => &["--resume", "-r", "--prompt", "-p"],
        "agy" => &["--conversation", "-c", "--prompt", "-p"],
        "grok" => &["--resume", "-r", "--prompt", "-p"],
        _ => &[
            "-r",
            "--resume",
            "-c",
            "--continue",
            "-s",
            "--session",
            "-p",
            "--print",
            "--prompt",
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_claude_replayable_preserves_permission_and_model_flags() {
        let raw = vec![
            "--dangerously-skip-permissions".into(),
            "--model".into(),
            "claude-opus-5".into(),
            "--print".into(),
            "do not replay this prompt".into(),
        ];
        let replay = replayable_args("claude", &raw);
        assert_eq!(
            replay,
            vec![
                "--dangerously-skip-permissions",
                "--model",
                "claude-opus-5"
            ]
        );
    }

    #[test]
    fn test_claude_replayable_strips_resume_and_prompt() {
        let raw = vec![
            "--resume".into(),
            "old-session-id".into(),
            "--permission-mode".into(),
            "bypassPermissions".into(),
            "initial positional prompt".into(),
        ];
        let replay = replayable_args("claude", &raw);
        assert_eq!(
            replay,
            vec!["--permission-mode", "bypassPermissions"]
        );
    }

    #[test]
    fn test_inline_option_values_preserved() {
        let raw = vec![
            "--model=claude-3-7-sonnet".into(),
            "--resume=12345".into(),
            "--verbose".into(),
        ];
        let replay = replayable_args("claude", &raw);
        assert_eq!(
            replay,
            vec!["--model=claude-3-7-sonnet", "--verbose"]
        );
    }
}
