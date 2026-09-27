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
}

#[derive(Debug, Clone)]
pub(crate) struct NotepadUsageRow {
    pub(crate) line: Line<'static>,
    pub(crate) action: NotepadUsageAction,
}

fn provider_presentation(provider: QuotaProvider, app: &AppState) -> (&'static str, Color) {
    match provider {
        QuotaProvider::Claude => ("claude", app.palette.peach),
        QuotaProvider::Codex => ("codex", app.palette.blue),
        QuotaProvider::Kimi => ("opencode", app.palette.mauve),
        QuotaProvider::Agy => ("antigravity", app.palette.teal),
    }
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
    let text = account_row_text(account, narrow_label, width, now);
    let style = super::status::provider_style(&account.usage, color, &app.palette);
    NotepadUsageRow {
        line: Line::from(Span::styled(text, style)),
        action: NotepadUsageAction::OpenDashboard,
    }
}

fn provider_header_row(app: &AppState, provider: QuotaProvider, width: u16) -> NotepadUsageRow {
    let (label, color) = provider_presentation(provider, app);
    NotepadUsageRow {
        line: Line::from(Span::styled(
            truncate_end(label, usize::from(width)),
            Style::default()
                .fg(color)
                .add_modifier(ratatui::style::Modifier::BOLD),
        )),
        action: NotepadUsageAction::None,
    }
}

enum UsageRowSource<'a> {
    Provider(QuotaProvider),
    Account(&'a str, &'a ProviderAccountUsage),
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
            sources.push(UsageRowSource::Provider(provider));
            sources.extend(
                narrow_labels
                    .iter()
                    .zip(app.provider_usage.accounts.iter())
                    .filter(|(_, account)| account.provider == provider)
                    .map(|(label, account)| UsageRowSource::Account(label, account)),
            );
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
            })
            .collect(),
        max_scroll,
    )
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

        assert_eq!(rows.len(), 8);
        assert_eq!(max_scroll, 0);
        assert_eq!(
            text.iter().map(String::as_str).collect::<Vec<_>>(),
            [
                "claude",
                "  Claude Code  5h ········ — — · 7d ········ — —",
                "  Claude Code/work  5h ········ — — · 7d ········ — —",
                "codex",
                "  Codex  7d ········ — —",
                "  Codex/work  7d ········ — —",
                "opencode",
                "  Kimi  5h ········ — — · 7d ········ — —",
            ]
        );
        assert_eq!(rows[0].action, NotepadUsageAction::None);
        assert_eq!(rows[1].action, NotepadUsageAction::OpenDashboard);
        assert_eq!(rows[2].action, NotepadUsageAction::OpenDashboard);
        assert_eq!(rows[3].action, NotepadUsageAction::None);
        assert_eq!(rows[6].action, NotepadUsageAction::None);
    }

    #[test]
    fn antigravity_accounts_have_a_provider_header_and_usage_row() {
        let mut app = AppState::test_new();
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

        assert_eq!(text[0], "antigravity");
        assert!(text[1].contains("Antigravity"));
        assert!(text[1].contains("5h"));
    }

    #[test]
    fn account_meters_degrade_at_wide_narrow_and_minimum_widths() {
        let mut app = AppState::test_new();
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
        assert_eq!(row_text(&wide[0]), "claude");
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
    fn narrow_rows_distinguish_similar_profile_ids() {
        let mut app = AppState::test_new();
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
        assert_eq!(labels, ["XSO", "XS0", "XSQ", "XS1"]);
        assert!(rows
            .iter()
            .skip(1)
            .all(|row| row.line.spans[0].content.ends_with("7█100@2d~")));
        let compact = rows.iter().skip(1).map(row_text).collect::<Vec<_>>();
        assert!(compact[0].contains("XSO"), "{}", compact[0]);
        assert!(compact[1].contains("XS0"), "{}", compact[1]);
        assert!(compact[2].contains("XSQ"), "{}", compact[2]);
        assert!(compact[3].contains("XS1"), "{}", compact[3]);
    }

    #[test]
    fn scrolling_counts_headers_and_empty_usage_stays_inert() {
        let mut app = AppState::test_new();
        app.provider_usage = ProviderUsageSnapshot::with_primary_accounts(
            AccountUsage::default(),
            AccountUsage::default(),
            AccountUsage::default(),
        );
        let (rows, max_scroll) = usage_rows_window(&app, 18, usize::MAX, 2);
        assert_eq!(max_scroll, 4);
        assert_eq!(
            rows.iter().map(row_text).collect::<Vec<_>>(),
            ["opencode", "  KKI 5·—/7·—@—/—"]
        );
        assert_eq!(rows[0].action, NotepadUsageAction::None);
        assert_eq!(rows[1].action, NotepadUsageAction::OpenDashboard);

        app.provider_usage.accounts.clear();
        let (rows, max_scroll) = usage_rows_window(&app, 8, 0, 3);
        assert_eq!(max_scroll, 0);
        assert_eq!(row_text(&rows[0]), "no acco…");
        assert_eq!(rows[0].action, NotepadUsageAction::None);
    }
}
