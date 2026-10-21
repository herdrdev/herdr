use std::collections::HashMap;

use clap::{builder::PossibleValue, Arg, ArgAction, ArgMatches, Command};

use crate::api::schema::plugins::{
    PluginActionInvokeProjection, PluginActionListProjection, PluginLinkProjection,
    PluginListProjection, PluginLogListProjection, PluginPaneCloseProjection,
    PluginPaneFocusProjection, PluginPaneOpenProjection, PluginSetEnabledProjection,
    PluginUnlinkProjection,
};
use crate::api::schema::{Method, PluginInvocationContext, PluginPanePlacement, SplitDirection};
use crate::cli::projection::{self, Input, ParseError};
use crate::popup_size::PopupSize;

pub(crate) use crate::cli::projection::{boolean, optional_text};

pub(crate) fn required_text(name: &'static str, value: &'static str) -> Input<String> {
    Input::value(name, value, |slot, value| {
        *slot = value.to_owned();
        Ok(())
    })
    .required()
}

pub(crate) fn id(name: &'static str, value: &'static str) -> Input<String> {
    Input::positional(name, 0, |slot, value| {
        *slot = value.to_owned();
        Ok(())
    })
    .value_name(value)
}

pub(crate) fn path() -> Input<String> {
    Input::positional("path", 0, |slot, value| {
        *slot = super::normalize_plugin_path_arg(value)?;
        Ok(())
    })
    .value_name("PATH")
    .path()
    .verbatim()
}

pub(crate) fn pane_id() -> Input<String> {
    Input::positional("pane_id", 0, |slot, value| {
        *slot = crate::cli::normalize_pane_id(value);
        Ok(())
    })
    .value_name("PANE_ID")
}

pub(crate) fn environment() -> Input<HashMap<String, String>> {
    Input::<HashMap<String, String>>::value("env", "KEY=VALUE", |slot, value| {
        let (key, value) = crate::cli::parse_env_assignment(value)?;
        slot.insert(key, value);
        Ok(())
    })
}

pub(crate) fn limit() -> Input<Option<usize>> {
    Input::value("limit", "N", |slot, value| {
        *slot = Some(
            value
                .parse()
                .map_err(|_| ParseError::Usage(format!("invalid --limit value: {value}")))?,
        );
        Ok(())
    })
}

const PLACEMENTS: [(&str, PluginPanePlacement, bool); 6] = [
    ("overlay", PluginPanePlacement::Overlay, false),
    ("popup", PluginPanePlacement::Popup, true),
    ("split", PluginPanePlacement::Split, false),
    ("tab", PluginPanePlacement::Tab, false),
    ("zoomed", PluginPanePlacement::Zoomed, false),
    ("fullscreen", PluginPanePlacement::Zoomed, true),
];

const DIRECTIONS: [(&str, SplitDirection); 2] = [
    ("right", SplitDirection::Right),
    ("down", SplitDirection::Down),
];

pub(crate) fn placement() -> Input<Option<PluginPanePlacement>> {
    Input::value("placement", "PLACEMENT", |slot, value| {
        *slot = Some(
            PLACEMENTS
                .iter()
                .find(|(name, _, _)| *name == value)
                .map(|(_, placement, _)| *placement)
                .ok_or_else(|| ParseError::Usage(format!("invalid pane placement: {value}")))?,
        );
        Ok(())
    })
    .configure(|arg| {
        arg.value_parser(clap::builder::PossibleValuesParser::new(
            PLACEMENTS
                .iter()
                .map(|(name, _, hidden)| PossibleValue::new(*name).hide(*hidden)),
        ))
    })
}

pub(crate) fn direction() -> Input<Option<SplitDirection>> {
    Input::value("direction", "DIRECTION", |slot, value| {
        *slot = Some(
            DIRECTIONS
                .iter()
                .find(|(name, _)| *name == value)
                .map(|(_, direction)| direction.clone())
                .ok_or_else(|| ParseError::Usage(format!("invalid split direction: {value}")))?,
        );
        Ok(())
    })
    .configure(|arg| {
        arg.value_parser(clap::builder::PossibleValuesParser::new(
            DIRECTIONS.iter().map(|(name, _)| *name),
        ))
    })
}

pub(crate) fn width() -> Input<Option<PopupSize>> {
    Input::value("width", "SIZE", |slot, value| {
        *slot = Some(PopupSize::parse_cli(value).map_err(|error| format!("--width {error}"))?);
        Ok(())
    })
}

pub(crate) fn height() -> Input<Option<PopupSize>> {
    Input::value("height", "SIZE", |slot, value| {
        *slot = Some(PopupSize::parse_cli(value).map_err(|error| format!("--height {error}"))?);
        Ok(())
    })
}

pub(crate) fn invocation_context() -> PluginInvocationContext {
    PluginInvocationContext {
        workspace_id: None,
        workspace_label: None,
        workspace_cwd: None,
        worktree: None,
        tab_id: None,
        tab_label: None,
        focused_pane_id: None,
        focused_pane_cwd: None,
        focused_pane_agent: None,
        focused_pane_status: None,
        selected_text: None,
        invocation_source: Some("cli".into()),
        correlation_id: None,
        clicked_url: None,
        link_handler_id: None,
    }
}

struct Leaf {
    name: &'static str,
    about: &'static str,
    arguments: fn() -> Vec<Arg>,
    method: fn(&ArgMatches) -> Result<Method, ParseError>,
    json: bool,
}

impl Leaf {
    fn command(&self) -> Command {
        let command = projection::command(self.name, (self.arguments)()).about(self.about);
        if self.json {
            command.arg(Arg::new("json").long("json").action(ArgAction::SetTrue))
        } else {
            command
        }
    }

    fn method(&self, matches: &ArgMatches) -> Result<(Method, bool), ParseError> {
        Ok((
            (self.method)(matches)?,
            self.json && matches.get_flag("json"),
        ))
    }
}

const DIRECT: &[Leaf] = &[
    Leaf {
        name: "link",
        about: "Link a local plugin",
        arguments: PluginLinkProjection::arguments,
        method: |m| PluginLinkProjection::from_matches(m).map(Method::PluginLink),
        json: false,
    },
    Leaf {
        name: "unlink",
        about: "Unlink a local plugin",
        arguments: PluginUnlinkProjection::arguments,
        method: |m| PluginUnlinkProjection::from_matches(m).map(Method::PluginUnlink),
        json: false,
    },
    Leaf {
        name: "enable",
        about: "Enable a plugin",
        arguments: PluginSetEnabledProjection::arguments,
        method: |m| PluginSetEnabledProjection::from_matches(m).map(Method::PluginEnable),
        json: false,
    },
    Leaf {
        name: "disable",
        about: "Disable a plugin",
        arguments: PluginSetEnabledProjection::arguments,
        method: |m| PluginSetEnabledProjection::from_matches(m).map(Method::PluginDisable),
        json: false,
    },
    Leaf {
        name: "list",
        about: "List installed plugins",
        arguments: PluginListProjection::arguments,
        method: |m| PluginListProjection::from_matches(m).map(Method::PluginList),
        json: true,
    },
];

const ACTION: &[Leaf] = &[
    Leaf {
        name: "list",
        about: "List plugin actions",
        arguments: PluginActionListProjection::arguments,
        method: |m| PluginActionListProjection::from_matches(m).map(Method::PluginActionList),
        json: false,
    },
    Leaf {
        name: "invoke",
        about: "Invoke a plugin action",
        arguments: PluginActionInvokeProjection::arguments,
        method: |m| PluginActionInvokeProjection::from_matches(m).map(Method::PluginActionInvoke),
        json: false,
    },
];

const PANE: &[Leaf] = &[
    Leaf {
        name: "open",
        about: "Open a plugin pane",
        arguments: PluginPaneOpenProjection::arguments,
        method: |m| PluginPaneOpenProjection::from_matches(m).map(Method::PluginPaneOpen),
        json: false,
    },
    Leaf {
        name: "focus",
        about: "Focus a plugin pane",
        arguments: PluginPaneFocusProjection::arguments,
        method: |m| PluginPaneFocusProjection::from_matches(m).map(Method::PluginPaneFocus),
        json: false,
    },
    Leaf {
        name: "close",
        about: "Close a plugin pane",
        arguments: PluginPaneCloseProjection::arguments,
        method: |m| PluginPaneCloseProjection::from_matches(m).map(Method::PluginPaneClose),
        json: false,
    },
];

const LOG: Leaf = Leaf {
    name: "list",
    about: "List plugin command logs",
    arguments: PluginLogListProjection::arguments,
    method: |m| PluginLogListProjection::from_matches(m).map(Method::PluginLogList),
    json: false,
};

fn group(name: &'static str, about: &'static str, leaves: &[Leaf]) -> Command {
    Command::new(name)
        .about(about)
        .subcommand_required(true)
        .subcommands(leaves.iter().map(Leaf::command))
}

pub(crate) fn command() -> Command {
    Command::new("plugin")
        .about("Install and run workflow plugins")
        .subcommand_required(true)
        .subcommand(
            Command::new("install")
                .about("Install a plugin from GitHub")
                .arg(
                    Arg::new("source")
                        .value_name("OWNER/REPO[/SUBDIR]")
                        .required(true),
                )
                .arg(Arg::new("ref").long("ref").value_name("REF"))
                .arg(
                    Arg::new("yes")
                        .short('y')
                        .long("yes")
                        .action(ArgAction::SetTrue),
                ),
        )
        .subcommand(
            Command::new("uninstall")
                .about("Uninstall a plugin")
                .arg(Arg::new("plugin").value_name("PLUGIN").required(true)),
        )
        .subcommand(
            Command::new("update")
                .about("Update GitHub-installed plugins")
                .arg(Arg::new("plugins").value_name("PLUGIN").num_args(0..))
                .arg(
                    Arg::new("yes")
                        .short('y')
                        .long("yes")
                        .action(ArgAction::SetTrue),
                ),
        )
        .subcommands(DIRECT.iter().map(Leaf::command))
        .subcommand(
            Command::new("config-dir")
                .about("Print a plugin config directory")
                .arg(Arg::new("plugin_id").value_name("PLUGIN_ID").required(true)),
        )
        .subcommand(group("action", "List or invoke plugin actions", ACTION))
        .subcommand(
            projection::command("log", (LOG.arguments)())
                .about("Inspect plugin command logs")
                .visible_alias("logs")
                .args_conflicts_with_subcommands(true)
                .subcommand(LOG.command()),
        )
        .subcommand(group("pane", "Manage plugin-owned panes", PANE))
}

fn leaf_matches(leaves: &[Leaf], matches: &ArgMatches) -> Result<(Method, bool), ParseError> {
    let (name, matches) = matches
        .subcommand()
        .ok_or_else(|| ParseError::Usage("missing plugin command".into()))?;
    leaves
        .iter()
        .find(|leaf| leaf.name == name)
        .ok_or_else(|| ParseError::Usage(format!("unknown plugin command: {name}")))?
        .method(matches)
}

pub(crate) fn from_matches(matches: &ArgMatches) -> Result<(Method, bool), ParseError> {
    match matches.subcommand() {
        Some(("action", matches)) => leaf_matches(ACTION, matches),
        Some(("pane", matches)) => leaf_matches(PANE, matches),
        Some(("log", matches)) => LOG.method(matches.subcommand_matches("list").unwrap_or(matches)),
        _ => leaf_matches(DIRECT, matches),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<(Method, bool), ParseError> {
        projection::parse(
            command(),
            &args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>(),
            from_matches,
        )
    }

    #[test]
    fn plugin_clap_preserves_context_filters_and_ordered_updates() {
        let (Method::PluginLink(params), _) =
            parse(&["link", "./plugin", "--disabled", "--enabled", "--disabled"]).unwrap()
        else {
            panic!("link request");
        };
        assert!(!params.enabled);
        assert!(params.source.is_none());
        assert_eq!(
            params.path,
            std::env::current_dir()
                .unwrap()
                .join("./plugin")
                .display()
                .to_string()
        );
        // The first operand is always the path, even when it looks like a flag.
        let (Method::PluginLink(params), _) = parse(&["link", "--disabled"]).unwrap() else {
            panic!("flag-shaped link path");
        };
        assert!(params.enabled);
        assert!(params.path.ends_with("--disabled"), "{}", params.path);
        let (Method::PluginActionInvoke(params), _) =
            parse(&["action", "invoke", "apply", "--plugin=plugin:7"]).unwrap()
        else {
            panic!("action request");
        };
        assert_eq!(params.action_id, "apply");
        assert_eq!(params.plugin_id.as_deref(), Some("plugin:7"));
        assert_eq!(
            serde_json::to_value(params.context).unwrap(),
            serde_json::json!({"invocation_source":"cli"})
        );
        let (Method::PluginPaneOpen(params), _) = parse(&[
            "pane",
            "open",
            "--plugin=x",
            "--entrypoint=main",
            "--no-focus",
            "--focus",
            "--no-focus",
            "--env=KEY=first",
            "--env=KEY=last=value",
            "--env=EMPTY=",
        ])
        .unwrap() else {
            panic!("pane request");
        };
        assert!(!params.focus);
        assert_eq!(
            params.env.get("KEY").map(String::as_str),
            Some("last=value")
        );
        assert_eq!(params.env.get("EMPTY").map(String::as_str), Some(""));
        let (Method::PluginList(params), json) = parse(&["list", "--json", "--plugin="]).unwrap()
        else {
            panic!("list request");
        };
        assert!(json);
        assert_eq!(params.plugin_id.as_deref(), Some(""));
    }

    #[test]
    fn plugin_clap_preserves_remote_path_policy() {
        crate::cli::target::with_test_client(crate::api::client::ApiClient::local(), || {
            assert!(matches!(
                parse(&["link", "relative/path"]),
                Err(ParseError::Io(_))
            ));
            for path in ["/remote/plugin", "C:\\remote\\plugin"] {
                let (Method::PluginLink(params), _) = parse(&["link", path]).unwrap() else {
                    panic!("link request");
                };
                assert_eq!(params.path, path);
            }
        });
    }

    #[test]
    fn plugin_clap_keeps_hidden_popup_controls_and_help() {
        let mut family = command();
        family.clone().debug_assert();
        let help = family
            .find_subcommand_mut("pane")
            .unwrap()
            .find_subcommand_mut("open")
            .unwrap()
            .render_long_help()
            .to_string();
        for hidden in ["--width", "--height", "popup", "fullscreen"] {
            assert!(!help.contains(hidden), "{help}");
        }
        for placement in ["popup", "fullscreen"] {
            assert!(matches!(
                parse(&[
                    "pane",
                    "open",
                    "--plugin=x",
                    "--entrypoint=main",
                    "--placement",
                    placement,
                    "--width=0",
                    "--height=100%"
                ]),
                Ok((Method::PluginPaneOpen(_), _))
            ));
        }
        for dimension in ["0%", "101%", "65536", "bad"] {
            assert!(parse(&[
                "pane",
                "open",
                "--plugin=x",
                "--entrypoint=main",
                "--width",
                dimension
            ])
            .is_err());
        }
        assert!(
            matches!(parse(&["pane", "open", "--help"]), Err(ParseError::Clap(error)) if error.kind() == clap::error::ErrorKind::DisplayHelp)
        );
    }
}
