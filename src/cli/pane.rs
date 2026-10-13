pub(crate) mod bindings;

use clap::ArgMatches;

use super::projection::ParseError;
use crate::api::schema::panes::{PaneMetadataProjection, PaneMoveProjection, PaneSwapProjection};
use crate::api::schema::{Method, PaneDirection, PaneMoveDestination};

pub(crate) fn command() -> clap::Command {
    bindings::command()
}

pub(super) fn run_pane_command(args: &[String]) -> std::io::Result<i32> {
    let Some(name) = args.first() else {
        command().print_help()?;
        println!();
        return Ok(2);
    };
    if matches!(name.as_str(), "help" | "--help" | "-h") {
        command().print_help()?;
        println!();
        return Ok(0);
    }
    let Some(leaf) = bindings::LEAVES.iter().find(|leaf| leaf.name == name) else {
        if let Err(error) = command().no_binary_name(true).try_get_matches_from(args) {
            return super::print_clap_error(&error);
        }
        return Ok(2);
    };
    let request = match leaf.request(&args[1..]) {
        Ok(request) => request,
        Err(ParseError::Io(error)) => return Err(error),
        Err(ParseError::Usage(message)) => {
            eprintln!("{message}");
            return Ok(2);
        }
        Err(ParseError::Clap(error)) => return super::print_clap_error(&error),
    };
    if matches!(leaf.output, bindings::Output::Quiet) {
        return super::send_ok_request(request.method);
    }
    let response = super::send_request(&request)?;
    match leaf.output {
        bindings::Output::Json => super::print_response(&response),
        bindings::Output::Read => super::print_read_response(&response),
        bindings::Output::Quiet => {
            if response.get("error").is_some() {
                super::print_response(&response)
            } else {
                Ok(0)
            }
        }
    }
}

fn last_index(matches: &ArgMatches, names: &[&str]) -> Option<usize> {
    names
        .iter()
        .filter_map(|name| {
            if matches.value_source(name) == Some(clap::parser::ValueSource::CommandLine) {
                matches
                    .indices_of(name)
                    .and_then(|mut indices| indices.next_back())
            } else {
                None
            }
        })
        .max()
}

fn caller_target(
    matches: &ArgMatches,
    pane: Option<String>,
    caller: Option<&str>,
    default_caller: bool,
    require_current_caller: bool,
) -> Result<Option<String>, ParseError> {
    let current = last_index(matches, &["current"]);
    let mut explicit = last_index(matches, &["pane"]);
    if matches.try_get_one::<String>("pane_id").is_ok() {
        explicit = explicit.max(last_index(matches, &["pane_id"]));
    }
    if current.is_some() && current > explicit {
        let caller = caller.map(super::normalize_pane_id);
        if require_current_caller && caller.is_none() {
            return Err(ParseError::Usage("--current requires HERDR_PANE_ID".into()));
        }
        return Ok(caller);
    }
    if current.is_none() && explicit.is_none() && default_caller {
        return Ok(caller.map(super::normalize_pane_id));
    }
    Ok(pane)
}

fn direction(value: &str) -> Result<PaneDirection, ParseError> {
    bindings::parse_choice(bindings::DIRECTIONS, "pane direction", value)
}

fn finite(name: &str, value: &str) -> Result<f32, ParseError> {
    value
        .parse::<f32>()
        .ok()
        .filter(|value| value.is_finite())
        .ok_or_else(|| ParseError::Usage(format!("invalid {name}: {value}")))
}

fn source(value: String) -> Result<String, ParseError> {
    let value = value.trim().to_owned();
    if value.is_empty() {
        return Err(ParseError::Usage("missing required --source".into()));
    }
    Ok(value)
}

fn swap(matches: &ArgMatches) -> Result<Method, ParseError> {
    let mut params = PaneSwapProjection::from_matches(matches)?;
    let directional = params.direction.is_some();
    let explicit = params.source_pane_id.is_some() || params.target_pane_id.is_some();
    if !((directional && !explicit)
        || (!directional
            && explicit
            && params.pane_id.is_none()
            && params.source_pane_id.is_some()
            && params.target_pane_id.is_some()))
    {
        return Err(ParseError::Usage(
            "provide a direction or both source and target panes".into(),
        ));
    }
    params.source_pane_id = params
        .source_pane_id
        .map(|value| super::normalize_pane_id(&value));
    params.target_pane_id = params
        .target_pane_id
        .map(|value| super::normalize_pane_id(&value));
    Ok(Method::PaneSwap(params))
}

fn move_pane(matches: &ArgMatches) -> Result<Method, ParseError> {
    let mut params = PaneMoveProjection::from_matches(matches)?;
    let text = |name| {
        matches
            .get_many::<String>(name)
            .and_then(|mut values| values.next_back())
            .cloned()
    };
    let ratio = text("ratio")
        .map(|value| finite("ratio", &value))
        .transpose()?;
    params.destination = if let Some(tab) = text("tab") {
        let split =
            text("split").ok_or_else(|| ParseError::Usage("missing required --split".into()))?;
        PaneMoveDestination::Tab {
            tab_id: super::normalize_tab_id(&tab),
            target_pane_id: text("target-pane").map(|value| super::normalize_pane_id(&value)),
            split: bindings::parse_choice(bindings::SPLITS, "split direction", &split)?,
            ratio,
        }
    } else if matches.get_flag("new-tab") {
        PaneMoveDestination::NewTab {
            workspace_id: text("workspace").map(|value| super::normalize_workspace_id(&value)),
            label: text("label"),
        }
    } else {
        PaneMoveDestination::NewWorkspace {
            label: text("label"),
            tab_label: text("tab-label"),
        }
    };
    Ok(Method::PaneMove(params))
}

fn metadata(matches: &ArgMatches) -> Result<Method, ParseError> {
    let mut params = PaneMetadataProjection::from_matches(matches)?;
    params.source = source(params.source)?;
    if params
        .applies_to_source
        .as_deref()
        .is_some_and(|value| value.trim().is_empty())
    {
        return Err(ParseError::Usage(
            "missing value for --applies-to-source".into(),
        ));
    }
    if params.title.is_none()
        && params.display_agent.is_none()
        && params.state_labels.is_empty()
        && params.tokens.is_empty()
        && !params.clear_title
        && !params.clear_display_agent
        && !params.clear_state_labels
    {
        return Err(ParseError::Usage(
            "missing metadata field to set or clear".into(),
        ));
    }
    Ok(Method::PaneReportMetadata(params))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{
        OutputMatch, PaneRightClickTarget, PaneZoomMode, ReadFormat, ReadSource, SplitDirection,
    };

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn request(name: &str, values: &[&str]) -> Result<crate::api::schema::Request, ParseError> {
        bindings::LEAVES
            .iter()
            .find(|leaf| leaf.name == name)
            .unwrap()
            .request(&args(values))
    }

    fn parse_pane_split_args(
        values: &[&str],
        caller: Option<&str>,
    ) -> crate::api::schema::PaneSplitParams {
        let leaf = bindings::LEAVES
            .iter()
            .find(|leaf| leaf.name == "split")
            .unwrap();
        let matches = leaf
            .command()
            .no_binary_name(true)
            .try_get_matches_from(values)
            .unwrap();
        let mut params =
            crate::api::schema::panes::PaneSplitProjection::from_matches(&matches).unwrap();
        params.target_pane_id =
            caller_target(&matches, params.target_pane_id, caller, true, true).unwrap();
        params
    }

    #[test]
    fn pane_projection_all_leaf_request_values() {
        use serde_json::json;
        let cases = [
            ("list", vec![], "cli:pane:list", "pane.list", json!({})),
            (
                "current",
                vec!["--pane", "pane"],
                "cli:pane:current",
                "pane.current",
                json!({"caller_pane_id": "pane"}),
            ),
            (
                "get",
                vec!["pane"],
                "cli:pane:get",
                "pane.get",
                json!({"pane_id": "pane"}),
            ),
            (
                "layout",
                vec!["--pane", "pane"],
                "cli:pane:layout",
                "pane.layout",
                json!({"pane_id": "pane"}),
            ),
            (
                "process-info",
                vec!["--pane", "pane"],
                "cli:pane:process_info",
                "pane.process_info",
                json!({"pane_id": "pane"}),
            ),
            (
                "neighbor",
                vec!["--direction", "left"],
                "cli:pane:neighbor",
                "pane.neighbor",
                json!({"direction": "left"}),
            ),
            (
                "edges",
                vec!["--pane", "pane"],
                "cli:pane:edges",
                "pane.edges",
                json!({"pane_id": "pane"}),
            ),
            (
                "focus",
                vec!["--direction", "right"],
                "cli:pane:focus",
                "pane.focus_direction",
                json!({"direction": "right"}),
            ),
            (
                "resize",
                vec!["--direction", "up"],
                "cli:pane:resize",
                "pane.resize",
                json!({"direction": "up"}),
            ),
            (
                "zoom",
                vec![],
                "cli:pane:zoom",
                "pane.zoom",
                json!({"mode": "toggle"}),
            ),
            (
                "read",
                vec!["pane"],
                "cli:pane:read",
                "pane.read",
                json!({"pane_id": "pane", "source": "recent", "format": "text", "strip_ansi": true}),
            ),
            (
                "rename",
                vec!["pane", "--clear"],
                "cli:pane:rename",
                "pane.rename",
                json!({"pane_id": "pane"}),
            ),
            (
                "input",
                vec!["pane", "--right-click", "herdr"],
                "cli:pane:input:set",
                "pane.input.set",
                json!({"pane_id": "pane", "right_click": "herdr"}),
            ),
            (
                "split",
                vec!["pane", "--direction", "down"],
                "cli:pane:split",
                "pane.split",
                json!({"target_pane_id": "pane", "direction": "down", "focus": false, "right_click": "herdr"}),
            ),
            (
                "swap",
                vec!["--direction", "down"],
                "cli:pane:swap",
                "pane.swap",
                json!({"direction": "down"}),
            ),
            (
                "move",
                vec!["pane", "--new-tab"],
                "cli:pane:move",
                "pane.move",
                json!({"pane_id": "pane", "destination": {"type": "new_tab"}, "focus": true}),
            ),
            (
                "close",
                vec!["pane"],
                "cli:pane:close",
                "pane.close",
                json!({"pane_id": "pane"}),
            ),
            (
                "send-text",
                vec!["pane", ""],
                "cli:request",
                "pane.send_text",
                json!({"pane_id": "pane", "text": ""}),
            ),
            (
                "send-keys",
                vec!["pane", "Enter", "esc"],
                "cli:request",
                "pane.send_keys",
                json!({"pane_id": "pane", "keys": ["Enter", "esc"]}),
            ),
            (
                "wait-output",
                vec!["pane", "--match", ""],
                "cli:pane:wait-output",
                "pane.wait_for_output",
                json!({"pane_id": "pane", "source": "recent", "match": {"type": "substring", "value": ""}, "strip_ansi": true}),
            ),
            (
                "run",
                vec!["pane", ""],
                "cli:request",
                "pane.send_input",
                json!({"pane_id": "pane", "keys": ["Enter"]}),
            ),
            (
                "report-agent",
                vec![
                    "pane", "--source", " hook ", "--agent", "", "--state", "unknown",
                ],
                "cli:request",
                "pane.report_agent",
                json!({"pane_id": "pane", "source": "hook", "agent": "", "state": "unknown"}),
            ),
            (
                "report-agent-session",
                vec!["pane", "--source", " hook ", "--agent", "a"],
                "cli:request",
                "pane.report_agent_session",
                json!({"pane_id": "pane", "source": "hook", "agent": "a"}),
            ),
            (
                "release-agent",
                vec!["pane", "--source", " hook ", "--agent", "a"],
                "cli:request",
                "pane.release_agent",
                json!({"pane_id": "pane", "source": "hook", "agent": "a"}),
            ),
            (
                "report-metadata",
                vec!["pane", "--source", " hook ", "--clear-title"],
                "cli:request",
                "pane.report_metadata",
                json!({"pane_id": "pane", "source": "hook", "clear_title": true, "clear_display_agent": false, "clear_state_labels": false}),
            ),
        ];
        assert_eq!(cases.len(), bindings::LEAVES.len());
        for (name, values, id, method, params) in cases {
            assert_eq!(
                serde_json::to_value(request(name, &values).unwrap()).unwrap(),
                json!({"id": id, "method": method, "params": params}),
                "{name}"
            );
        }
    }

    #[test]
    fn pane_projection_dash_leading_targets_and_empty_values() {
        let params = parse_pane_split_args(&["-literal", "--direction", "right"], None);
        assert_eq!(params.target_pane_id.as_deref(), Some("-literal"));
        for name in ["get", "close", "zoom"] {
            let json = serde_json::to_value(request(name, &["-literal"]).unwrap()).unwrap();
            assert_eq!(json["params"]["pane_id"], "-literal");
        }
        let json = serde_json::to_value(
            request(
                "move",
                &[
                    "pane",
                    "--new-workspace",
                    "--label",
                    "",
                    "--tab-label",
                    "-literal",
                ],
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            json["params"]["destination"],
            serde_json::json!({"type": "new_workspace", "label": "", "tab_label": "-literal"})
        );
        let json = serde_json::to_value(
            request(
                "move",
                &[
                    "pane",
                    "--new-tab",
                    "--workspace",
                    "workspace",
                    "--label",
                    "label",
                ],
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            json["params"]["destination"],
            serde_json::json!({"type": "new_tab", "workspace_id": "workspace", "label": "label"})
        );
    }

    #[test]
    fn pane_projection_command_is_the_execution_grammar() {
        command().debug_assert();
        for leaf in bindings::LEAVES {
            let command = command().find_subcommand(leaf.name).unwrap().clone();
            assert_eq!(
                command
                    .get_arguments()
                    .map(|arg| arg.get_id().to_string())
                    .collect::<Vec<_>>(),
                leaf.command()
                    .get_arguments()
                    .map(|arg| arg.get_id().to_string())
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn parse_pane_split_args_accepts_ratio() {
        let params = parse_pane_split_args(
            &["issue-1", "--direction", "right", "--ratio", "0.333"],
            None,
        );
        assert_eq!(params.target_pane_id, Some("issue-1".into()));
        assert_eq!(params.direction, SplitDirection::Right);
        assert_eq!(params.ratio, Some(0.333));
        assert_eq!(params.right_click, PaneRightClickTarget::Herdr);
    }

    #[test]
    fn parse_pane_split_args_accepts_pane_right_click_target() {
        let params =
            parse_pane_split_args(&["--direction", "right", "--right-click", "pane"], None);
        assert_eq!(params.right_click, PaneRightClickTarget::Pane);
    }

    #[test]
    fn parse_pane_split_args_accepts_equals_right_click_target() {
        let params = parse_pane_split_args(&["--direction", "right", "--right-click=pane"], None);
        assert_eq!(params.right_click, PaneRightClickTarget::Pane);
    }

    #[test]
    fn parse_pane_split_args_accepts_current_target() {
        let params = parse_pane_split_args(&["--direction", "down", "--current"], Some("issue-1"));
        assert_eq!(params.target_pane_id, Some("issue-1".into()));
        assert_eq!(params.direction, SplitDirection::Down);
    }

    #[test]
    fn parse_pane_split_args_omitted_target_uses_caller() {
        let params = parse_pane_split_args(
            &["--no-focus", "--direction", "right", "--cwd", "/var/tmp"],
            Some("w1:p2"),
        );
        assert_eq!(params.target_pane_id, Some("w1:p2".into()));
        assert!(!params.focus);
    }

    #[test]
    fn parse_pane_split_args_without_caller_keeps_focused_fallback() {
        assert_eq!(
            parse_pane_split_args(&["--direction", "down"], None).target_pane_id,
            None
        );
    }

    #[test]
    fn parse_pane_split_args_explicit_target_overrides_caller() {
        for values in [
            vec!["w2:p3", "--direction", "right"],
            vec!["--pane", "w2:p3", "--direction", "right"],
        ] {
            assert_eq!(
                parse_pane_split_args(&values, Some("w1:p2")).target_pane_id,
                Some("w2:p3".into())
            );
        }
    }

    #[test]
    fn pane_projection_characterization_caller_and_focused_policies() {
        for (name, default_caller) in [
            ("current", true),
            ("layout", false),
            ("process-info", false),
            ("edges", false),
        ] {
            let leaf = bindings::LEAVES
                .iter()
                .find(|leaf| leaf.name == name)
                .unwrap();
            for (values, expected) in [
                (vec![], if default_caller { Some("caller") } else { None }),
                (vec!["--pane", "explicit"], Some("explicit")),
                (vec!["--pane", "explicit", "--current"], Some("caller")),
                (vec!["--current", "--pane", "explicit"], Some("explicit")),
            ] {
                let matches = leaf
                    .command()
                    .no_binary_name(true)
                    .try_get_matches_from(&values)
                    .unwrap();
                let mut field = bindings::target(false);
                field.read_matches(&matches).unwrap();
                assert_eq!(
                    caller_target(
                        &matches,
                        field.finish().unwrap(),
                        Some("caller"),
                        default_caller,
                        false
                    )
                    .unwrap()
                    .as_deref(),
                    expected
                );
                if values.is_empty() {
                    assert_eq!(
                        caller_target(&matches, None, None, default_caller, false).unwrap(),
                        None
                    );
                }
            }
        }
        for name in ["neighbor", "focus", "resize", "swap"] {
            let parsed = request(
                name,
                &["--pane", "explicit", "--current", "--direction", "left"],
            )
            .unwrap();
            assert!(serde_json::to_value(parsed).unwrap()["params"]
                .get("pane_id")
                .is_none());
        }
    }

    #[test]
    fn parse_pane_input_args_requires_and_uses_calling_pane() {
        let leaf = bindings::LEAVES
            .iter()
            .find(|leaf| leaf.name == "input")
            .unwrap();
        let matches = leaf
            .command()
            .no_binary_name(true)
            .try_get_matches_from(["--current", "--right-click", "pane"])
            .unwrap();
        let mut params =
            crate::api::schema::panes::PaneInputProjection::from_matches(&matches).unwrap();
        params.pane_id = caller_target(&matches, None, Some("issue-1:p1"), false, true)
            .unwrap()
            .unwrap();
        assert_eq!(params.pane_id, "issue-1:p1");
        assert_eq!(params.right_click, PaneRightClickTarget::Pane);
        assert!(caller_target(&matches, None, None, false, true).is_err());
    }

    #[test]
    fn parse_pane_input_args_rejects_conflicting_selectors() {
        assert!(request(
            "input",
            &["pane-a", "--pane", "pane-b", "--right-click", "pane"]
        )
        .is_err());
        assert!(request(
            "input",
            &["--pane", "pane-a", "--current", "--right-click", "pane"]
        )
        .is_err());
    }

    #[test]
    fn pane_projection_split_ordered_values() {
        let params = parse_pane_split_args(
            &[
                "--pane",
                "explicit",
                "--current",
                "--direction",
                "right",
                "--direction",
                "down",
                "--focus",
                "--no-focus",
                "--focus",
                "--env",
                "A=first",
                "--env",
                "A=last",
            ],
            Some("caller"),
        );
        assert_eq!(params.target_pane_id.as_deref(), Some("caller"));
        assert_eq!(params.direction, SplitDirection::Down);
        assert!(params.focus);
        assert_eq!(params.env.get("A").map(String::as_str), Some("last"));
    }

    #[test]
    fn parse_pane_swap_args_accepts_explicit_source_and_target() {
        let Method::PaneSwap(params) = request(
            "swap",
            &["--source-pane", "issue-1", "--target-pane", "issue-2"],
        )
        .unwrap()
        .method
        else {
            panic!()
        };
        assert_eq!(params.direction, None);
        assert_eq!(params.source_pane_id, Some("issue-1".into()));
        assert_eq!(params.target_pane_id, Some("issue-2".into()));
        assert!(request(
            "swap",
            &[
                "--direction",
                "left",
                "--source-pane",
                "a",
                "--target-pane",
                "b"
            ]
        )
        .is_err());
    }

    #[test]
    fn parse_pane_move_args_accepts_existing_tab_destination() {
        let Method::PaneMove(params) = request(
            "move",
            &[
                "issue-1",
                "--tab",
                "issue:2",
                "--split",
                "right",
                "--target-pane",
                "issue-3",
                "--ratio",
                "0.25",
                "--no-focus",
            ],
        )
        .unwrap()
        .method
        else {
            panic!()
        };
        assert_eq!(params.pane_id, "issue-1");
        assert!(!params.focus);
        assert_eq!(
            params.destination,
            PaneMoveDestination::Tab {
                tab_id: "issue:2".into(),
                target_pane_id: Some("issue-3".into()),
                split: SplitDirection::Right,
                ratio: Some(0.25)
            }
        );
    }

    #[test]
    fn pane_projection_move_and_finite_values() {
        for destination in ["--new-tab", "--new-workspace"] {
            let parsed = request(
                "move",
                &[
                    "pane",
                    destination,
                    "--ratio",
                    "0.5",
                    "--no-focus",
                    "--focus",
                ],
            )
            .unwrap();
            let json = serde_json::to_value(parsed).unwrap();
            assert_eq!(json["params"]["focus"], true);
            assert!(json["params"]["destination"].get("ratio").is_none());
        }
        for value in ["NaN", "inf", "-inf"] {
            for (name, values) in [
                ("resize", vec!["--direction", "right", "--amount", value]),
                ("split", vec!["--direction", "right", "--ratio", value]),
                (
                    "move",
                    vec!["pane", "--tab", "tab", "--split", "right", "--ratio", value],
                ),
            ] {
                assert!(request(name, &values).is_err());
            }
        }
    }

    #[test]
    fn parse_pane_zoom_args_defaults_to_current_toggle() {
        let Method::PaneZoom(params) = request("zoom", &[]).unwrap().method else {
            panic!()
        };
        assert_eq!(params.pane_id, None);
        assert_eq!(params.mode, PaneZoomMode::Toggle);
        for (values, pane, mode) in [
            (vec!["issue-1", "--on"], "issue-1", PaneZoomMode::On),
            (
                vec!["--pane", "issue-2", "--off"],
                "issue-2",
                PaneZoomMode::Off,
            ),
        ] {
            let Method::PaneZoom(params) = request("zoom", &values).unwrap().method else {
                panic!()
            };
            assert_eq!(params.pane_id.as_deref(), Some(pane));
            assert_eq!(params.mode, mode);
        }
        assert!(request("zoom", &["--on", "--off"]).is_err());
    }

    #[test]
    fn pane_projection_read_preserves_defaults_and_ansi_order() {
        for (values, source, lines, format, strip) in [
            (
                vec!["issue-1"],
                ReadSource::Recent,
                None,
                ReadFormat::Text,
                true,
            ),
            (
                vec!["issue-1", "--source", "visible", "--lines", "5", "--ansi"],
                ReadSource::Visible,
                Some(5),
                ReadFormat::Ansi,
                true,
            ),
            (
                vec!["--source=visible", "--lines=5", "issue-1"],
                ReadSource::Visible,
                Some(5),
                ReadFormat::Text,
                true,
            ),
            (
                vec![
                    "--raw",
                    "issue-1",
                    "--format=text",
                    "--ansi",
                    "--format",
                    "text",
                    "--lines=0",
                    "--source=recent_unwrapped",
                ],
                ReadSource::RecentUnwrapped,
                Some(0),
                ReadFormat::Text,
                false,
            ),
            (
                vec!["issue-1", "--format=text", "--raw"],
                ReadSource::Recent,
                None,
                ReadFormat::Ansi,
                false,
            ),
        ] {
            let Method::PaneRead(params) = request("read", &values).unwrap().method else {
                panic!()
            };
            assert_eq!(params.pane_id, "issue-1");
            assert_eq!(params.source, source);
            assert_eq!(params.lines, lines);
            assert_eq!(params.format, format);
            assert_eq!(params.strip_ansi, strip);
            assert_eq!(params.intent, crate::api::schema::ReadIntent::Interactive);
        }
    }

    #[test]
    fn pane_projection_wait_preserves_matcher_and_defaults() {
        let Method::PaneWaitForOutput(params) = request(
            "wait-output",
            &["issue-1", "--match", "ready", "--timeout", "5000"],
        )
        .unwrap()
        .method
        else {
            panic!()
        };
        assert_eq!(params.pane_id, "issue-1");
        assert_eq!(
            params.r#match,
            OutputMatch::Substring {
                value: "ready".into()
            }
        );
        assert_eq!(params.timeout_ms, Some(5000));
        assert_eq!(params.source, ReadSource::Recent);
        assert!(params.strip_ansi);
        let Method::PaneWaitForOutput(params) = request(
            "wait-output",
            &[
                "--regex=a=b",
                "--source=detection",
                "issue-1",
                "--timeout=0",
                "--raw",
            ],
        )
        .unwrap()
        .method
        else {
            panic!()
        };
        assert_eq!(
            params.r#match,
            OutputMatch::Regex {
                value: "a=b".into()
            }
        );
        assert_eq!(params.source, ReadSource::Detection);
        assert_eq!(params.timeout_ms, Some(0));
        assert!(!params.strip_ansi);
        assert!(request("wait-output", &["issue-1"]).is_err());
        assert!(request("wait-output", &["issue-1", "--match", "a", "--regex", "b"]).is_err());
    }

    #[test]
    fn pane_projection_atomic_run_joined_text_and_raw_keys() {
        for (name, field) in [("run", "text"), ("send-text", "text"), ("rename", "label")] {
            let parsed = request(name, &["pane", "a", "--literal", "b"]).unwrap();
            let json = serde_json::to_value(parsed).unwrap();
            assert_eq!(json["params"][field], "a --literal b");
            if name == "run" {
                assert_eq!(json["params"]["keys"], serde_json::json!(["Enter"]));
                assert_eq!(json["method"], "pane.send_input");
                assert_eq!(json["id"], "cli:request");
            }
        }
        let Method::PaneSendKeys(params) =
            request("send-keys", &["pane", "Enter", "--literal", "esc"])
                .unwrap()
                .method
        else {
            panic!()
        };
        assert_eq!(params.keys, args(&["Enter", "--literal", "esc"]));
        let Method::PaneRename(params) = request("rename", &["pane", "--clear"]).unwrap().method
        else {
            panic!()
        };
        assert_eq!(params.label, None);
    }

    #[test]
    fn pane_projection_resume_presence_and_metadata_patches() {
        for name in ["report-agent", "report-agent-session"] {
            for (suffix, expected) in [
                (vec![], None),
                (vec!["--"], Some(vec![])),
                (
                    vec!["--", "agent", "--source=y", "--"],
                    Some(args(&["agent", "--source=y", "--"])),
                ),
            ] {
                let mut values = vec![
                    "--source=  hook  ",
                    "pane",
                    "--agent=a",
                    "--seq=0",
                    "--agent-session-id=session",
                    "--agent-session-path=/path",
                ];
                if name == "report-agent" {
                    values.push("--state=idle");
                }
                values.extend(suffix);
                let json = serde_json::to_value(request(name, &values).unwrap()).unwrap();
                assert_eq!(json["params"]["source"], "hook");
                assert_eq!(json["params"]["seq"], 0);
                match expected {
                    Some(argv) => assert_eq!(
                        json["params"]["resume_argv"],
                        serde_json::to_value(argv).unwrap()
                    ),
                    None => assert!(json["params"].get("resume_argv").is_none()),
                }
            }
        }
        let json = serde_json::to_value(
            request(
                "report-metadata",
                &[
                    "pane",
                    "--source",
                    "  hook  ",
                    "--title",
                    "",
                    "--token",
                    "K=a=b",
                    "--clear-token",
                    "K",
                    "--token",
                    "E=",
                    "--state-label",
                    " Idle =ready",
                    "--seq",
                    "0",
                ],
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(json["params"]["source"], "hook");
        assert_eq!(json["params"]["title"], "");
        assert_eq!(
            json["params"]["tokens"],
            serde_json::json!({"K": null, "E": ""})
        );
        assert_eq!(
            json["params"]["state_labels"],
            serde_json::json!({"idle": "ready"})
        );
        assert!(request("report-metadata", &["pane", "--source", "hook"]).is_err());
        assert!(request(
            "report-metadata",
            &["pane", "--source", "hook", "--title", "x", "--clear-title"]
        )
        .is_err());
    }

    #[test]
    fn pane_projection_help_and_invalid_syntax_do_not_use_transport() {
        for leaf in bindings::LEAVES {
            assert_eq!(run_pane_command(&args(&[leaf.name, "--help"])).unwrap(), 0);
        }
        for name in [
            "get",
            "close",
            "send-text",
            "send-keys",
            "run",
            "rename",
            "input",
            "move",
            "report-agent",
            "report-agent-session",
            "release-agent",
            "report-metadata",
        ] {
            assert_eq!(run_pane_command(&args(&[name])).unwrap(), 2);
        }
    }
}
