use std::io::{IsTerminal as _, Write as _};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::style::{Attribute, SetAttribute};
use crossterm::{cursor, execute, terminal};
use serde::Serialize;

use crate::client::endpoint::{EndpointCatalog, ProfileId};
use crate::client::TextEditor;

const HELP: &str = "Usage:
  herdr machine list [--json]
  herdr machine add <ssh-target> [--label <label>] [--remote-session <name>]
  herdr machine rename <profile-id> --label <label>
  herdr machine remove <profile-id>
  herdr machine enable <profile-id>
  herdr machine disable <profile-id>

Add prepares the remote Herdr installation and starts its server before saving.
Missing or incompatible installations require approval in an interactive terminal.
Changes apply automatically to open local Herdr clients.
Removing or disabling a machine leaves its remote sessions running.
Saved machines contain only a label, SSH target, explicit Herdr session, and enabled state.
SSH credentials and key material remain owned by OpenSSH.";

#[derive(Serialize)]
struct MachineListRow<'a> {
    id: &'a str,
    label: &'a str,
    target: &'a str,
    session: &'a str,
    enabled: bool,
    selected: bool,
}

pub(super) fn run_machine_command(args: &[String]) -> std::io::Result<i32> {
    match args.first().map(String::as_str) {
        Some("list") => list(&args[1..]),
        Some("add") => add(&args[1..]),
        Some("rename") => rename(&args[1..]),
        Some("remove") => remove(&args[1..]),
        Some("enable") => set_enabled(&args[1..], true),
        Some("disable") => set_enabled(&args[1..], false),
        Some("help" | "--help" | "-h") => {
            println!("{HELP}");
            Ok(0)
        }
        _ => {
            eprintln!("{HELP}");
            Ok(2)
        }
    }
}

fn list(args: &[String]) -> std::io::Result<i32> {
    let json = match args {
        [] => false,
        [flag] if flag == "--json" => true,
        _ => {
            eprintln!("usage: herdr machine list [--json]");
            return Ok(2);
        }
    };
    let catalog = load_catalog()?;
    let rows = catalog
        .ssh
        .iter()
        .map(|profile| MachineListRow {
            id: profile.id.as_str(),
            label: &profile.label,
            target: &profile.target,
            session: &profile.session,
            enabled: profile.enabled,
            selected: catalog.selected_profile.as_ref() == Some(&profile.id),
        })
        .collect::<Vec<_>>();
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&rows).map_err(std::io::Error::other)?
        );
        return Ok(0);
    }
    if rows.is_empty() {
        println!("No saved SSH machines.");
        return Ok(0);
    }
    for row in rows {
        let state = if row.enabled { "enabled" } else { "disabled" };
        println!(
            "{}\t{}\t{}\t{}\t{}",
            row.id, row.label, row.target, row.session, state
        );
    }
    Ok(0)
}

#[derive(Debug, PartialEq, Eq)]
struct AddArgs {
    target: String,
    label: Option<String>,
    session: Option<String>,
}

fn parse_add_args(args: &[String]) -> Result<AddArgs, String> {
    let args = super::expand_equals_args(args, &["--label", "--remote-session"]);
    let mut target = None;
    let mut label = None;
    let mut session = None;
    let mut index = 0;
    while index < args.len() {
        let (name, value) = match args[index].as_str() {
            "--label" | "--remote-session" => {
                let Some(value) = args.get(index + 1) else {
                    return Err(format!("missing value for {}", args[index]));
                };
                index += 2;
                (args[index - 2].as_str(), value.clone())
            }
            positional if !positional.starts_with('-') && target.is_none() => {
                target = Some(positional.to_owned());
                index += 1;
                continue;
            }
            unknown => {
                return Err(format!("unknown machine add option: {unknown}"));
            }
        };
        match name {
            "--label" if label.is_none() => label = Some(value),
            "--remote-session" if session.is_none() => session = Some(value),
            "--remote-session" => {
                return Err("--remote-session can only be specified once".into());
            }
            "--label" => {
                return Err("--label can only be specified once".into());
            }
            _ => unreachable!("validated machine add option"),
        }
    }
    let target = target.ok_or_else(|| {
        "usage: herdr machine add <ssh-target> [--label <label>] [--remote-session <name>]"
            .to_owned()
    })?;
    Ok(AddArgs {
        target,
        label,
        session,
    })
}

fn add(args: &[String]) -> std::io::Result<i32> {
    let AddArgs {
        target,
        label,
        session,
    } = match parse_add_args(args) {
        Ok(args) => args,
        Err(error) => {
            eprintln!("{error}");
            return Ok(2);
        }
    };
    let session = match session {
        Some(session) => session,
        None => match crate::remote::discover_running_ssh_sessions(&target) {
            Ok(sessions) => match select_remote_session(
                &sessions,
                &target,
                std::io::stdin().is_terminal() && std::io::stderr().is_terminal(),
            ) {
                Ok(session) => session,
                Err(error) => {
                    eprintln!("error: {error}; machine was not saved");
                    return Ok(1);
                }
            },
            Err(error) => {
                eprintln!("error: {error}; machine was not saved");
                crate::remote::print_saved_ssh_error_hint(&error, &target);
                return Ok(1);
            }
        },
    };
    let interactive = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
    let label = match label {
        Some(label) => label,
        None if interactive => {
            let default = default_machine_label(&target, &session);
            match prompt_machine_label(&default) {
                Ok(label) => label,
                Err(error) => {
                    eprintln!("error: {error}; machine was not saved");
                    return Ok(1);
                }
            }
        }
        None => default_machine_label(&target, &session),
    };
    let mut catalog = load_catalog()?;
    match catalog.add_ssh(label.clone(), &target, session.clone()) {
        Ok(_) => {}
        Err(error) => {
            eprintln!("error: {error}");
            return Ok(2);
        }
    }
    if let Err(error) = crate::remote::prepare_saved_ssh(&target, &session) {
        eprintln!("error: {error}; machine was not saved");
        crate::remote::print_saved_ssh_error_hint(&error, &target);
        return Ok(1);
    }
    // Setup can wait for human approval. Do not overwrite catalog edits made meanwhile.
    let mut catalog = load_catalog().map_err(|error| {
        std::io::Error::other(format!(
            "remote prepared, but machine was not saved: {error}"
        ))
    })?;
    let id = match catalog.add_ssh(label.clone(), target, session) {
        Ok(id) => id,
        Err(error) => {
            eprintln!("error: {error}");
            return Ok(2);
        }
    };
    store_catalog(&catalog).map_err(|error| {
        std::io::Error::other(format!(
            "remote prepared, but machine was not saved: {error}"
        ))
    })?;
    println!("Saved SSH machine \"{label}\" ({id}). Remote server is ready.");
    println!("Open Herdr clients connect automatically.");
    Ok(0)
}

fn default_machine_label(target: &str, session: &str) -> String {
    if session == crate::session::DEFAULT_SESSION_NAME {
        target.to_owned()
    } else {
        format!("{target}/{session}")
    }
}

fn prompt_machine_label(default: &str) -> std::io::Result<String> {
    let _raw_mode = RawModeGuard::enable()?;
    let mut output = std::io::stderr();
    let mut editor = TextEditor::from(default);
    render_machine_label_prompt(&mut output, &editor)?;
    loop {
        let Event::Key(key) = crossterm::event::read()? else {
            continue;
        };
        match machine_label_action(&mut editor, key) {
            LabelAction::Edit => render_machine_label_prompt(&mut output, &editor)?,
            LabelAction::Confirm => {
                editor.trim_and_accept();
                write!(output, "\r\n")?;
                return Ok(editor.to_string());
            }
            LabelAction::Cancel => {
                write!(output, "\r\n")?;
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "machine label entry cancelled",
                ));
            }
            LabelAction::Ignore => {}
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum LabelAction {
    Edit,
    Confirm,
    Cancel,
    Ignore,
}

fn machine_label_action(editor: &mut TextEditor, key: KeyEvent) -> LabelAction {
    if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
        return LabelAction::Ignore;
    }
    match key.code {
        KeyCode::Enter => LabelAction::Confirm,
        KeyCode::Esc => LabelAction::Cancel,
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => LabelAction::Cancel,
        _ if editor.handle_key(&key.into()).is_some() => LabelAction::Edit,
        _ => LabelAction::Ignore,
    }
}

fn render_machine_label_prompt(
    output: &mut impl std::io::Write,
    editor: &TextEditor,
) -> std::io::Result<()> {
    const PREFIX: &str = "Machine label: ";
    let width = terminal::size()?.0.saturating_sub(PREFIX.len() as u16);
    let (text, cursor_column) = editor.viewport(width);
    execute!(
        output,
        cursor::MoveToColumn(0),
        terminal::Clear(terminal::ClearType::CurrentLine)
    )?;
    write!(output, "{PREFIX}{text}")?;
    execute!(
        output,
        cursor::MoveToColumn(PREFIX.len() as u16 + cursor_column)
    )?;
    output.flush()
}

fn select_remote_session(
    sessions: &[String],
    target: &str,
    interactive: bool,
) -> std::io::Result<String> {
    match sessions {
        [] => return Ok(crate::session::DEFAULT_SESSION_NAME.to_owned()),
        [session] => return Ok(session.clone()),
        _ if !interactive => {
            return Err(std::io::Error::other(format!(
                "multiple remote Herdr sessions are running ({}); specify one with --remote-session",
                sessions.join(", ")
            )));
        }
        _ => {}
    }

    let _raw_mode = RawModeGuard::enable()?;
    let mut output = std::io::stderr();
    let mut selected = 0;
    render_remote_session_picker(&mut output, target, sessions, selected, false)?;
    loop {
        let Event::Key(key) = crossterm::event::read()? else {
            continue;
        };
        match remote_session_picker_action(selected, sessions.len(), key) {
            PickerAction::Select(index) => {
                selected = index;
                render_remote_session_picker(&mut output, target, sessions, selected, true)?;
            }
            PickerAction::Confirm => return Ok(sessions[selected].clone()),
            PickerAction::Cancel => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "remote session selection cancelled",
                ));
            }
            PickerAction::Ignore => {}
        }
    }
}

struct RawModeGuard;

impl RawModeGuard {
    fn enable() -> std::io::Result<Self> {
        terminal::enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
    }
}

#[derive(Debug, PartialEq, Eq)]
enum PickerAction {
    Select(usize),
    Confirm,
    Cancel,
    Ignore,
}

fn remote_session_picker_action(selected: usize, len: usize, key: KeyEvent) -> PickerAction {
    if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
        return PickerAction::Ignore;
    }
    match key.code {
        KeyCode::Up => PickerAction::Select(selected.checked_sub(1).unwrap_or(len - 1)),
        KeyCode::Down => PickerAction::Select((selected + 1) % len),
        KeyCode::Enter => PickerAction::Confirm,
        KeyCode::Esc => PickerAction::Cancel,
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => PickerAction::Cancel,
        _ => PickerAction::Ignore,
    }
}

fn render_remote_session_picker(
    output: &mut impl std::io::Write,
    target: &str,
    sessions: &[String],
    selected: usize,
    redraw: bool,
) -> std::io::Result<()> {
    if redraw {
        execute!(output, cursor::MoveUp((sessions.len() + 2) as u16))?;
    }
    execute!(output, terminal::Clear(terminal::ClearType::CurrentLine))?;
    writeln!(output, "Running sessions on {target}:")?;
    for (index, session) in sessions.iter().enumerate() {
        execute!(output, terminal::Clear(terminal::ClearType::CurrentLine))?;
        if index == selected {
            execute!(output, SetAttribute(Attribute::Bold))?;
            write!(output, "> {session}")?;
            execute!(output, SetAttribute(Attribute::Reset))?;
            writeln!(output)?;
        } else {
            writeln!(output, "  {session}")?;
        }
    }
    execute!(output, terminal::Clear(terminal::ClearType::CurrentLine))?;
    writeln!(output, "↑/↓ select · Enter confirm · Esc cancel")?;
    output.flush()
}

fn rename(args: &[String]) -> std::io::Result<i32> {
    let args = super::expand_equals_args(args, &["--label"]);
    let [raw_id, flag, label] = args.as_slice() else {
        eprintln!("usage: herdr machine rename <profile-id> --label <label>");
        return Ok(2);
    };
    if flag != "--label" {
        eprintln!("usage: herdr machine rename <profile-id> --label <label>");
        return Ok(2);
    }
    let id = match ProfileId::parse(raw_id.clone()) {
        Ok(id) => id,
        Err(error) => {
            eprintln!("error: {error}");
            return Ok(2);
        }
    };
    let mut catalog = load_catalog()?;
    match catalog.rename_ssh(&id, label) {
        Ok(true) => {}
        Ok(false) => {
            eprintln!("machine profile {id} was not found");
            return Ok(1);
        }
        Err(error) => {
            eprintln!("error: {error}");
            return Ok(2);
        }
    }
    store_catalog(&catalog)?;
    println!("Renamed SSH machine {id}.");
    Ok(0)
}

fn remove(args: &[String]) -> std::io::Result<i32> {
    let Some(id) = one_profile_id(args, "usage: herdr machine remove <profile-id>")? else {
        return Ok(2);
    };
    let mut catalog = load_catalog()?;
    let previous_selection = catalog.selected_profile.clone();
    if !catalog.remove_ssh(&id) {
        eprintln!("machine profile {id} was not found");
        return Ok(1);
    }
    store_catalog(&catalog)?;
    if catalog.selected_profile != previous_selection {
        catalog.store_selection().map_err(std::io::Error::other)?;
    }
    println!("Removed SSH machine {id}.");
    Ok(0)
}

fn set_enabled(args: &[String], enabled: bool) -> std::io::Result<i32> {
    let action = if enabled { "enable" } else { "disable" };
    let usage = format!("usage: herdr machine {action} <profile-id>");
    let Some(id) = one_profile_id(args, &usage)? else {
        return Ok(2);
    };
    let mut catalog = load_catalog()?;
    let previous_selection = catalog.selected_profile.clone();
    if !catalog.set_enabled(&id, enabled) {
        eprintln!("machine profile {id} was not found");
        return Ok(1);
    }
    store_catalog(&catalog)?;
    if catalog.selected_profile != previous_selection {
        catalog.store_selection().map_err(std::io::Error::other)?;
    }
    println!(
        "{} SSH machine {id}.",
        if enabled { "Enabled" } else { "Disabled" }
    );
    Ok(0)
}

fn one_profile_id(args: &[String], usage: &str) -> std::io::Result<Option<ProfileId>> {
    let [raw] = args else {
        eprintln!("{usage}");
        return Ok(None);
    };
    match ProfileId::parse(raw.clone()) {
        Ok(id) => Ok(Some(id)),
        Err(error) => {
            eprintln!("error: {error}");
            Ok(None)
        }
    }
}

fn load_catalog() -> std::io::Result<EndpointCatalog> {
    EndpointCatalog::load().map_err(std::io::Error::other)
}

fn store_catalog(catalog: &EndpointCatalog) -> std::io::Result<()> {
    catalog.store_profiles().map_err(std::io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_parser_preserves_values_across_argument_orders() {
        for (args, label, session) in [
            (vec!["workstation.coder"], None, None),
            (
                vec!["--label", "coder", "workstation.coder"],
                Some("coder"),
                None,
            ),
            (
                vec!["workstation.coder", "--label", "coder"],
                Some("coder"),
                None,
            ),
            (
                vec![
                    "--remote-session",
                    "agents",
                    "workstation.coder",
                    "--label",
                    "coder",
                ],
                Some("coder"),
                Some("agents"),
            ),
            (
                vec![
                    "--label=coder",
                    "--remote-session=agents",
                    "workstation.coder",
                ],
                Some("coder"),
                Some("agents"),
            ),
        ] {
            let args = args.into_iter().map(str::to_owned).collect::<Vec<_>>();
            assert_eq!(
                parse_add_args(&args).unwrap(),
                AddArgs {
                    target: "workstation.coder".into(),
                    label: label.map(str::to_owned),
                    session: session.map(str::to_owned),
                },
                "{args:?}"
            );
        }
    }

    #[test]
    fn add_parser_rejects_incomplete_duplicate_and_extra_arguments() {
        for args in [
            vec![],
            vec!["--label", "coder"],
            vec!["workstation.coder", "--label"],
            vec!["workstation.coder", "--label", "coder", "--remote-session"],
            vec!["--label", "coder", "--label", "other", "workstation.coder"],
            vec![
                "workstation.coder",
                "--label",
                "coder",
                "--remote-session",
                "a",
                "--remote-session",
                "b",
            ],
            vec!["--label", "coder", "workstation.coder", "other-host"],
            vec!["--unknown", "workstation.coder", "--label", "coder"],
            vec!["--label", "--remote-session", "agents", "workstation.coder"],
        ] {
            let args = args.into_iter().map(str::to_owned).collect::<Vec<_>>();
            assert!(parse_add_args(&args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn remote_session_picker_navigates_wraps_confirms_and_cancels() {
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        assert_eq!(
            remote_session_picker_action(0, 3, key(KeyCode::Up)),
            PickerAction::Select(2)
        );
        assert_eq!(
            remote_session_picker_action(2, 3, key(KeyCode::Down)),
            PickerAction::Select(0)
        );
        assert_eq!(
            remote_session_picker_action(1, 3, key(KeyCode::Enter)),
            PickerAction::Confirm
        );
        assert_eq!(
            remote_session_picker_action(1, 3, key(KeyCode::Esc)),
            PickerAction::Cancel
        );
        assert_eq!(
            remote_session_picker_action(
                1,
                3,
                KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
            ),
            PickerAction::Cancel
        );
    }

    #[test]
    fn machine_label_defaults_and_prefilled_editing() {
        assert_eq!(default_machine_label("jjdesktop", "default"), "jjdesktop");
        assert_eq!(
            default_machine_label("jjdesktop", "agents"),
            "jjdesktop/agents"
        );

        let mut editor = TextEditor::from("jjdesktop");
        for ch in "-test".chars() {
            assert_eq!(
                machine_label_action(
                    &mut editor,
                    KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE)
                ),
                LabelAction::Edit
            );
        }
        assert_eq!(editor.as_str(), "jjdesktop-test");
        assert_eq!(
            machine_label_action(
                &mut editor,
                KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
            ),
            LabelAction::Cancel
        );
    }

    #[test]
    fn remote_session_selection_defaults_selects_and_requires_a_noninteractive_choice() {
        assert_eq!(
            select_remote_session(&[], "host", false).unwrap(),
            "default"
        );
        assert_eq!(
            select_remote_session(&["agents".into()], "host", false).unwrap(),
            "agents"
        );
        assert!(
            select_remote_session(&["default".into(), "agents".into()], "host", false)
                .unwrap_err()
                .to_string()
                .contains("--remote-session")
        );
    }

    #[test]
    fn profile_id_parser_rejects_target_text() {
        assert!(one_profile_id(&["build.example".into()], "usage")
            .unwrap()
            .is_none());
    }

    #[test]
    fn list_rows_do_not_have_credential_fields() {
        let encoded = serde_json::to_string(&MachineListRow {
            id: "0123456789abcdef0123456789abcdef",
            label: "Build",
            target: "dev@build",
            session: "agents",
            enabled: true,
            selected: false,
        })
        .unwrap();
        assert!(!encoded.contains("password"));
        assert!(!encoded.contains("key"));
    }
}
