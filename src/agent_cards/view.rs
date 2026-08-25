use herdr_agent_watcher::daemon::store::PaneTelemetry;
use herdr_agent_watcher::sidebar::view::{Line, Role, Semantic, Span, Style as WatcherStyle};
use ratatui::style::{Color, Modifier, Style};
use serde_json::Value;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::state::Palette;
use crate::detect::AgentState;

pub(crate) struct CardInput<'a> {
    pub workspace: &'a str,
    pub name: &'a str,
    pub task: Option<&'a str>,
    pub state: AgentState,
    pub seen: bool,
    pub telemetry: Option<&'a PaneTelemetry>,
}

pub(crate) struct BuiltCard {
    pub lines: Vec<Line>,
}

pub(crate) fn build_card(
    input: CardInput<'_>,
    width: u16,
    body_height: u16,
    expanded: bool,
) -> BuiltCard {
    let width = width.max(1);
    let mut lines = vec![
        header(&input, width, expanded),
        location(&input, width),
        summary(&input, width),
    ];
    if !expanded || input.telemetry.is_none() || body_height <= 3 {
        lines.truncate(body_height as usize);
        return BuiltCard { lines };
    }

    let telemetry = input.telemetry.expect("checked telemetry");
    let groups = [
        model_rows(telemetry),
        gauge_rows(telemetry, width),
        tool_rows(telemetry),
        trace_rows(telemetry),
    ];
    let mut remaining = body_height.saturating_sub(3) as usize;
    for (index, group) in groups.into_iter().enumerate() {
        if index == 3 {
            lines.extend(group.into_iter().take(remaining));
            break;
        }
        if group.len() > remaining {
            break;
        }
        remaining -= group.len();
        lines.extend(group);
    }
    for line in &mut lines {
        *line = fit_line(std::mem::take(line), width as usize);
    }
    BuiltCard { lines }
}

fn lifecycle(state: AgentState, seen: bool) -> (&'static str, &'static str, Semantic) {
    match (state, seen) {
        (AgentState::Working, _) => ("●", "working", Semantic::Good),
        (AgentState::Blocked, _) => ("◐", "blocked", Semantic::Warn),
        (AgentState::Idle, false) => ("✓", "done", Semantic::Good),
        (AgentState::Idle, true) => ("○", "idle", Semantic::Accent),
        (AgentState::Unknown, _) => ("?", "unknown", Semantic::Bad),
    }
}

fn header(input: &CardInput<'_>, width: u16, expanded: bool) -> Line {
    let (glyph, label, semantic) = lifecycle(input.state, input.seen);
    let chevron = if expanded { "▾ " } else { "▸ " };
    let mut fixed = UnicodeWidthStr::width(chevron) + UnicodeWidthStr::width(glyph) + 1;
    let show_label = width >= 32;
    if show_label {
        fixed += UnicodeWidthStr::width(label) + 1;
    }
    let name = truncate(input.name, (width as usize).saturating_sub(fixed));
    let mut line = vec![
        Span::new(chevron, WatcherStyle::role(Role::Label)),
        Span::new(glyph, WatcherStyle::semantic(Role::Body, semantic)),
        Span::body(" "),
        Span::new(name, WatcherStyle::role(Role::Emphasis)),
    ];
    if show_label {
        line.push(Span::body(" "));
        line.push(Span::new(
            label,
            WatcherStyle::semantic(Role::Label, semantic),
        ));
    }
    fit_line(line, width as usize)
}

fn location(input: &CardInput<'_>, width: u16) -> Line {
    let cwd = input
        .telemetry
        .and_then(|telemetry| telemetry.cwd.as_deref())
        .and_then(|cwd| cwd.trim_end_matches('/').rsplit('/').next())
        .filter(|cwd| !cwd.is_empty());
    let telemetry_task = input
        .telemetry
        .and_then(|telemetry| telemetry.title.as_ref())
        .and_then(|title| title.get("title"))
        .and_then(Value::as_str);
    let mut text = input.workspace.to_string();
    if let Some(cwd) = cwd {
        text.push_str(" · ");
        text.push_str(cwd);
    }
    if let Some(task) = telemetry_task
        .or(input.task)
        .filter(|task| !task.is_empty())
    {
        text.push_str(" › ");
        text.push_str(task);
    }
    fit_line(
        vec![
            Span::body("  "),
            Span::new(text, WatcherStyle::role(Role::Label)),
        ],
        width as usize,
    )
}

fn summary(input: &CardInput<'_>, width: u16) -> Line {
    let Some(telemetry) = input.telemetry else {
        return fit_line(
            vec![Span::body("  "), Span::label("— no telemetry")],
            width as usize,
        );
    };
    let context = telemetry.status.as_ref().and_then(context_percent);
    let gauge_cells = width.saturating_sub(10).clamp(6, 14);
    let gauge = context.map_or_else(|| "—".to_string(), |pct| gauge(pct, gauge_cells as usize));
    let mut line = vec![
        Span::body("  "),
        Span::new(gauge, WatcherStyle::semantic(Role::Body, Semantic::Accent)),
    ];
    if width >= 25 {
        if let Some(pct) = context {
            line.push(Span::body(format!(" {:>3}%", pct.round() as u64)));
        }
        line.push(Span::label(format!(
            " · {} calls",
            telemetry.tool_call_total
        )));
    }
    fit_line(line, width as usize)
}

fn model_rows(telemetry: &PaneTelemetry) -> Vec<Line> {
    let model = telemetry
        .status
        .as_ref()
        .and_then(|status| status.get("modelDisplayName"))
        .and_then(Value::as_str)
        .unwrap_or("—");
    vec![labeled("MODEL", model, Semantic::Accent)]
}

fn gauge_rows(telemetry: &PaneTelemetry, width: u16) -> Vec<Line> {
    let status = telemetry.status.as_ref();
    let context = status.and_then(context_percent);
    let cache = status.and_then(|status| cache_percent(status, telemetry.agent.as_deref()));
    let cost = status
        .and_then(|status| status.get("cost"))
        .and_then(|cost| cost.get("totalCostUsd"))
        .and_then(Value::as_f64);
    let cells = width.saturating_sub(14).clamp(6, 14) as usize;
    vec![
        metric("CONTEXT", context, cells, Semantic::Accent),
        metric("CACHE", cache, cells, Semantic::Good),
        labeled(
            "COST",
            &cost.map_or_else(|| "—".to_string(), |cost| format!("${cost:.2}")),
            Semantic::Accent,
        ),
    ]
}

fn tool_rows(telemetry: &PaneTelemetry) -> Vec<Line> {
    let mut tools: Vec<_> = telemetry
        .tool_counts
        .iter()
        .filter(|(_, count)| **count > 0)
        .collect();
    tools.sort_by(|left, right| right.1.cmp(left.1).then(left.0.cmp(right.0)));
    let text = if tools.is_empty() {
        format!("{} calls", telemetry.tool_call_total)
    } else {
        tools
            .into_iter()
            .take(3)
            .map(|(name, count)| format!("{name} {count}"))
            .collect::<Vec<_>>()
            .join(" · ")
    };
    vec![labeled("TOOLS", &text, Semantic::Accent)]
}

fn trace_rows(telemetry: &PaneTelemetry) -> Vec<Line> {
    telemetry
        .tool_calls
        .iter()
        .rev()
        .take(5)
        .map(|call| {
            let failed = call.get("status").and_then(Value::as_str) == Some("failed");
            let glyph = if failed { "✕" } else { "✓" };
            let semantic = if failed {
                Semantic::Bad
            } else {
                Semantic::Good
            };
            let tool = call.get("tool").and_then(Value::as_str).unwrap_or("?");
            let args = call.get("args").and_then(Value::as_str).unwrap_or("");
            vec![
                Span::body("  "),
                Span::new(glyph, WatcherStyle::semantic(Role::Body, semantic)),
                Span::body(" "),
                Span::new(format!("{tool} {args}"), WatcherStyle::role(Role::Label)),
            ]
        })
        .collect()
}

fn labeled(label: &str, value: &str, semantic: Semantic) -> Line {
    vec![
        Span::new(format!("{label:<8}"), WatcherStyle::role(Role::Label)),
        Span::new(value, WatcherStyle::semantic(Role::Body, semantic)),
    ]
}

fn metric(label: &str, percent: Option<f64>, cells: usize, semantic: Semantic) -> Line {
    let value = percent.map_or_else(
        || "—".to_string(),
        |percent| format!("{} {:>3}%", gauge(percent, cells), percent.round() as u64),
    );
    labeled(label, &value, semantic)
}

fn context_percent(status: &Value) -> Option<f64> {
    status
        .get("contextWindow")?
        .get("usedPercentage")?
        .as_f64()
        .filter(|percent| percent.is_finite() && *percent >= 0.0)
}

fn cache_percent(status: &Value, agent: Option<&str>) -> Option<f64> {
    let usage = status.get("contextWindow")?.get("currentUsage")?;
    let input = usage.get("inputTokens")?.as_u64()?;
    let read = usage
        .get("cacheReadInputTokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let created = usage
        .get("cacheCreationInputTokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let denominator = match agent {
        Some("codex") => input,
        Some("claude" | "claude-code" | "kimi" | "opencode") => input + read + created,
        _ => return None,
    };
    (denominator > 0).then(|| read as f64 * 100.0 / denominator as f64)
}

fn gauge(percent: f64, cells: usize) -> String {
    let filled = ((percent.clamp(0.0, 100.0) * cells as f64 / 100.0).round() as usize).min(cells);
    format!("{}{}", "█".repeat(filled), "░".repeat(cells - filled))
}

fn fit_line(line: Line, width: usize) -> Line {
    let mut remaining = width;
    let mut fitted = Vec::new();
    for span in line {
        if remaining == 0 {
            break;
        }
        let text = truncate(&span.text, remaining);
        remaining = remaining.saturating_sub(UnicodeWidthStr::width(text.as_str()));
        fitted.push(Span { text, ..span });
    }
    fitted
}

fn truncate(text: &str, width: usize) -> String {
    if UnicodeWidthStr::width(text) <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    if width == 1 {
        return "…".to_string();
    }
    let mut taken = String::new();
    let mut used = 0;
    for character in text.chars() {
        let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if used + character_width > width - 1 {
            break;
        }
        taken.push(character);
        used += character_width;
    }
    taken.push('…');
    taken
}

pub(crate) fn palette_style(style: WatcherStyle, palette: &Palette) -> Style {
    let mut foreground = match style.role {
        Role::Body | Role::Emphasis => palette.text,
        Role::Label => palette.overlay0,
        Role::Rule => palette.surface_dim,
    };
    if let Some(semantic) = style.semantic {
        foreground = match semantic {
            Semantic::Good => palette.green,
            Semantic::Warn => palette.yellow,
            Semantic::Bad => palette.red,
            Semantic::Accent => palette.accent,
        };
    }
    if let Some((red, green, blue)) = style.rgb {
        foreground = Color::Rgb(red, green, blue);
    }
    let mut output = Style::default().fg(foreground);
    if style.role == Role::Emphasis {
        output = output.add_modifier(Modifier::BOLD);
    }
    if matches!(style.role, Role::Label | Role::Rule) {
        output = output.add_modifier(Modifier::DIM);
    }
    if style.reverse {
        output = output.add_modifier(Modifier::REVERSED);
    }
    output
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, VecDeque};

    use super::*;

    fn telemetry() -> PaneTelemetry {
        let mut telemetry = PaneTelemetry::with_agent("claude");
        telemetry.cwd = Some("/work/vimeflow-terminal".into());
        telemetry.status = Some(serde_json::json!({
            "modelDisplayName": "Claude Sonnet",
            "contextWindow": {
                "usedPercentage": 50.0,
                "currentUsage": {
                    "inputTokens": 500,
                    "cacheReadInputTokens": 500,
                    "cacheCreationInputTokens": 0
                }
            },
            "cost": {"totalCostUsd": 1.25}
        }));
        telemetry.title = Some(serde_json::json!({"title": "ship cards"}));
        telemetry.tool_counts = BTreeMap::from([("Edit".into(), 4), ("Bash".into(), 2)]);
        telemetry.tool_call_total = 6;
        telemetry.tool_calls = VecDeque::from([
            serde_json::json!({"tool":"Edit","args":"sidebar.rs","status":"done"}),
            serde_json::json!({"tool":"Bash","args":"cargo test","status":"done"}),
            serde_json::json!({"tool":"Read","args":"spec.md","status":"done"}),
            serde_json::json!({"tool":"Edit","args":"config.rs","status":"done"}),
            serde_json::json!({"tool":"Bash","args":"cargo clippy","status":"done"}),
        ]);
        telemetry
    }

    fn plain(card: &BuiltCard) -> Vec<String> {
        card.lines
            .iter()
            .map(|line| line.iter().map(|span| span.text.as_str()).collect())
            .collect()
    }

    #[test]
    fn adaptive_matrix_stays_bounded_and_collapsed_is_three_lines() {
        let telemetry = telemetry();
        for width in [16, 17, 24, 25, 34, 35] {
            for (state, seen) in [
                (AgentState::Idle, true),
                (AgentState::Working, true),
                (AgentState::Blocked, true),
                (AgentState::Idle, false),
            ] {
                for present in [false, true] {
                    for expanded in [false, true] {
                        for height in [3, 4, 7, 8, 13] {
                            let card = build_card(
                                CardInput {
                                    workspace: "workspace-six",
                                    name: "claude",
                                    task: Some("fallback task"),
                                    state,
                                    seen,
                                    telemetry: present.then_some(&telemetry),
                                },
                                width,
                                height,
                                expanded,
                            );
                            assert!(card.lines.len() <= height as usize);
                            assert!(card.lines.iter().all(|line| {
                                UnicodeWidthStr::width(
                                    line.iter()
                                        .map(|span| span.text.as_str())
                                        .collect::<String>()
                                        .as_str(),
                                ) <= width as usize
                            }));
                            if !expanded || !present {
                                assert_eq!(card.lines.len(), 3);
                            }
                            assert!(plain(&card)[1].contains("workspace"));
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn expansion_drops_traces_then_tools_then_gauges_then_model() {
        let telemetry = telemetry();
        let render = |height| {
            plain(&build_card(
                CardInput {
                    workspace: "w6",
                    name: "claude",
                    task: None,
                    state: AgentState::Working,
                    seen: true,
                    telemetry: Some(&telemetry),
                },
                35,
                height,
                true,
            ))
        };

        assert_eq!(render(3).len(), 3);
        assert!(render(4).iter().any(|line| line.starts_with("MODEL")));
        assert!(!render(6).iter().any(|line| line.starts_with("CONTEXT")));
        assert!(render(7).iter().any(|line| line.starts_with("COST")));
        assert!(render(8).iter().any(|line| line.starts_with("TOOLS")));
        assert_eq!(render(13).len(), 13);
    }

    #[test]
    fn role_and_semantic_styles_map_to_the_app_palette() {
        let palette = Palette::catppuccin();
        assert_eq!(
            palette_style(WatcherStyle::semantic(Role::Body, Semantic::Warn), &palette).fg,
            Some(palette.yellow)
        );
        assert!(palette_style(WatcherStyle::role(Role::Emphasis), &palette)
            .add_modifier
            .contains(Modifier::BOLD));
    }
}
