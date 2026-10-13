use std::collections::HashMap;

use clap::{ArgMatches, Command};

use crate::api::schema::panes::*;
use crate::api::schema::{
    Method, OutputMatch, PaneAgentState, PaneDirection, PaneTarget, PaneWaitForOutputParams,
    ReadFormat, ReadSource, Request, SplitDirection, READ_FORMATS, READ_SOURCES,
};
pub(crate) use crate::cli::projection::parse_choice;
use crate::cli::projection::{choices, choices_where, Choice};

pub(crate) const DIRECTIONS: &[Choice<PaneDirection>] = &[
    Choice::new("left", PaneDirection::Left),
    Choice::new("right", PaneDirection::Right),
    Choice::new("up", PaneDirection::Up),
    Choice::new("down", PaneDirection::Down),
];
pub(crate) const SPLITS: &[Choice<SplitDirection>] = &[
    Choice::new("right", SplitDirection::Right),
    Choice::new("down", SplitDirection::Down),
];
const RIGHT_CLICK: &[Choice<PaneRightClickTarget>] = &[
    Choice::new("herdr", PaneRightClickTarget::Herdr),
    Choice::new("pane", PaneRightClickTarget::Pane),
];
const STATES: &[Choice<PaneAgentState>] = &[
    Choice::new("idle", PaneAgentState::Idle),
    Choice::new("working", PaneAgentState::Working),
    Choice::new("blocked", PaneAgentState::Blocked),
    Choice::new("unknown", PaneAgentState::Unknown),
];

pub(crate) fn state() -> Input<PaneAgentState> {
    Input::value("state", "STATE", |slot, value| {
        *slot = parse_choice(STATES, "agent state", value)?;
        Ok(())
    })
    .required()
    .configure(|arg| choices(arg, STATES))
}
use crate::cli::projection::{self, cli_projection, Field, Input, ParseError};
pub(crate) use crate::cli::projection::{boolean, optional_text};

pub(crate) fn pane_id() -> Input<String> {
    Input::positional("pane_id", 0, |slot, value| {
        *slot = crate::cli::normalize_pane_id(value);
        Ok(())
    })
    .value_name("PANE_ID")
}

pub(crate) fn text(name: &'static str, value: &'static str) -> Input<String> {
    Input::value(name, value, |slot, value| {
        *slot = value.to_owned();
        Ok(())
    })
}

pub(crate) fn target(positional: bool) -> Field<Option<String>> {
    let mut field = Field::new(None)
        .input(Input::value("pane", "ID", |slot, value| {
            *slot = Some(crate::cli::normalize_pane_id(value));
            Ok(())
        }))
        .input(Input::flag("current", |slot, _| {
            *slot = None;
            Ok(())
        }));
    if positional {
        field = field.input(
            Input::positional("pane_id", 0, |slot, value| {
                // Single-dash ids are accepted; `--` words are mistyped options.
                if value.starts_with("--") {
                    return Err(ParseError::Usage(format!("unknown option: {value}")));
                }
                *slot = Some(crate::cli::normalize_pane_id(value));
                Ok(())
            })
            .value_name("PANE_ID")
            .configure(|arg| arg.required(false).allow_hyphen_values(true)),
        );
    }
    field
}

pub(crate) fn input_target() -> Field<String> {
    Field::new(String::new())
        .input(pane_id().configure(|arg| {
            arg.required(false)
                .allow_hyphen_values(false)
                .conflicts_with_all(["pane", "current"])
        }))
        .input(
            Input::value("pane", "ID", |slot, value| {
                *slot = crate::cli::normalize_pane_id(value);
                Ok(())
            })
            .configure(|arg| arg.conflicts_with("current")),
        )
        .input(Input::flag("current", |_, _| Ok(())))
}

pub(crate) fn direction() -> Input<crate::api::schema::PaneDirection> {
    Input::value("direction", "DIRECTION", |slot, value| {
        *slot = super::direction(value)?;
        Ok(())
    })
    .required()
    .configure(|arg| choices(arg, DIRECTIONS))
}

pub(crate) fn optional_direction() -> Input<Option<crate::api::schema::PaneDirection>> {
    Input::value("direction", "DIRECTION", |slot, value| {
        *slot = Some(super::direction(value)?);
        Ok(())
    })
    .configure(|arg| choices(arg, DIRECTIONS))
}

pub(crate) fn split_direction() -> Input<SplitDirection> {
    Input::value("direction", "DIRECTION", |slot, value| {
        *slot = parse_choice(SPLITS, "split direction", value)?;
        Ok(())
    })
    .required()
    .configure(|arg| choices(arg, SPLITS))
}

pub(crate) fn finite(name: &'static str) -> Input<Option<f32>> {
    let update: fn(&mut Option<f32>, &str) -> Result<(), ParseError> = if name == "ratio" {
        |slot, value| {
            *slot = Some(super::finite("ratio", value)?);
            Ok(())
        }
    } else {
        |slot, value| {
            *slot = Some(super::finite("amount", value)?);
            Ok(())
        }
    };
    Input::value(name, "FLOAT", update)
}

pub(crate) fn right_click() -> Input<PaneRightClickTarget> {
    Input::value("right-click", "TARGET", |slot, value| {
        *slot = parse_choice(RIGHT_CLICK, "right-click target", value)?;
        Ok(())
    })
    .configure(|arg| choices(arg, RIGHT_CLICK))
}

pub(crate) fn environment() -> Input<HashMap<String, String>> {
    Input::<HashMap<String, String>>::value("env", "KEY=VALUE", |slot, value| {
        let (key, value) = crate::cli::parse_env_assignment(value)?;
        slot.insert(key, value);
        Ok(())
    })
    .help("Set an environment variable for the launched process")
}

pub(crate) fn joined(name: &'static str, value: &'static str) -> Input<String> {
    Input::rest(name, value, 1, |slot, value| {
        *slot = value.to_owned();
        Ok(())
    })
    .verbatim()
}

pub(crate) fn keys() -> Input<Vec<String>> {
    Input::positional("key", 1, |slot: &mut Vec<String>, value| {
        slot.push(value.to_owned());
        Ok(())
    })
    .value_name("KEY")
    .configure(|arg| arg.num_args(1..))
    .verbatim()
}

pub(crate) fn label() -> Field<Option<String>> {
    Field::new(None)
        .input(
            Input::rest("label", "LABEL", 1, |slot, value| {
                *slot = Some(value.to_owned());
                Ok(())
            })
            .verbatim()
            .configure(|arg| {
                arg.required(false)
                    .required_unless_present("clear")
                    .conflicts_with("clear")
            }),
        )
        .input(Input::flag("clear", |slot, _| {
            *slot = None;
            Ok(())
        }))
}

pub(crate) fn read_source(include_detection: bool) -> Input<ReadSource> {
    Input::value("source", "SOURCE", |slot, value| {
        *slot = parse_choice(READ_SOURCES, "read source", value)?;
        Ok(())
    })
    .configure(|arg| {
        choices_where(arg, READ_SOURCES, |source| {
            include_detection || *source != ReadSource::Detection
        })
    })
}

pub(crate) fn lines() -> Input<Option<u32>> {
    Input::value("lines", "N", |slot, value| {
        *slot = Some(crate::cli::parse_u32_flag("--lines", value)?);
        Ok(())
    })
}

pub(crate) fn number(name: &'static str, value: &'static str) -> Input<Option<u64>> {
    let update: fn(&mut Option<u64>, &str) -> Result<(), ParseError> = match name {
        "seq" => |slot, value| {
            *slot = Some(crate::cli::parse_u64_flag("--seq", value)?);
            Ok(())
        },
        "ttl-ms" => |slot, value| {
            *slot = Some(crate::cli::parse_u64_flag("--ttl-ms", value)?);
            Ok(())
        },
        _ => |slot, value| {
            *slot = Some(crate::cli::parse_u64_flag("--timeout", value)?);
            Ok(())
        },
    };
    Input::value(name, value, update)
}

pub(crate) fn read_format() -> Field<ReadFormat> {
    Field::new(ReadFormat::Text)
        .input(
            Input::value("format", "FORMAT", |slot, value| {
                *slot = parse_choice(READ_FORMATS, "read format", value)?;
                Ok(())
            })
            .configure(|arg| choices(arg, READ_FORMATS)),
        )
        .input(Input::flag("ansi", |slot, _| {
            *slot = ReadFormat::Ansi;
            Ok(())
        }))
}

pub(crate) fn matcher() -> Field<OutputMatch> {
    Field::new(OutputMatch::Substring {
        value: String::new(),
    })
    .input(
        Input::value("match", "TEXT", |slot, value| {
            *slot = OutputMatch::Substring {
                value: value.to_owned(),
            };
            Ok(())
        })
        .once()
        .configure(|arg| arg.conflicts_with("regex").required_unless_present("regex"))
        .help("Match a literal substring"),
    )
    .input(
        Input::value("regex", "PATTERN", |slot, value| {
            *slot = OutputMatch::Regex {
                value: value.to_owned(),
            };
            Ok(())
        })
        .once()
        .configure(|arg| arg.conflicts_with("match").required_unless_present("match"))
        .help("Match a Rust regular expression"),
    )
}

pub(crate) fn source() -> Input<String> {
    text("source", "ID").required()
}

pub(crate) fn resume() -> Input<Option<Vec<String>>> {
    Input::positional("resume_argv", 1, |slot: &mut Option<Vec<String>>, value| {
        slot.get_or_insert_with(Vec::new).push(value.to_owned());
        Ok(())
    })
    .value_name("RESUME_ARG")
    .configure(|arg| arg.required(false).num_args(0..).last(true))
    .help(
        "Command that resumes this session after a Herdr restart; starts with a plain command name",
    )
}

pub(crate) fn token() -> Input<HashMap<String, Option<String>>> {
    Input::<HashMap<String, Option<String>>>::value("token", "NAME=VALUE", |slot, value| {
        let (key, value) = crate::cli::parse_token_assignment(value)?;
        slot.insert(key, value);
        Ok(())
    })
}

pub(crate) fn clear_token() -> Input<HashMap<String, Option<String>>> {
    Input::<HashMap<String, Option<String>>>::value("clear-token", "NAME", |slot, value| {
        slot.insert(value.to_owned(), None);
        Ok(())
    })
}

pub(crate) fn state_label() -> Input<HashMap<String, String>> {
    Input::<HashMap<String, String>>::value("state-label", "STATUS=TEXT", |slot, value| {
        let Some((status, label)) = value.split_once('=') else {
            return Err(ParseError::Usage(
                "expected --state-label STATUS=TEXT".into(),
            ));
        };
        let status = status.trim().to_ascii_lowercase();
        if !matches!(
            status.as_str(),
            "idle" | "working" | "blocked" | "done" | "unknown"
        ) {
            return Err(ParseError::Usage(format!("unknown state label: {status}")));
        }
        slot.insert(status, label.to_owned());
        Ok(())
    })
}

// The destination is validated and constructed as a tagged value after Clap parses
// these inputs. Its grammar still belongs to this shared field declaration.
pub(crate) fn destination() -> Field<PaneMoveDestination> {
    let value = |name, label| Input::value(name, label, |_: &mut PaneMoveDestination, _| Ok(()));
    let flag = |name| Input::flag(name, |_: &mut PaneMoveDestination, _| Ok(()));
    Field::new(PaneMoveDestination::NewTab {
        workspace_id: None,
        label: None,
    })
    .input(value("tab", "TAB_ID").configure(|arg| {
        arg.conflicts_with_all(["new-tab", "new-workspace"])
            .requires("split")
    }))
    // Clap skips `requires` when the required arg conflicts with a present one,
    // so each destination's options also conflict with the other destinations.
    .input(value("split", "DIRECTION").configure(|arg| {
        choices(arg, SPLITS)
            .requires("tab")
            .conflicts_with_all(["new-tab", "new-workspace"])
    }))
    .input(value("target-pane", "ID").configure(|arg| {
        arg.requires("tab")
            .conflicts_with_all(["new-tab", "new-workspace"])
    }))
    .input(Input::value(
        "ratio",
        "FLOAT",
        |_: &mut PaneMoveDestination, value| super::finite("ratio", value).map(drop),
    ))
    .input(flag("new-tab").configure(|arg| arg.conflicts_with_all(["tab", "new-workspace"])))
    .input(value("workspace", "ID").configure(|arg| {
        arg.requires("new-tab")
            .conflicts_with_all(["tab", "new-workspace"])
    }))
    .input(flag("new-workspace").configure(|arg| arg.conflicts_with_all(["tab", "new-tab"])))
    .input(value("label", "TEXT").configure(|arg| arg.conflicts_with("tab")))
    .input(value("tab-label", "TEXT").configure(|arg| {
        arg.requires("new-workspace")
            .conflicts_with_all(["tab", "new-tab"])
    }))
}

cli_projection! { pub(crate) PaneWaitProjection for PaneWaitForOutputParams {
    pane_id => Field::new(String::new()).input(pane_id()),
    source => Field::new(ReadSource::Recent).input(read_source(false)),
    lines => Field::new(None).input(lines().help("Restrict the searched snapshot to N lines")),
    r#match => matcher(),
    timeout_ms => Field::new(None).input(number("timeout", "MS").help("Fail after this many milliseconds")),
    strip_ansi => Field::new(true).input(boolean("raw", false).help("Keep ANSI escape sequences while matching")),
} }

cli_projection! { pub(crate) PaneTargetProjection for PaneTarget {
    pane_id => Field::new(String::new()).input(pane_id()),
} }

#[derive(Clone, Copy)]
pub(super) enum Output {
    Json,
    Quiet,
    Read,
}

pub(super) struct Leaf {
    pub(super) name: &'static str,
    about: &'static str,
    id: &'static str,
    pub(super) output: Output,
    arguments: fn() -> Vec<clap::Arg>,
    method: fn(&ArgMatches) -> Result<Method, ParseError>,
}

impl Leaf {
    pub(super) fn command(&self) -> Command {
        let command = projection::command(self.name, (self.arguments)()).about(self.about);
        match self.name {
            "input" => command.group(clap::ArgGroup::new("selector").args(["pane_id", "pane", "current"]).required(true)),
            "move" => command.group(clap::ArgGroup::new("destination").args(["tab", "new-tab", "new-workspace"]).required(true)),
            "send-text" => command.after_help("next: herdr pane run <PANE_ID> <COMMAND> sends text and Enter in one call"),
            "send-keys" => command.after_help("Use esc as the canonical Escape key name; escape is also accepted."),
            "wait-output" => command.after_help("The selected snapshot is searched immediately, including existing output, then polled. Without --timeout, this waits indefinitely."),
            _ => command,
        }
    }

    pub(super) fn request(&self, args: &[String]) -> Result<Request, ParseError> {
        let mut method = projection::parse(
            self.command().bin_name(format!("herdr pane {}", self.name)),
            args,
            self.method,
        )?;
        // Clap cannot distinguish an absent zero-arity last positional from an
        // explicit empty separator. This is presence metadata, not another parser.
        if args.iter().any(|arg| arg == "--") {
            match &mut method {
                Method::PaneReportAgent(params) => {
                    params.resume_argv.get_or_insert_with(Vec::new);
                }
                Method::PaneReportAgentSession(params) => {
                    params.resume_argv.get_or_insert_with(Vec::new);
                }
                _ => {}
            }
        }
        Ok(Request {
            id: self.id.into(),
            method,
        })
    }
}

macro_rules! leaf {
    ($name:literal, $about:literal, $id:literal, $output:ident, $projection:ident, $method:ident) => {
        Leaf {
            name: $name,
            about: $about,
            id: $id,
            output: Output::$output,
            arguments: $projection::arguments,
            method: |matches| $projection::from_matches(matches).map(Method::$method),
        }
    };
}

pub(super) const LEAVES: &[Leaf] = &[
    leaf!(
        "list",
        "List panes",
        "cli:pane:list",
        Json,
        PaneListProjection,
        PaneList
    ),
    Leaf {
        name: "current",
        about: "Show the current pane",
        id: "cli:pane:current",
        output: Output::Json,
        arguments: PaneCurrentProjection::arguments,
        method: |matches| {
            let mut params = PaneCurrentProjection::from_matches(matches)?;
            params.caller_pane_id = super::caller_target(
                matches,
                params.caller_pane_id,
                crate::cli::target::caller_pane_id().as_deref(),
                true,
                false,
            )?;
            Ok(Method::PaneCurrent(params))
        },
    },
    leaf!(
        "get",
        "Show a pane",
        "cli:pane:get",
        Json,
        PaneTargetProjection,
        PaneGet
    ),
    Leaf {
        name: "layout",
        about: "Show pane layout information",
        id: "cli:pane:layout",
        output: Output::Json,
        arguments: PaneLayoutProjection::arguments,
        method: |matches| {
            let mut params = PaneLayoutProjection::from_matches(matches)?;
            params.pane_id = super::caller_target(
                matches,
                params.pane_id,
                crate::cli::target::caller_pane_id().as_deref(),
                false,
                false,
            )?;
            Ok(Method::PaneLayout(params))
        },
    },
    Leaf {
        name: "process-info",
        about: "Show pane process information",
        id: "cli:pane:process_info",
        output: Output::Json,
        arguments: PaneProcessInfoProjection::arguments,
        method: |matches| {
            let mut params = PaneProcessInfoProjection::from_matches(matches)?;
            params.pane_id = super::caller_target(
                matches,
                params.pane_id,
                crate::cli::target::caller_pane_id().as_deref(),
                false,
                false,
            )?;
            Ok(Method::PaneProcessInfo(params))
        },
    },
    leaf!(
        "neighbor",
        "Find a pane neighbor",
        "cli:pane:neighbor",
        Json,
        PaneNeighborProjection,
        PaneNeighbor
    ),
    Leaf {
        name: "edges",
        about: "Show pane edge information",
        id: "cli:pane:edges",
        output: Output::Json,
        arguments: PaneEdgesProjection::arguments,
        method: |matches| {
            let mut params = PaneEdgesProjection::from_matches(matches)?;
            params.pane_id = super::caller_target(
                matches,
                params.pane_id,
                crate::cli::target::caller_pane_id().as_deref(),
                false,
                false,
            )?;
            Ok(Method::PaneEdges(params))
        },
    },
    leaf!(
        "focus",
        "Focus a neighboring pane",
        "cli:pane:focus",
        Json,
        PaneFocusProjection,
        PaneFocusDirection
    ),
    leaf!(
        "resize",
        "Resize a pane split",
        "cli:pane:resize",
        Json,
        PaneResizeProjection,
        PaneResize
    ),
    leaf!(
        "zoom",
        "Toggle or set pane zoom",
        "cli:pane:zoom",
        Json,
        PaneZoomProjection,
        PaneZoom
    ),
    Leaf {
        name: "read",
        about: "Read pane terminal output",
        id: "cli:pane:read",
        output: Output::Read,
        arguments: PaneReadProjection::arguments,
        method: |matches| {
            let mut params = PaneReadProjection::from_matches(matches)?;
            if matches.get_flag("raw")
                && matches.index_of("raw") > super::last_index(matches, &["format", "ansi"])
            {
                params.format = ReadFormat::Ansi;
            }
            Ok(Method::PaneRead(params))
        },
    },
    leaf!(
        "rename",
        "Rename a pane",
        "cli:pane:rename",
        Json,
        PaneRenameProjection,
        PaneRename
    ),
    Leaf {
        name: "input",
        about: "Set pane input routing",
        id: "cli:pane:input:set",
        output: Output::Json,
        arguments: PaneInputProjection::arguments,
        method: |matches| {
            let mut params = PaneInputProjection::from_matches(matches)?;
            if matches.get_flag("current") {
                params.pane_id = crate::cli::target::caller_pane_id()
                    .map(|value| crate::cli::normalize_pane_id(&value))
                    .ok_or_else(|| ParseError::Usage("--current requires HERDR_PANE_ID".into()))?;
            }
            Ok(Method::PaneInputSet(params))
        },
    },
    Leaf {
        name: "split",
        about: "Split a pane",
        id: "cli:pane:split",
        output: Output::Json,
        arguments: PaneSplitProjection::arguments,
        method: |matches| {
            let mut params = PaneSplitProjection::from_matches(matches)?;
            params.target_pane_id = super::caller_target(
                matches,
                params.target_pane_id,
                crate::cli::target::caller_pane_id().as_deref(),
                true,
                true,
            )?;
            Ok(Method::PaneSplit(params))
        },
    },
    Leaf {
        name: "swap",
        about: "Swap panes",
        id: "cli:pane:swap",
        output: Output::Json,
        arguments: PaneSwapProjection::arguments,
        method: super::swap,
    },
    Leaf {
        name: "move",
        about: "Move a pane",
        id: "cli:pane:move",
        output: Output::Json,
        arguments: PaneMoveProjection::arguments,
        method: super::move_pane,
    },
    leaf!(
        "close",
        "Close a pane",
        "cli:pane:close",
        Json,
        PaneTargetProjection,
        PaneClose
    ),
    leaf!(
        "send-text",
        "Send literal text to a pane",
        "cli:request",
        Quiet,
        PaneSendTextProjection,
        PaneSendText
    ),
    leaf!(
        "send-keys",
        "Send key presses to a pane",
        "cli:request",
        Quiet,
        PaneSendKeysProjection,
        PaneSendKeys
    ),
    leaf!(
        "wait-output",
        "Wait for matching pane output",
        "cli:pane:wait-output",
        Json,
        PaneWaitProjection,
        PaneWaitForOutput
    ),
    leaf!(
        "run",
        "Run a command in a pane",
        "cli:request",
        Quiet,
        PaneRunProjection,
        PaneSendInput
    ),
    Leaf {
        name: "report-agent",
        about: "Report pane agent lifecycle state",
        id: "cli:request",
        output: Output::Quiet,
        arguments: PaneReportAgentProjection::arguments,
        method: |matches| {
            let mut params = PaneReportAgentProjection::from_matches(matches)?;
            params.source = super::source(params.source)?;
            Ok(Method::PaneReportAgent(params))
        },
    },
    Leaf {
        name: "report-agent-session",
        about: "Report pane agent session identity",
        id: "cli:request",
        output: Output::Quiet,
        arguments: PaneReportSessionProjection::arguments,
        method: |matches| {
            let mut params = PaneReportSessionProjection::from_matches(matches)?;
            params.source = super::source(params.source)?;
            Ok(Method::PaneReportAgentSession(params))
        },
    },
    Leaf {
        name: "release-agent",
        about: "Release pane agent lifecycle authority",
        id: "cli:request",
        output: Output::Quiet,
        arguments: PaneReleaseProjection::arguments,
        method: |matches| {
            let mut params = PaneReleaseProjection::from_matches(matches)?;
            params.source = super::source(params.source)?;
            Ok(Method::PaneReleaseAgent(params))
        },
    },
    Leaf {
        name: "report-metadata",
        about: "Report display-only pane metadata",
        id: "cli:request",
        output: Output::Quiet,
        arguments: PaneMetadataProjection::arguments,
        method: super::metadata,
    },
];

pub(super) fn command() -> Command {
    Command::new("pane")
        .about("Control terminal panes")
        .subcommands(LEAVES.iter().map(Leaf::command))
}

#[cfg(test)]
mod choice_tests {
    use crate::cli::projection::assert_choices_cover_api_enum;

    #[test]
    fn pane_choices_cover_every_api_value() {
        assert_choices_cover_api_enum(super::DIRECTIONS);
        assert_choices_cover_api_enum(super::SPLITS);
        assert_choices_cover_api_enum(super::RIGHT_CLICK);
        assert_choices_cover_api_enum(super::STATES);
    }
}
