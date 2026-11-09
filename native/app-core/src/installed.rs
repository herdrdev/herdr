//! Which agent CLIs are installed on this computer and which models they offer.
//!
//! Installed-ness is asked of the login shell, as the desktop app does (a window opened from a launcher does not
//! inherit the terminal's `PATH`), and of `shepherd vendors list`, which says whether each configured vendor
//! can run. Models come from the vendor's own config entry, from the agent itself when it can list them
//! (`agy models`) and, for the model it would use by default, from the agent's own settings file (only that one
//! key is read).

use shepherd_core::config::Vendor;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Agents Shepherd knows by name (each is also the executable `shepherd agent start --kind` expects).
pub const KNOWN_KINDS: &[&str] = &[
    "pi",
    "claude",
    "codex",
    "gemini",
    "cursor",
    "devin",
    "agy",
    "cline",
    "omp",
    "mastracode",
    "opencode",
    "copilot",
    "kimi",
    "kiro",
    "droid",
    "amp",
    "grok",
    "hermes",
    "kilo",
    "qodercli",
    "qwen",
    "letta",
    "maki",
    "muse",
];

/// One agent CLI, as the Models tab lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledRow {
    /// The executable (`claude`, `agy`, …).
    pub command: String,
    /// Where it lives; `None` when it is not installed.
    pub path: Option<String>,
    /// The name of the configured vendor that runs it, if any.
    pub vendor: Option<String>,
    /// What `shepherd vendors list` says about the vendor (`available`, …), when it is configured.
    pub state: Option<String>,
    /// The model the agent would use, from its own settings.
    pub default_model: Option<String>,
    /// The models known for it: its vendor entry's list plus what the agent itself lists.
    pub models: Vec<String>,
}

impl InstalledRow {
    pub fn installed(&self) -> bool {
        self.path.is_some()
    }
}

struct Output {
    stdout: String,
    timed_out: bool,
}

/// Runs a command and waits at most `limit` for it.
fn run(mut cmd: Command, limit: Duration) -> Result<Output, String> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let started = Instant::now();
    loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(_) => break,
            None if started.elapsed() > limit => {
                let _ = child.kill();
                let _ = child.wait();
                return Ok(Output {
                    stdout: String::new(),
                    timed_out: true,
                });
            }
            None => std::thread::sleep(Duration::from_millis(25)),
        }
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    Ok(Output {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        timed_out: false,
    })
}

/// `command<TAB>path` per line (what the shell prints). Aliases and builtins print a bare word instead of a path,
/// so only absolute paths count as installed.
pub fn parse_installed(stdout: &str) -> HashMap<String, String> {
    stdout
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .filter(|(_, path)| path.starts_with('/'))
        .map(|(cmd, path)| (cmd.to_owned(), path.trim().to_owned()))
        .collect()
}

fn login_shell() -> String {
    std::env::var("SHEPHERD_SHELL")
        .ok()
        .or_else(|| std::env::var("SHELL").ok())
        .filter(|s| Path::new(s).is_file())
        .unwrap_or_else(|| "/bin/sh".into())
}

/// Where each of `commands` lives, as the login shell sees it. Commands travel as positional arguments, so
/// nothing is spliced into the script.
fn ask_shell(commands: &[String]) -> HashMap<String, String> {
    let mut cmd = Command::new(login_shell());
    cmd.arg("-lc")
        .arg(r#"for c in "$@"; do p=$(command -v "$c" 2>/dev/null) && printf '%s\t%s\n' "$c" "$p"; done"#)
        .arg("sh")
        .args(commands);
    match run(cmd, Duration::from_secs(8)) {
        Ok(out) if !out.timed_out => parse_installed(&out.stdout),
        _ => HashMap::new(),
    }
}

/// A command found on this process's own `PATH` (a slow or odd shell must not hide plainly installed agents).
fn on_process_path(command: &str) -> Option<String> {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .map(|dir| dir.join(command))
        .find(|candidate| candidate.is_file())
        .map(|f| f.display().to_string())
}

/// `id<TAB>label` per line, as `agy models` prints it; lines without a tab (progress text) are skipped.
pub fn parse_models_tsv(text: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    text.lines()
        .filter_map(|line| line.split_once('\t'))
        .map(|(id, _)| id.trim())
        .filter(|id| {
            !id.is_empty() && !id.contains(char::is_whitespace) && seen.insert(id.to_string())
        })
        .map(str::to_owned)
        .collect()
}

/// The agents that can list their own models, and how to ask them.
fn live_models_command(kind: &str) -> Option<&'static [&'static str]> {
    match kind {
        "agy" => Some(&["agy", "models"]),
        _ => None,
    }
}

fn live_models(kind: &str) -> Vec<String> {
    let Some(argv) = live_models_command(kind) else {
        return Vec::new();
    };
    let mut cmd = Command::new(login_shell());
    cmd.arg("-lc").arg(r#"exec "$@""#).arg("sh").args(argv);
    match run(cmd, Duration::from_secs(25)) {
        Ok(out) if !out.timed_out => parse_models_tsv(&out.stdout),
        _ => Vec::new(),
    }
}

/// The `model` key of Claude's `settings.json`.
pub fn claude_default_model(settings_json: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(settings_json)
        .ok()?
        .get("model")?
        .as_str()
        .map(str::to_owned)
}

/// The top-level `model = "…"` of Codex's `config.toml` (keys inside a `[table]` do not count).
pub fn codex_default_model(config_toml: &str) -> Option<String> {
    for line in config_toml.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            return None;
        }
        if let Some(rest) = line.strip_prefix("model") {
            let value = rest.trim_start().strip_prefix('=')?.trim();
            return value
                .strip_prefix('"')
                .and_then(|v| v.split('"').next())
                .map(str::to_owned);
        }
    }
    None
}

/// The model the agent itself would use, from its own settings file.
pub fn agent_default_model(kind: &str, home: &Path) -> Option<String> {
    match kind {
        "claude" => {
            claude_default_model(&std::fs::read_to_string(home.join(".claude/settings.json")).ok()?)
        }
        "codex" => {
            codex_default_model(&std::fs::read_to_string(home.join(".codex/config.toml")).ok()?)
        }
        _ => None,
    }
}

/// `shepherd vendors list --json`: the state of each configured vendor, by command.
fn vendor_states() -> HashMap<String, String> {
    let binary = std::env::var_os("SHEPHERD_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("shepherd"));
    let mut cmd = Command::new(binary);
    cmd.args(["vendors", "list", "--json"]);
    match run(cmd, Duration::from_secs(8)) {
        Ok(out) if !out.timed_out => parse_vendor_states(&out.stdout),
        _ => HashMap::new(),
    }
}

/// The reply of `shepherd vendors list --json`: `[{"command":"claude","name":"claude","state":"available"}, …]`.
pub fn parse_vendor_states(json: &str) -> HashMap<String, String> {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|v| v.as_array().cloned())
        .map(|list| {
            list.iter()
                .filter_map(|v| {
                    Some((
                        v["command"].as_str()?.to_owned(),
                        v["state"].as_str()?.to_owned(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Puts what was found together: one row per agent that is installed or has a vendor, installed ones first.
pub fn assemble(
    vendors: &BTreeMap<String, Vendor>,
    paths: &HashMap<String, String>,
    states: &HashMap<String, String>,
    live: &HashMap<String, Vec<String>>,
    defaults: &HashMap<String, String>,
) -> Vec<InstalledRow> {
    let mut commands: Vec<String> = KNOWN_KINDS.iter().map(|k| (*k).to_owned()).collect();
    for v in vendors.values() {
        let c = v.command.trim();
        if !c.is_empty() && !commands.iter().any(|k| k == c) {
            commands.push(c.to_owned());
        }
    }
    let mut rows: Vec<InstalledRow> = commands
        .into_iter()
        .filter_map(|command| {
            let vendor = vendors.iter().find(|(_, v)| v.command == command);
            let path = paths.get(&command).cloned();
            if path.is_none() && vendor.is_none() {
                return None;
            }
            let mut models: Vec<String> = vendor.map(|(_, v)| v.models.clone()).unwrap_or_default();
            for m in live
                .get(&command)
                .into_iter()
                .flatten()
                .chain(defaults.get(&command))
            {
                if !models.contains(m) {
                    models.push(m.clone());
                }
            }
            Some(InstalledRow {
                state: states.get(&command).cloned(),
                default_model: defaults.get(&command).cloned(),
                vendor: vendor.map(|(name, _)| name.clone()),
                models,
                path,
                command,
            })
        })
        .collect();
    rows.sort_by_key(|r| (!r.installed(), r.vendor.is_none(), r.command.clone()));
    rows
}

/// The quick look at this computer: the agents installed, the vendors' states and the default models. It asks a
/// login shell and `shepherd`, so call it off the UI thread; the models an agent lists itself come later, from
/// [`live_for`], because some agents take seconds to answer.
pub fn detect(vendors: &BTreeMap<String, Vendor>) -> Vec<InstalledRow> {
    let mut commands: Vec<String> = KNOWN_KINDS.iter().map(|k| (*k).to_owned()).collect();
    for v in vendors.values() {
        let c = v.command.trim();
        if !c.is_empty() && !c.contains(['\n', '\t', '\0']) && !commands.iter().any(|k| k == c) {
            commands.push(c.to_owned());
        }
    }
    let mut paths = ask_shell(&commands);
    for c in &commands {
        if !paths.contains_key(c) {
            if let Some(p) = on_process_path(c) {
                paths.insert(c.clone(), p);
            }
        }
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let defaults: HashMap<String, String> = paths
        .keys()
        .filter_map(|c| agent_default_model(c, &home).map(|m| (c.clone(), m)))
        .collect();
    assemble(
        vendors,
        &paths,
        &vendor_states(),
        &HashMap::new(),
        &defaults,
    )
}

/// The models the installed agents list themselves, by command. Slow for some (they may call the network).
pub fn live_for(rows: &[InstalledRow]) -> HashMap<String, Vec<String>> {
    rows.iter()
        .filter(|r| r.installed())
        .filter_map(|r| {
            let models = live_models(&r.command);
            (!models.is_empty()).then(|| (r.command.clone(), models))
        })
        .collect()
}

/// Adds what the agents listed to the rows' models, without repeating any.
pub fn merge_live(rows: &mut [InstalledRow], live: &HashMap<String, Vec<String>>) {
    for row in rows {
        for model in live.get(&row.command).into_iter().flatten() {
            if !row.models.contains(model) {
                row.models.push(model.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vendor(command: &str, models: &[&str]) -> Vendor {
        Vendor {
            command: command.into(),
            models: models.iter().map(|m| (*m).to_owned()).collect(),
            ..Vendor::default()
        }
    }

    #[test]
    fn the_shells_answer_keeps_only_absolute_paths() {
        let found = parse_installed("claude\t/home/u/.local/bin/claude\nll\talias ll='ls -l'\ncodex\t/usr/bin/codex\nnoise\n");
        assert_eq!(found.len(), 2);
        assert_eq!(found["claude"], "/home/u/.local/bin/claude");
        assert!(!found.contains_key("ll"));
    }

    #[test]
    fn the_models_an_agent_lists_are_read_and_progress_text_is_skipped() {
        let out = "loading…\ngemini-2.5-pro\tGemini 2.5 Pro\ngemini-2.5-flash\t\ngemini-2.5-pro\tdup\nhas space\tx\n";
        assert_eq!(
            parse_models_tsv(out),
            ["gemini-2.5-pro", "gemini-2.5-flash"]
        );
    }

    #[test]
    fn the_default_model_is_the_one_key_of_the_agents_own_settings() {
        assert_eq!(
            claude_default_model(r#"{"theme":"dark","model":"opus"}"#).as_deref(),
            Some("opus")
        );
        assert_eq!(claude_default_model("{}"), None);
        assert_eq!(
            codex_default_model("model = \"gpt-5\"\nmodel_provider = \"x\"\n").as_deref(),
            Some("gpt-5")
        );
        // a `model` inside a table belongs to something else
        assert_eq!(
            codex_default_model("[profiles.a]\nmodel = \"gpt-4\"\n"),
            None
        );
        assert_eq!(codex_default_model(""), None);
    }

    #[test]
    fn what_shepherd_says_about_its_vendors_is_read_by_command() {
        let states = parse_vendor_states(
            r#"[{"command":"claude","name":"claude","state":"available"},{"command":"agy","name":"gemini","state":"missing"}]"#,
        );
        assert_eq!(states["claude"], "available");
        assert_eq!(states["agy"], "missing");
        assert!(parse_vendor_states("not json").is_empty());
    }

    #[test]
    fn the_rows_join_installed_agents_vendors_and_models() {
        let vendors: BTreeMap<String, Vendor> = [
            ("claude".to_owned(), vendor("claude", &["opus", "sonnet"])),
            ("gemini".to_owned(), vendor("agy", &["gemini-2.5-pro"])),
            ("ghost".to_owned(), vendor("ghost-cli", &[])),
        ]
        .into();
        let paths: HashMap<String, String> = [
            ("claude".to_owned(), "/bin/claude".to_owned()),
            ("agy".to_owned(), "/bin/agy".to_owned()),
            ("codex".to_owned(), "/bin/codex".to_owned()),
        ]
        .into();
        let states: HashMap<String, String> =
            [("claude".to_owned(), "available".to_owned())].into();
        let live: HashMap<String, Vec<String>> = [(
            "agy".to_owned(),
            vec!["gemini-2.5-pro".to_owned(), "gemini-2.5-flash".to_owned()],
        )]
        .into();
        let defaults: HashMap<String, String> = [("claude".to_owned(), "haiku".to_owned())].into();

        let rows = assemble(&vendors, &paths, &states, &live, &defaults);
        let names: Vec<_> = rows.iter().map(|r| r.command.as_str()).collect();
        // installed with a vendor first, then installed without one, then a vendor whose command is missing
        assert_eq!(names, ["agy", "claude", "codex", "ghost-cli"]);
        let claude = &rows[1];
        assert_eq!(claude.vendor.as_deref(), Some("claude"));
        assert_eq!(claude.state.as_deref(), Some("available"));
        assert_eq!(
            claude.models,
            ["opus", "sonnet", "haiku"],
            "the default is added once"
        );
        assert_eq!(
            rows[0].models,
            ["gemini-2.5-pro", "gemini-2.5-flash"],
            "no duplicates from the live list"
        );
        assert_eq!(rows[2].vendor, None);
        assert!(!rows[3].installed() && rows[3].vendor.as_deref() == Some("ghost"));
    }

    #[test]
    fn what_an_agent_lists_later_is_added_without_repeats() {
        let row = |command: &str, models: &[&str]| InstalledRow {
            command: command.into(),
            path: Some(format!("/bin/{command}")),
            vendor: None,
            state: None,
            default_model: None,
            models: models.iter().map(|m| (*m).to_owned()).collect(),
        };
        let mut rows = vec![row("agy", &["gemini-2.5-pro"]), row("claude", &["opus"])];
        let live: HashMap<String, Vec<String>> = [(
            "agy".to_owned(),
            vec!["gemini-2.5-pro".to_owned(), "gemini-2.5-flash".to_owned()],
        )]
        .into();
        merge_live(&mut rows, &live);
        assert_eq!(rows[0].models, ["gemini-2.5-pro", "gemini-2.5-flash"]);
        assert_eq!(
            rows[1].models,
            ["opus"],
            "an agent with nothing listed is untouched"
        );
        // only installed agents are asked, and only those that can list models answer
        let missing = InstalledRow {
            path: None,
            ..row("agy", &[])
        };
        assert!(live_for(&[missing]).is_empty());
    }
}
