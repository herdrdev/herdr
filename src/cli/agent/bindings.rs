use clap::{ArgGroup, ArgMatches, Command};

use crate::api::schema::agents::{
    AgentPromptProjection, AgentPromptWaitProjection, AgentReadProjection, AgentRenameProjection,
    AgentSendKeysProjection, AgentStartProjection, AgentWaitProjection,
};
use crate::api::schema::transcript::AgentTranscriptProjection;
use crate::api::schema::{
    AgentArchiveParams, AgentForgetParams, AgentPromptParams, AgentReadParams, AgentRenameParams,
    AgentRestartParams, AgentSendKeysParams, AgentSessionTransferHarness, AgentStartParams,
    AgentStatus, AgentTarget, AgentTranscriptParams, AgentUnarchiveParams, AgentWaitParams,
    ReadFormat, ReadSource, READ_FORMATS, READ_SOURCES,
};
pub(crate) use crate::cli::projection::optional_text;
use crate::cli::projection::{
    choices, cli_projection, parse_choice, Choice, Field, Input, ParseError,
};

const STATUSES: &[Choice<AgentStatus>] = &[
    Choice::new("idle", AgentStatus::Idle),
    Choice::new("working", AgentStatus::Working),
    Choice::new("blocked", AgentStatus::Blocked),
    Choice::new("done", AgentStatus::Done),
    Choice::new("unknown", AgentStatus::Unknown),
];
const EXPLAIN_FORMATS: &[Choice<bool>] = &[Choice::new("text", false), Choice::new("json", true)];
const TRANSFER_HARNESSES: &[Choice<AgentSessionTransferHarness>] = &[
    Choice::new("claude", AgentSessionTransferHarness::Claude),
    Choice::new("codex", AgentSessionTransferHarness::Codex),
    Choice::new("omp", AgentSessionTransferHarness::Omp),
];

/// The `<TARGET>` every agent subcommand takes, with its accepted forms spelled
/// out so a caller hitting a resolution failure has a documented form to try (#181).
pub(crate) fn target() -> Input<String> {
    text_position("target", "TARGET", 0).help(
        "Agent name, pane id (w1:p1), terminal id, tab id, or <machine>/<id> for an \
         agent on a configured peer",
    )
}

pub(crate) fn text_position(
    name: &'static str,
    value_name: &'static str,
    index: usize,
) -> Input<String> {
    Input::positional(name, index, |slot, value| {
        *slot = value.to_owned();
        Ok(())
    })
    .value_name(value_name)
}

pub(crate) fn until() -> Input<Vec<AgentStatus>> {
    Input::<Vec<AgentStatus>>::value("until", "STATUS", |slot, value| {
        slot.push(parse_choice(STATUSES, "agent status", value)?);
        Ok(())
    })
    .configure(|arg| choices(arg, STATUSES))
}

pub(crate) fn timeout() -> Input<Option<u64>> {
    Input::value("timeout", "MS", |slot, value| {
        *slot = Some(super::super::parse_u64_flag("--timeout", value)?);
        Ok(())
    })
    .configure(|arg| arg.value_parser(clap::builder::ValueParser::new(validate_timeout)))
}

fn validate_timeout(value: &str) -> Result<String, String> {
    super::super::parse_u64_flag("--timeout", value)
        .map(|_| value.to_owned())
        .map_err(|error| error.to_string())
}

pub(crate) fn source() -> Input<ReadSource> {
    Input::value("source", "SOURCE", |slot, value| {
        *slot = parse_choice(READ_SOURCES, "read source", value)?;
        Ok(())
    })
    .configure(|arg| choices(arg, READ_SOURCES))
}

pub(crate) fn format() -> Input<ReadFormat> {
    Input::value("format", "FORMAT", |slot, value| {
        *slot = parse_choice(READ_FORMATS, "read format", value)?;
        Ok(())
    })
    .configure(|arg| choices(arg, READ_FORMATS))
}

pub(crate) fn ansi() -> Input<ReadFormat> {
    Input::flag("ansi", |slot, _| {
        *slot = ReadFormat::Ansi;
        Ok(())
    })
}

pub(crate) fn lines() -> Input<Option<u32>> {
    Input::value("lines", "N", |slot, value| {
        *slot = Some(super::super::parse_u32_flag("--lines", value)?);
        Ok(())
    })
}

pub(crate) fn transcript_before() -> Input<Option<String>> {
    optional_text("before", "CURSOR")
        .help("Page older turns ending just before this cursor (a previous page's `before`)")
        .configure(|arg| arg.conflicts_with("after"))
}

pub(crate) fn transcript_after() -> Input<Option<String>> {
    optional_text("after", "CURSOR")
        .help("Entries appended or changed since this cursor (a previous page's `after`)")
}

pub(crate) fn transcript_limit() -> Input<Option<u32>> {
    Input::value("limit", "TURNS", |slot, value| {
        *slot = Some(super::super::parse_u32_flag("--limit", value)?);
        Ok(())
    })
    .help("Turns per page (default 20, at most 50)")
}

pub(crate) fn transcript_subagent() -> Input<Option<String>> {
    optional_text("subagent", "ID")
        .help("Page this subagent's own conversation (an id from a page's `subagents`)")
}

pub(crate) fn keys() -> Input<Vec<String>> {
    Input::<Vec<String>>::positional("key", 1, |slot, value| {
        slot.push(value.to_owned());
        Ok(())
    })
    .value_name("KEY")
    .configure(|arg| arg.num_args(1..))
    .verbatim()
}

pub(crate) fn agent_args() -> Input<Vec<String>> {
    Input::<Vec<String>>::positional("agent_args", 1, |slot, value| {
        slot.push(value.to_owned());
        Ok(())
    })
    .value_name("AGENT_ARG")
    .configure(|arg| arg.required(false).num_args(0..).last(true))
}

pub(crate) fn kind() -> Input<String> {
    Input::value("kind", "KIND", |slot, value| {
        *slot = value.to_owned();
        Ok(())
    })
    .required()
    .configure(|arg| arg.value_parser(AgentKindCodec))
    .help("Supported agent kind and canonical executable")
}

/// Lists kinds for help and completion. Only the final `--kind` is validated, by
/// `agent_start`, so an overwritten earlier value is not rejected.
#[derive(Clone)]
struct AgentKindCodec;

impl clap::builder::TypedValueParser for AgentKindCodec {
    type Value = String;

    fn parse_ref(
        &self,
        command: &Command,
        argument: Option<&clap::Arg>,
        value: &std::ffi::OsStr,
    ) -> Result<String, clap::Error> {
        clap::builder::StringValueParser::new().parse_ref(command, argument, value)
    }

    fn possible_values(
        &self,
    ) -> Option<Box<dyn Iterator<Item = clap::builder::PossibleValue> + '_>> {
        Some(Box::new(crate::detect::Agent::ALL.into_iter().map(
            |agent| clap::builder::PossibleValue::new(crate::detect::agent_label(agent)),
        )))
    }
}

pub(crate) fn pane() -> Input<String> {
    Input::value("pane", "ID", |slot, value| {
        *slot = super::super::normalize_pane_id(value);
        Ok(())
    })
    .required()
    .help("Existing pane at an interactive shell prompt")
}

pub(crate) fn rename_name() -> Input<Option<String>> {
    Input::positional("name", 1, |slot, value| {
        *slot = Some(value.to_owned());
        Ok(())
    })
    .value_name("NAME")
    .verbatim()
    .configure(|arg| arg.required(false).conflicts_with("clear"))
}

pub(crate) fn clear_name() -> Input<Option<String>> {
    Input::flag("clear", |slot, _| {
        *slot = None;
        Ok(())
    })
}

pub(crate) fn wait_presence() -> Input<Option<crate::api::schema::AgentPromptWaitOptions>> {
    Input::flag("wait", |slot, _| {
        *slot = Some(crate::api::schema::AgentPromptWaitOptions {
            until: Vec::new(),
            timeout_ms: None,
            submission_deadline: None,
        });
        Ok(())
    })
    .help("Observe submission and then wait for the first matching state, when submission is verified")
}

cli_projection! {
    pub(crate) TargetProjection for AgentTarget {
        target => Field::new(String::new()).input(target()),
    }
}

pub(crate) struct Attach {
    pub(crate) target: String,
    pub(crate) takeover: bool,
}

cli_projection! {
    AttachProjection for Attach {
        target => Field::new(String::new()).input(target()),
        takeover => Field::new(false).input(Input::flag("takeover", |slot, _| { *slot = true; Ok(()) })),
    }
}

pub(crate) struct Explain {
    pub(crate) target: Option<String>,
    pub(crate) file: Option<String>,
    pub(crate) agent: Option<String>,
    pub(crate) json: bool,
    pub(crate) verbose: bool,
}

cli_projection! {
    ExplainProjection for Explain {
        target => Field::new(None).input(Input::positional("target", 0, |slot, value| {
            *slot = Some(value.to_owned()); Ok(())
        }).value_name("TARGET").configure(|arg| arg.required(false).allow_hyphen_values(false).conflicts_with("file"))),
        file => Field::new(None).input(optional_text("file", "PATH").path().configure(|arg| arg.requires("agent"))),
        agent => Field::new(None).input(optional_text("agent", "LABEL").configure(|arg| arg.requires("file").conflicts_with("target"))),
        json => Field::new(false)
            .input(Input::flag("json", |slot, _| { *slot = true; Ok(()) }))
            .input(Input::value("format", "FORMAT", |slot, value| { *slot = parse_choice(EXPLAIN_FORMATS, "explain format", value)?; Ok(()) })
                .configure(|arg| choices(arg, EXPLAIN_FORMATS))),
        verbose => Field::new(false).input(Input::flag("verbose", |slot, _| { *slot = true; Ok(()) }).configure(|arg| arg.short('v'))),
    }
}

pub(crate) fn account() -> Input<Option<String>> {
    optional_text("account", "ID")
}

/// `--json` on commands whose output is already JSON; accepted so documented
/// invocations stay copy-safe.
fn json_no_op() -> clap::Arg {
    clap::Arg::new("json")
        .long("json")
        .action(clap::ArgAction::SetTrue)
        .help("Accepted for symmetry; output is always JSON")
}

fn caller_pane() -> Input<Option<String>> {
    optional_text("caller-pane", "ID")
        .help("Live local agent pane or terminal id (default: HERDR_PANE_ID)")
}

/// `agent federated-list`: the caller identity is resolved by `agent.rs`.
pub(crate) struct FederatedList {
    pub(crate) caller_pane: Option<String>,
}

cli_projection! {
    FederatedListProjection for FederatedList {
        caller_pane => Field::new(None).input(caller_pane()),
    }
}

pub(crate) struct FederatedPrompt {
    pub(crate) target: String,
    pub(crate) text: String,
    pub(crate) caller_pane: Option<String>,
}

cli_projection! {
    FederatedPromptProjection for FederatedPrompt {
        target => Field::new(String::new()).input(target()),
        text => Field::new(String::new()).input(text_position("text", "TEXT", 1).verbatim()),
        caller_pane => Field::new(None).input(caller_pane()),
    }
}

cli_projection! {
    RestartProjection for AgentRestartParams {
        target => Field::new(String::new()).input(target()),
        account => Field::new(None).input(account()
            .help("Resume under this configured account (kept for later restarts)")),
    }
}

fn parked_work() -> Input<Vec<serde_json::Value>> {
    Input::value("parked-work", "FILE", |slot, path| {
        let content = std::fs::read_to_string(path)?;
        match serde_json::from_str::<serde_json::Value>(&content) {
            Ok(serde_json::Value::Array(items)) => {
                *slot = items;
                Ok(())
            }
            Ok(_) => Err(ParseError::Usage(
                "--parked-work file must contain a JSON array".into(),
            )),
            Err(err) => Err(ParseError::Usage(format!(
                "--parked-work file is not valid JSON: {err}"
            ))),
        }
    })
    .path()
    .help("JSON array of open work, stored and returned verbatim")
}

cli_projection! {
    ArchiveProjection for AgentArchiveParams {
        target => Field::new(String::new()).input(target()),
        reason => Field::new(None).input(optional_text("reason", "TEXT")
            .help("Note recorded on the archived record")),
        by => Field::new(None).input(optional_text("by", "WHO")
            .help("Who requested the archive (default: api)")),
        parked_work => Field::new(Vec::new()).input(parked_work()),
        force => Field::new(false).input(Input::flag("force", |slot, _| { *slot = true; Ok(()) })
            .help("Archive even when the agent is mid-turn")),
    }
}

cli_projection! {
    UnarchiveProjection for AgentUnarchiveParams {
        target => Field::new(String::new()).input(target()),
        fresh => Field::new(false).input(Input::flag("fresh", |slot, _| { *slot = true; Ok(()) })
            .help("Start a clean agent instead of resuming the archived session")),
    }
}

cli_projection! {
    ForgetProjection for AgentForgetParams {
        target => Field::new(String::new()).input(target()),
    }
}

/// `agent transfer-session` CLI arguments. `yes` is CLI-only: it confirms right
/// after staging instead of stopping for a person to read the summary.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct TransferSession {
    pub(crate) target: String,
    pub(crate) to: AgentSessionTransferHarness,
    pub(crate) account: Option<String>,
    pub(crate) confirm: Option<String>,
    pub(crate) yes: bool,
}

cli_projection! {
    TransferSessionProjection for TransferSession {
        target => Field::new(String::new()).input(target()),
        to => Field::new(AgentSessionTransferHarness::Claude).input(
            Input::value("to", "HARNESS", |slot, value| {
                *slot = parse_choice(TRANSFER_HARNESSES, "transfer harness", value)?;
                Ok(())
            })
            .required()
            .configure(|arg| choices(arg, TRANSFER_HARNESSES))),
        account => Field::new(None).input(account().help("Target harness account id")),
        confirm => Field::new(None).input(optional_text("confirm", "TRANSFER_ID")
            .help("Confirm an already staged transfer")),
        yes => Field::new(false).input(Input::flag("yes", |slot, _| { *slot = true; Ok(()) })
            .help("Confirm immediately after staging and verification")
            .configure(|arg| arg.conflicts_with("confirm"))),
    }
}

pub(crate) enum Operation {
    List,
    FederatedList(FederatedList),
    FederatedPrompt(FederatedPrompt),
    Get(AgentTarget),
    Focus(AgentTarget),
    Read(AgentReadParams),
    Transcript(AgentTranscriptParams),
    SendKeys(AgentSendKeysParams),
    Rename(AgentRenameParams),
    Archive(AgentArchiveParams),
    Unarchive(AgentUnarchiveParams),
    Forget(AgentForgetParams),
    Wait(AgentWaitParams),
    Prompt(AgentPromptParams),
    Start(AgentStartParams),
    Restart(AgentRestartParams),
    TransferSession(TransferSession),
    Attach(Attach),
    Explain(Explain),
}

fn leaf(name: &'static str, arguments: Vec<clap::Arg>, about: &'static str) -> Command {
    crate::cli::projection::command(name, arguments).about(about)
}

pub(crate) fn command() -> Command {
    Command::new("agent")
        .about("Control and inspect agent panes")
        .subcommand_required(true)
        .subcommand(leaf("list", vec![json_no_op()], "List agents"))
        .subcommand(leaf("federated-list", FederatedListProjection::arguments(), "List coordinator and permitted peer agents via an explicit trusted-machine reverse grant")
            .after_help("Requires federation.reverse_coordinator_machine_id on this machine. Same-user processes can spoof a granted HERDR_PANE_ID; this is not per-process isolation."))
        .subcommand(leaf("federated-prompt", FederatedPromptProjection::arguments(), "Prompt an authorized coordinator or explicitly permitted peer agent")
            .override_usage("herdr agent federated-prompt <TARGET> <TEXT> [--caller-pane ID]")
            .after_help("TARGET is a terminal id from `herdr agent federated-list`."))
        .subcommand(leaf("get", TargetProjection::arguments(), "Show an agent"))
        .subcommand(leaf("focus", TargetProjection::arguments(), "Focus an agent"))
        .subcommand(leaf("read", AgentReadProjection::arguments(), "Read agent terminal output")
            .override_usage("herdr agent read <TARGET> [OPTIONS]"))
        .subcommand(leaf("transcript", AgentTranscriptProjection::arguments(), "Read an omp agent's conversation as chat entries")
            .arg(json_no_op())
            .override_usage("herdr agent transcript <TARGET> [--before CURSOR | --after CURSOR] [--limit TURNS] [--json]")
            .after_help("Without --before or --after, returns the newest page. Pass a page's `before` to page older turns, or its `after` to fetch what changed since."))
        .subcommand(leaf("send-keys", AgentSendKeysProjection::arguments(), "Send key presses to an agent")
            .after_help("Use esc as the canonical Escape key name; escape is also accepted."))
        .subcommand(leaf("rename", AgentRenameProjection::arguments(), "Rename an agent")
            .override_usage("herdr agent rename <TARGET> <NAME>|--clear")
            .group(ArgGroup::new("rename").args(["name", "clear"]).required(true)))
        .subcommand(leaf("archive", ArchiveProjection::arguments(), "Archive an agent, closing its pane and preserving its session")
            .arg(json_no_op())
            .override_usage("herdr agent archive <TARGET> [--reason TEXT] [--by WHO] [--parked-work FILE] [--force] [--json]"))
        .subcommand(leaf("unarchive", UnarchiveProjection::arguments(), "Reopen an archived agent, resuming its session")
            .arg(json_no_op())
            .override_usage("herdr agent unarchive <TARGET> [--fresh] [--json]"))
        .subcommand(leaf("forget", ForgetProjection::arguments(), "Remove an archived agent's record")
            .arg(json_no_op()))
        .subcommand(leaf("wait", AgentWaitProjection::arguments(), "Wait until an agent reaches one of the requested states")
            .override_usage("herdr agent wait <TARGET> [OPTIONS]")
            .after_help("Without --until, matches idle, done, or blocked. Use --until unknown explicitly when needed. Without --timeout, waits indefinitely."))
        .subcommand(leaf("prompt", AgentPromptProjection::arguments(), "Submit a prompt to an agent")
            .args(AgentPromptWaitProjection::arguments().into_iter().map(|arg| arg.requires("wait")))
            .override_usage("herdr agent prompt <TARGET> <TEXT> [OPTIONS]")
            .after_help(concat!(
                "A blocked agent or visible modal rejects input before any PTY write. ",
                "Otherwise Herdr writes text+Enter first. With --wait it observes ",
                "submission and, if verified, the requested agent state. A proven ",
                "submission returns delivery=submitted. If observation stalls or ",
                "composer verification is unsupported, the response is agent_prompted ",
                "with delivery=written_to_pty, not a confirmation of submission; ",
                "do not resend without checking the pane. An attributed draft still ",
                "visible after the observation deadline returns agent_prompt_unsubmitted. ",
                "A timeout before the PTY write remains an error. Run ",
                "`herdr server agent-manifests` to see active composer coverage. ",
                "A settled agent's lifecycle advance or an observed composer clear ",
                "confirms submission; an unrelated completion of an already-working ",
                "agent does not. Without --timeout, the settled-state wait is indefinite."
            )))
        .subcommand(leaf("start", AgentStartProjection::arguments(), "Start a supported interactive agent in an existing pane")
            .override_usage("herdr agent start <NAME> --kind <KIND> --pane <ID> [OPTIONS] [-- [AGENT_ARG]...]")
            .after_help("The pane must be at its interactive shell prompt. Success means the expected agent was detected in the same terminal and is ready for input.\n\nnext: herdr agent prompt <TARGET> <TEXT> --wait"))
        .subcommand(leaf("restart", RestartProjection::arguments(), "Restart an agent in place, resuming its session")
            .override_usage("herdr agent restart <TARGET> [--account <ID>]"))
        .subcommand(leaf("transfer-session", TransferSessionProjection::arguments(), "Stage or confirm a Claude Code/Codex/OMP session transfer")
            .override_usage("herdr agent transfer-session <TARGET> --to claude|codex|omp [OPTIONS]")
            .after_help("Without --yes or --confirm, Herdr stages and verifies the destination, leaves the source running, and prints the transfer id for a later confirmation."))
        .subcommand(leaf("attach", AttachProjection::arguments(), "Attach directly to an agent terminal")
            .override_usage("herdr agent attach <TARGET> [OPTIONS]"))
        .subcommand(leaf("explain", ExplainProjection::arguments(), "Explain agent detection state")
            .group(ArgGroup::new("explain_source").args(["target", "file"]).required(true)))
}

pub(crate) fn from_matches(matches: &ArgMatches) -> Result<Operation, ParseError> {
    let Some((name, matches)) = matches.subcommand() else {
        return Err(ParseError::Usage("missing agent command".into()));
    };
    Ok(match name {
        "list" => Operation::List,
        "federated-list" => {
            Operation::FederatedList(FederatedListProjection::from_matches(matches)?)
        }
        "federated-prompt" => {
            Operation::FederatedPrompt(FederatedPromptProjection::from_matches(matches)?)
        }
        "get" => Operation::Get(TargetProjection::from_matches(matches)?),
        "focus" => Operation::Focus(TargetProjection::from_matches(matches)?),
        "transcript" => Operation::Transcript(AgentTranscriptProjection::from_matches(matches)?),
        "read" => {
            let mut params = AgentReadProjection::from_matches(matches)?;
            params.strip_ansi = !matches!(params.format, ReadFormat::Ansi);
            Operation::Read(params)
        }
        "send-keys" => Operation::SendKeys(AgentSendKeysProjection::from_matches(matches)?),
        "rename" => Operation::Rename(AgentRenameProjection::from_matches(matches)?),
        "archive" => Operation::Archive(ArchiveProjection::from_matches(matches)?),
        "unarchive" => Operation::Unarchive(UnarchiveProjection::from_matches(matches)?),
        "forget" => Operation::Forget(ForgetProjection::from_matches(matches)?),
        "wait" => Operation::Wait(AgentWaitProjection::from_matches(matches)?),
        "prompt" => {
            let mut params = AgentPromptProjection::from_matches(matches)?;
            if params.wait.is_some() {
                params.wait = Some(AgentPromptWaitProjection::from_matches(matches)?);
            }
            Operation::Prompt(params)
        }
        "start" => Operation::Start(AgentStartProjection::from_matches(matches)?),
        "restart" => Operation::Restart(RestartProjection::from_matches(matches)?),
        "transfer-session" => {
            Operation::TransferSession(TransferSessionProjection::from_matches(matches)?)
        }
        "attach" => Operation::Attach(AttachProjection::from_matches(matches)?),
        "explain" => Operation::Explain(ExplainProjection::from_matches(matches)?),
        _ => return Err(ParseError::Usage(format!("unknown agent command: {name}"))),
    })
}

#[cfg(test)]
mod choice_tests {
    #[test]
    fn agent_statuses_cover_every_api_value() {
        crate::cli::projection::assert_choices_cover_api_enum(super::STATUSES);
    }

    #[test]
    fn transfer_harnesses_cover_every_api_value() {
        crate::cli::projection::assert_choices_cover_api_enum(super::TRANSFER_HARNESSES);
    }
}
