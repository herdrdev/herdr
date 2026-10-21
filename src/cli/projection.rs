use std::any::TypeId;

use clap::builder::{StringValueParser, TypedValueParser};
use clap::{parser::ValueSource, Arg, ArgAction, ArgMatches, Command, ValueHint};

use crate::api::schema::{Method, Request};

#[derive(Debug)]
pub(crate) enum ParseError {
    Usage(String),
    Io(std::io::Error),
    Clap(clap::Error),
}

impl From<String> for ParseError {
    fn from(message: String) -> Self {
        Self::Usage(message)
    }
}

impl From<std::io::Error> for ParseError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<clap::Error> for ParseError {
    fn from(error: clap::Error) -> Self {
        Self::Clap(error)
    }
}

pub(crate) fn command(name: &'static str, arguments: Vec<Arg>) -> Command {
    Command::new(name).args_override_self(true).args(arguments)
}

pub(crate) fn parse<T>(
    command: Command,
    args: &[String],
    from_matches: impl FnOnce(&ArgMatches) -> Result<T, ParseError>,
) -> Result<T, ParseError> {
    let args = protect_verbatim_slots(&command, args);
    let matches = command.no_binary_name(true).try_get_matches_from(args)?;
    from_matches(&matches)
}

/// An API-backed subcommand: the request id it sends and how its matches become the method.
pub(crate) struct Leaf {
    pub(crate) name: &'static str,
    pub(crate) about: &'static str,
    pub(crate) id: &'static str,
    pub(crate) arguments: fn() -> Vec<Arg>,
    pub(crate) method: fn(&ArgMatches) -> Result<Method, ParseError>,
}

impl Leaf {
    pub(crate) fn command(&self) -> Command {
        command(self.name, (self.arguments)()).about(self.about)
    }
}

fn render_argument(argument: &Arg) -> String {
    let value = argument
        .get_value_names()
        .and_then(|names| names.first())
        .map_or_else(|| argument.get_id().as_str(), |name| name.as_str());
    let repeated = argument
        .get_num_args()
        .is_some_and(|range| range.max_values() > 1);
    match argument.get_long() {
        Some(long) if argument.get_action().takes_values() => format!("--{long} {value}"),
        Some(long) => format!("--{long}"),
        None if argument.is_last_set() => format!("-- <{value}>..."),
        None if repeated => format!("<{value}>..."),
        None => format!("<{value}>"),
    }
}

/// One-line usage for `command`, derived from its declared arguments and groups.
fn synopsis(path: &str, command: &Command) -> String {
    let required_groups: Vec<_> = command
        .get_groups()
        .filter(|group| group.is_required_set())
        .collect();
    let grouped = |argument: &Arg| {
        required_groups
            .iter()
            .any(|group| group.get_args().any(|id| id == argument.get_id()))
    };
    let visible = || {
        command
            .get_arguments()
            .filter(|argument| !argument.is_hide_set() && !grouped(argument))
    };
    let render = |argument: &Arg| {
        let text = render_argument(argument);
        if argument.is_required_set() {
            text
        } else {
            format!("[{text}]")
        }
    };
    let mut parts = vec![path.to_owned()];
    parts.extend(
        visible()
            .filter(|argument| argument.is_positional() && !argument.is_last_set())
            .map(render),
    );
    for group in &required_groups {
        let members: Vec<_> = group
            .get_args()
            .filter_map(|id| {
                command
                    .get_arguments()
                    .find(|argument| argument.get_id() == id)
            })
            .map(render_argument)
            .collect();
        parts.push(format!("({})", members.join(" | ")));
    }
    parts.extend(
        visible()
            .filter(|argument| !argument.is_positional())
            .map(render),
    );
    parts.extend(
        visible()
            .filter(|argument| argument.is_last_set())
            .map(render),
    );
    parts.join(" ")
}

fn push_synopses(path: &str, command: &Command, lines: &mut Vec<String>) {
    if command.has_subcommands() {
        for subcommand in command.get_subcommands().filter(|sub| !sub.is_hide_set()) {
            push_synopses(
                &format!("{path} {}", subcommand.get_name()),
                subcommand,
                lines,
            );
        }
    } else {
        lines.push(synopsis(path, command));
    }
}

/// The command list a bare group prints, derived from the group's declarations.
pub(crate) fn group_commands(group: &Command) -> String {
    let path = format!("herdr {}", group.get_name());
    let mut lines = vec![format!("{path} commands:")];
    let mut synopses = Vec::new();
    push_synopses(&path, group, &mut synopses);
    lines.extend(synopses.into_iter().map(|line| format!("  {line}")));
    lines.join("\n")
}

/// Parses `args` against a command group and builds the selected leaf's request.
pub(crate) fn group_request(
    group: Command,
    leaves: &[Leaf],
    args: &[String],
) -> Result<Request, ParseError> {
    let group_name = group.get_name().to_owned();
    parse(
        group.bin_name(format!("herdr {group_name}")),
        args,
        |matches| {
            let (name, matches) = matches
                .subcommand()
                .ok_or_else(|| ParseError::Usage(format!("missing {group_name} command")))?;
            let leaf = leaves
                .iter()
                .find(|leaf| leaf.name == name)
                .ok_or_else(|| {
                    ParseError::Usage(format!("unknown {group_name} command: {name}"))
                })?;
            Ok(Request {
                id: leaf.id.into(),
                method: (leaf.method)(matches)?,
            })
        },
    )
}

const VERBATIM_ESCAPE: char = '\u{1}';

/// Marks verbatim operands by value type so `parse` can find them on any `Arg`.
#[derive(Clone)]
struct VerbatimText(String);

fn is_verbatim(argument: &Arg) -> bool {
    argument.get_value_parser().type_id() == TypeId::of::<VerbatimText>()
}

/// The innermost subcommand named by `args` and the index of its first argument.
fn select_leaf<'a>(command: &'a Command, args: &[String]) -> (&'a Command, usize) {
    let mut leaf = command;
    let mut start = 0;
    while let Some(subcommand) = args.get(start).and_then(|name| leaf.find_subcommand(name)) {
        leaf = subcommand;
        start += 1;
    }
    (leaf, start)
}

/// The declared option that `token` names, if any. Undeclared dash words are operands.
fn declared_option<'a>(leaf: &'a Command, token: &str) -> Option<&'a Arg> {
    if let Some(long) = token.strip_prefix("--") {
        let name = long.split('=').next().unwrap_or(long);
        leaf.get_arguments().find(|argument| {
            argument.get_long() == Some(name)
                || argument
                    .get_all_aliases()
                    .is_some_and(|aliases| aliases.contains(&name))
        })
    } else {
        let short = token.strip_prefix('-')?.chars().next()?;
        leaf.get_arguments()
            .find(|argument| argument.get_short() == Some(short))
    }
}

/// Operands of the selected leaf, skipping declared options and their values.
pub(crate) fn operands<'a>(command: &Command, args: &'a [String]) -> Vec<&'a str> {
    let (leaf, start) = select_leaf(command, args);
    let mut operands = Vec::new();
    let mut tokens = args[start..].iter();
    while let Some(token) = tokens.next() {
        match declared_option(leaf, token) {
            Some(option) if option.get_action().takes_values() && !token.contains('=') => {
                tokens.next();
            }
            Some(_) => {}
            None => operands.push(token.as_str()),
        }
    }
    operands
}

/// Text operands are read by argv position, so words such as `--wait` or `-h`
/// in those positions stay text instead of becoming flags. The escape keeps
/// Clap from treating them as options and is removed by `Input::verbatim`.
fn protect_verbatim_slots(command: &Command, args: &[String]) -> Vec<String> {
    let (leaf, start) = select_leaf(command, args);
    let operands = &args[start..];
    let mut protected = args.to_vec();
    for argument in leaf
        .get_positionals()
        .filter(|argument| is_verbatim(argument))
    {
        let Some(slot) = argument.get_index().and_then(|index| index.checked_sub(1)) else {
            continue;
        };
        // Positions are only fixed while no declared option precedes the slot.
        if slot >= operands.len()
            || operands[..slot]
                .iter()
                .any(|arg| declared_option(leaf, arg).is_some())
        {
            continue;
        }
        let end = if argument
            .get_num_args()
            .is_some_and(|range| range.max_values() > 1)
        {
            operands.len()
        } else {
            slot + 1
        };
        // A lone flag declared as this operand's alternative (`rename ID --clear`) stays a flag.
        if end == operands.len() && end == slot + 1 {
            let token = operands[slot].as_str();
            if leaf.get_arg_conflicts_with(argument).iter().any(|flag| {
                flag.get_long()
                    .is_some_and(|long| token.strip_prefix("--") == Some(long))
            }) {
                continue;
            }
        }
        for token in &mut protected[start + slot..start + end] {
            if token.starts_with('-') || token.starts_with(VERBATIM_ESCAPE) {
                token.insert(0, VERBATIM_ESCAPE);
            }
        }
    }
    protected
}

fn string_values(matches: &ArgMatches, argument: &Arg) -> Vec<String> {
    let id = argument.get_id().as_str();
    if is_verbatim(argument) {
        matches
            .get_many::<VerbatimText>(id)
            .map(|values| values.map(|value| value.0.clone()).collect())
            .unwrap_or_default()
    } else {
        matches
            .get_many::<String>(id)
            .map(|values| values.cloned().collect())
            .unwrap_or_default()
    }
}

enum InputKind {
    Value,
    Joined,
    Flag,
}

type Update<T> = fn(&mut T, &str) -> Result<(), ParseError>;

pub(crate) struct Input<T> {
    argument: Arg,
    kind: InputKind,
    update: Update<T>,
    once: bool,
}

impl<T> Input<T> {
    pub(crate) fn positional(name: &'static str, index: usize, update: Update<T>) -> Self {
        Self {
            argument: Arg::new(name)
                .value_name(name)
                .index(index + 1)
                .allow_hyphen_values(true)
                .required(true),
            kind: InputKind::Value,
            update,
            once: false,
        }
    }

    pub(crate) fn rest(
        name: &'static str,
        value_name: &'static str,
        index: usize,
        update: Update<T>,
    ) -> Self {
        Self {
            argument: Arg::new(name)
                .value_name(value_name)
                .index(index + 1)
                .allow_hyphen_values(true)
                .required(true)
                .num_args(1..),
            kind: InputKind::Joined,
            update,
            once: false,
        }
    }

    /// Every occurrence is validated in argv order; the field reducer decides
    /// whether the last one wins or occurrences accumulate.
    pub(crate) fn value(name: &'static str, value_name: &'static str, update: Update<T>) -> Self {
        Self {
            argument: Arg::new(name)
                .long(name)
                .value_name(value_name)
                .allow_hyphen_values(true)
                .action(ArgAction::Append),
            kind: InputKind::Value,
            update,
            once: false,
        }
    }

    pub(crate) fn flag(name: &'static str, update: Update<T>) -> Self {
        Self {
            argument: Arg::new(name).long(name).action(ArgAction::SetTrue),
            kind: InputKind::Flag,
            update,
            once: false,
        }
    }

    pub(crate) fn configure(mut self, configure: impl FnOnce(Arg) -> Arg) -> Self {
        self.argument = configure(self.argument);
        self
    }

    /// Reject a repeated option instead of letting the last occurrence win.
    pub(crate) fn once(mut self) -> Self {
        self.once = true;
        self
    }

    /// Read this operand by position, keeping flag-shaped words as text.
    pub(crate) fn verbatim(mut self) -> Self {
        self.argument = self
            .argument
            .value_parser(StringValueParser::new().map(|value| {
                VerbatimText(
                    value
                        .strip_prefix(VERBATIM_ESCAPE)
                        .map(str::to_owned)
                        .unwrap_or(value),
                )
            }));
        self
    }

    pub(crate) fn value_name(self, name: &'static str) -> Self {
        self.configure(|argument| argument.value_name(name))
    }

    pub(crate) fn path(mut self) -> Self {
        self.argument = self.argument.value_hint(ValueHint::AnyPath);
        self
    }

    pub(crate) fn order(mut self, order: usize) -> Self {
        self.argument = self.argument.display_order(order);
        self
    }

    pub(crate) fn required(mut self) -> Self {
        self.argument = self.argument.required(true);
        self
    }

    pub(crate) fn help(mut self, help: &'static str) -> Self {
        self.argument = self.argument.help(help);
        self
    }

    pub(crate) fn hidden(mut self) -> Self {
        self.argument = self.argument.hide(true);
        self
    }
}

pub(crate) fn optional_text(name: &'static str, value_name: &'static str) -> Input<Option<String>> {
    Input::value(name, value_name, |slot, value| {
        *slot = Some(value.to_owned());
        Ok(())
    })
}

/// CLI spelling of an API value. Hidden spellings parse but stay out of help and completion.
pub(crate) struct Choice<T> {
    pub(crate) name: &'static str,
    pub(crate) value: T,
    pub(crate) hidden: bool,
}

impl<T> Choice<T> {
    pub(crate) const fn new(name: &'static str, value: T) -> Self {
        Self {
            name,
            value,
            hidden: false,
        }
    }

    pub(crate) const fn hidden(name: &'static str, value: T) -> Self {
        Self {
            name,
            value,
            hidden: true,
        }
    }
}

pub(crate) fn parse_choice<T: Clone>(
    choices: &[Choice<T>],
    label: &str,
    value: &str,
) -> Result<T, ParseError> {
    choices
        .iter()
        .find(|choice| choice.name == value)
        .map(|choice| choice.value.clone())
        .ok_or_else(|| ParseError::Usage(format!("invalid {label}: {value}")))
}

/// Fails when an API enum gains a value the CLI table cannot spell.
#[cfg(test)]
pub(crate) fn assert_choices_cover_api_enum<T: serde::Serialize + schemars::JsonSchema>(
    table: &[Choice<T>],
) {
    use std::collections::BTreeSet;
    let schema = serde_json::to_value(schemars::schema_for!(T)).expect("enum schema");
    let api: BTreeSet<String> = schema["enum"]
        .as_array()
        .expect("unit enum schema")
        .iter()
        .map(|value| value.as_str().expect("string enum value").to_owned())
        .collect();
    let cli: BTreeSet<String> = table
        .iter()
        .map(|choice| {
            serde_json::to_value(&choice.value)
                .expect("serializable choice")
                .as_str()
                .expect("string enum value")
                .to_owned()
        })
        .collect();
    assert_eq!(cli, api, "{}", std::any::type_name::<T>());
}

pub(crate) fn choices<T>(argument: Arg, table: &[Choice<T>]) -> Arg {
    choices_where(argument, table, |_| true)
}

/// Like `choices`, but hides values the command does not advertise.
pub(crate) fn choices_where<T>(
    argument: Arg,
    table: &[Choice<T>],
    advertised: impl Fn(&T) -> bool,
) -> Arg {
    argument.value_parser(clap::builder::PossibleValuesParser::new(table.iter().map(
        |choice| {
            clap::builder::PossibleValue::new(choice.name)
                .hide(choice.hidden || !advertised(&choice.value))
        },
    )))
}

pub(crate) fn boolean(name: &'static str, value: bool) -> Input<bool> {
    // Updates are function pointers, so each choice uses a non-capturing reducer.
    if value {
        Input::flag(name, |slot, _| {
            *slot = true;
            Ok(())
        })
    } else {
        Input::flag(name, |slot, _| {
            *slot = false;
            Ok(())
        })
    }
}

type Validate<T> = fn(&T) -> Result<(), ParseError>;

pub(crate) struct Field<T> {
    value: T,
    inputs: Vec<Input<T>>,
    validate: Option<Validate<T>>,
}

impl<T> Field<T> {
    pub(crate) fn new(value: T) -> Self {
        Self {
            value,
            inputs: Vec::new(),
            validate: None,
        }
    }

    pub(crate) fn input(mut self, input: Input<T>) -> Self {
        self.inputs.push(input);
        self
    }

    pub(crate) fn validate(mut self, validate: Validate<T>) -> Self {
        self.validate = Some(validate);
        self
    }

    pub(crate) fn arguments(self) -> Vec<Arg> {
        self.inputs
            .into_iter()
            .map(|input| input.argument)
            .collect()
    }

    pub(crate) fn read_matches(&mut self, matches: &ArgMatches) -> Result<(), ParseError> {
        // A field can have several inputs (set/clear, focus/no-focus). Reduce them
        // in argv order rather than field-declaration order.
        let mut updates = Vec::new();
        for input in &self.inputs {
            let id = input.argument.get_id().as_str();
            if matches.value_source(id) != Some(ValueSource::CommandLine) {
                continue;
            }
            let Some(indices) = matches.indices_of(id) else {
                continue;
            };
            match input.kind {
                InputKind::Flag => {
                    for index in indices {
                        updates.push((index, input, String::new()));
                    }
                }
                InputKind::Value => {
                    if input.once && indices.len() > 1 {
                        return Err(ParseError::Clap(clap::Error::raw(
                            clap::error::ErrorKind::ArgumentConflict,
                            format!("the argument '--{id}' cannot be used multiple times\n"),
                        )));
                    }
                    for (index, value) in indices.zip(string_values(matches, &input.argument)) {
                        updates.push((index, input, value));
                    }
                }
                InputKind::Joined => {
                    if let Some(index) = indices.into_iter().next() {
                        let values = string_values(matches, &input.argument);
                        updates.push((index, input, values.join(" ")));
                    }
                }
            }
        }
        updates.sort_by_key(|(index, _, _)| *index);
        for (_, input, value) in updates {
            (input.update)(&mut self.value, &value)?;
        }
        Ok(())
    }

    pub(crate) fn finish(self) -> Result<T, ParseError> {
        if let Some(validate) = self.validate {
            validate(&self.value)?;
        }
        Ok(self.value)
    }
}

macro_rules! cli_projection {
    ($visibility:vis $projection:ident for $params:ident {
        $($field:ident => $binding:expr),* $(,)?
    }) => {
        $visibility struct $projection;

        impl $projection {
            pub(crate) fn arguments() -> Vec<clap::Arg> {
                let arguments: Vec<Vec<clap::Arg>> = vec![$(($binding).arguments()),*];
                let mut arguments: Vec<_> = arguments.into_iter().flatten().collect();
                arguments.sort_by_key(clap::Arg::get_display_order);
                arguments
            }

            pub(crate) fn from_matches(
                matches: &clap::ArgMatches,
            ) -> Result<$params, $crate::cli::projection::ParseError> {
                $(let mut $field = $binding;
                  $field.read_matches(matches)?;)*
                let _ = matches;
                Ok($params { $($field: $field.finish()?),* })
            }

        }
    };
}

macro_rules! cli_params {
    ($(#[$attribute:meta])* $visibility:vis struct $params:ident => $projection:ident {
        $($(#[$field_attribute:meta])* $field_visibility:vis $field:ident: $type:ty => $binding:expr),* $(,)?
    }) => {
        $(#[$attribute])*
        $visibility struct $params {
            $($(#[$field_attribute])* $field_visibility $field: $type),*
        }
        $crate::cli::projection::cli_projection! {
            pub(crate) $projection for $params {
                $($field => $binding),*
            }
        }
    };
}

pub(crate) use cli_params;
pub(crate) use cli_projection;

#[cfg(test)]
mod tests {
    use super::*;

    struct Sample {
        id: String,
        value: Option<String>,
        enabled: bool,
    }

    cli_projection! {
        SampleProjection for Sample {
            id => Field::new(String::new()).input(Input::positional("id", 0, |id, value| {
                *id = value.to_owned();
                Ok(())
            })),
            value => Field::new(None).input(Input::value("value", "TEXT", |slot, value| {
                *slot = Some(value.to_owned());
                Ok(())
            }).configure(|argument| argument.short('v').alias("text"))),
            enabled => Field::new(false)
                .input(Input::flag("on", |slot, _| { *slot = true; Ok(()) }))
                .input(Input::flag("off", |slot, _| { *slot = false; Ok(()) }).hidden()),
        }
    }

    fn args(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| (*arg).to_owned()).collect()
    }

    fn parse_sample(args: &[String]) -> Result<Sample, ParseError> {
        parse(
            command("sample", SampleProjection::arguments()),
            args,
            SampleProjection::from_matches,
        )
    }

    #[test]
    fn projection_uses_clap_for_ordered_values_and_hidden_flags() {
        let parsed = parse_sample(&args(&[
            "--literal-id",
            "--value",
            "first",
            "--on",
            "--value",
            "--literal-value",
            "--off",
        ]))
        .unwrap();
        assert_eq!(parsed.id, "--literal-id");
        assert_eq!(parsed.value.as_deref(), Some("--literal-value"));
        assert!(!parsed.enabled);
        let arguments = SampleProjection::arguments();
        assert!(arguments
            .iter()
            .any(|arg| arg.get_long() == Some("off") && arg.is_hide_set()));
    }

    #[test]
    fn projection_parses_the_composed_command_relations() {
        let composed = || {
            command("sample", SampleProjection::arguments()).group(
                clap::ArgGroup::new("switch")
                    .args(["on", "off"])
                    .required(true),
            )
        };
        assert!(matches!(
            parse(composed(), &args(&["id"]), SampleProjection::from_matches),
            Err(ParseError::Clap(error)) if error.kind() == clap::error::ErrorKind::MissingRequiredArgument
        ));
        let parsed = parse(
            composed(),
            &args(&["id", "--text=x", "--off"]),
            SampleProjection::from_matches,
        )
        .unwrap();
        assert_eq!(parsed.value.as_deref(), Some("x"));
        assert!(!parsed.enabled);
        assert!(!command("sample", SampleProjection::arguments())
            .render_long_help()
            .to_string()
            .contains("--off"));
    }

    #[test]
    fn projection_accepts_clap_normalization_and_diagnostics() {
        let parsed = parse_sample(&args(&["--value=x", "id"])).unwrap();
        assert_eq!(parsed.value.as_deref(), Some("x"));
        for (args, kind) in [
            (args(&[]), clap::error::ErrorKind::MissingRequiredArgument),
            (
                args(&["id", "--value"]),
                clap::error::ErrorKind::InvalidValue,
            ),
            (
                args(&["id", "extra"]),
                clap::error::ErrorKind::UnknownArgument,
            ),
        ] {
            assert!(
                matches!(parse_sample(&args), Err(ParseError::Clap(error)) if error.kind() == kind)
            );
        }
    }

    struct Text {
        target: String,
        text: String,
        wait: bool,
    }

    cli_projection! {
        TextProjection for Text {
            target => Field::new(String::new()).input(Input::positional("target", 0, |slot, value| {
                *slot = value.to_owned();
                Ok(())
            })),
            text => Field::new(String::new()).input(Input::positional("text", 1, |slot, value| {
                *slot = value.to_owned();
                Ok(())
            }).verbatim()),
            wait => Field::new(false).input(boolean("wait", true)),
        }
    }

    fn parse_text(values: &[&str]) -> Result<Text, ParseError> {
        let group = Command::new("group").subcommand(command("leaf", TextProjection::arguments()));
        parse(group, &args(values), |matches| {
            TextProjection::from_matches(matches.subcommand_matches("leaf").expect("leaf"))
        })
    }

    #[test]
    fn verbatim_slot_keeps_flag_shaped_words_as_text() {
        for (values, text, wait) in [
            (&["leaf", "t", "--wait"][..], "--wait", false),
            (&["leaf", "t", "-h", "--wait"][..], "-h", true),
            (&["leaf", "t", "--", "--wait"][..], "--", true),
            (&["leaf", "t", "\u{1}x"][..], "\u{1}x", false),
            (&["leaf", "--wait", "t", "plain"][..], "plain", true),
        ] {
            let parsed = parse_text(values).unwrap();
            assert_eq!(parsed.target, "t");
            assert_eq!(
                (parsed.text.as_str(), parsed.wait),
                (text, wait),
                "{values:?}"
            );
        }
        // An undeclared dash word is an operand, so it does not shift the slot.
        let parsed = parse_text(&["leaf", "-t", "--wait"]).unwrap();
        assert_eq!(
            (parsed.target.as_str(), parsed.text.as_str(), parsed.wait),
            ("-t", "--wait", false)
        );
    }

    #[test]
    fn group_commands_are_derived_from_declarations() {
        let group = Command::new("group")
            .subcommand(command("label", LabelProjection::arguments()))
            .subcommand(
                Command::new("nested")
                    .subcommand(command("numbers", NumbersProjection::arguments())),
            )
            .subcommand(
                command("pick", NumbersProjection::arguments()).group(
                    clap::ArgGroup::new("pick")
                        .args(["seq", "pattern"])
                        .required(true),
                ),
            );
        assert_eq!(
            group_commands(&group),
            "herdr group commands:\n  herdr group label <id> [<LABEL>...] [--clear]\n  herdr group nested numbers [--seq N] [--pattern TEXT]\n  herdr group pick (--seq N | --pattern TEXT)"
        );
    }

    #[test]
    fn operands_skip_declared_options_and_their_values() {
        let group =
            Command::new("group").subcommand(command("leaf", NumbersProjection::arguments()));
        let values = args(&["leaf", "--seq", "help", "--pattern=x", "-u", "help"]);
        assert_eq!(operands(&group, &values), ["-u", "help"]);
    }

    struct Label {
        id: String,
        label: Option<String>,
    }

    cli_projection! {
        LabelProjection for Label {
            id => Field::new(String::new()).input(Input::positional("id", 0, |slot, value| {
                *slot = value.to_owned();
                Ok(())
            })),
            label => Field::new(None)
                .input(Input::rest("label", "LABEL", 1, |slot, value| {
                    *slot = Some(value.to_owned());
                    Ok(())
                }).verbatim().configure(|arg| arg.required(false).conflicts_with("clear")))
                .input(Input::flag("clear", |slot, _| {
                    *slot = None;
                    Ok(())
                })),
        }
    }

    #[test]
    fn verbatim_rest_joins_flags_but_a_lone_alternative_stays_a_flag() {
        let parse_label = |values: &[&str]| {
            let parsed = parse(
                command("label", LabelProjection::arguments()),
                &args(values),
                LabelProjection::from_matches,
            )
            .unwrap();
            assert_eq!(parsed.id, "id");
            parsed.label
        };
        assert_eq!(parse_label(&["id", "--clear"]), None);
        assert_eq!(
            parse_label(&["id", "--clear", "x"]).as_deref(),
            Some("--clear x")
        );
        assert_eq!(
            parse_label(&["id", "-h", "--help"]).as_deref(),
            Some("-h --help")
        );
    }

    struct Numbers {
        seq: Option<u64>,
        pattern: Option<String>,
    }

    cli_projection! {
        NumbersProjection for Numbers {
            seq => Field::new(None).input(Input::value("seq", "N", |slot, value| {
                *slot = Some(value.parse::<u64>().map_err(|_| ParseError::Usage(format!("invalid seq: {value}")))?);
                Ok(())
            })),
            pattern => Field::new(None).input(optional_text("pattern", "TEXT").once()),
        }
    }

    #[test]
    fn repeated_options_validate_every_occurrence_unless_declared_once() {
        let parse_numbers = |values: &[&str]| {
            parse(
                command("numbers", NumbersProjection::arguments()),
                &args(values),
                NumbersProjection::from_matches,
            )
        };
        assert_eq!(
            parse_numbers(&["--seq", "1", "--seq", "2"]).unwrap().seq,
            Some(2)
        );
        assert!(matches!(
            parse_numbers(&["--seq", "bogus", "--seq", "1"]),
            Err(ParseError::Usage(message)) if message == "invalid seq: bogus"
        ));
        assert_eq!(
            parse_numbers(&["--pattern", "a"])
                .unwrap()
                .pattern
                .as_deref(),
            Some("a")
        );
        assert!(matches!(
            parse_numbers(&["--pattern", "a", "--pattern", "b"]),
            Err(ParseError::Clap(error)) if error.kind() == clap::error::ErrorKind::ArgumentConflict
        ));
    }
}
