//! Attach-local usage presentation; no provider I/O runs in layout or render.
use super::text::{display_width, truncate_end};
use crate::app::state::AppState;
use crate::provider_usage::{
    AccountUsage, ProviderAccountUsage, QuotaProvider, QuotaWindow, UsageUnavailable,
};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};
use std::collections::{BTreeMap, BTreeSet};

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
    pub(crate) tooltip: Option<String>,
}
const PROVIDERS: [QuotaProvider; 5] = [
    QuotaProvider::Claude,
    QuotaProvider::Codex,
    QuotaProvider::Kimi,
    QuotaProvider::Agy,
    QuotaProvider::OpenCode,
];
fn provider_name(provider: QuotaProvider) -> &'static str {
    match provider {
        QuotaProvider::Claude => "Claude Code",
        QuotaProvider::Codex => "Codex",
        QuotaProvider::Kimi => "Kimi",
        QuotaProvider::Agy => "Antigravity",
        QuotaProvider::OpenCode => "OpenCode",
    }
}
fn provider_color(provider: QuotaProvider, app: &AppState) -> Color {
    match provider {
        QuotaProvider::Claude => super::icons::claude_color(&app.palette),
        QuotaProvider::Codex => super::icons::codex_color(&app.palette),
        QuotaProvider::Kimi => app.palette.mauve,
        QuotaProvider::Agy => app.palette.teal,
        QuotaProvider::OpenCode => app.palette.blue,
    }
}
fn quota_provider_for_agent(agent: crate::detect::Agent) -> Option<QuotaProvider> {
    use crate::detect::Agent;
    match agent {
        Agent::Claude => Some(QuotaProvider::Claude),
        Agent::Codex => Some(QuotaProvider::Codex),
        Agent::Kimi => Some(QuotaProvider::Kimi),
        Agent::Antigravity => Some(QuotaProvider::Agy),
        Agent::OpenCode => Some(QuotaProvider::OpenCode),
        _ => None,
    }
}
type ActiveAccounts<'a> = BTreeMap<QuotaProvider, BTreeSet<(Option<&'a str>, bool)>>;
fn accounts_in_use(app: &AppState) -> ActiveAccounts<'_> {
    let mut active = ActiveAccounts::new();
    for terminal in app.terminals.values() {
        if let Some(provider) = terminal
            .effective_known_agent()
            .and_then(quota_provider_for_agent)
        {
            let profile = terminal.metadata_tokens.get("profile");
            active.entry(provider).or_default().insert((
                profile.or_else(|| terminal.metadata_tokens.get("account")),
                profile.is_some(),
            ));
        }
    }
    for remote in &app.remote_agent_panel_entries {
        if let Some(provider) = remote.entry.agent.and_then(quota_provider_for_agent) {
            let profile = remote.entry.tokens.get("profile");
            active.entry(provider).or_default().insert((
                profile
                    .or_else(|| remote.entry.tokens.get("account"))
                    .map(String::as_str),
                profile.is_some(),
            ));
        }
    }
    active
}
fn is_primary(app: &AppState, account: &ProviderAccountUsage) -> bool {
    app.provider_usage
        .primary(account.provider)
        .or_else(|| {
            app.provider_usage
                .accounts
                .iter()
                .find(|a| a.provider == account.provider)
        })
        .is_some_and(|a| a.profile_id == account.profile_id)
}
fn in_use(app: &AppState, account: &ProviderAccountUsage, active: &ActiveAccounts<'_>) -> bool {
    active.get(&account.provider).is_some_and(|profiles| {
        profiles.iter().any(|(profile, _)| match profile {
            None => is_primary(app, account),
            Some(id) => *id == account.profile_id || *id == account.label,
        })
    })
}
enum UsageRowSource<'a> {
    Account(&'a ProviderAccountUsage, bool),
    Unknown(QuotaProvider, Option<&'a str>, bool),
}
/// Shared source builder: one terminal metadata pass, no formatting or I/O.
fn usage_sources(app: &AppState) -> Vec<UsageRowSource<'_>> {
    let active = accounts_in_use(app);
    let mut sources = Vec::new();
    let mut groups = BTreeMap::<_, Vec<_>>::new();
    for account in &app.provider_usage.accounts {
        groups.entry(account.provider).or_default().push(account);
    }
    for provider in PROVIDERS {
        let accounts = groups.remove(&provider).unwrap_or_default();
        // OpenCode is always listed (folded and expanded) so a broken source
        // stays visible; other providers hide when unused and disconnected.
        if provider != QuotaProvider::OpenCode
            && !active.contains_key(&provider)
            && accounts.iter().all(|a| a.usage.unavailable.is_some())
        {
            continue;
        }
        let mut lead = true;
        if app.notepad.usage_collapsed {
            if let Some(account) = app
                .provider_usage
                .primary(provider)
                .or_else(|| accounts.first().copied())
            {
                sources.push(UsageRowSource::Account(account, true));
            }
        } else {
            let mut seen = BTreeSet::new();
            for account in &accounts {
                if seen.insert(account.label.as_str()) {
                    sources.push(UsageRowSource::Account(account, lead));
                    lead = false;
                }
            }
        }
        if let Some(profiles) = active.get(&provider) {
            for (profile, is_profile) in profiles {
                let known = profile.map_or(!accounts.is_empty(), |id| {
                    accounts.iter().any(|a| a.profile_id == id || a.label == id)
                });
                if !known {
                    sources.push(UsageRowSource::Unknown(provider, *profile, *is_profile));
                }
            }
        }
    }
    sources
}
pub(crate) fn usage_row_count(app: &AppState) -> usize {
    usage_sources(app).len().max(1)
}
fn now(app: &AppState) -> i64 {
    app.status_now_unix
        .unwrap_or(app.view_observed_unix_s.min(i64::MAX as u64) as i64)
}
fn windows(usage: &AccountUsage) -> impl Iterator<Item = (&'static str, QuotaWindow)> {
    [
        (
            "5h",
            usage
                .five_hour
                .filter(|w| w.used_percent != 0 || w.resets_at.is_some()),
        ),
        ("7d", usage.seven_day),
    ]
    .into_iter()
    .filter_map(|(name, window)| window.map(|w| (name, w)))
}
fn code(account: &ProviderAccountUsage) -> &str {
    account
        .usage
        .account
        .as_deref()
        .unwrap_or(match account.provider {
            QuotaProvider::Claude | QuotaProvider::Codex => "?",
            _ => "",
        })
}
fn reason(account: &ProviderAccountUsage, reason: UsageUnavailable) -> String {
    let helper = if account.provider == QuotaProvider::Kimi {
        "kimi-usage"
    } else {
        "agy-usage"
    };
    match reason {
        UsageUnavailable::NoStatuslineData => "no statusline data".into(),
        UsageUnavailable::NoSessionData => "no session data".into(),
        UsageUnavailable::HelperMissing => format!("{helper} missing"),
        UsageUnavailable::HelperTimedOut => format!("{helper} timed out"),
        UsageUnavailable::HelperFailed => format!("{helper} failed"),
        UsageUnavailable::HelperUnreadable => format!("{helper} unreadable"),
        UsageUnavailable::NoQuotaSource => "no quota source".into(),
        UsageUnavailable::UnknownProfile => format!("unknown profile {}", account.profile_id),
        UsageUnavailable::UnknownAccount => format!("unknown account {}", account.profile_id),
        UsageUnavailable::AuthExpired => {
            "stored login refused or expired without a refresh token".into()
        }
    }
}
fn long_reason(account: &ProviderAccountUsage, unavailable: UsageUnavailable) -> String {
    let detail = match unavailable {
        UsageUnavailable::NoStatuslineData => {
            "no current quota window in the local Claude statusline cache"
        }
        UsageUnavailable::NoSessionData => "no rate-limit record in local Codex sessions",
        UsageUnavailable::HelperMissing => "the local usage helper is not installed",
        UsageUnavailable::HelperTimedOut => "the local usage helper exceeded its deadline",
        UsageUnavailable::HelperFailed => "the local usage helper could not run successfully",
        UsageUnavailable::HelperUnreadable => {
            "helper output is oversized, invalid, or has no current quota windows"
        }
        UsageUnavailable::NoQuotaSource => {
            "OpenCode does not expose a supported account quota source"
        }
        UsageUnavailable::UnknownProfile | UsageUnavailable::UnknownAccount => {
            "no matching collected quota account"
        }
        UsageUnavailable::AuthExpired => return reason(account, unavailable),
    };
    format!("{}: {detail}", reason(account, unavailable))
}
fn format_local_timestamp(at: i64) -> Option<String> {
    let datetime = crate::platform::local_datetime_at(u64::try_from(at).ok()?)?;
    datetime
        .format(
            &time::format_description::parse_borrowed::<1>("[weekday repr:short] [hour]:[minute]")
                .ok()?,
        )
        .ok()
}
fn format_local_refresh_time(at: i64) -> Option<String> {
    let datetime = crate::platform::local_datetime_at(u64::try_from(at).ok()?)?;
    datetime
        .format(&time::format_description::parse_borrowed::<1>("[hour]:[minute]").ok()?)
        .ok()
}
fn account_tooltip(
    app: &AppState,
    account: &ProviderAccountUsage,
    active: &ActiveAccounts<'_>,
) -> String {
    let mut lines = vec![
        format!("{} · {}", provider_name(account.provider), code(account)),
        account
            .usage
            .email
            .clone()
            .unwrap_or_else(|| "no email reported".into()),
    ];
    let mut flags = Vec::new();
    if is_primary(app, account) {
        flags.push("selected");
    }
    if in_use(app, account, active) {
        flags.push("in use");
    }
    lines.push(format!(
        "profile {}{}",
        account.profile_id,
        if flags.is_empty() {
            String::new()
        } else {
            format!(" · {}", flags.join(" · "))
        }
    ));
    for (name, w) in windows(&account.usage) {
        let reset = w
            .resets_at
            .and_then(|at| crate::provider_usage::reset_label(at, now(app)))
            .unwrap_or_else(|| "unknown".into());
        let stamp = w
            .resets_at
            .and_then(format_local_timestamp)
            .unwrap_or_else(|| "unknown".into());
        lines.push(format!(
            "{name} {}% used, resets in {reset} ({stamp})",
            w.used_percent
        ));
    }
    if account.provider == QuotaProvider::Codex {
        if let Some(cycles) = crate::provider_usage::five_hour_cycles_until_reset(
            account.usage.seven_day.and_then(|w| w.resets_at),
            now(app),
        ) {
            lines.push(format!("{cycles}× 5h cycles before weekly reset"));
        }
    }
    let refreshed = account
        .usage
        .last_refresh_unix
        .and_then(format_local_refresh_time)
        .unwrap_or_else(|| "never".into());
    lines.push(format!(
        "{} {refreshed}",
        if account.usage.stale {
            "stale: last update"
        } else {
            "updated"
        }
    ));
    if let Some(r) = account.usage.unavailable {
        lines.push(format!(
            "{}: {}",
            if r == UsageUnavailable::AuthExpired {
                "auth expired"
            } else {
                "not connected"
            },
            long_reason(account, r)
        ));
    }
    lines.join("\n")
}
pub(crate) fn device_name(host: &str, tight: bool) -> String {
    let name: String = host
        .split('.')
        .next()
        .unwrap_or("")
        .to_lowercase()
        .chars()
        .take(6)
        .collect();
    if tight {
        match name.as_str() {
            "mbpro" => "p".into(),
            "mbair" => "a".into(),
            "ub1" => "1".into(),
            "ub2" => "2".into(),
            _ => name,
        }
    } else {
        name
    }
}
pub(crate) fn title_tooltip(app: &AppState) -> String {
    let device = device_name(&app.agent_host_name, false);
    let refreshed = app
        .provider_usage
        .collected_unix
        .or_else(|| {
            app.provider_usage
                .accounts
                .iter()
                .filter_map(|a| a.usage.last_refresh_unix)
                .max()
        })
        .and_then(format_local_refresh_time)
        .unwrap_or_else(|| "never".into());
    let mut lines = vec![format!(
        "{device} · usage · refreshed {refreshed}, every 60s"
    )];
    let active = accounts_in_use(app);
    let unknown_accounts: Vec<_> = usage_sources(app)
        .into_iter()
        .filter_map(|source| {
            if let UsageRowSource::Unknown(provider, profile, is_profile) = source {
                let id = profile.unwrap_or("default");
                Some(ProviderAccountUsage {
                    provider,
                    profile_id: id.into(),
                    label: id.into(),
                    usage: AccountUsage {
                        unavailable: Some(if is_profile {
                            UsageUnavailable::UnknownProfile
                        } else {
                            UsageUnavailable::UnknownAccount
                        }),
                        ..AccountUsage::default()
                    },
                })
            } else {
                None
            }
        })
        .collect();

    for provider in PROVIDERS {
        if provider != QuotaProvider::OpenCode
            && !active.contains_key(&provider)
            && app
                .provider_usage
                .accounts
                .iter()
                .filter(|a| a.provider == provider)
                .all(|a| a.usage.unavailable.is_some())
        {
            continue;
        }
        if !app
            .provider_usage
            .accounts
            .iter()
            .chain(unknown_accounts.iter())
            .any(|a| a.provider == provider)
        {
            continue;
        }
        lines.push(provider_name(provider).into());
        for account in app
            .provider_usage
            .accounts
            .iter()
            .chain(unknown_accounts.iter())
            .filter(|a| a.provider == provider)
        {
            let mut flags = Vec::new();
            if is_primary(app, account) {
                flags.push("selected");
            }
            if in_use(app, account, &active) {
                flags.push("in use");
            }
            if flags.is_empty() {
                flags.push("not in use");
            }
            lines.push(format!(
                " {} {} · {}",
                code(account),
                account.usage.email.as_deref().unwrap_or("account unknown"),
                flags.join(" · ")
            ));
            for terminal in app.terminals.values() {
                if terminal
                    .effective_known_agent()
                    .and_then(quota_provider_for_agent)
                    != Some(provider)
                {
                    continue;
                }
                let profile = terminal
                    .metadata_tokens
                    .get("profile")
                    .or_else(|| terminal.metadata_tokens.get("account"));
                if !profile.map_or_else(
                    || is_primary(app, account),
                    |p| p == account.profile_id || p == account.label,
                ) {
                    continue;
                }
                for workspace in &app.workspaces {
                    for (tab_index, tab) in workspace.tabs.iter().enumerate() {
                        if tab
                            .panes
                            .keys()
                            .any(|pane| tab.terminal_id(*pane) == Some(&terminal.id))
                        {
                            lines.push(format!(
                                "  {device} {}/{} · {}",
                                workspace.display_name_from_terminals(&app.terminals),
                                workspace
                                    .tab_display_name_from(&app.terminals, tab_index)
                                    .unwrap_or_else(|| "tab".into()),
                                terminal.agent_model.as_deref().unwrap_or("model unknown")
                            ));
                        }
                    }
                }
            }
            for remote in &app.remote_agent_panel_entries {
                if remote.entry.agent.and_then(quota_provider_for_agent) != Some(provider) {
                    continue;
                }
                let profile = remote
                    .entry
                    .tokens
                    .get("profile")
                    .or_else(|| remote.entry.tokens.get("account"));
                if profile.map_or_else(
                    || is_primary(app, account),
                    |p| *p == account.profile_id || *p == account.label,
                ) {
                    lines.push(format!(
                        "  {} {} · {}",
                        device_name(&remote.agent_ref.host, false),
                        remote.render_title,
                        remote.model.as_deref().unwrap_or("model unknown")
                    ));
                }
            }
            if let Some(r) = account.usage.unavailable {
                lines.push(format!("  {}", reason(account, r)));
            }
            let quota = windows(&account.usage)
                .map(|(name, w)| {
                    format!(
                        "{name} {}% to {}",
                        w.used_percent,
                        w.resets_at
                            .and_then(if name == "5h" {
                                format_local_refresh_time
                            } else {
                                format_local_timestamp
                            })
                            .unwrap_or_else(|| "unknown".into())
                    )
                })
                .collect::<Vec<_>>()
                .join(" · ");
            if !quota.is_empty() {
                lines.push(format!("  {quota}"));
            }
            if provider == QuotaProvider::Claude {
                lines.push(format!(
                    "  {}plan not read",
                    account
                        .usage
                        .cost_usd
                        .map(|cost| format!("5h cost ${cost:.2} · "))
                        .unwrap_or_default()
                ));
            }
        }
    }
    lines.join("\n")
}
fn account_row(
    app: &AppState,
    account: &ProviderAccountUsage,
    lead: bool,
    width: u16,
    active: &ActiveAccounts<'_>,
) -> NotepadUsageRow {
    let icon = super::icons::usage_label(account.provider, app.nerd_font);
    let lead = if lead {
        format!("{icon} ")
    } else {
        " ".repeat(display_width(icon) + 1)
    };
    let identity = if matches!(
        account.usage.unavailable,
        Some(UsageUnavailable::UnknownProfile | UsageUnavailable::UnknownAccount)
    ) {
        ""
    } else {
        code(account)
    };
    let mut identity = truncate_end(identity, 3);
    identity.push_str(&" ".repeat(3usize.saturating_sub(display_width(&identity))));
    let mut spans = vec![
        Span::styled(
            lead,
            Style::default().fg(provider_color(account.provider, app)),
        ),
        Span::styled(identity, Style::default().fg(app.palette.subtext0)),
    ];
    if let Some(r) = account.usage.unavailable {
        spans.push(Span::raw(" "));
        let expired = r == UsageUnavailable::AuthExpired;
        let marker = if expired {
            if app.nerd_font {
                "\u{F023} "
            } else {
                "! "
            }
        } else {
            "- "
        };
        spans.push(Span::styled(
            marker,
            Style::default().fg(if expired {
                app.palette.red
            } else {
                app.palette.overlay0
            }),
        ));
        let short = if expired {
            "auth expired".into()
        } else {
            reason(account, r)
        };
        let used = spans
            .iter()
            .map(|s| display_width(&s.content))
            .sum::<usize>();
        let exhausted = windows(&account.usage).any(|(_, w)| w.used_percent >= 100);
        spans.push(Span::styled(
            truncate_end(
                &short,
                usize::from(width).saturating_sub(used + if exhausted { 2 } else { 0 }),
            ),
            Style::default().fg(app.palette.subtext0),
        ));
        if exhausted {
            spans.push(Span::styled(" ⊘", Style::default().fg(app.palette.red)));
        }
    } else {
        spans.push(Span::styled(
            if account.usage.stale { "~" } else { " " },
            Style::default().fg(app.palette.overlay0),
        ));
        let windows: Vec<_> = windows(&account.usage).collect();
        let exhausted = windows.iter().any(|(_, w)| w.used_percent >= 100);
        let hot = windows
            .iter()
            .max_by_key(|(name, w)| (w.used_percent, *name == "5h"));
        let reset = hot
            .and_then(|(_, w)| w.resets_at)
            .and_then(|at| crate::provider_usage::reset_label(at, now(app)));
        let prefix = spans
            .iter()
            .map(|s| display_width(&s.content))
            .sum::<usize>();
        let bw = pair_bar_width(usize::from(width), prefix);
        if windows.is_empty() {
            spans.push(Span::styled("-", Style::default().fg(app.palette.overlay0)));
        } else {
            // Both slots are always laid out, so every row's 7d bar starts in
            // the same column; an absent window is blank space, never a shift.
            for (i, name) in ["5h", "7d"].into_iter().enumerate() {
                if i > 0 {
                    spans.push(Span::raw(" "));
                }
                let Some((_, w)) = windows.iter().find(|(n, _)| *n == name) else {
                    spans.push(Span::raw(" ".repeat(2 + bw)));
                    continue;
                };
                spans.push(Span::styled(
                    name,
                    Style::default().fg(app.palette.subtext0),
                ));
                spans.extend(pair_bar_spans(app, w.used_percent, bw, account.usage.stale));
            }
        }
        let used = spans
            .iter()
            .map(|s| display_width(&s.content))
            .sum::<usize>();
        let tail = if exhausted { 2 } else { 0 };
        if let Some(reset) = reset.filter(|_| width >= 32) {
            if used + 1 + display_width(&reset) + tail <= usize::from(width) {
                spans.push(Span::styled(
                    format!(" {reset}"),
                    Style::default().fg(app.palette.subtext0),
                ));
            }
        }
        if exhausted {
            spans.push(Span::styled(" ⊘", Style::default().fg(app.palette.red)));
        }
    }
    NotepadUsageRow {
        line: Line::from(spans),
        action: NotepadUsageAction::OpenDashboard,
        tooltip: Some(account_tooltip(app, account, active)),
    }
}
/// Cells per usage bar. Fixed per width (not per row) so every row's bars line
/// up. Four cells still hold a centred `100%`.
fn pair_bar_width(width: usize, prefix: usize) -> usize {
    let preferred = if width >= 32 { 6 } else { 4 };
    // prefix + "5h" + bar + " " + "7d" + bar + " ⊘"
    let room = width.saturating_sub(prefix + 7) / 2;
    preferred.min(room).max(3)
}

/// A solid block bar: the used share is the threshold colour, the rest a dim
/// track, with the bold percent centred across both.
fn pair_bar_spans(app: &AppState, percent: u8, bw: usize, stale: bool) -> Vec<Span<'static>> {
    let palette = &app.palette;
    // Same colour source and 80/90 thresholds as the status bar's usage
    // segments (`status::load_color`), so a level reads the same everywhere.
    let fill = if stale {
        palette.overlay0
    } else {
        super::status::load_color(percent, palette)
    };
    let track = palette.surface1;
    let filled = (usize::from(percent.min(100)) * bw + 50) / 100;
    let mut label = format!("{percent}%");
    if display_width(&label) > bw {
        label = percent.to_string();
    }
    let label: Vec<char> = label.chars().collect();
    let start = bw.saturating_sub(label.len()) / 2;
    let on = |bg: Color| {
        // Highest contrast per cell, so the label reads on fill and track alike.
        let fg = [palette.text, palette.panel_bg]
            .into_iter()
            .max_by(|a, b| {
                let ratio = |c| super::widgets::contrast_ratio(c, bg).unwrap_or(0.0);
                ratio(*a).total_cmp(&ratio(*b))
            })
            .unwrap_or(palette.text);
        let style = Style::default().bg(bg).fg(fg).add_modifier(Modifier::BOLD);
        if stale {
            style.add_modifier(Modifier::DIM)
        } else {
            style
        }
    };
    (0..bw)
        .map(|cell| {
            let ch = cell
                .checked_sub(start)
                .and_then(|i| label.get(i))
                .copied()
                .unwrap_or(' ');
            Span::styled(ch.to_string(), on(if cell < filled { fill } else { track }))
        })
        .collect()
}

pub(crate) fn usage_rows_window(
    app: &AppState,
    width: u16,
    scroll: usize,
    visible: usize,
) -> (Vec<NotepadUsageRow>, usize) {
    let sources = usage_sources(app);
    let active = accounts_in_use(app);
    if sources.is_empty() {
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
    let max_scroll = sources.len().saturating_sub(visible.max(1));
    (
        sources
            .into_iter()
            .skip(scroll.min(max_scroll))
            .take(visible.max(1))
            .map(|source| match source {
                UsageRowSource::Account(account, lead) => {
                    account_row(app, account, lead, width, &active)
                }
                UsageRowSource::Unknown(provider, profile, is_profile) => {
                    let id = profile.unwrap_or("default");
                    let account = ProviderAccountUsage {
                        provider,
                        profile_id: id.into(),
                        label: id.into(),
                        usage: AccountUsage {
                            unavailable: Some(if is_profile {
                                UsageUnavailable::UnknownProfile
                            } else {
                                UsageUnavailable::UnknownAccount
                            }),
                            ..AccountUsage::default()
                        },
                    };
                    account_row(app, &account, true, width, &active)
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
    use crate::provider_usage::ProviderUsageSnapshot;
    fn text(row: &NotepadUsageRow) -> String {
        row.line.spans.iter().map(|s| s.content.as_ref()).collect()
    }
    fn usage(five: u8, seven: u8) -> AccountUsage {
        AccountUsage {
            account: Some("SHQ".into()),
            email: Some("account@example.test".into()),
            five_hour: Some(QuotaWindow {
                used_percent: five,
                resets_at: Some(200804),
            }),
            seven_day: Some(QuotaWindow {
                used_percent: seven,
                resets_at: Some(450000),
            }),
            last_refresh_unix: Some(192700),
            ..AccountUsage::default()
        }
    }
    fn fixture() -> AppState {
        let mut app = AppState::test_new();
        app.agent_host_name = "ub2.example.test".into();
        app.status_now_unix = Some(192764);
        app.provider_usage = ProviderUsageSnapshot::with_primary_accounts(
            usage(81, 40),
            usage(34, 63),
            AccountUsage {
                five_hour: Some(QuotaWindow {
                    used_percent: 0,
                    resets_at: None,
                }),
                seven_day: Some(QuotaWindow {
                    used_percent: 18,
                    resets_at: Some(450000),
                }),
                ..AccountUsage::default()
            },
        );
        let mut sso = app.provider_usage.accounts[0].clone();
        sso.profile_id = "sso".into();
        sso.label = "SSO".into();
        sso.usage = usage(12, 22);
        sso.usage.account = Some("SSO".into());
        app.provider_usage.accounts.push(sso);
        app.provider_usage.accounts.push(ProviderAccountUsage {
            provider: QuotaProvider::Agy,
            profile_id: "default".into(),
            label: "Antigravity".into(),
            usage: AccountUsage {
                five_hour: Some(QuotaWindow {
                    used_percent: 23,
                    resets_at: Some(200804),
                }),
                ..AccountUsage::default()
            },
        });
        app.provider_usage.accounts.push(ProviderAccountUsage {
            provider: QuotaProvider::OpenCode,
            profile_id: "default".into(),
            label: "OpenCode".into(),
            usage: AccountUsage {
                unavailable: Some(UsageUnavailable::NoQuotaSource),
                ..AccountUsage::default()
            },
        });
        app.workspaces = vec![crate::workspace::Workspace::test_new("usage")];
        app.workspaces[0].tabs[0].custom_name = Some("build".into());
        app.ensure_test_terminals();
        let id = app.workspaces[0].tabs[0]
            .terminal_id(app.workspaces[0].tabs[0].root_pane)
            .unwrap()
            .clone();
        let terminal = app.terminals.get_mut(&id).unwrap();
        terminal.detected_agent = Some(crate::detect::Agent::Claude);
        terminal.agent_model = Some("Luna".into());
        app.notepad.set_visible_tabs(vec!["usage".into()]);
        app.notepad.select_usage_tab();
        app
    }
    #[test]
    fn usage_collapsed_selected_accounts_and_dynamic_height() {
        let mut app = fixture();
        let (rows, _) = usage_rows_window(&app, 40, 0, 30);
        // OpenCode is always listed, folded too, even with no pane using it.
        assert_eq!(rows.len(), 5);
        assert!(text(&rows[0]).contains("SHQ"));
        assert!(text(&rows[4]).contains("no quota source"));
        assert!(!rows.iter().any(|r| text(r).contains("SSO")));
        crate::ui::compute_view(&mut app, Rect::new(0, 0, 120, 40));
        assert_eq!(app.view.notepad_usage_content_rows, 5);
        assert_eq!(app.view.notepad_rect.height, 6);
        let folded_list =
            super::super::sidebar::workspace_list_rect_for_app(&app, app.view.sidebar_rect);
        app.notepad.usage_collapsed = false;
        crate::ui::compute_view(&mut app, Rect::new(0, 0, 120, 40));
        assert_eq!(app.view.notepad_usage_content_rows, 6);
        assert_eq!(app.view.notepad_rect.height, 7);
        let expanded_list =
            super::super::sidebar::workspace_list_rect_for_app(&app, app.view.sidebar_rect);
        assert_eq!(folded_list.height, expanded_list.height + 1);
    }
    #[test]
    fn usage_account_code_padding_uses_terminal_cells() {
        let mut app = fixture();
        app.provider_usage.accounts[0].usage.account = Some("界".into());
        let (rows, _) = usage_rows_window(&app, 26, 0, 20);
        assert_eq!(display_width(&rows[0].line.spans[1].content), 3);
        assert_eq!(rows[0].line.spans[1].content, "界 ");
        assert!(display_width(&text(&rows[0])) <= 26);
    }
    #[test]
    fn usage_expanded_groups_accounts_dedupes_and_opencode() {
        let mut app = fixture();
        app.notepad.usage_collapsed = false;
        app.provider_usage
            .accounts
            .push(app.provider_usage.accounts[3].clone());
        let (rows, _) = usage_rows_window(&app, 40, 0, 30);
        // OpenCode is listed even though no pane uses it.
        assert_eq!(rows.len(), 6);
        assert!(text(&rows[0]).starts_with(super::super::icons::usage_label(
            QuotaProvider::Claude,
            app.nerd_font
        )));
        assert!(text(&rows[1]).starts_with("  SSO"));
        assert!(text(&rows[5]).contains("no quota source"));
        assert!(rows
            .iter()
            .all(|r| r.action == NotepadUsageAction::OpenDashboard));
    }
    #[test]
    fn usage_pair_widths_windows_exhaustion_thresholds_and_stale() {
        for width in [26, 40] {
            for nerd in [false, true] {
                let mut app = fixture();
                app.nerd_font = nerd;
                for percent in [79, 80, 90, 100] {
                    app.provider_usage.accounts[0]
                        .usage
                        .five_hour
                        .as_mut()
                        .unwrap()
                        .used_percent = percent;
                    let (rows, _) = usage_rows_window(&app, width, 0, 30);
                    let line = text(&rows[0]);
                    assert!(display_width(&line) <= usize::from(width), "{line}");
                    assert!(line.contains("5h") && line.contains("7d"));
                    if percent == 100 {
                        assert!(line.contains('⊘'));
                    }
                    let expected = super::super::status::load_color(percent, &app.palette);
                    assert!(rows[0]
                        .line
                        .spans
                        .iter()
                        .any(|s| s.style.bg == Some(expected)
                            && s.style.add_modifier.contains(Modifier::BOLD)));
                    let label = format!("{percent}");
                    assert!(line.contains(&label), "{line}");
                }
                let (rows, _) = usage_rows_window(&app, width, 0, 30);
                assert!(!text(&rows[2]).contains("5h"));
                assert!(text(&rows[2]).contains("7d"));
                assert!(text(&rows[3]).contains("5h"));
                assert!(!text(&rows[3]).contains("7d"));
                // The 7d bar sits in the same column whether or not 5h exists.
                let col = |row: &NotepadUsageRow| {
                    let line = text(row);
                    display_width(&line[..line.find("7d").unwrap()])
                };
                assert_eq!(col(&rows[0]), col(&rows[2]));
                app.provider_usage.accounts[0].usage.stale = true;
                let (rows, _) = usage_rows_window(&app, width, 0, 30);
                assert!(text(&rows[0]).contains('~'));
                assert!(rows[0]
                    .line
                    .spans
                    .iter()
                    .any(|s| s.style.bg == Some(app.palette.overlay0)
                        && s.style.add_modifier.contains(Modifier::DIM)));
            }
        }
    }
    #[test]
    fn usage_reasons_auth_and_disconnected_visibility() {
        let mut app = fixture();
        let active = accounts_in_use(&app);
        for r in [
            UsageUnavailable::NoStatuslineData,
            UsageUnavailable::NoSessionData,
            UsageUnavailable::HelperMissing,
            UsageUnavailable::HelperTimedOut,
            UsageUnavailable::HelperFailed,
            UsageUnavailable::HelperUnreadable,
            UsageUnavailable::NoQuotaSource,
            UsageUnavailable::UnknownProfile,
            UsageUnavailable::UnknownAccount,
            UsageUnavailable::AuthExpired,
        ] {
            let mut account = app.provider_usage.accounts[0].clone();
            account.usage.unavailable = Some(r);
            let row = account_row(&app, &account, true, 80, &active);
            if r == UsageUnavailable::AuthExpired {
                assert!(text(&row).contains('\u{F023}'));
                assert!(row.tooltip.unwrap().contains("auth expired:"));
            } else {
                assert!(text(&row).contains(&reason(&account, r)));
            }
        }
        app.provider_usage.accounts[1].usage.unavailable = Some(UsageUnavailable::NoSessionData);
        // Codex hides (disconnected, unused); OpenCode stays listed.
        assert_eq!(usage_row_count(&app), 4);
        app.provider_usage.accounts[0].usage.unavailable = Some(UsageUnavailable::AuthExpired);
        app.nerd_font = false;
        let (rows, _) = usage_rows_window(&app, 26, 0, 20);
        assert!(text(&rows[0]).contains("! auth expired"));
        for a in app
            .provider_usage
            .accounts
            .iter_mut()
            .filter(|a| a.provider == QuotaProvider::Claude)
        {
            a.usage.unavailable = Some(UsageUnavailable::NoStatuslineData);
        }
        assert!(usage_sources(&app).iter().any(
            |s| matches!(s,UsageRowSource::Account(a,_) if a.provider==QuotaProvider::Claude)
        ));
    }
    #[test]
    fn usage_auth_expired_retains_exhaustion_marker_at_both_widths() {
        let mut app = fixture();
        app.provider_usage.accounts[0].usage.unavailable = Some(UsageUnavailable::AuthExpired);
        app.provider_usage.accounts[0]
            .usage
            .five_hour
            .as_mut()
            .unwrap()
            .used_percent = 100;
        for width in [26, 40] {
            for nerd in [false, true] {
                app.nerd_font = nerd;
                let (rows, _) = usage_rows_window(&app, width, 0, 20);
                let line = text(&rows[0]);
                assert!(line.contains('⊘'));
                assert!(display_width(&line) <= usize::from(width));
            }
        }
    }
    #[test]
    fn usage_unknown_profile_and_account_in_both_folds() {
        for collapsed in [false, true] {
            for token in ["profile", "account"] {
                let mut app = fixture();
                app.notepad.usage_collapsed = collapsed;
                let terminal = app.terminals.values_mut().next().unwrap();
                terminal.metadata_tokens.patch(
                    [(token.into(), Some("night".into()))].into(),
                    None,
                    std::time::Instant::now(),
                );
                let (rows, _) = usage_rows_window(&app, 40, 0, 30);
                assert!(rows
                    .iter()
                    .any(|r| text(r).contains(&format!("unknown {token} night"))));
            }
        }
    }
    #[test]
    fn usage_hover_details_device_and_refresh() {
        let app = fixture();
        let (rows, _) = usage_rows_window(&app, 40, 0, 30);
        let tip = rows[0].tooltip.as_ref().unwrap();
        for expected in [
            "Claude Code · SHQ",
            "account@example.test",
            "profile default · selected · in use",
            "5h 81% used, resets in 2h14",
            "updated",
        ] {
            assert!(tip.contains(expected), "{tip}");
        }
        let title = title_tooltip(&app);
        for expected in [
            "ub2 · usage · refreshed",
            "every 60s",
            "ub2 usage/build · Luna",
            "plan not read",
            "SSO",
        ] {
            assert!(title.contains(expected), "{title}");
        }
        for (host, normal, tight) in [
            ("MBPRO.domain", "mbpro", "p"),
            ("mbair", "mbair", "a"),
            ("ub1.local", "ub1", "1"),
            ("ub2", "ub2", "2"),
            ("LONGHOST.domain", "longho", "longho"),
        ] {
            assert_eq!(device_name(host, false), normal);
            assert_eq!(device_name(host, true), tight);
        }
    }
    #[test]
    fn usage_selected_account_precedes_first_account_and_falls_back() {
        let mut app = fixture();
        app.provider_usage.accounts.swap(0, 3);
        let (rows, _) = usage_rows_window(&app, 40, 0, 20);
        assert!(text(&rows[0]).contains("SHQ"));
        app.provider_usage
            .accounts
            .retain(|a| !(a.provider == QuotaProvider::Claude && a.profile_id == "default"));
        let (rows, _) = usage_rows_window(&app, 40, 0, 20);
        assert!(text(&rows[0]).contains("SSO"));
    }
    #[test]
    fn usage_remote_panes_keep_disconnected_accounts_visible_and_describe_models() {
        let mut app = fixture();
        let entry = crate::ui::sidebar_thread_entries(&app)
            .into_iter()
            .next()
            .unwrap();
        let agent_ref = crate::api::schema::AgentRef::new("ub1", "w1:p1").unwrap();
        let mut remote = crate::ui::RemoteAgentPanelEntry::new(agent_ref, entry);
        remote.render_title = "remote-build".into();
        remote.model = Some("Luna".into());
        app.remote_agent_panel_entries = vec![std::sync::Arc::new(remote)];
        for terminal in app.terminals.values_mut() {
            terminal.detected_agent = None;
        }
        for account in app
            .provider_usage
            .accounts
            .iter_mut()
            .filter(|a| a.provider == QuotaProvider::Claude)
        {
            account.usage.unavailable = Some(UsageUnavailable::NoStatuslineData);
        }
        let (rows, _) = usage_rows_window(&app, 40, 0, 20);
        assert!(text(&rows[0]).contains("no statusline data"));
        let title = title_tooltip(&app);
        assert!(title.contains("ub1 remote-build · Luna"), "{title}");
        assert!(title.contains("selected · in use"));
    }
    #[test]
    #[ignore = "manual fixed-geometry usage render scaling profile"]
    fn usage_render_scale_profile() {
        use ratatui::{backend::TestBackend, Terminal};
        for count in [1, 15] {
            let mut app = fixture();
            for index in 1..count {
                let mut workspace =
                    crate::workspace::Workspace::test_new(&format!("agent-{index}"));
                workspace.tabs[0].custom_name = Some("build".into());
                app.workspaces.push(workspace);
            }
            app.ensure_test_terminals();
            for terminal in app.terminals.values_mut() {
                terminal.detected_agent = Some(crate::detect::Agent::Claude);
            }
            let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
            for _ in 0..10 {
                crate::ui::compute_view(&mut app, Rect::new(0, 0, 120, 40));
                terminal.draw(|f| crate::ui::render(&app, f)).unwrap();
            }
            let start = std::time::Instant::now();
            for _ in 0..200 {
                crate::ui::compute_view(&mut app, Rect::new(0, 0, 120, 40));
                terminal.draw(|f| crate::ui::render(&app, f)).unwrap();
            }
            println!(
                "usage fixed 120x40 populated panes={count}: {:.2} us/frame",
                start.elapsed().as_secs_f64() * 1e6 / 200.0
            );
        }
    }
    #[test]
    #[ignore]
    fn usage_capture_dump() {
        use ratatui::{backend::TestBackend, Terminal};
        let dir = std::path::PathBuf::from(
            std::env::var("HERDR_USAGE_CAPTURE_DIR").expect("capture directory"),
        );
        std::fs::create_dir_all(&dir).unwrap();
        for scenario in ["collapsed", "expanded", "disconnected", "auth-expired"] {
            for width in [26, 40] {
                for theme in ["dark", "light"] {
                    let mut app = fixture();
                    app.sidebar_width = width;
                    app.sidebar_max_width = width;
                    if theme == "light" {
                        app.palette = crate::app::state::Palette::catppuccin_latte();
                    }
                    app.notepad.usage_collapsed = scenario == "collapsed";
                    if scenario == "disconnected" {
                        app.provider_usage.accounts[1].usage.unavailable =
                            Some(UsageUnavailable::NoSessionData);
                        app.provider_usage.accounts[2].usage.unavailable =
                            Some(UsageUnavailable::HelperTimedOut);
                        app.provider_usage.accounts[4].usage.unavailable =
                            Some(UsageUnavailable::HelperMissing);
                        app.terminals
                            .values_mut()
                            .next()
                            .unwrap()
                            .metadata_tokens
                            .patch(
                                [("profile".into(), Some("night".into()))].into(),
                                None,
                                std::time::Instant::now(),
                            );
                        // Keep disconnected providers visible by recording actual pane use.
                        for provider in [
                            crate::detect::Agent::Codex,
                            crate::detect::Agent::Kimi,
                            crate::detect::Agent::Antigravity,
                        ] {
                            let id = crate::terminal::TerminalId::alloc();
                            let mut terminal = crate::terminal::TerminalState::new(
                                id.clone(),
                                std::path::PathBuf::from("/tmp"),
                            );
                            terminal.detected_agent = Some(provider);
                            app.terminals.insert(id, terminal);
                        }
                    }
                    if scenario == "auth-expired" {
                        app.provider_usage.accounts[3].usage.unavailable =
                            Some(UsageUnavailable::AuthExpired);
                        app.provider_usage.accounts[0]
                            .usage
                            .five_hour
                            .as_mut()
                            .unwrap()
                            .used_percent = 100;
                    }
                    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
                    crate::ui::compute_view(&mut app, Rect::new(0, 0, 120, 40));
                    terminal
                        .draw(|frame| crate::ui::render(&app, frame))
                        .unwrap();
                    let buffer = terminal.backend().buffer();
                    let sidebar = app.view.sidebar_rect;
                    let mut plain = String::new();
                    let mut ansi = String::new();
                    for y in sidebar.y..sidebar.bottom() {
                        for x in sidebar.x..sidebar.right() {
                            let cell = &buffer[(x, y)];
                            plain.push_str(cell.symbol());
                            for (color, fg) in [(cell.fg, true), (cell.bg, false)] {
                                if let Color::Rgb(r, g, b) = color {
                                    ansi.push_str(&format!(
                                        "\x1b[{};2;{r};{g};{b}m",
                                        if fg { 38 } else { 48 }
                                    ));
                                }
                            }
                            if cell.modifier.contains(Modifier::DIM) {
                                ansi.push_str("\x1b[2m");
                            }
                            ansi.push_str(cell.symbol());
                            ansi.push_str("\x1b[0m");
                        }
                        plain.push('\n');
                        ansi.push('\n');
                    }
                    let stem = format!("{scenario}-{width}-{theme}");
                    std::fs::write(dir.join(format!("{stem}.txt")), plain).unwrap();
                    std::fs::write(dir.join(format!("{stem}.ansi")), ansi).unwrap();
                }
            }
        }
    }
}
