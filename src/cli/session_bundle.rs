//! `herdr session export|import`: portable session bundles.

use std::path::PathBuf;

use crate::api::schema::{EmptyParams, Method, PaneReadParams, ReadFormat, ReadSource, Request};
use crate::bundle::export::{ExportOptions, ScrollbackEntry};
use crate::bundle::import::{ImportOptions, ImportTarget};
use crate::bundle::stores::StoreRoots;
use crate::bundle::PathMap;

const EXPORT_USAGE: &str =
    "usage: herdr session export <dir> [--force] [--no-scrollback] [--no-agent-files] [--json]";
const IMPORT_USAGE: &str = "usage: herdr session import <dir> [--name NAME] [--map OLD=NEW] [--clone-missing] [--dry-run] [--force] [--json]";

pub(super) fn run_session_export(args: &[String]) -> std::io::Result<i32> {
    let parsed = match parse_export_args(args) {
        Ok(Some(parsed)) => parsed,
        Ok(None) => return Ok(0),
        Err(code) => return Ok(code),
    };
    let options = ExportOptions {
        output: PathBuf::from(&parsed.output),
        force: parsed.force,
        include_agent_files: !parsed.no_agent_files,
        include_scrollback: !parsed.no_scrollback,
    };
    let roots = StoreRoots::from_env();

    let result = if super::target::is_remote() {
        export_from_machine(&options, parsed.no_scrollback)
    } else {
        let session_dir = crate::session::data_dir();
        // Local export works from the saved layout alone; a reachable server
        // only adds scrollback. A stopped server is not an error here.
        let live_snapshot = fetch_api_snapshot().ok();
        if live_snapshot.is_some() {
            wait_for_first_save(&session_dir);
        }
        let scrollbacks = if parsed.no_scrollback {
            Vec::new()
        } else {
            live_snapshot
                .as_ref()
                .map(fetch_scrollbacks)
                .unwrap_or_default()
        };
        crate::bundle::export::export_persisted(
            &session_dir,
            crate::session::active_name()
                .as_deref()
                .unwrap_or("default"),
            roots.as_ref(),
            &scrollbacks,
            &options,
        )
        .map_err(std::io::Error::from)
    };

    match result {
        Ok(outcome) => {
            if parsed.json {
                super::_print_json(&serde_json::json!({
                    "exported": true,
                    "bundle": parsed.output,
                    "panes": outcome.panes,
                    "agents_copied": outcome.agents_copied,
                    "agents_missing_files": outcome.agents_missing_files,
                    "warnings": outcome.warnings,
                }));
            } else {
                for warning in &outcome.warnings {
                    eprintln!("warning: {warning}");
                }
                println!(
                    "saved {} pane(s), {} agent session(s) to {}",
                    outcome.panes, outcome.agents_copied, parsed.output
                );
                println!("bundle contains full agent transcripts - keep it private");
            }
            Ok(0)
        }
        Err(error) if super::server_not_running_was_reported(&error) => {
            if let Some(response) = super::server_not_running_reported_response(&error) {
                eprintln!(
                    "{}",
                    serde_json::to_string(&serde_json::json!({
                        "id": response.id,
                        "error": response.error,
                    }))
                    .unwrap_or_default()
                );
            }
            Ok(1)
        }
        Err(error) => {
            eprintln!("error: {error}");
            Ok(1)
        }
    }
}

fn export_from_machine(
    options: &ExportOptions,
    no_scrollback: bool,
) -> std::io::Result<crate::bundle::export::ExportOutcome> {
    let snapshot = fetch_api_snapshot()?;
    let scrollbacks = if no_scrollback {
        Vec::new()
    } else {
        fetch_scrollbacks(&snapshot)
    };
    let label = super::target::remote_label().unwrap_or_else(super::target::socket_label);
    crate::bundle::export::export_remote(&snapshot, &label, &scrollbacks, options)
        .map_err(std::io::Error::from)
}

fn fetch_api_snapshot() -> std::io::Result<crate::api::schema::SessionSnapshot> {
    let response = super::send_request(&Request {
        id: "cli:session:export:snapshot".into(),
        method: Method::SessionSnapshot(EmptyParams::default()),
    })?;
    if response.get("error").is_some() {
        return Err(std::io::Error::other(format!(
            "server returned an error for session.snapshot: {response}"
        )));
    }
    serde_json::from_value(response["result"]["snapshot"].clone())
        .map_err(|error| std::io::Error::other(format!("invalid session snapshot: {error}")))
}

/// A brand-new server writes its first `session.json` a few seconds after
/// the first change (autosave is debounced). When the server is reachable
/// but has not saved yet, wait briefly for that first save instead of
/// failing an export of a session that visibly exists.
fn wait_for_first_save(session_dir: &std::path::Path) {
    let session_file = session_dir.join("session.json");
    if session_file.exists() {
        return;
    }
    for _ in 0..30 {
        std::thread::sleep(std::time::Duration::from_millis(200));
        if session_file.exists() {
            return;
        }
    }
}

pub(super) fn run_session_import(args: &[String]) -> std::io::Result<i32> {
    let parsed = match parse_import_args(args) {
        Ok(Some(parsed)) => parsed,
        Ok(None) => return Ok(0),
        Err(code) => return Ok(code),
    };
    if super::target::is_remote() {
        eprintln!("error: session import always writes a local session; it cannot be combined with --machine");
        return Ok(2);
    }
    let bundle_dir = PathBuf::from(&parsed.bundle);
    let name = match parsed
        .name
        .clone()
        .or_else(|| default_import_name(&bundle_dir))
    {
        Some(name) => name,
        None => {
            eprintln!(
                "error: could not derive a session name from the bundle folder; pass --name NAME"
            );
            return Ok(2);
        }
    };
    let target_name = match crate::session::parse_target_name(&name) {
        Ok(target_name) => target_name,
        Err(message) => {
            eprintln!("error: {message}");
            return Ok(1);
        }
    };
    let display_name = target_name.clone().unwrap_or_else(|| "default".to_string());
    let running = crate::session::list_sessions()
        .map(|sessions| {
            sessions
                .iter()
                .any(|session| session.name == display_name && session.running)
        })
        .unwrap_or(false);
    let target = ImportTarget {
        name: display_name.clone(),
        data_dir: crate::session::data_dir_for(target_name.as_deref()),
        running,
    };
    let options = ImportOptions {
        maps: parsed.maps.clone(),
        clone_missing: parsed.clone_missing,
        dry_run: parsed.dry_run,
        force: parsed.force,
    };
    let roots = StoreRoots::from_env();
    match crate::bundle::import::import_bundle(&bundle_dir, &target, roots.as_ref(), &options) {
        Ok(outcome) => {
            if parsed.json {
                super::_print_json(&serde_json::json!({
                    "imported": !parsed.dry_run,
                    "dry_run": parsed.dry_run,
                    "session": outcome.session_name,
                    "panes": outcome.panes,
                    "agents_installed": outcome.agents_installed,
                    "agents_already_present": outcome.agents_already_present,
                    "actions": outcome.actions,
                    "notes": outcome.notes,
                }));
            } else {
                let prefix = if parsed.dry_run { "plan: " } else { "do:   " };
                for action in &outcome.actions {
                    println!("{prefix}{action}");
                }
                for note in &outcome.notes {
                    println!("note: {note}");
                }
                if parsed.dry_run {
                    println!("dry run: nothing was written");
                } else {
                    println!(
                        "imported {} pane(s) into session '{}'",
                        outcome.panes, outcome.session_name
                    );
                    if outcome.session_name == "default" {
                        println!("open it with: herdr");
                    } else {
                        println!(
                            "open it with: herdr session attach {}",
                            outcome.session_name
                        );
                    }
                }
            }
            Ok(0)
        }
        Err(error) => {
            eprintln!("error: {error}");
            Ok(1)
        }
    }
}

fn default_import_name(bundle_dir: &std::path::Path) -> Option<String> {
    let name = bundle_dir.file_name()?.to_str()?.to_string();
    crate::session::parse_target_name(&name).ok()?;
    Some(name)
}

struct ExportArgs {
    output: String,
    force: bool,
    no_scrollback: bool,
    no_agent_files: bool,
    json: bool,
}

fn parse_export_args(args: &[String]) -> Result<Option<ExportArgs>, i32> {
    if matches!(
        args.first().map(String::as_str),
        Some("help" | "--help" | "-h")
    ) {
        eprintln!("{EXPORT_USAGE}");
        return Ok(None);
    }
    let args = super::expand_equals_args(args, &["--output", "-o"]);
    let mut parsed = ExportArgs {
        output: String::new(),
        force: false,
        no_scrollback: false,
        no_agent_files: false,
        json: false,
    };
    let mut output: Option<String> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--force" => parsed.force = true,
            "--no-scrollback" => parsed.no_scrollback = true,
            "--no-agent-files" => parsed.no_agent_files = true,
            "--json" => parsed.json = true,
            "--output" | "-o" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("{EXPORT_USAGE}");
                    return Err(2);
                };
                if output.is_some() {
                    eprintln!("{EXPORT_USAGE}");
                    return Err(2);
                }
                output = Some(value.clone());
                index += 1;
            }
            other if other.starts_with('-') && other != "-" => {
                eprintln!("unknown option: {other}");
                eprintln!("{EXPORT_USAGE}");
                return Err(2);
            }
            value => {
                if output.is_some() {
                    eprintln!("{EXPORT_USAGE}");
                    return Err(2);
                }
                output = Some(value.to_string());
            }
        }
        index += 1;
    }
    let Some(output_value) = output else {
        eprintln!("{EXPORT_USAGE}");
        return Err(2);
    };
    parsed.output = output_value;
    Ok(Some(parsed))
}

struct ImportArgs {
    bundle: String,
    name: Option<String>,
    maps: Vec<PathMap>,
    clone_missing: bool,
    dry_run: bool,
    force: bool,
    json: bool,
}

fn parse_import_args(args: &[String]) -> Result<Option<ImportArgs>, i32> {
    if matches!(
        args.first().map(String::as_str),
        Some("help" | "--help" | "-h")
    ) {
        eprintln!("{IMPORT_USAGE}");
        return Ok(None);
    }
    let args = super::expand_equals_args(args, &["--name", "--map"]);
    let mut parsed = ImportArgs {
        bundle: String::new(),
        name: None,
        maps: Vec::new(),
        clone_missing: false,
        dry_run: false,
        force: false,
        json: false,
    };
    let mut bundle: Option<String> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--clone-missing" => parsed.clone_missing = true,
            "--dry-run" => parsed.dry_run = true,
            "--force" => parsed.force = true,
            "--json" => parsed.json = true,
            "--name" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("{IMPORT_USAGE}");
                    return Err(2);
                };
                parsed.name = Some(value.clone());
                index += 1;
            }
            "--map" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("{IMPORT_USAGE}");
                    return Err(2);
                };
                match PathMap::parse(value) {
                    Ok(map) => parsed.maps.push(map),
                    Err(error) => {
                        eprintln!("error: {error}");
                        return Err(2);
                    }
                }
                index += 1;
            }
            other if other.starts_with('-') && other != "-" => {
                eprintln!("unknown option: {other}");
                eprintln!("{IMPORT_USAGE}");
                return Err(2);
            }
            value => {
                if bundle.is_some() {
                    eprintln!("{IMPORT_USAGE}");
                    return Err(2);
                }
                bundle = Some(value.to_string());
            }
        }
        index += 1;
    }
    let Some(bundle_value) = bundle else {
        eprintln!("{IMPORT_USAGE}");
        return Err(2);
    };
    parsed.bundle = bundle_value;
    Ok(Some(parsed))
}

fn fetch_scrollbacks(snapshot: &crate::api::schema::SessionSnapshot) -> Vec<ScrollbackEntry> {
    let mut entries = Vec::new();
    for pane in &snapshot.panes {
        let response = super::send_request_unchecked(&Request {
            id: format!("cli:session:export:read:{}", pane.pane_id),
            method: Method::PaneRead(PaneReadParams {
                pane_id: pane.pane_id.clone(),
                source: ReadSource::RecentUnwrapped,
                lines: None,
                format: ReadFormat::Text,
                strip_ansi: true,
                intent: crate::api::schema::ReadIntent::Passive,
            }),
        });
        if let Ok(response) = response {
            if let Some(text) = response["result"]["read"]["text"].as_str() {
                entries.push(ScrollbackEntry {
                    pane_id: pane.pane_id.clone(),
                    text: text.to_string(),
                });
            }
        }
    }
    entries
}
