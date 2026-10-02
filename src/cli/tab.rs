use std::collections::HashMap;

use crate::api::schema::{
    TabCreateParams, TabListParams, TabParkParams, TabPinMode, TabPinParams, TabPrioMode,
    TabPrioParams, TabRenameParams,
};

pub(super) fn run_tab_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        print_tab_help();
        return Ok(2);
    };

    match subcommand {
        "list" => tab_list(&args[1..]),
        "create" => tab_create(&args[1..]),
        "get" => tab_get(&args[1..]),
        "focus" => tab_focus(&args[1..]),
        "pin" => tab_pin(&args[1..], TabPinMode::Pin),
        "unpin" => tab_pin(&args[1..], TabPinMode::Unpin),
        "park" => tab_park(&args[1..], true),
        "unpark" => tab_park(&args[1..], false),
        "rename" => tab_rename(&args[1..]),
        "prio" => tab_prio(&args[1..]),
        "close" => tab_close(&args[1..]),
        "help" | "--help" | "-h" => {
            print_tab_help();
            Ok(0)
        }
        _ => {
            print_tab_help();
            Ok(2)
        }
    }
}

fn tab_list(args: &[String]) -> std::io::Result<i32> {
    let mut workspace_id = None;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--workspace" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --workspace");
                    return Ok(2);
                };
                workspace_id = Some(super::normalize_workspace_id(value));
                index += 2;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    super::runtime::tab_list(TabListParams { workspace_id })
}

fn tab_create(args: &[String]) -> std::io::Result<i32> {
    let mut workspace_id = None;
    let mut cwd = None;
    let mut focus = false;
    let mut label = None;
    let mut env = HashMap::new();
    let mut work_context = None;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--workspace" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --workspace");
                    return Ok(2);
                };
                workspace_id = Some(super::normalize_workspace_id(value));
                index += 2;
            }
            "--cwd" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --cwd");
                    return Ok(2);
                };
                cwd = Some(value.clone());
                index += 2;
            }
            "--label" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --label");
                    return Ok(2);
                };
                label = Some(value.clone());
                index += 2;
            }
            "--focus" => {
                focus = true;
                index += 1;
            }
            "--no-focus" => {
                focus = false;
                index += 1;
            }
            "--env" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --env");
                    return Ok(2);
                };
                let (key, value) = match super::parse_env_assignment(value) {
                    Ok(pair) => pair,
                    Err(err) => {
                        eprintln!("{err}");
                        return Ok(2);
                    }
                };
                env.insert(key, value);
                index += 2;
            }
            other => match super::parse_spawn_work_context_arg(args, index, &mut work_context) {
                Ok(Some(next)) => index = next,
                Ok(None) => {
                    eprintln!("unknown option: {other}");
                    return Ok(2);
                }
                Err(message) => {
                    eprintln!("{message}");
                    return Ok(2);
                }
            },
        }
    }

    super::runtime::tab_create(TabCreateParams {
        workspace_id,
        cwd,
        focus,
        label,
        env,
        work_context,
    })
}

fn tab_get(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_tab_id) = args.first() else {
        eprintln!("usage: herdr tab get <tab_id>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: herdr tab get <tab_id>");
        return Ok(2);
    }

    super::runtime::tab_get(super::normalize_tab_id(raw_tab_id))
}

fn tab_focus(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_tab_id) = args.first() else {
        eprintln!("usage: herdr tab focus <tab_id>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: herdr tab focus <tab_id>");
        return Ok(2);
    }

    super::runtime::tab_focus(super::normalize_tab_id(raw_tab_id))
}

fn tab_pin(args: &[String], mode: TabPinMode) -> std::io::Result<i32> {
    let Some(params) = parse_tab_pin_args(args, mode) else {
        eprintln!(
            "usage: herdr tab {} <tab_id>",
            match mode {
                TabPinMode::Pin => "pin",
                _ => "unpin",
            }
        );
        return Ok(2);
    };
    super::runtime::tab_pin(params)
}

fn parse_tab_pin_args(args: &[String], mode: TabPinMode) -> Option<TabPinParams> {
    let [raw_tab_id] = args else {
        return None;
    };
    Some(TabPinParams {
        tab_id: super::normalize_tab_id(raw_tab_id),
        mode,
    })
}

fn tab_park(args: &[String], parked: bool) -> std::io::Result<i32> {
    let Some(params) = parse_tab_park_args(args, parked) else {
        eprintln!(
            "usage: herdr tab {} <tab_id>",
            if parked { "park" } else { "unpark" }
        );
        return Ok(2);
    };
    super::runtime::tab_park(params)
}

fn parse_tab_park_args(args: &[String], parked: bool) -> Option<TabParkParams> {
    let [tab_id] = args else { return None };
    Some(TabParkParams {
        tab_id: tab_id.clone(),
        parked,
    })
}

fn tab_rename(args: &[String]) -> std::io::Result<i32> {
    let Some(params) = parse_tab_rename_args(args) else {
        eprintln!("usage: herdr tab rename <tab_id> <label>|--clear");
        return Ok(2);
    };

    super::runtime::tab_rename(params)
}

fn parse_tab_rename_args(args: &[String]) -> Option<TabRenameParams> {
    let (raw_tab_id, rest) = args.split_first()?;
    if rest.is_empty() {
        return None;
    }

    // Mirrors `agent rename --clear`: the flag stands in for the label rather
    // than sitting beside it, so the two can never disagree. A label may contain
    // spaces, so anything past the first word is joined back together.
    let label = if rest == ["--clear"] {
        None
    } else {
        Some(rest.join(" "))
    };

    Some(TabRenameParams {
        tab_id: super::normalize_tab_id(raw_tab_id),
        label,
    })
}

fn tab_prio(args: &[String]) -> std::io::Result<i32> {
    let params = match parse_tab_prio_args(args) {
        Ok(params) => params,
        Err(message) => {
            eprintln!("{message}");
            eprintln!("usage: herdr tab prio [<tab_id>|--tab ID|--current] [--toggle|--on|--off]");
            return Ok(2);
        }
    };

    super::runtime::tab_prio(params)
}

fn parse_tab_prio_args(args: &[String]) -> Result<TabPrioParams, String> {
    let mut tab_id = None;
    let mut mode = TabPrioMode::Toggle;
    let mut mode_seen = false;
    let mut index = 0;
    if args
        .first()
        .is_some_and(|arg| !arg.as_str().starts_with("--"))
    {
        tab_id = args.first().map(|arg| super::normalize_tab_id(arg));
        index = 1;
    }
    while index < args.len() {
        match args[index].as_str() {
            "--tab" => {
                let Some(value) = args.get(index + 1) else {
                    return Err("missing value for --tab".into());
                };
                tab_id = Some(super::normalize_tab_id(value));
                index += 2;
            }
            "--current" => {
                tab_id = None;
                index += 1;
            }
            "--toggle" => {
                if mode_seen {
                    return Err("provide only one of --toggle, --on, or --off".into());
                }
                mode = TabPrioMode::Toggle;
                mode_seen = true;
                index += 1;
            }
            "--on" => {
                if mode_seen {
                    return Err("provide only one of --toggle, --on, or --off".into());
                }
                mode = TabPrioMode::On;
                mode_seen = true;
                index += 1;
            }
            "--off" => {
                if mode_seen {
                    return Err("provide only one of --toggle, --on, or --off".into());
                }
                mode = TabPrioMode::Off;
                mode_seen = true;
                index += 1;
            }
            other => return Err(format!("unknown option: {other}")),
        }
    }

    Ok(TabPrioParams { tab_id, mode })
}

fn tab_close(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_tab_id) = args.first() else {
        eprintln!("usage: herdr tab close <tab_id>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: herdr tab close <tab_id>");
        return Ok(2);
    }

    super::runtime::tab_close(super::normalize_tab_id(raw_tab_id))
}

fn print_tab_help() {
    eprintln!("herdr tab commands:");
    eprintln!("  herdr tab list [--workspace <workspace_id>]");
    eprintln!(
        "  herdr tab create [--workspace <workspace_id>] [--cwd PATH] [--label TEXT] [--env KEY=VALUE] [--ticket ID] [--pr URL --branch BRANCH --role ROLE [--active-owner]] [--focus] [--no-focus]"
    );
    eprintln!("  herdr tab get <tab_id>");
    eprintln!("  herdr tab focus <tab_id>");
    eprintln!("  herdr tab pin <tab_id>");
    eprintln!("  herdr tab unpin <tab_id>");
    eprintln!("  herdr tab park <tab_id>");
    eprintln!("  herdr tab unpark <tab_id>");
    eprintln!("  herdr tab rename <tab_id> <label>");
    eprintln!("  herdr tab prio [<tab_id>|--tab ID|--current] [--toggle|--on|--off]");
    eprintln!("  herdr tab close <tab_id>");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn tab_rename_joins_a_multi_word_label() {
        let params = parse_tab_rename_args(&args(&["w1:t2", "Find", "repairable", "GPUs"]))
            .expect("a label should parse");
        assert_eq!(params.label.as_deref(), Some("Find repairable GPUs"));
    }

    #[test]
    fn tab_rename_clear_drops_the_label() {
        let params =
            parse_tab_rename_args(&args(&["w1:t2", "--clear"])).expect("--clear should parse");
        assert_eq!(params.label, None);
    }

    #[test]
    fn tab_rename_treats_clear_among_words_as_a_literal_label() {
        // Only a lone `--clear` clears; otherwise it is part of the name the
        // user typed and must not silently wipe the tab label instead.
        let params = parse_tab_rename_args(&args(&["w1:t2", "--clear", "later"]))
            .expect("a label should parse");
        assert_eq!(params.label.as_deref(), Some("--clear later"));
    }

    #[test]
    fn tab_rename_needs_a_tab_and_a_label() {
        assert!(parse_tab_rename_args(&args(&[])).is_none());
        assert!(parse_tab_rename_args(&args(&["w1:t2"])).is_none());
    }

    #[test]
    fn sidebar_pin_cli_parses_a_tab_id_for_pin_and_unpin() {
        let pin =
            parse_tab_pin_args(&args(&["w1:t2"]), TabPinMode::Pin).expect("pin takes one tab id");
        assert_eq!(pin.tab_id, "w1:t2");
        assert_eq!(pin.mode, TabPinMode::Pin);
        let unpin = parse_tab_pin_args(&args(&["w1:t2"]), TabPinMode::Unpin)
            .expect("unpin takes one tab id");
        assert_eq!(unpin.mode, TabPinMode::Unpin);
        assert!(parse_tab_pin_args(&args(&[]), TabPinMode::Pin).is_none());
        assert!(parse_tab_pin_args(&args(&["w1:t2", "extra"]), TabPinMode::Pin).is_none());
    }

    #[test]
    fn parked_tab_cli_parses_explicit_park_and_unpark_commands() {
        let park = parse_tab_park_args(&args(&["w1:t2"]), true).expect("park takes one id");
        assert_eq!(park.tab_id, "w1:t2");
        assert!(park.parked);
        let unpark = parse_tab_park_args(&args(&["w1:t2"]), false).expect("unpark takes one id");
        assert!(!unpark.parked);
        assert!(parse_tab_park_args(&args(&[]), true).is_none());
        assert!(parse_tab_park_args(&args(&["w1:t2", "extra"]), false).is_none());
    }
}
