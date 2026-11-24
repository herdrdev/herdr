use clap::{Arg, ArgMatches, Command};

use crate::api::schema::sidebar::{
    SidebarClearWidgetProjection, SidebarReportWidgetProjection, SidebarWidgetSlot,
};
use crate::api::schema::{Method, Request};

use super::projection::{Input, ParseError};

pub(crate) fn source() -> Input<String> {
    Input::value("source", "ID", |slot, value| {
        *slot = value.to_owned();
        Ok(())
    })
    .required()
}

pub(crate) fn validate_source(source: &str) -> Result<(), ParseError> {
    if source.trim().is_empty() {
        return Err(ParseError::Usage("missing required --source".into()));
    }
    Ok(())
}

pub(crate) fn slot() -> Input<SidebarWidgetSlot> {
    Input::value("slot", "SLOT", |slot, value| match value {
        "top" => {
            *slot = SidebarWidgetSlot::Top;
            Ok(())
        }
        other => Err(ParseError::Usage(format!(
            "unsupported --slot {other:?}; expected: top"
        ))),
    })
}

pub(crate) fn title() -> Input<String> {
    Input::value("title", "TEXT", |slot, value| {
        *slot = value.to_owned();
        Ok(())
    })
    .required()
}

pub(crate) fn validate_title(title: &str) -> Result<(), ParseError> {
    if title.trim().is_empty() {
        return Err(ParseError::Usage("missing required --title".into()));
    }
    Ok(())
}

pub(crate) fn right() -> Input<Option<String>> {
    Input::value("right", "TEXT", |slot, value| {
        *slot = Some(value.to_owned());
        Ok(())
    })
}

pub(crate) fn line() -> Input<Vec<String>> {
    Input::<Vec<String>>::value("line", "TEXT", |slot, value| {
        slot.push(value.to_owned());
        Ok(())
    })
}

pub(crate) fn ttl_ms() -> Input<Option<u64>> {
    Input::value("ttl-ms", "N", |slot, value| {
        *slot = Some(super::parse_u64_flag("--ttl-ms", value)?);
        Ok(())
    })
}

pub(crate) struct Leaf {
    pub(crate) name: &'static str,
    about: &'static str,
    id: &'static str,
    pub(crate) quiet: bool,
    arguments: fn() -> Vec<Arg>,
    method: fn(&ArgMatches) -> Result<Method, ParseError>,
}

impl Leaf {
    pub(crate) fn request(&self, args: &[String]) -> Result<Request, ParseError> {
        let method = super::projection::parse(
            self.command()
                .bin_name(format!("herdr sidebar {}", self.name)),
            args,
            self.method,
        )?;
        Ok(Request {
            id: self.id.into(),
            method,
        })
    }

    fn command(&self) -> Command {
        super::projection::command(self.name, (self.arguments)()).about(self.about)
    }
}

pub(crate) const LEAVES: [Leaf; 2] = [
    Leaf {
        name: "report-widget",
        about: "Report a compact display-only sidebar widget",
        id: "cli:request",
        quiet: true,
        arguments: SidebarReportWidgetProjection::arguments,
        method: |matches| {
            SidebarReportWidgetProjection::from_matches(matches).map(Method::SidebarReportWidget)
        },
    },
    Leaf {
        name: "clear-widget",
        about: "Clear a previously reported sidebar widget",
        id: "cli:request",
        quiet: true,
        arguments: SidebarClearWidgetProjection::arguments,
        method: |matches| {
            SidebarClearWidgetProjection::from_matches(matches).map(Method::SidebarClearWidget)
        },
    },
];

pub(crate) fn command() -> Command {
    Command::new("sidebar")
        .about("Manage sidebar widgets over the socket API")
        .subcommands(LEAVES.iter().map(Leaf::command))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(command: &str, args: &[&str]) -> Result<Request, ParseError> {
        LEAVES
            .iter()
            .find(|leaf| leaf.name == command)
            .unwrap()
            .request(&args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>())
    }

    #[test]
    fn report_widget_parses_repeated_lines_and_defaults_slot_to_top() {
        let req = request(
            "report-widget",
            &[
                "--source",
                "drover",
                "--title",
                "drover",
                "--right",
                "2/5",
                "--line",
                "● 2 working · ✓ 3 done",
                "--line",
                "PR #42 waiting review",
                "--ttl-ms",
                "30000",
            ],
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(req).unwrap(),
            serde_json::json!({
                "id": "cli:request",
                "method": "sidebar.report_widget",
                "params": {
                    "source": "drover",
                    "slot": "top",
                    "title": "drover",
                    "right": "2/5",
                    "lines": ["● 2 working · ✓ 3 done", "PR #42 waiting review"],
                    "ttl_ms": 30000
                }
            })
        );
    }

    #[test]
    fn clear_widget_defaults_slot_to_top() {
        let req = request("clear-widget", &["--source", "drover"]).unwrap();
        assert_eq!(
            serde_json::to_value(req).unwrap(),
            serde_json::json!({
                "id": "cli:request",
                "method": "sidebar.clear_widget",
                "params": {
                    "source": "drover",
                    "slot": "top"
                }
            })
        );
    }
}
