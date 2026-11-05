use clap::{ArgMatches, Command};

use crate::api::schema::shepherd::*;
use crate::api::schema::{Method, Request};
use crate::cli::projection::{self, cli_params, cli_projection, Field, Input, ParseError};

// =========================================================================
// Input Helpers for Shepherd CLI Projections
// =========================================================================

pub(crate) fn root_option() -> Input<Option<String>> {
    Input::value("root", "PATH", |slot, val| {
        *slot = Some(val.to_owned());
        Ok(())
    })
    .hidden()
}

pub(crate) fn text_position(
    name: &'static str,
    value_name: &'static str,
    index: usize,
) -> Input<String> {
    Input::positional(name, index, |slot, val| {
        *slot = val.to_owned();
        Ok(())
    })
    .value_name(value_name)
    .verbatim()
}

pub(crate) fn optional_text_position(
    name: &'static str,
    value_name: &'static str,
    index: usize,
) -> Input<Option<String>> {
    Input::positional(name, index, |slot, val| {
        *slot = Some(val.to_owned());
        Ok(())
    })
    .value_name(value_name)
    .configure(|arg| arg.required(false))
    .verbatim()
}

pub(crate) fn rest_position(
    name: &'static str,
    value_name: &'static str,
    index: usize,
) -> Input<String> {
    Input::rest(name, value_name, index, |slot, val| {
        *slot = val.to_owned();
        Ok(())
    })
    .configure(|arg| arg.allow_hyphen_values(false))
    .verbatim()
}

pub(crate) fn optional_text(name: &'static str, value_name: &'static str) -> Input<Option<String>> {
    projection::optional_text(name, value_name)
}

pub(crate) fn text_value(name: &'static str, value_name: &'static str) -> Input<String> {
    Input::value(name, value_name, |slot, val| {
        *slot = val.to_owned();
        Ok(())
    })
}

pub(crate) fn boolean_flag(name: &'static str) -> Input<bool> {
    Input::flag(name, |slot, _| {
        *slot = true;
        Ok(())
    })
}

pub(crate) fn optional_bool_flag(name: &'static str) -> Input<Option<bool>> {
    Input::flag(name, |slot, _| {
        *slot = Some(true);
        Ok(())
    })
}

pub(crate) fn u32_value(name: &'static str, value_name: &'static str) -> Input<u32> {
    Input::value(name, value_name, |slot, val| {
        *slot = val
            .parse::<u32>()
            .map_err(|e| ParseError::Usage(format!("invalid number: {e}")))?;
        Ok(())
    })
}

pub(crate) fn optional_u32(name: &'static str, value_name: &'static str) -> Input<Option<u32>> {
    Input::value(name, value_name, |slot, val| {
        *slot = Some(
            val.parse::<u32>()
                .map_err(|e| ParseError::Usage(format!("invalid number: {e}")))?,
        );
        Ok(())
    })
}

pub(crate) fn u64_value(name: &'static str, value_name: &'static str) -> Input<u64> {
    Input::value(name, value_name, |slot, val| {
        *slot = val
            .parse::<u64>()
            .map_err(|e| ParseError::Usage(format!("invalid number: {e}")))?;
        Ok(())
    })
}

pub(crate) fn optional_usize(name: &'static str, value_name: &'static str) -> Input<Option<usize>> {
    Input::value(name, value_name, |slot, val| {
        *slot = Some(
            val.parse::<usize>()
                .map_err(|e| ParseError::Usage(format!("invalid number: {e}")))?,
        );
        Ok(())
    })
}

pub(crate) fn string_list(name: &'static str, value_name: &'static str) -> Input<Vec<String>> {
    Input::<Vec<String>>::value(name, value_name, |slot, val| {
        slot.push(val.to_owned());
        Ok(())
    })
}

pub(crate) fn optional_positional_list(
    name: &'static str,
    value_name: &'static str,
    index: usize,
) -> Input<Vec<String>> {
    Input::<Vec<String>>::positional(name, index, |slot, val| {
        slot.push(val.to_owned());
        Ok(())
    })
    .value_name(value_name)
    .configure(|arg| arg.required(false).num_args(0..))
    .verbatim()
}

// =========================================================================
// Local (Non-API) Parameter Schemas and Projections
// =========================================================================

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Default)]
    pub struct TaskShowParams => TaskShowProjection {
        pub reference: String => Field::new(String::new()).input(text_position("id-or-name", "ID-OR-NAME", 0)),
        pub section: Option<String> => Field::new(None)
            .input(optional_text_position("section", "SECTION", 1)),
    }
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Default)]
    pub struct TaskPlanParams => TaskPlanProjection {
        pub reference: String => Field::new(String::new()).input(text_position("id-or-name", "ID-OR-NAME", 0)),
        pub save: bool => Field::new(false)
            .input(boolean_flag("save")),
    }
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Default)]
    pub struct TaskEditParams => TaskEditProjection {
        pub reference: String => Field::new(String::new()).input(text_position("id-or-name", "ID-OR-NAME", 0)),
        pub section: Option<String> => Field::new(None)
            .input(optional_text_position("section", "SECTION", 1)),
    }
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Default)]
    pub struct TaskPathParams => TaskPathProjection {
        pub reference: String => Field::new(String::new())
            .input(text_position("id-or-name", "ID-OR-NAME", 0)),
    }
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Default)]
    pub struct PatternEditParams => PatternEditProjection {
        pub name: String => Field::new(String::new())
            .input(text_position("name", "NAME", 0)),
    }
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Default)]
    pub struct PatternNewLocalParams => PatternNewLocalProjection {
        pub name: String => Field::new(String::new()).input(text_position("name", "NAME", 0)),
        pub from: Option<String> => Field::new(None).input(optional_text("from", "TEMPLATE")),
        pub global: bool => Field::new(false)
            .input(boolean_flag("global")),
    }
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Default)]
    pub struct McpServeParams => McpServeProjection {
        pub port: Option<u16> => Field::new(None)
            .input(Input::positional("port", 0, |slot, val| {
                *slot = Some(val.parse::<u16>().map_err(|e| ParseError::Usage(format!("invalid port: {e}")))?);
                Ok(())
            }).configure(|a| a.required(false))),
    }
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Default)]
    pub struct AttachParams => AttachProjection {
        pub target: Option<String> => Field::new(None).input(optional_text_position("target", "TARGET", 0)),
        pub takeover: bool => Field::new(false)
            .input(boolean_flag("takeover")),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LocalEmptyParams {}

cli_projection! {
    pub(crate) LocalEmptyProjection for LocalEmptyParams {}
}

// =========================================================================
// Operation Enum: Typed API Request or Local Command
// =========================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LocalCommand {
    McpServe {
        port: u16,
    },
    Attach {
        target: Option<String>,
        takeover: bool,
    },
    ConfigPath,
    ConfigValidate,
    RolesList,
    VendorsList,
    TaskShow {
        reference: String,
        section: Option<String>,
    },
    TaskPlan {
        reference: String,
        save: bool,
    },
    TaskEdit {
        reference: String,
        section: Option<String>,
    },
    TaskPath {
        reference: String,
    },
    PatternNew {
        name: String,
        from: String,
        global: bool,
    },
    PatternEdit {
        name: String,
    },
}

#[derive(Debug, Clone)]
pub(crate) enum ShepherdOperation {
    Api(Request),
    Local(LocalCommand),
}

pub(crate) const USAGE_STRING: &str =
    "usage: shepherd attach [shepherd-agent-name-or-pane-id] [--takeover] | shepherd mcp serve [port] | shepherd run <task-id> [--mcp-url <url>] [--auto-push] [--wait] [--json] | shepherd approve <task-id> plan|<subtask-number> [--json] | shepherd reject <task-id> plan|<subtask-number> [--reason <text>] [--json] | shepherd spawn <role> <task...> [--task-id <id>] [--vendor <name>] [--model <name>] [--mcp-url <url>] [--json] | shepherd agents [--json] | shepherd skills list [--json] | shepherd runs list [--json] | shepherd roles list [--json] | shepherd vendors list [--json] | shepherd config path|validate [--json] | shepherd task new <title> [--description <text>] [--criteria <text>]... [--id <number>] [--json] | task list [--archived] [--json] | task show <id-or-name> [section] [--json] | task plan <id-or-name> [--save] [--json] | task edit <id-or-name> [section] | task path <id-or-name> [--json] | task archive <id-or-name> [--json] | shepherd pattern list [--json] | pattern show <name> [--json] | pattern new <name> --from <template|name> [--global] [--json] | pattern validate <name|--file path> [--json] | pattern delete <name> [--json] | pattern edit <name> | shepherd topic list [--json] | topic create <name> [--partitions <n>] [--json] | topic publish <topic> <content> [--key <key>] [--partition <p>] [--from <from>] [--json] | topic tail <topic> [--partition <p>] [--limit <n>] [--json] | topic replay <topic> [--partition <p>] [--from <offset>] [--limit <n>] [--json] | topic groups [topic] [--json]";

// =========================================================================
// Command Definitions
// =========================================================================

pub(crate) fn agents_command() -> Command {
    Command::new("agents")
        .about("List managed Shepherd agents")
        .args(ShepherdAgentListProjection::arguments())
}

pub(crate) fn spawn_command() -> Command {
    Command::new("spawn")
        .about("Spawn a new Shepherd agent")
        .args(ShepherdAgentSpawnProjection::arguments())
}

pub(crate) fn kill_command() -> Command {
    Command::new("kill")
        .about("Kill a running Shepherd agent")
        .args(ShepherdAgentTargetProjection::arguments())
}

pub(crate) fn clean_command() -> Command {
    Command::new("clean")
        .about("Clean finished or killed agents")
        .args(ShepherdAgentCleanProjection::arguments())
}

pub(crate) fn focus_command() -> Command {
    Command::new("focus")
        .about("Focus a Shepherd agent's terminal pane")
        .args(ShepherdAgentTargetProjection::arguments())
}

#[allow(dead_code)] // Reserved for full clap command tree and help projection
pub(crate) fn task_command() -> Command {
    Command::new("task")
        .about("Manage Shepherd tasks")
        .subcommand_required(true)
        .subcommand(
            Command::new("list")
                .about("List tasks")
                .args(ShepherdTaskListProjection::arguments()),
        )
        .subcommand(
            Command::new("new")
                .about("Create a new task")
                .args(ShepherdTaskCreateProjection::arguments()),
        )
        .subcommand(
            Command::new("archive")
                .about("Archive a task")
                .args(ShepherdTaskArchiveProjection::arguments()),
        )
        .subcommand(
            Command::new("show")
                .about("Show task details")
                .args(TaskShowProjection::arguments()),
        )
        .subcommand(
            Command::new("plan")
                .about("Display or evaluate task plan")
                .args(TaskPlanProjection::arguments()),
        )
        .subcommand(
            Command::new("edit")
                .about("Edit task in $EDITOR")
                .args(TaskEditProjection::arguments()),
        )
        .subcommand(
            Command::new("path")
                .about("Print task directory path")
                .args(TaskPathProjection::arguments()),
        )
}

pub(crate) fn tasks_command() -> Command {
    Command::new("tasks")
        .about("List tasks (alias for task list)")
        .args(ShepherdTaskListProjection::arguments())
}

#[allow(dead_code)] // Reserved for full clap command tree and help projection
pub(crate) fn skills_command() -> Command {
    Command::new("skills")
        .about("Manage skills")
        .subcommand_required(true)
        .subcommand(
            Command::new("list")
                .about("List available skills")
                .args(ShepherdSkillListProjection::arguments()),
        )
}

#[allow(dead_code)] // Reserved for full clap command tree and help projection
pub(crate) fn runs_command() -> Command {
    Command::new("runs")
        .about("Manage workflow runs")
        .subcommand_required(true)
        .subcommand(
            Command::new("list")
                .about("List runs")
                .args(ShepherdRunListProjection::arguments()),
        )
}

pub(crate) fn run_command() -> Command {
    Command::new("run")
        .about("Start a workflow run for a task")
        .args(ShepherdRunStartProjection::arguments())
}

pub(crate) fn approve_command() -> Command {
    Command::new("approve")
        .about("Approve a pending workflow gate")
        .args(ShepherdGateActionProjection::arguments())
}

pub(crate) fn reject_command() -> Command {
    Command::new("reject")
        .about("Reject a pending workflow gate")
        .args(ShepherdGateRejectProjection::arguments())
}

pub(crate) fn git_command() -> Command {
    Command::new("git")
        .about("Git repository and worktree status")
        .args(ShepherdGitStatusProjection::arguments())
        .subcommand(
            Command::new("status")
                .about("Git status")
                .args(ShepherdGitStatusProjection::arguments()),
        )
}

#[allow(dead_code)] // Reserved for full clap command tree and help projection
pub(crate) fn pattern_command() -> Command {
    Command::new("pattern")
        .about("Manage multi-agent collaboration patterns")
        .subcommand_required(true)
        .subcommand(
            Command::new("list")
                .about("List patterns")
                .args(ShepherdPatternListProjection::arguments()),
        )
        .subcommand(
            Command::new("show")
                .about("Show pattern details")
                .args(ShepherdPatternGetProjection::arguments()),
        )
        .subcommand(
            Command::new("new")
                .about("Create a pattern from a template")
                .args(PatternNewLocalProjection::arguments()),
        )
        .subcommand(
            Command::new("validate")
                .about("Validate a pattern")
                .args(ShepherdPatternValidateProjection::arguments()),
        )
        .subcommand(
            Command::new("delete")
                .about("Delete a custom pattern")
                .args(ShepherdPatternDeleteProjection::arguments()),
        )
        .subcommand(
            Command::new("edit")
                .about("Edit a custom pattern in $EDITOR")
                .args(PatternEditProjection::arguments()),
        )
}

pub(crate) fn patterns_command() -> Command {
    Command::new("patterns")
        .about("List patterns (alias for pattern list)")
        .args(ShepherdPatternListProjection::arguments())
}

#[allow(dead_code)] // Reserved for full clap command tree and help projection
pub(crate) fn topic_command() -> Command {
    Command::new("topic")
        .about("Manage message bus topics")
        .subcommand_required(true)
        .subcommand(
            Command::new("list")
                .about("List topics")
                .args(LocalEmptyProjection::arguments()),
        )
        .subcommand(
            Command::new("create")
                .about("Create a topic")
                .args(ShepherdTopicCreateProjection::arguments()),
        )
        .subcommand(
            Command::new("publish")
                .about("Publish message to a topic")
                .args(ShepherdTopicPublishProjection::arguments()),
        )
        .subcommand(
            Command::new("tail")
                .about("Tail messages from a topic")
                .args(ShepherdTopicTailProjection::arguments()),
        )
        .subcommand(
            Command::new("replay")
                .about("Replay messages from an offset")
                .args(ShepherdTopicReplayProjection::arguments()),
        )
        .subcommand(
            Command::new("groups")
                .about("List consumer groups")
                .args(ShepherdTopicGroupsProjection::arguments()),
        )
}

pub(crate) fn topics_command() -> Command {
    Command::new("topics")
        .about("List topics (alias for topic list)")
        .args(LocalEmptyProjection::arguments())
}

#[allow(dead_code)] // Reserved for full clap command tree and help projection
pub(crate) fn roles_command() -> Command {
    Command::new("roles")
        .about("List agent roles")
        .subcommand_required(true)
        .subcommand(
            Command::new("list")
                .about("List roles")
                .args(LocalEmptyProjection::arguments()),
        )
}

#[allow(dead_code)] // Reserved for full clap command tree and help projection
pub(crate) fn vendors_command() -> Command {
    Command::new("vendors")
        .about("List supported LLM CLI vendors")
        .subcommand_required(true)
        .subcommand(
            Command::new("list")
                .about("List vendors")
                .args(LocalEmptyProjection::arguments()),
        )
}

#[allow(dead_code)] // Reserved for full clap command tree and help projection
pub(crate) fn config_command() -> Command {
    Command::new("config")
        .about("Shepherd configuration inspection")
        .subcommand_required(true)
        .subcommand(
            Command::new("path")
                .about("Show configuration file paths")
                .args(LocalEmptyProjection::arguments()),
        )
        .subcommand(
            Command::new("validate")
                .about("Validate configuration")
                .args(LocalEmptyProjection::arguments()),
        )
}

#[allow(dead_code)] // Reserved for full clap command tree and help projection
pub(crate) fn mcp_command() -> Command {
    Command::new("mcp")
        .about("Run embedded Model Context Protocol (MCP) server")
        .subcommand_required(true)
        .subcommand(
            Command::new("serve")
                .about("Serve MCP over HTTP")
                .args(McpServeProjection::arguments()),
        )
}

pub(crate) fn attach_command() -> Command {
    Command::new("attach")
        .about("Attach to a running Shepherd agent or session")
        .args(AttachProjection::arguments())
}

// =========================================================================
// Top-level `shepherd` command tree
// =========================================================================

#[allow(dead_code)] // Reserved for full clap command tree and help projection
pub(crate) fn command() -> Command {
    Command::new("shepherd")
        .about("Shepherd multi-agent orchestration runtime")
        .subcommand(agents_command())
        .subcommand(spawn_command())
        .subcommand(kill_command())
        .subcommand(clean_command())
        .subcommand(focus_command())
        .subcommand(task_command())
        .subcommand(tasks_command())
        .subcommand(runs_command())
        .subcommand(run_command())
        .subcommand(approve_command())
        .subcommand(reject_command())
        .subcommand(skills_command())
        .subcommand(git_command())
        .subcommand(pattern_command())
        .subcommand(patterns_command())
        .subcommand(topic_command())
        .subcommand(topics_command())
        .subcommand(roles_command())
        .subcommand(vendors_command())
        .subcommand(config_command())
        .subcommand(mcp_command())
        .subcommand(attach_command())
}

// =========================================================================
// Parsing logic
// =========================================================================

pub(crate) fn parse_operation(args: &[String]) -> Result<ShepherdOperation, ParseError> {
    if args.is_empty() || args.iter().any(|a| a == "--help" || a == "-h") {
        return Err(ParseError::Usage(USAGE_STRING.to_string()));
    }

    let cmd_name = args[0].as_str();
    let rest = &args[1..];

    match cmd_name {
        "agents" => {
            let matches = parse_matches(agents_command(), rest)?;
            let params = ShepherdAgentListProjection::from_matches(&matches)?;
            Ok(ShepherdOperation::Api(Request {
                id: "cli:shepherd:agent:list".into(),
                method: Method::ShepherdAgentList(params),
            }))
        }
        "spawn" => {
            let matches = parse_matches(spawn_command(), rest)?;
            let params = ShepherdAgentSpawnProjection::from_matches(&matches)?;
            if params.role.is_empty() {
                return Err(ParseError::Usage(USAGE_STRING.to_string()));
            }
            Ok(ShepherdOperation::Api(Request {
                id: "cli:shepherd:agent:spawn".into(),
                method: Method::ShepherdAgentSpawn(params),
            }))
        }
        "kill" => {
            let matches = parse_matches(kill_command(), rest)?;
            let params = ShepherdAgentTargetProjection::from_matches(&matches)?;
            if params.id.is_empty() {
                return Err(ParseError::Usage(USAGE_STRING.to_string()));
            }
            Ok(ShepherdOperation::Api(Request {
                id: "cli:shepherd:agent:kill".into(),
                method: Method::ShepherdAgentKill(params),
            }))
        }
        "clean" => {
            let matches = parse_matches(clean_command(), rest)?;
            let params = ShepherdAgentCleanProjection::from_matches(&matches)?;
            Ok(ShepherdOperation::Api(Request {
                id: "cli:shepherd:agent:clean".into(),
                method: Method::ShepherdAgentClean(params),
            }))
        }
        "focus" => {
            let matches = parse_matches(focus_command(), rest)?;
            let params = ShepherdAgentTargetProjection::from_matches(&matches)?;
            if params.id.is_empty() {
                return Err(ParseError::Usage(USAGE_STRING.to_string()));
            }
            Ok(ShepherdOperation::Api(Request {
                id: "cli:shepherd:agent:focus".into(),
                method: Method::ShepherdAgentFocus(params),
            }))
        }
        "tasks" => {
            let matches = parse_matches(tasks_command(), rest)?;
            let params = ShepherdTaskListProjection::from_matches(&matches)?;
            Ok(ShepherdOperation::Api(Request {
                id: "cli:shepherd:task:list".into(),
                method: Method::ShepherdTaskList(params),
            }))
        }
        "task" => {
            let Some(subcmd) = rest.first().map(String::as_str) else {
                return Err(ParseError::Usage(USAGE_STRING.to_string()));
            };
            let sub_args = &rest[1..];
            match subcmd {
                "list" => {
                    let matches = parse_matches(
                        Command::new("list").args(ShepherdTaskListProjection::arguments()),
                        sub_args,
                    )?;
                    let params = ShepherdTaskListProjection::from_matches(&matches)?;
                    Ok(ShepherdOperation::Api(Request {
                        id: "cli:shepherd:task:list".into(),
                        method: Method::ShepherdTaskList(params),
                    }))
                }
                "new" => {
                    let matches = parse_matches(
                        Command::new("new").args(ShepherdTaskCreateProjection::arguments()),
                        sub_args,
                    )?;
                    let params = ShepherdTaskCreateProjection::from_matches(&matches)?;
                    if params.title.is_empty() {
                        return Err(ParseError::Usage(USAGE_STRING.to_string()));
                    }
                    Ok(ShepherdOperation::Api(Request {
                        id: "cli:shepherd:task:create".into(),
                        method: Method::ShepherdTaskCreate(params),
                    }))
                }
                "archive" => {
                    let matches = parse_matches(
                        Command::new("archive").args(ShepherdTaskArchiveProjection::arguments()),
                        sub_args,
                    )?;
                    let params = ShepherdTaskArchiveProjection::from_matches(&matches)?;
                    if params.id.is_empty() {
                        return Err(ParseError::Usage(USAGE_STRING.to_string()));
                    }
                    Ok(ShepherdOperation::Api(Request {
                        id: "cli:shepherd:task:archive".into(),
                        method: Method::ShepherdTaskArchive(params),
                    }))
                }
                "show" => {
                    let matches = parse_matches(
                        Command::new("show").args(TaskShowProjection::arguments()),
                        sub_args,
                    )?;
                    let params = TaskShowProjection::from_matches(&matches)?;
                    if params.reference.is_empty() {
                        return Err(ParseError::Usage(USAGE_STRING.to_string()));
                    }
                    Ok(ShepherdOperation::Local(LocalCommand::TaskShow {
                        reference: params.reference,
                        section: params.section,
                    }))
                }
                "plan" => {
                    let matches = parse_matches(
                        Command::new("plan").args(TaskPlanProjection::arguments()),
                        sub_args,
                    )?;
                    let params = TaskPlanProjection::from_matches(&matches)?;
                    if params.reference.is_empty() {
                        return Err(ParseError::Usage(USAGE_STRING.to_string()));
                    }
                    Ok(ShepherdOperation::Local(LocalCommand::TaskPlan {
                        reference: params.reference,
                        save: params.save,
                    }))
                }
                "edit" => {
                    let matches = parse_matches(
                        Command::new("edit").args(TaskEditProjection::arguments()),
                        sub_args,
                    )?;
                    let params = TaskEditProjection::from_matches(&matches)?;
                    if params.reference.is_empty() {
                        return Err(ParseError::Usage(USAGE_STRING.to_string()));
                    }
                    Ok(ShepherdOperation::Local(LocalCommand::TaskEdit {
                        reference: params.reference,
                        section: params.section,
                    }))
                }
                "path" => {
                    let matches = parse_matches(
                        Command::new("path").args(TaskPathProjection::arguments()),
                        sub_args,
                    )?;
                    let params = TaskPathProjection::from_matches(&matches)?;
                    if params.reference.is_empty() {
                        return Err(ParseError::Usage(USAGE_STRING.to_string()));
                    }
                    Ok(ShepherdOperation::Local(LocalCommand::TaskPath {
                        reference: params.reference,
                    }))
                }
                _ => Err(ParseError::Usage(USAGE_STRING.to_string())),
            }
        }
        "runs" => {
            let Some("list") = rest.first().map(String::as_str) else {
                return Err(ParseError::Usage(USAGE_STRING.to_string()));
            };
            let matches = parse_matches(
                Command::new("list").args(ShepherdRunListProjection::arguments()),
                &rest[1..],
            )?;
            let params = ShepherdRunListProjection::from_matches(&matches)?;
            Ok(ShepherdOperation::Api(Request {
                id: "cli:shepherd:run:list".into(),
                method: Method::ShepherdRunList(params),
            }))
        }
        "run" => {
            let matches = parse_matches(run_command(), rest)?;
            let params = ShepherdRunStartProjection::from_matches(&matches)?;
            if params.task.is_empty() {
                return Err(ParseError::Usage(USAGE_STRING.to_string()));
            }
            Ok(ShepherdOperation::Api(Request {
                id: "cli:shepherd:run:start".into(),
                method: Method::ShepherdRunStart(params),
            }))
        }
        "approve" => {
            let matches = parse_matches(approve_command(), rest)?;
            let params = ShepherdGateActionProjection::from_matches(&matches)?;
            if params.task.is_empty() || params.target.is_empty() {
                return Err(ParseError::Usage(USAGE_STRING.to_string()));
            }
            Ok(ShepherdOperation::Api(Request {
                id: "cli:shepherd:run:approve".into(),
                method: Method::ShepherdRunApprove(params),
            }))
        }
        "reject" => {
            let matches = parse_matches(reject_command(), rest)?;
            let params = ShepherdGateRejectProjection::from_matches(&matches)?;
            if params.task.is_empty() || params.target.is_empty() {
                return Err(ParseError::Usage(USAGE_STRING.to_string()));
            }
            Ok(ShepherdOperation::Api(Request {
                id: "cli:shepherd:run:reject".into(),
                method: Method::ShepherdRunReject(params),
            }))
        }
        "skills" => {
            let Some("list") = rest.first().map(String::as_str) else {
                return Err(ParseError::Usage(USAGE_STRING.to_string()));
            };
            let matches = parse_matches(
                Command::new("list").args(ShepherdSkillListProjection::arguments()),
                &rest[1..],
            )?;
            let params = ShepherdSkillListProjection::from_matches(&matches)?;
            Ok(ShepherdOperation::Api(Request {
                id: "cli:shepherd:skill:list".into(),
                method: Method::ShepherdSkillList(params),
            }))
        }
        "git" => {
            let sub_args = if rest.first().map(String::as_str) == Some("status") {
                &rest[1..]
            } else {
                rest
            };
            let matches = parse_matches(git_command(), sub_args)?;
            let params = ShepherdGitStatusProjection::from_matches(&matches)?;
            Ok(ShepherdOperation::Api(Request {
                id: "cli:shepherd:git:status".into(),
                method: Method::ShepherdGitStatus(params),
            }))
        }
        "patterns" => {
            let matches = parse_matches(patterns_command(), rest)?;
            let params = ShepherdPatternListProjection::from_matches(&matches)?;
            Ok(ShepherdOperation::Api(Request {
                id: "cli:shepherd:pattern:list".into(),
                method: Method::ShepherdPatternList(params),
            }))
        }
        "pattern" => {
            let Some(subcmd) = rest.first().map(String::as_str) else {
                return Err(ParseError::Usage(USAGE_STRING.to_string()));
            };
            let sub_args = &rest[1..];
            match subcmd {
                "list" => {
                    let matches = parse_matches(
                        Command::new("list").args(ShepherdPatternListProjection::arguments()),
                        sub_args,
                    )?;
                    let params = ShepherdPatternListProjection::from_matches(&matches)?;
                    Ok(ShepherdOperation::Api(Request {
                        id: "cli:shepherd:pattern:list".into(),
                        method: Method::ShepherdPatternList(params),
                    }))
                }
                "show" => {
                    let matches = parse_matches(
                        Command::new("show").args(ShepherdPatternGetProjection::arguments()),
                        sub_args,
                    )?;
                    let params = ShepherdPatternGetProjection::from_matches(&matches)?;
                    if params.name.is_empty() {
                        return Err(ParseError::Usage(USAGE_STRING.to_string()));
                    }
                    Ok(ShepherdOperation::Api(Request {
                        id: "cli:shepherd:pattern:get".into(),
                        method: Method::ShepherdPatternGet(params),
                    }))
                }
                "new" => {
                    let matches = parse_matches(
                        Command::new("new").args(PatternNewLocalProjection::arguments()),
                        sub_args,
                    )?;
                    let params = PatternNewLocalProjection::from_matches(&matches)?;
                    let Some(from) = params.from else {
                        return Err(ParseError::Usage(USAGE_STRING.to_string()));
                    };
                    if params.name.is_empty() {
                        return Err(ParseError::Usage(USAGE_STRING.to_string()));
                    }
                    Ok(ShepherdOperation::Local(LocalCommand::PatternNew {
                        name: params.name,
                        from,
                        global: params.global,
                    }))
                }
                "validate" => {
                    let matches = parse_matches(
                        Command::new("validate")
                            .args(ShepherdPatternValidateProjection::arguments()),
                        sub_args,
                    )?;
                    let params = ShepherdPatternValidateProjection::from_matches(&matches)?;
                    Ok(ShepherdOperation::Api(Request {
                        id: "cli:shepherd:pattern:validate".into(),
                        method: Method::ShepherdPatternValidate(params),
                    }))
                }
                "delete" => {
                    let matches = parse_matches(
                        Command::new("delete").args(ShepherdPatternDeleteProjection::arguments()),
                        sub_args,
                    )?;
                    let params = ShepherdPatternDeleteProjection::from_matches(&matches)?;
                    if params.name.is_empty() {
                        return Err(ParseError::Usage(USAGE_STRING.to_string()));
                    }
                    Ok(ShepherdOperation::Api(Request {
                        id: "cli:shepherd:pattern:delete".into(),
                        method: Method::ShepherdPatternDelete(params),
                    }))
                }
                "edit" => {
                    let matches = parse_matches(
                        Command::new("edit").args(PatternEditProjection::arguments()),
                        sub_args,
                    )?;
                    let params = PatternEditProjection::from_matches(&matches)?;
                    if params.name.is_empty() {
                        return Err(ParseError::Usage(USAGE_STRING.to_string()));
                    }
                    Ok(ShepherdOperation::Local(LocalCommand::PatternEdit {
                        name: params.name,
                    }))
                }
                _ => Err(ParseError::Usage(USAGE_STRING.to_string())),
            }
        }
        "topics" => {
            let _matches = parse_matches(topics_command(), rest)?;
            let params = ShepherdEmptyParams::default();
            Ok(ShepherdOperation::Api(Request {
                id: "cli:shepherd:topic:list".into(),
                method: Method::ShepherdTopicList(params),
            }))
        }
        "topic" => {
            let Some(subcmd) = rest.first().map(String::as_str) else {
                return Err(ParseError::Usage(USAGE_STRING.to_string()));
            };
            let sub_args = &rest[1..];
            match subcmd {
                "list" => {
                    let matches = parse_matches(
                        Command::new("list").args(LocalEmptyProjection::arguments()),
                        sub_args,
                    )?;
                    let _ = LocalEmptyProjection::from_matches(&matches)?;
                    Ok(ShepherdOperation::Api(Request {
                        id: "cli:shepherd:topic:list".into(),
                        method: Method::ShepherdTopicList(ShepherdEmptyParams::default()),
                    }))
                }
                "create" => {
                    let matches = parse_matches(
                        Command::new("create").args(ShepherdTopicCreateProjection::arguments()),
                        sub_args,
                    )?;
                    let params = ShepherdTopicCreateProjection::from_matches(&matches)?;
                    if params.name.is_empty() {
                        return Err(ParseError::Usage(USAGE_STRING.to_string()));
                    }
                    Ok(ShepherdOperation::Api(Request {
                        id: "cli:shepherd:topic:create".into(),
                        method: Method::ShepherdTopicCreate(params),
                    }))
                }
                "publish" => {
                    let matches = parse_matches(
                        Command::new("publish").args(ShepherdTopicPublishProjection::arguments()),
                        sub_args,
                    )?;
                    let params = ShepherdTopicPublishProjection::from_matches(&matches)?;
                    if params.topic.is_empty() || params.content.is_empty() {
                        return Err(ParseError::Usage(USAGE_STRING.to_string()));
                    }
                    Ok(ShepherdOperation::Api(Request {
                        id: "cli:shepherd:topic:publish".into(),
                        method: Method::ShepherdTopicPublish(params),
                    }))
                }
                "tail" => {
                    let matches = parse_matches(
                        Command::new("tail").args(ShepherdTopicTailProjection::arguments()),
                        sub_args,
                    )?;
                    let params = ShepherdTopicTailProjection::from_matches(&matches)?;
                    if params.topic.is_empty() {
                        return Err(ParseError::Usage(USAGE_STRING.to_string()));
                    }
                    Ok(ShepherdOperation::Api(Request {
                        id: "cli:shepherd:topic:tail".into(),
                        method: Method::ShepherdTopicTail(params),
                    }))
                }
                "replay" => {
                    let matches = parse_matches(
                        Command::new("replay").args(ShepherdTopicReplayProjection::arguments()),
                        sub_args,
                    )?;
                    let params = ShepherdTopicReplayProjection::from_matches(&matches)?;
                    if params.topic.is_empty() {
                        return Err(ParseError::Usage(USAGE_STRING.to_string()));
                    }
                    Ok(ShepherdOperation::Api(Request {
                        id: "cli:shepherd:topic:replay".into(),
                        method: Method::ShepherdTopicReplay(params),
                    }))
                }
                "groups" => {
                    let matches = parse_matches(
                        Command::new("groups").args(ShepherdTopicGroupsProjection::arguments()),
                        sub_args,
                    )?;
                    let params = ShepherdTopicGroupsProjection::from_matches(&matches)?;
                    Ok(ShepherdOperation::Api(Request {
                        id: "cli:shepherd:topic:groups".into(),
                        method: Method::ShepherdTopicGroups(params),
                    }))
                }
                _ => Err(ParseError::Usage(USAGE_STRING.to_string())),
            }
        }
        "roles" => {
            let Some("list") = rest.first().map(String::as_str) else {
                return Err(ParseError::Usage(USAGE_STRING.to_string()));
            };
            let matches = parse_matches(
                Command::new("list").args(LocalEmptyProjection::arguments()),
                &rest[1..],
            )?;
            let _ = LocalEmptyProjection::from_matches(&matches)?;
            Ok(ShepherdOperation::Local(LocalCommand::RolesList))
        }
        "vendors" => {
            let Some("list") = rest.first().map(String::as_str) else {
                return Err(ParseError::Usage(USAGE_STRING.to_string()));
            };
            let matches = parse_matches(
                Command::new("list").args(LocalEmptyProjection::arguments()),
                &rest[1..],
            )?;
            let _ = LocalEmptyProjection::from_matches(&matches)?;
            Ok(ShepherdOperation::Local(LocalCommand::VendorsList))
        }
        "config" => {
            let Some(subcmd) = rest.first().map(String::as_str) else {
                return Err(ParseError::Usage(USAGE_STRING.to_string()));
            };
            let sub_args = &rest[1..];
            match subcmd {
                "path" => {
                    let matches = parse_matches(
                        Command::new("path").args(LocalEmptyProjection::arguments()),
                        sub_args,
                    )?;
                    let _ = LocalEmptyProjection::from_matches(&matches)?;
                    Ok(ShepherdOperation::Local(LocalCommand::ConfigPath))
                }
                "validate" => {
                    let matches = parse_matches(
                        Command::new("validate").args(LocalEmptyProjection::arguments()),
                        sub_args,
                    )?;
                    let _ = LocalEmptyProjection::from_matches(&matches)?;
                    Ok(ShepherdOperation::Local(LocalCommand::ConfigValidate))
                }
                _ => Err(ParseError::Usage(USAGE_STRING.to_string())),
            }
        }
        "mcp" => {
            let Some("serve") = rest.first().map(String::as_str) else {
                return Err(ParseError::Usage(USAGE_STRING.to_string()));
            };
            let matches = parse_matches(
                Command::new("serve").args(McpServeProjection::arguments()),
                &rest[1..],
            )?;
            let params = McpServeProjection::from_matches(&matches)?;
            Ok(ShepherdOperation::Local(LocalCommand::McpServe {
                port: params.port.unwrap_or(0),
            }))
        }
        "attach" => {
            let matches = parse_matches(attach_command(), rest)?;
            let params = AttachProjection::from_matches(&matches)?;
            Ok(ShepherdOperation::Local(LocalCommand::Attach {
                target: params.target,
                takeover: params.takeover,
            }))
        }
        _ => Err(ParseError::Usage(USAGE_STRING.to_string())),
    }
}

fn parse_matches(command: Command, args: &[String]) -> Result<ArgMatches, ParseError> {
    let clean_args: Vec<String> = args
        .iter()
        .filter(|a| a.as_str() != "--json" && a.as_str() != "--no-server")
        .cloned()
        .collect();
    command
        .no_binary_name(true)
        .try_get_matches_from(clean_args)
        .map_err(|e| {
            if matches!(
                e.kind(),
                clap::error::ErrorKind::MissingRequiredArgument
                    | clap::error::ErrorKind::UnknownArgument
                    | clap::error::ErrorKind::InvalidValue
            ) {
                ParseError::Usage(USAGE_STRING.to_string())
            } else {
                ParseError::Clap(e)
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_agents_command() {
        let op = parse_operation(&args(&["agents"])).unwrap();
        assert!(matches!(
            op,
            ShepherdOperation::Api(Request {
                method: Method::ShepherdAgentList(_),
                ..
            })
        ));
    }

    #[test]
    fn parses_spawn_command() {
        let op = parse_operation(&args(&[
            "spawn",
            "coder",
            "build feature x",
            "--vendor",
            "claude",
        ]))
        .unwrap();
        if let ShepherdOperation::Api(Request {
            method: Method::ShepherdAgentSpawn(params),
            ..
        }) = op
        {
            assert_eq!(params.role, "coder");
            assert_eq!(params.task, "build feature x");
            assert_eq!(params.vendor.as_deref(), Some("claude"));
        } else {
            panic!("expected spawn operation");
        }
    }

    #[test]
    fn parses_tasks_alias() {
        let op = parse_operation(&args(&["tasks"])).unwrap();
        assert!(matches!(
            op,
            ShepherdOperation::Api(Request {
                method: Method::ShepherdTaskList(_),
                ..
            })
        ));
    }

    #[test]
    fn parses_task_new() {
        let op = parse_operation(&args(&[
            "task",
            "new",
            "A new title",
            "--description",
            "Desc",
        ]))
        .unwrap();
        if let ShepherdOperation::Api(Request {
            method: Method::ShepherdTaskCreate(params),
            ..
        }) = op
        {
            assert_eq!(params.title, "A new title");
            assert_eq!(params.description.as_deref(), Some("Desc"));
        } else {
            panic!("expected task create");
        }
    }

    #[test]
    fn parses_topic_create() {
        let op = parse_operation(&args(&[
            "topic",
            "create",
            "test-topic",
            "--partitions",
            "3",
        ]))
        .unwrap();
        if let ShepherdOperation::Api(Request {
            method: Method::ShepherdTopicCreate(params),
            ..
        }) = op
        {
            assert_eq!(params.name, "test-topic");
            assert_eq!(params.partitions, 3);
        } else {
            panic!("expected topic create");
        }
    }

    #[test]
    fn usage_error_on_missing_required_args() {
        let err = parse_operation(&args(&["spawn"])).unwrap_err();
        if let ParseError::Usage(msg) = err {
            assert!(msg.starts_with("usage: shepherd"));
        } else {
            panic!("expected usage error");
        }
    }
}
