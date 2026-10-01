//! Attach-local read-only quota rows for the notepad's Usage tab.

use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};
use std::collections::HashSet;

use crate::app::state::AppState;
use crate::provider_usage::{ProviderAccountUsage, QuotaProvider, QuotaWindow};

use super::text::{display_width, truncate_end};

pub(crate) const TAB_LABEL: &str = "usage";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NotepadUsageAction {
    None,
    OpenDashboard,
    ToggleProvider(QuotaProvider),
}

#[derive(Debug, Clone)]
pub(crate) struct NotepadUsageRow {
    pub(crate) line: Line<'static>,
    pub(crate) action: NotepadUsageAction,
    pub(crate) tooltip: Option<String>,
}

fn provider_label(provider: QuotaProvider) -> &'static str {
    match provider {
        QuotaProvider::Claude => "claude",
        QuotaProvider::Codex => "codex",
        QuotaProvider::Kimi => "opencode",
        QuotaProvider::Agy => "antigravity",
    }
}

fn provider_presentation(provider: QuotaProvider, app: &AppState) -> (&'static str, Color) {
    let color = match provider {
        QuotaProvider::Claude => crate::ui::icons::claude_color(&app.palette),
        QuotaProvider::Codex => crate::ui::icons::codex_color(&app.palette),
        QuotaProvider::Kimi => app.palette.mauve,
        QuotaProvider::Agy => app.palette.teal,
    };
    (provider_label(provider), color)
}

fn account_label(account: &ProviderAccountUsage) -> String {
    if account.profile_id == "default" || account.label == account.profile_id {
        account.label.clone()
    } else {
        format!("{}/{}", account.label, account.profile_id)
    }
}

fn window_percent(window: Option<QuotaWindow>, suffix: bool) -> String {
    window.map_or_else(
        || "—".into(),
        |window| {
            if suffix {
                format!("{}%", window.used_percent)
            } else {
                window.used_percent.to_string()
            }
        },
    )
}

fn reset_text(window: Option<QuotaWindow>, now: i64) -> String {
    window
        .and_then(|window| window.resets_at)
        .and_then(|at| crate::provider_usage::reset_label(at, now))
        .unwrap_or_else(|| "—".into())
}

fn format_local_datetime(datetime: time::PrimitiveDateTime) -> Option<String> {
    let format = time::format_description::parse_borrowed::<1>(
        "[weekday repr:short] [day] [month repr:short] [hour]:[minute]",
    )
    .ok()?;
    datetime.format(&format).ok()
}

fn format_local_timestamp(unix_seconds: i64) -> Option<String> {
    let datetime = crate::platform::local_datetime_at(u64::try_from(unix_seconds).ok()?)?;
    format_local_datetime(datetime)
}

fn format_local_refresh_time(unix_seconds: i64) -> Option<String> {
    let datetime = crate::platform::local_datetime_at(u64::try_from(unix_seconds).ok()?)?;
    let format = time::format_description::parse_borrowed::<1>("[hour]:[minute]").ok()?;
    datetime.format(&format).ok()
}

fn window_tooltip(name: &str, window: Option<QuotaWindow>) -> String {
    let Some(window) = window else {
        return format!("{name} window: no data");
    };
    let left = 100u8.saturating_sub(window.used_percent);
    let mut line = format!(
        "{name} window: {}% used · {left}% left",
        window.used_percent
    );
    if let Some(reset_at) = window.resets_at.and_then(format_local_timestamp) {
        line.push_str(" until ");
        line.push_str(&reset_at);
    }
    line
}

fn account_tooltip(account: &ProviderAccountUsage) -> String {
    let mut lines = vec![
        window_tooltip("5h", account.usage.five_hour),
        window_tooltip("7d", account.usage.seven_day),
    ];
    if account.usage.stale {
        let stale = account
            .usage
            .last_refresh_unix
            .and_then(format_local_refresh_time)
            .map_or_else(
                || "(stale)".to_string(),
                |time| format!("(stale — last refresh {time})"),
            );
        lines.push(stale);
    }
    lines.join("\n")
}

fn provider_tooltip(provider: QuotaProvider) -> String {
    let label = provider_label(provider);
    let codex_resets = if provider == QuotaProvider::Codex {
        "; N× 5h = five-hour resets left before the weekly reset"
    } else {
        ""
    };
    format!(
        "{label} quota: bars show % of each window used; 5h = rolling five-hour window, 7d = weekly window{codex_resets}"
    )
}

fn narrow_reset_text(window: Option<QuotaWindow>, now: i64) -> String {
    let Some(reset_at) = window.and_then(|window| window.resets_at) else {
        return "—".into();
    };
    let remaining = reset_at.saturating_sub(now);
    if remaining <= 0 {
        return "0m".into();
    }
    let hours = (remaining.saturating_add(3_599) / 3_600).clamp(1, 9);
    if remaining < 86_400 {
        format!("{hours}h")
    } else {
        let days = (remaining.saturating_add(86_399) / 86_400).clamp(1, 9);
        format!("{days}d")
    }
}

fn provider_initial(provider: QuotaProvider) -> char {
    match provider {
        QuotaProvider::Claude => 'C',
        QuotaProvider::Codex => 'X',
        QuotaProvider::Kimi => 'K',
        QuotaProvider::Agy => 'A',
    }
}

fn label_initials(label: &str) -> (char, char) {
    let mut alphanumeric = label.chars().filter(|ch| ch.is_ascii_alphanumeric());
    let first = alphanumeric.next().unwrap_or('A').to_ascii_uppercase();
    let last = alphanumeric
        .next_back()
        .unwrap_or(first)
        .to_ascii_uppercase();
    (first, last)
}

fn narrow_account_labels(accounts: &[ProviderAccountUsage]) -> Vec<String> {
    let mut used = HashSet::new();
    accounts
        .iter()
        .map(|account| {
            let provider = provider_initial(account.provider);
            let (first, last) = label_initials(&account.label);
            let preferred = format!("{provider}{first}{last}");
            if used.insert(preferred.clone()) {
                return preferred;
            }
            for suffix in '0'..='9' {
                let candidate = format!("{provider}{first}{suffix}");
                if used.insert(candidate.clone()) {
                    return candidate;
                }
            }
            for suffix in 'A'..='Z' {
                let candidate = format!("{provider}{first}{suffix}");
                if used.insert(candidate.clone()) {
                    return candidate;
                }
            }
            preferred
        })
        .collect()
}

fn window_meter(window: Option<QuotaWindow>, width: usize) -> String {
    window.map_or_else(
        || "·".repeat(width),
        |window| super::bar::percent_meter(window.used_percent, width),
    )
}

fn codex_reset_count(account: &ProviderAccountUsage, now: i64) -> Option<String> {
    crate::provider_usage::five_hour_cycles_until_reset(
        account.usage.seven_day.and_then(|window| window.resets_at),
        now,
    )
    .map(|cycles| format!("{cycles}× 5h"))
}

fn codex_window_text(
    account: &ProviderAccountUsage,
    identity: &str,
    meter_width: usize,
    now: i64,
    include_count: bool,
    stale: &str,
) -> String {
    let weekly = account.usage.seven_day.map(|window| {
        format!(
            "7d {} {} {}",
            window_meter(Some(window), meter_width),
            window_percent(Some(window), true),
            reset_text(Some(window), now),
        )
    });
    let count = include_count
        .then(|| codex_reset_count(account, now))
        .flatten();
    let five_hour = account.usage.five_hour.map(|window| {
        format!(
            "5h {} {} {}",
            window_meter(Some(window), meter_width),
            window_percent(Some(window), true),
            reset_text(Some(window), now),
        )
    });
    let mut parts = Vec::new();
    if let Some(weekly) = weekly {
        parts.push(weekly);
    }
    if let Some(count) = count {
        parts.push(count);
    }
    if let Some(five_hour) = five_hour {
        parts.push(five_hour);
    }
    if parts.is_empty() {
        // An account that reports no window at all still needs a visible row.
        parts.push(format!(
            "7d {} {} {}",
            window_meter(None, meter_width),
            window_percent(None, true),
            reset_text(None, now),
        ));
    }
    format!("  {identity}  {}{stale}", parts.join(" · "))
}

fn codex_row_text(
    account: &ProviderAccountUsage,
    narrow_label: &str,
    width: u16,
    now: i64,
) -> String {
    let stale = if account.usage.stale { " stale" } else { "" };
    for meter_width in [8, 6, 4] {
        let text = codex_window_text(
            account,
            &account_label(account),
            meter_width,
            now,
            true,
            stale,
        );
        if display_width(&text) <= usize::from(width) {
            return text;
        }
    }
    let stale_mark = if account.usage.stale { "~" } else { "" };
    let medium = codex_window_text(account, narrow_label, 2, now, false, stale_mark);
    if width >= 32 && display_width(&medium) <= usize::from(width) {
        return medium;
    }
    let compact = format!(
        "  {narrow_label} 7{}{}@{}{stale_mark}",
        window_meter(account.usage.seven_day, 1),
        window_percent(account.usage.seven_day, false),
        narrow_reset_text(account.usage.seven_day, now),
    );
    if display_width(&compact) <= usize::from(width) {
        return compact;
    }
    let minimum = format!(
        "  {narrow_label} 7{}{}{stale_mark}",
        window_meter(account.usage.seven_day, 1),
        window_percent(account.usage.seven_day, false),
    );
    truncate_end(&minimum, usize::from(width))
}

fn account_row_text(
    account: &ProviderAccountUsage,
    narrow_label: &str,
    width: u16,
    now: i64,
) -> String {
    if account.provider == QuotaProvider::Codex {
        return codex_row_text(account, narrow_label, width, now);
    }
    let stale = if account.usage.stale { " stale" } else { "" };
    let stale_mark = if account.usage.stale { "~" } else { "" };
    let full_identity = account_label(account);
    for meter_width in [8, 6, 4] {
        let text = format!(
            "  {full_identity}  5h {} {} {} · 7d {} {} {}{stale}",
            window_meter(account.usage.five_hour, meter_width),
            window_percent(account.usage.five_hour, true),
            reset_text(account.usage.five_hour, now),
            window_meter(account.usage.seven_day, meter_width),
            window_percent(account.usage.seven_day, true),
            reset_text(account.usage.seven_day, now),
        );
        if display_width(&text) <= usize::from(width) {
            return text;
        }
    }
    let medium = format!(
        "  {narrow_label} 5h {} {} 7d {} {}{stale_mark}",
        window_meter(account.usage.five_hour, 2),
        window_percent(account.usage.five_hour, false),
        window_meter(account.usage.seven_day, 2),
        window_percent(account.usage.seven_day, false),
    );
    if width >= 32 && display_width(&medium) <= usize::from(width) {
        return medium;
    }
    let compact = format!(
        "  {narrow_label} 5{}{}/7{}{}@{}/{}{stale_mark}",
        window_meter(account.usage.five_hour, 1),
        window_percent(account.usage.five_hour, false),
        window_meter(account.usage.seven_day, 1),
        window_percent(account.usage.seven_day, false),
        narrow_reset_text(account.usage.five_hour, now),
        narrow_reset_text(account.usage.seven_day, now),
    );
    if display_width(&compact) <= usize::from(width) {
        return compact;
    }
    let minimum = format!(
        "  {narrow_label} 5{}{}/7{}{}{stale_mark}",
        window_meter(account.usage.five_hour, 1),
        window_percent(account.usage.five_hour, false),
        window_meter(account.usage.seven_day, 1),
        window_percent(account.usage.seven_day, false),
    );
    truncate_end(&minimum, usize::from(width))
}

fn account_row(
    app: &AppState,
    account: &ProviderAccountUsage,
    narrow_label: &str,
    width: u16,
) -> NotepadUsageRow {
    let (_, color) = provider_presentation(account.provider, app);
    let now = app
        .status_now_unix
        .unwrap_or_else(|| app.view_observed_unix_s.min(i64::MAX as u64) as i64);
    let mut text = account_row_text(account, narrow_label, width, now);
    if is_out_of_usage(account) {
        text = truncate_end(&format!("{text} ⊘"), usize::from(width));
    }
    let inactive = !is_primary(app, account) || account.usage.stale || is_out_of_usage(account);
    let style = if inactive {
        Style::default()
            .fg(app.palette.overlay0)
            .add_modifier(ratatui::style::Modifier::DIM)
    } else {
        super::status::provider_style(&account.usage, color, &app.palette)
    };
    NotepadUsageRow {
        line: Line::from(Span::styled(text, style)),
        action: NotepadUsageAction::OpenDashboard,
        tooltip: Some(account_tooltip(account)),
    }
}

fn short_account_label(account: &ProviderAccountUsage) -> String {
    account.label.chars().take(3).collect()
}

fn is_primary(app: &AppState, account: &ProviderAccountUsage) -> bool {
    app.provider_usage
        .primary(account.provider)
        .map(|primary| primary.profile_id == account.profile_id)
        .unwrap_or_else(|| {
            app.provider_usage
                .accounts
                .iter()
                .find(|candidate| candidate.provider == account.provider)
                .is_some_and(|first| first.profile_id == account.profile_id)
        })
}

fn is_out_of_usage(account: &ProviderAccountUsage) -> bool {
    account.usage.peak_percent() == Some(100)
}

fn provider_header_row(app: &AppState, provider: QuotaProvider, width: u16) -> NotepadUsageRow {
    let (label, color) = provider_presentation(provider, app);
    let expanded =
        !app.notepad.usage_collapsed || app.notepad.usage_expanded_providers.contains(&provider);
    let text = format!(
        "{} {} {label}",
        if expanded { "▾" } else { "▸" },
        crate::ui::icons::usage_label(provider, app.nerd_font),
    );
    NotepadUsageRow {
        line: Line::from(Span::styled(
            truncate_end(&text, usize::from(width)),
            Style::default()
                .fg(color)
                .add_modifier(ratatui::style::Modifier::BOLD),
        )),
        action: NotepadUsageAction::ToggleProvider(provider),
        tooltip: Some(provider_tooltip(provider)),
    }
}

fn provider_summary_row(
    app: &AppState,
    provider: QuotaProvider,
    account: &ProviderAccountUsage,
    width: u16,
) -> NotepadUsageRow {
    let (label, color) = provider_presentation(provider, app);
    let prefix = format!(
        "{} {}",
        label.chars().take(3).collect::<String>(),
        short_account_label(account)
    );
    let summary = format!(
        "{} {}",
        window_meter(account.usage.five_hour, 1),
        window_meter(account.usage.seven_day, 1)
    );
    let prefix = truncate_end(&prefix, usize::from(width));
    let prefix_width = display_width(&prefix);
    let summary_width = usize::from(width).saturating_sub(prefix_width.saturating_add(1));
    let summary = truncate_end(&summary, summary_width);
    let out_of_usage = is_out_of_usage(account)
        && prefix_width
            .saturating_add(display_width(&summary))
            .saturating_add(2)
            <= usize::from(width);
    let inactive = !is_primary(app, account) || account.usage.stale || is_out_of_usage(account);
    let mut spans = vec![Span::styled(
        prefix,
        Style::default()
            .fg(if inactive {
                app.palette.overlay0
            } else {
                color
            })
            .add_modifier(
                ratatui::style::Modifier::BOLD
                    | if inactive {
                        ratatui::style::Modifier::DIM
                    } else {
                        ratatui::style::Modifier::empty()
                    },
            ),
    )];
    if !summary.is_empty() && prefix_width < usize::from(width) {
        spans.push(Span::raw(" "));
        spans.push(Span::raw(summary));
    }
    if out_of_usage {
        spans.push(Span::raw(" ⊘"));
    }
    NotepadUsageRow {
        line: Line::from(spans),
        action: NotepadUsageAction::ToggleProvider(provider),
        tooltip: Some(provider_tooltip(provider)),
    }
}

enum UsageRowSource<'a> {
    Provider(QuotaProvider),
    Account(&'a str, &'a ProviderAccountUsage),
    Summary(QuotaProvider, &'a ProviderAccountUsage),
}

pub(crate) fn usage_rows_window(
    app: &AppState,
    width: u16,
    scroll: usize,
    visible: usize,
) -> (Vec<NotepadUsageRow>, usize) {
    if app.provider_usage.accounts.is_empty() {
        return (
            vec![NotepadUsageRow {
                line: Line::from(Span::styled(
                    truncate_end("no account usage", usize::from(width)),
                    Style::default().fg(app.palette.overlay0),
                )),
                action: NotepadUsageAction::None,
                tooltip: None,
            }],
            0,
        );
    }
    let visible = visible.max(1);
    let narrow_labels = narrow_account_labels(&app.provider_usage.accounts);
    let mut sources = Vec::with_capacity(app.provider_usage.accounts.len().saturating_add(3));
    for provider in [
        QuotaProvider::Claude,
        QuotaProvider::Codex,
        QuotaProvider::Kimi,
        QuotaProvider::Agy,
    ] {
        if app
            .provider_usage
            .accounts
            .iter()
            .any(|account| account.provider == provider)
        {
            if app.notepad.usage_collapsed
                && !app.notepad.usage_expanded_providers.contains(&provider)
            {
                if let Some(account) = app.provider_usage.primary(provider).or_else(|| {
                    app.provider_usage
                        .accounts
                        .iter()
                        .find(|account| account.provider == provider)
                }) {
                    sources.push(UsageRowSource::Summary(provider, account));
                }
            } else {
                sources.push(UsageRowSource::Provider(provider));
                let mut seen_accounts = HashSet::new();
                sources.extend(
                    narrow_labels
                        .iter()
                        .zip(app.provider_usage.accounts.iter())
                        .filter(|(_, account)| {
                            account.provider == provider
                                && seen_accounts.insert(account.label.as_str())
                        })
                        .map(|(label, account)| UsageRowSource::Account(label, account)),
                );
            }
        }
    }
    let max_scroll = sources.len().saturating_sub(visible);
    let scroll = scroll.min(max_scroll);
    (
        sources
            .into_iter()
            .skip(scroll)
            .take(visible)
            .map(|source| match source {
                UsageRowSource::Provider(provider) => provider_header_row(app, provider, width),
                UsageRowSource::Account(narrow_label, account) => {
                    account_row(app, account, narrow_label, width)
                }
                UsageRowSource::Summary(provider, account) => {
                    provider_summary_row(app, provider, account, width)
                }
            })
            .collect(),
        max_scroll,
    )
}

pub(crate) fn usage_row_hit_areas(rows: &[NotepadUsageRow], body: Rect) -> Vec<Rect> {
    rows.iter()
        .take(usize::from(body.height))
        .enumerate()
        .map(|(index, _)| {
            Rect::new(
                body.x,
                body.y
                    .saturating_add(u16::try_from(index).unwrap_or(u16::MAX)),
                body.width,
                1,
            )
        })
        .collect()
}

pub(crate) fn render_usage_body(app: &AppState, frame: &mut Frame, body: Rect) {
    for (offset, row) in app
        .view
        .notepad_usage_rows
        .iter()
        .take(usize::from(body.height))
        .enumerate()
    {
        frame.render_widget(
            Paragraph::new(row.line.clone()),
            Rect::new(body.x, body.y.saturating_add(offset as u16), body.width, 1),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_usage::{AccountUsage, ProviderUsageSnapshot, QuotaWindow};

    fn row_text(row: &NotepadUsageRow) -> String {
        row.line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn rows_group_multiple_accounts_under_provider_headers() {
        let mut app = AppState::test_new();
        app.notepad.usage_collapsed = false;
        app.provider_usage = ProviderUsageSnapshot::with_primary_accounts(
            AccountUsage::default(),
            AccountUsage::default(),
            AccountUsage::default(),
        );
        let claude = app.provider_usage.accounts[0].clone();
        let codex = app.provider_usage.accounts[1].clone();
        let kimi = app.provider_usage.accounts[2].clone();
        let mut claude_work = claude.clone();
        claude_work.profile_id = "work".into();
        let mut codex_work = codex.clone();
        codex_work.profile_id = "work".into();
        app.provider_usage.accounts = vec![claude, codex, kimi, claude_work, codex_work];

        let (rows, max_scroll) = usage_rows_window(&app, 100, 0, 20);
        let text = rows.iter().map(row_text).collect::<Vec<_>>();

        assert_eq!(rows.len(), 6);
        assert_eq!(max_scroll, 0);
        assert_eq!(
            text.iter().map(String::as_str).collect::<Vec<_>>(),
            [
                "▾ \u{EC82} claude",
                "  Claude Code  5h ········ — — · 7d ········ — —",
                "▾ \u{EC81} codex",
                "  Codex  7d ········ — —",
                "▾ \u{F6001} opencode",
                "  Kimi  5h ········ — — · 7d ········ — —",
            ]
        );
        assert_eq!(
            rows[0].action,
            NotepadUsageAction::ToggleProvider(QuotaProvider::Claude)
        );
        assert_eq!(rows[1].action, NotepadUsageAction::OpenDashboard);
        assert_eq!(
            rows[2].action,
            NotepadUsageAction::ToggleProvider(QuotaProvider::Codex)
        );
        assert_eq!(rows[3].action, NotepadUsageAction::OpenDashboard);
        assert_eq!(
            rows[4].action,
            NotepadUsageAction::ToggleProvider(QuotaProvider::Kimi)
        );
    }

    #[test]
    fn antigravity_accounts_have_a_provider_header_and_usage_row() {
        let mut app = AppState::test_new();
        app.notepad.usage_collapsed = false;
        app.provider_usage.accounts.push(ProviderAccountUsage {
            provider: QuotaProvider::Agy,
            profile_id: "default".into(),
            label: "Antigravity".into(),
            usage: crate::provider_usage::AccountUsage {
                five_hour: Some(QuotaWindow {
                    used_percent: 1,
                    resets_at: None,
                }),
                ..crate::provider_usage::AccountUsage::default()
            },
        });

        let (rows, _) = usage_rows_window(&app, 100, 0, 20);
        let text = rows.iter().map(row_text).collect::<Vec<_>>();

        assert_eq!(text[0], "▾ \u{F6000} antigravity");
        assert!(text[1].contains("Antigravity"));
        assert!(text[1].contains("5h"));
    }

    #[test]
    fn account_meters_degrade_at_wide_narrow_and_minimum_widths() {
        let mut app = AppState::test_new();
        app.notepad.usage_collapsed = false;
        app.status_now_unix = Some(1_800_000_000);
        app.provider_usage = ProviderUsageSnapshot::with_primary_accounts(
            AccountUsage {
                five_hour: Some(QuotaWindow {
                    used_percent: 81,
                    resets_at: Some(1_800_000_720),
                }),
                seven_day: Some(QuotaWindow {
                    used_percent: 92,
                    resets_at: Some(1_800_009_900),
                }),
                stale: true,
                ..AccountUsage::default()
            },
            AccountUsage::default(),
            AccountUsage::default(),
        );
        app.provider_usage.accounts[0].label = "SHQ".into();
        app.provider_usage.accounts[0].profile_id = "scalablehq".into();
        app.provider_usage
            .accounts
            .retain(|account| account.provider == QuotaProvider::Claude);

        let (wide, _) = usage_rows_window(&app, 80, 0, 10);
        assert_eq!(row_text(&wide[0]), "▾ \u{EC82} claude");
        let text = row_text(&wide[1]);
        for expected in [
            "SHQ/scalablehq",
            "5h ██████▄· 81% 12m",
            "7d ███████▄ 92% 2h45",
            "stale",
        ] {
            assert!(text.contains(expected), "missing {expected:?}: {text}");
        }
        let (narrow, _) = usage_rows_window(&app, 26, 0, 10);
        assert_eq!(row_text(&narrow[1]), "  CSQ 5▇81/7▇92@1h/3h~");
        let (minimum, _) = usage_rows_window(&app, 18, 0, 10);
        assert_eq!(row_text(&minimum[1]), "  CSQ 5▇81/7▇92~");
        let hit_areas = usage_row_hit_areas(&minimum, Rect::new(4, 20, 18, 10));
        assert_eq!(hit_areas.len(), minimum.len());
        assert!(hit_areas.iter().all(|area| area.width == 18 && area.x == 4));
        assert_eq!(hit_areas[0].y, 20);
        assert_eq!(hit_areas[1].y, 21);
        for (width, rows) in [(80u16, wide), (26, narrow), (18, minimum)] {
            assert!(
                rows.iter()
                    .all(|row| display_width(&row_text(row)) <= usize::from(width)),
                "width {width}: {:?}",
                rows.iter().map(row_text).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn codex_rows_show_weekly_count_and_only_reported_windows() {
        let mut app = AppState::test_new();
        app.notepad.usage_collapsed = false;
        app.status_now_unix = Some(1_800_000_000);
        app.provider_usage = ProviderUsageSnapshot::with_primary_accounts(
            AccountUsage::default(),
            AccountUsage {
                seven_day: Some(QuotaWindow {
                    used_percent: 70,
                    resets_at: Some(1_800_432_000),
                }),
                ..AccountUsage::default()
            },
            AccountUsage::default(),
        );
        app.provider_usage.accounts[1].label = "scalable-so".into();
        app.provider_usage.accounts[1].profile_id = "default".into();

        let (rows, _) = usage_rows_window(&app, 100, 0, 10);
        let text = row_text(&rows[3]);
        assert!(text.starts_with("  scalable-so  7d "), "{text}");
        assert!(text.contains("70% 5d0h · 24× 5h"), "{text}");
    }

    #[test]
    fn codex_team_rows_keep_both_windows_and_reset_count() {
        let mut app = AppState::test_new();
        app.notepad.usage_collapsed = false;
        app.status_now_unix = Some(1_800_000_000);
        app.provider_usage = ProviderUsageSnapshot::with_primary_accounts(
            AccountUsage::default(),
            AccountUsage {
                five_hour: Some(QuotaWindow {
                    used_percent: 20,
                    resets_at: Some(1_800_007_200),
                }),
                seven_day: Some(QuotaWindow {
                    used_percent: 70,
                    resets_at: Some(1_800_432_000),
                }),
                ..AccountUsage::default()
            },
            AccountUsage::default(),
        );

        let (rows, _) = usage_rows_window(&app, 120, 0, 10);
        let text = row_text(&rows[3]);
        assert!(text.contains("7d "), "{text}");
        assert!(text.contains("70% 5d0h"), "{text}");
        assert!(text.contains("24× 5h"), "{text}");
        assert!(text.contains("5h █▅······ 20% 2h00"), "{text}");
    }

    #[test]
    fn codex_rows_drop_count_before_weekly_details_as_width_shrinks() {
        let mut app = AppState::test_new();
        app.notepad.usage_collapsed = false;
        app.status_now_unix = Some(1_800_000_000);
        app.provider_usage = ProviderUsageSnapshot::with_primary_accounts(
            AccountUsage::default(),
            AccountUsage {
                seven_day: Some(QuotaWindow {
                    used_percent: 70,
                    resets_at: Some(1_800_432_000),
                }),
                ..AccountUsage::default()
            },
            AccountUsage::default(),
        );
        let (wide, _) = usage_rows_window(&app, 100, 0, 10);
        let (narrow, _) = usage_rows_window(&app, 32, 0, 10);
        let (minimum, _) = usage_rows_window(&app, 18, 0, 10);
        let wide_text = row_text(&wide[3]);
        let narrow_text = row_text(&narrow[3]);
        let minimum_text = row_text(&minimum[3]);
        assert!(wide_text.contains("24× 5h"), "{wide_text}");
        assert!(!narrow_text.contains("24× 5h"), "{narrow_text}");
        assert!(narrow_text.contains("7d"), "{narrow_text}");
        assert!(minimum_text.contains("7"), "{minimum_text}");
        for (width, rows) in [(100u16, wide), (32, narrow), (18, minimum)] {
            assert!(
                rows.iter()
                    .all(|row| display_width(&row_text(row)) <= usize::from(width)),
                "width {width}: {:?}",
                rows.iter().map(row_text).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn narrow_rows_drop_repeated_provider_account_profiles() {
        let mut app = AppState::test_new();
        app.notepad.usage_collapsed = false;
        app.status_now_unix = Some(1_800_000_000);
        app.provider_usage = ProviderUsageSnapshot::with_primary_accounts(
            AccountUsage::default(),
            AccountUsage {
                five_hour: Some(QuotaWindow {
                    used_percent: 100,
                    resets_at: Some(1_800_003_600),
                }),
                seven_day: Some(QuotaWindow {
                    used_percent: 100,
                    resets_at: Some(1_800_172_800),
                }),
                stale: true,
                ..AccountUsage::default()
            },
            AccountUsage::default(),
        );
        let mut scalable_so = app.provider_usage.accounts[1].clone();
        scalable_so.label = "SSO".into();
        scalable_so.profile_id = "scalable-so".into();
        let mut scalable_so_2 = scalable_so.clone();
        scalable_so_2.profile_id = "scalable-so-2".into();
        let mut matthias_scalablehq = scalable_so.clone();
        matthias_scalablehq.label = "SHQ".into();
        matthias_scalablehq.profile_id = "matthias-scalablehq".into();
        let mut matthias_scalable_so = scalable_so.clone();
        matthias_scalable_so.profile_id = "matthias-scalable-so".into();
        app.provider_usage.accounts = vec![
            scalable_so,
            scalable_so_2,
            matthias_scalablehq,
            matthias_scalable_so,
        ];

        let (rows, _) = usage_rows_window(&app, 26, 0, 10);
        let labels = rows
            .iter()
            .skip(1)
            .map(|row| {
                row.line.spans[0]
                    .content
                    .trim_start()
                    .split(' ')
                    .next()
                    .unwrap_or("")
            })
            .collect::<Vec<_>>();
        assert_eq!(labels, ["XSO", "XSQ"]);
        let compact = rows.iter().skip(1).map(row_text).collect::<Vec<_>>();
        assert!(compact[0].contains("XSO"), "{}", compact[0]);
        assert!(compact[1].contains("XSQ"), "{}", compact[1]);
        assert!(compact.iter().all(|row| row.contains("100@2d~")));
    }

    #[test]
    fn scrolling_counts_headers_and_empty_usage_stays_inert() {
        let mut app = AppState::test_new();
        app.notepad.usage_collapsed = false;
        app.provider_usage = ProviderUsageSnapshot::with_primary_accounts(
            AccountUsage::default(),
            AccountUsage::default(),
            AccountUsage::default(),
        );
        let (rows, max_scroll) = usage_rows_window(&app, 18, usize::MAX, 2);
        assert_eq!(max_scroll, 4);
        assert_eq!(
            rows.iter().map(row_text).collect::<Vec<_>>(),
            ["▾ \u{F6001} opencode", "  KKI 5·—/7·—@—/—"]
        );
        assert_eq!(
            rows[0].action,
            NotepadUsageAction::ToggleProvider(QuotaProvider::Kimi)
        );
        assert_eq!(rows[1].action, NotepadUsageAction::OpenDashboard);

        app.provider_usage.accounts.clear();
        let (rows, max_scroll) = usage_rows_window(&app, 8, 0, 3);
        assert_eq!(max_scroll, 0);
        assert_eq!(row_text(&rows[0]), "no acco…");
        assert_eq!(rows[0].action, NotepadUsageAction::None);
    }

    fn tooltip_account(usage: crate::provider_usage::AccountUsage) -> ProviderAccountUsage {
        ProviderAccountUsage {
            provider: QuotaProvider::Claude,
            profile_id: "default".into(),
            label: "primary".into(),
            usage,
        }
    }

    #[test]
    fn local_reset_formatter_uses_the_fixed_epoch_and_timezone() {
        let utc =
            time::OffsetDateTime::from_unix_timestamp(1_790_606_400).expect("fixed reset epoch");
        let local = utc.to_offset(time::UtcOffset::from_hms(2, 0, 0).expect("fixed UTC+2"));
        let datetime = time::PrimitiveDateTime::new(local.date(), local.time());

        assert_eq!(
            format_local_datetime(datetime).as_deref(),
            Some("Mon 28 Sep 16:40")
        );
    }

    #[test]
    fn account_tooltip_formats_complete_windows_with_exact_local_resets() {
        let account = tooltip_account(crate::provider_usage::AccountUsage {
            five_hour: Some(QuotaWindow {
                used_percent: 42,
                resets_at: Some(1_790_606_400),
            }),
            seven_day: Some(QuotaWindow {
                used_percent: 71,
                resets_at: Some(1_790_606_400),
            }),
            ..crate::provider_usage::AccountUsage::default()
        });

        let reset = format_local_timestamp(1_790_606_400).expect("local reset time");
        assert_eq!(
            account_tooltip(&account),
            format!(
                "5h window: 42% used · 58% left until {reset}\n7d window: 71% used · 29% left until {reset}"
            )
        );
    }

    #[test]
    fn account_tooltip_handles_missing_windows_and_missing_reset_times() {
        let account = tooltip_account(crate::provider_usage::AccountUsage {
            seven_day: Some(QuotaWindow {
                used_percent: 71,
                resets_at: None,
            }),
            ..crate::provider_usage::AccountUsage::default()
        });

        assert_eq!(
            account_tooltip(&account),
            "5h window: no data\n7d window: 71% used · 29% left"
        );
    }

    #[test]
    fn stale_account_tooltip_includes_refresh_time_only_when_available() {
        let with_refresh = tooltip_account(crate::provider_usage::AccountUsage {
            stale: true,
            last_refresh_unix: Some(1_790_606_400),
            ..crate::provider_usage::AccountUsage::default()
        });
        let without_refresh = tooltip_account(crate::provider_usage::AccountUsage {
            stale: true,
            ..crate::provider_usage::AccountUsage::default()
        });

        let refresh = format_local_refresh_time(1_790_606_400).expect("local refresh time");
        assert!(
            account_tooltip(&with_refresh).ends_with(&format!("(stale — last refresh {refresh})"))
        );
        assert!(account_tooltip(&without_refresh).ends_with("(stale)"));
    }

    #[test]
    fn provider_header_tooltips_explain_windows_and_codex_reset_count() {
        assert_eq!(
            provider_tooltip(QuotaProvider::Claude),
            "claude quota: bars show % of each window used; 5h = rolling five-hour window, 7d = weekly window"
        );
        assert_eq!(
            provider_tooltip(QuotaProvider::Codex),
            "codex quota: bars show % of each window used; 5h = rolling five-hour window, 7d = weekly window; N× 5h = five-hour resets left before the weekly reset"
        );
    }

    #[test]
    fn provider_headers_and_collapsed_summaries_use_each_configured_icon_fallback() {
        let mut app = AppState::test_new();
        let usage = crate::provider_usage::AccountUsage {
            five_hour: Some(QuotaWindow {
                used_percent: 42,
                resets_at: None,
            }),
            seven_day: Some(QuotaWindow {
                used_percent: 71,
                resets_at: None,
            }),
            ..crate::provider_usage::AccountUsage::default()
        };
        let providers = [
            QuotaProvider::Claude,
            QuotaProvider::Codex,
            QuotaProvider::Kimi,
            QuotaProvider::Agy,
        ];

        for provider in providers {
            let label = provider_label(provider);
            app.provider_usage.accounts = vec![ProviderAccountUsage {
                provider,
                profile_id: "default".into(),
                label: label.into(),
                usage: usage.clone(),
            }];
            let color = match provider {
                QuotaProvider::Claude => crate::ui::icons::claude_color(&app.palette),
                QuotaProvider::Codex => crate::ui::icons::codex_color(&app.palette),
                QuotaProvider::Kimi => app.palette.mauve,
                QuotaProvider::Agy => app.palette.teal,
            };
            let fallback = match provider {
                QuotaProvider::Claude => "CC",
                QuotaProvider::Codex => "CX",
                QuotaProvider::Kimi => "KI",
                QuotaProvider::Agy => "AG",
            };

            for nerd_font in [true, false] {
                let icon = crate::ui::icons::usage_label(provider, nerd_font);
                if !nerd_font {
                    assert_eq!(icon, fallback);
                }
                app.nerd_font = nerd_font;
                app.notepad.usage_collapsed = false;
                let (expanded, _) = usage_rows_window(&app, 80, 0, 10);
                assert!(row_text(&expanded[0]).contains(label));
                assert!(expanded[0]
                    .line
                    .spans
                    .iter()
                    .any(|span| span.style.fg == Some(color)));

                app.notepad.usage_collapsed = true;
                let (collapsed, _) = usage_rows_window(&app, 80, 0, 10);
                assert!(
                    row_text(&collapsed[0]).contains(&label.chars().take(3).collect::<String>())
                );
                assert!(collapsed[0]
                    .line
                    .spans
                    .iter()
                    .any(|span| span.style.fg == Some(color)));
            }
        }
    }

    #[test]
    fn collapsing_usage_reduces_rendered_rows_to_one_per_provider() {
        let mut app = AppState::test_new();
        app.provider_usage = ProviderUsageSnapshot::with_primary_accounts(
            AccountUsage::default(),
            AccountUsage::default(),
            AccountUsage::default(),
        );
        let (collapsed, _) = usage_rows_window(&app, 100, 0, 20);
        assert_eq!(collapsed.len(), 3);

        app.notepad.toggle_usage_collapsed();
        let (expanded, _) = usage_rows_window(&app, 100, 0, 20);
        assert_eq!(expanded.len(), 6);
        assert!(expanded
            .iter()
            .any(|row| matches!(row.action, NotepadUsageAction::ToggleProvider(_))));
    }
}
