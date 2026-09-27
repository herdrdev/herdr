use std::collections::{BTreeMap, BTreeSet};

use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::{
    app::state::{
        AppState, Palette, UsageBreakdown, UsageHitArea, UsageHitTarget, UsageMetric, UsageRange,
        UsageViewState,
    },
    config::{UsageConfig, UsageModelPricing},
    provider_usage::{ProviderUsageSnapshot, QuotaWindow, UsageProvider, UsageSample},
};

const HEADER_CONTROLS: [(UsageHitTarget, u16); 7] = [
    (UsageHitTarget::Cost, 6),
    (UsageHitTarget::Tokens, 8),
    (UsageHitTarget::Hours24, 10),
    (UsageHitTarget::Days7, 8),
    (UsageHitTarget::Days30, 9),
    (UsageHitTarget::Days90, 9),
    (UsageHitTarget::Rescan, 1),
];
const HEADER_CONTROLS_WIDTH: u16 = 57;
const HEADER_SAME_ROW_MIN_WIDTH: u16 = 90;

#[derive(Debug, Clone, Copy, Default)]
struct Totals {
    input: u64,
    output: u64,
    cache_write: u64,
    cache_read: u64,
    known_cost: f64,
    priced_samples: usize,
    samples: usize,
}

impl Totals {
    fn add(&mut self, sample: &UsageSample, pricing: Option<UsageModelPricing>) {
        self.input = self.input.saturating_add(sample.input_tokens);
        self.output = self.output.saturating_add(sample.output_tokens);
        self.cache_write = self.cache_write.saturating_add(sample.cache_write_tokens);
        self.cache_read = self.cache_read.saturating_add(sample.cache_read_tokens);
        self.samples += 1;
        if let Some(pricing) = pricing {
            self.known_cost += sample_cost(sample, pricing);
            self.priced_samples += 1;
        }
    }

    fn tokens(self) -> u64 {
        self.input
            .saturating_add(self.output)
            .saturating_add(self.cache_write)
            .saturating_add(self.cache_read)
    }

    fn cost(self) -> Option<f64> {
        (self.priced_samples > 0).then_some(self.known_cost)
    }

    fn cost_label(self) -> String {
        match (self.priced_samples, self.samples) {
            (0, _) => "—".to_string(),
            (priced, samples) if priced < samples => format!("≥${:.2}", self.known_cost),
            _ => format!("${:.2}", self.known_cost),
        }
    }
}

#[derive(Debug, Clone)]
struct UsageProjection {
    now: i64,
    start: i64,
    total: Totals,
    sessions: usize,
    providers: BTreeMap<UsageProvider, (Totals, usize)>,
    breakdown: Vec<(String, Totals)>,
    bars: Vec<f64>,
    provider_bars: BTreeMap<UsageProvider, Vec<f64>>,
    cache_savings: f64,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct UsageLayout {
    pub(crate) header: Rect,
    pub(crate) summary: Rect,
    pub(crate) subscription: Rect,
    pub(crate) chart: Rect,
    pub(crate) totals: Rect,
    pub(crate) breakdown: Rect,
    pub(crate) footer: Rect,
}

/// Whether Antigravity reported a quota window, so the usage view gives it a row.
pub(crate) fn agy_has_data(snapshot: &ProviderUsageSnapshot) -> bool {
    let agy = snapshot.primary_usage(crate::provider_usage::QuotaProvider::Agy);
    agy.five_hour.is_some() || agy.seven_day.is_some()
}

pub(crate) fn layout(area: Rect, agy_row: bool) -> UsageLayout {
    let outer = Block::default().borders(Borders::ALL).inner(area);
    let narrow = outer.width < 80;
    // Narrow screens stack the subscription panel, so a fourth provider needs two more rows.
    let extra = if narrow && agy_row { 2 } else { 0 };
    let rows = Layout::vertical([
        Constraint::Length(header_rows(outer.width)),
        Constraint::Length(if narrow { 11 + extra } else { 9 }),
        Constraint::Length(if narrow { 3 } else { 2 }),
        Constraint::Min(3),
        Constraint::Length(if narrow { 2 } else { 1 }),
    ])
    .split(outer);
    let (summary, subscription, chart) = if narrow {
        let stacked =
            Layout::vertical([Constraint::Length(6 + extra), Constraint::Min(4)]).split(rows[1]);
        let columns =
            Layout::horizontal([Constraint::Length(34), Constraint::Min(20)]).split(stacked[0]);
        (columns[0], columns[1], stacked[1])
    } else {
        let columns = Layout::horizontal([
            Constraint::Length(34),
            Constraint::Length(44),
            Constraint::Min(24),
        ])
        .split(rows[1]);
        (columns[0], columns[1], columns[2])
    };
    UsageLayout {
        header: rows[0],
        summary,
        subscription,
        chart,
        totals: rows[2],
        breakdown: rows[3],
        footer: rows[4],
    }
}

fn header_rows(width: u16) -> u16 {
    if width >= HEADER_CONTROLS_WIDTH {
        return 2;
    }
    let mut control_rows = 1u16;
    let mut used = 0u16;
    for (_, control_width) in HEADER_CONTROLS {
        let needed = control_width.saturating_add(u16::from(used > 0));
        if used > 0 && used.saturating_add(needed) > width {
            control_rows = control_rows.saturating_add(1);
            used = control_width;
        } else {
            used = used.saturating_add(needed);
        }
    }
    control_rows.saturating_add(1)
}

pub(crate) fn hit_areas(area: Rect, agy_row: bool) -> Vec<UsageHitArea> {
    let layout = layout(area, agy_row);
    let mut areas = header_hit_areas(layout.header);
    areas.extend(breakdown_hit_areas(layout.breakdown));
    areas
}

fn header_hit_areas(header: Rect) -> Vec<UsageHitArea> {
    let wraps = header.width < HEADER_CONTROLS_WIDTH;
    let mut x = if wraps {
        header.x
    } else {
        header
            .right()
            .saturating_sub(HEADER_CONTROLS_WIDTH)
            .max(header.x)
    };
    let mut y = if header.width < HEADER_SAME_ROW_MIN_WIDTH {
        header.y.saturating_add(1)
    } else {
        header.y
    };
    let mut areas = Vec::new();
    for (target, width) in HEADER_CONTROLS {
        if wraps && x > header.x && x.saturating_add(width) > header.right() {
            x = header.x;
            y = y.saturating_add(1);
        }
        if y >= header.bottom() {
            break;
        }
        let width = width.min(header.right().saturating_sub(x));
        if width > 0 {
            areas.push(UsageHitArea {
                target,
                rect: Rect::new(x, y, width, 1),
            });
        }
        x = x.saturating_add(width.saturating_add(1));
    }
    areas
}

fn breakdown_hit_areas(breakdown: Rect) -> [UsageHitArea; 2] {
    let model = Rect::new(
        breakdown.right().saturating_sub(15),
        breakdown.y,
        7.min(breakdown.width),
        1,
    );
    let day = Rect::new(
        model.right().saturating_add(1),
        breakdown.y,
        5.min(
            breakdown
                .right()
                .saturating_sub(model.right().saturating_add(1)),
        ),
        1,
    );
    [
        UsageHitArea {
            target: UsageHitTarget::Model,
            rect: model,
        },
        UsageHitArea {
            target: UsageHitTarget::Day,
            rect: day,
        },
    ]
}

pub(crate) fn render(app: &AppState, area: Rect, frame: &mut Frame) {
    let Some(state) = app.usage_view.as_ref() else {
        return;
    };
    let palette = &app.palette;
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Usage ")
        .border_style(Style::default().fg(palette.accent));
    frame.render_widget(block, area);
    let layout = layout(area, agy_has_data(&app.provider_usage));
    let now =
        app.status_now_unix
            .or_else(|| {
                state.snapshot.as_ref().and_then(|snapshot| {
                    snapshot.samples.iter().map(|sample| sample.timestamp).max()
                })
            })
            .unwrap_or_default();
    render_header(state, layout.header, now, palette, frame);
    render_subscription_usage(
        &app.provider_usage,
        now,
        layout.subscription,
        palette,
        frame,
    );
    let Some(snapshot) = state.snapshot.as_ref() else {
        frame.render_widget(
            Paragraph::new(if state.scanning {
                "scanning…"
            } else {
                "no usage data"
            })
            .style(Style::default().fg(palette.subtext0)),
            layout.summary,
        );
        render_footer(layout.footer, palette, frame);
        return;
    };
    let projection = project(snapshot, state, &app.usage_pricing, now);
    render_summary(&projection, state.metric, layout.summary, palette, frame);
    render_chart(&projection, state.metric, layout.chart, palette, frame);
    render_totals(&projection, layout.totals, palette, frame);
    render_breakdown(&projection, state, layout.breakdown, palette, frame);
    render_footer(layout.footer, palette, frame);
}

fn render_subscription_usage(
    snapshot: &ProviderUsageSnapshot,
    now: i64,
    area: Rect,
    palette: &Palette,
    frame: &mut Frame,
) {
    let agy = snapshot.primary_usage(crate::provider_usage::QuotaProvider::Agy);
    let providers = [
        (
            "Claude Code",
            snapshot.primary_usage(crate::provider_usage::QuotaProvider::Claude),
            palette.peach,
        ),
        (
            "Codex",
            snapshot.primary_usage(crate::provider_usage::QuotaProvider::Codex),
            palette.blue,
        ),
        (
            "Kimi",
            snapshot.primary_usage(crate::provider_usage::QuotaProvider::Kimi),
            palette.mauve,
        ),
        ("Antigravity", agy, palette.teal),
    ];
    // Antigravity only takes a row when it reports data, so the panel keeps its size otherwise.
    let wanted: usize = if agy_has_data(snapshot) { 4 } else { 3 };
    let shown = wanted.min(usize::from(area.height / 2)).max(1);
    let rows = Layout::vertical(&[Constraint::Length(2); 4][..shown]).split(area);
    for ((label, usage, color), provider_area) in
        providers.into_iter().take(shown).zip(rows.iter().copied())
    {
        let style = if usage.stale {
            Style::default()
                .fg(palette.overlay0)
                .add_modifier(Modifier::DIM)
        } else {
            Style::default().fg(palette.text)
        };
        let mut header = vec![Span::styled(
            label,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        )];
        if label == "Claude Code" {
            header.push(Span::raw("  "));
            header.push(Span::styled(claude_details_text(usage), style));
        } else if let Some(balance) = usage.credits {
            let label_width = super::text::display_width(label);
            let available = usize::from(provider_area.width).saturating_sub(label_width + 2);
            header.push(Span::raw("  "));
            header.push(Span::styled(credits_text(balance, available), style));
        }
        let windows = Line::from(vec![
            Span::styled(subscription_window_text("5h", usage.five_hour, now), style),
            Span::raw("  "),
            Span::styled(
                subscription_window_text("week", usage.seven_day, now),
                style,
            ),
        ]);
        frame.render_widget(
            Paragraph::new(vec![Line::from(header), windows]),
            provider_area,
        );
    }
}

fn claude_details_text(usage: &crate::provider_usage::AccountUsage) -> String {
    let cost = usage
        .cost_usd
        .map_or_else(|| "—".to_string(), |cost| format!("${cost:.2}"));
    let remaining = usage
        .remaining_minutes
        .map_or_else(|| "—".to_string(), |minutes| format!("{minutes}m left"));
    format!("cost: {cost} · {remaining}")
}

fn subscription_window_text(label: &str, window: Option<QuotaWindow>, now: i64) -> String {
    let Some(window) = window else {
        return format!("{label} — · —");
    };
    let reset = window
        .resets_at
        .and_then(|resets_at| crate::provider_usage::reset_label(resets_at, now))
        .unwrap_or_else(|| "—".to_string());
    format!("{label} {}% · {reset}", window.used_percent)
}

// Kept byte-for-byte with the deleted info panel's credit formatting behavior.
fn credits_text(balance: f64, width: usize) -> String {
    let full = format!("credits: {balance:.2}");
    if super::text::display_width(&full) <= width {
        return full;
    }

    let compact = format!("credits: {balance:.0}");
    if super::text::display_width(&compact) <= width {
        return compact;
    }

    "credits: —".to_string()
}

fn project(
    snapshot: &crate::provider_usage::UsageSnapshot,
    state: &UsageViewState,
    pricing: &UsageConfig,
    now: i64,
) -> UsageProjection {
    let end = now.saturating_add(1);
    let start = end.saturating_sub(state.range.seconds());
    let bucket_seconds = if state.range == UsageRange::Hours24 {
        60 * 60
    } else {
        24 * 60 * 60
    };
    let bucket_count = if state.range == UsageRange::Hours24 {
        24
    } else {
        usize::try_from(state.range.seconds() / bucket_seconds).unwrap_or_default()
    };
    let mut total = Totals::default();
    let mut session_ids = BTreeSet::new();
    let mut provider_totals: BTreeMap<UsageProvider, Totals> = BTreeMap::new();
    let mut provider_sessions: BTreeMap<UsageProvider, BTreeSet<String>> = BTreeMap::new();
    let mut breakdown: BTreeMap<String, Totals> = BTreeMap::new();
    let mut bars = vec![0.0_f64; bucket_count];
    let mut provider_bars: BTreeMap<UsageProvider, Vec<f64>> = BTreeMap::new();
    let mut cache_savings = 0.0;
    for sample in snapshot.samples_between(start, end) {
        let model_pricing = pricing_for(&sample.model, pricing);
        total.add(sample, model_pricing);
        session_ids.insert((sample.provider, sample.session_id.clone()));
        provider_totals
            .entry(sample.provider)
            .or_default()
            .add(sample, model_pricing);
        provider_sessions
            .entry(sample.provider)
            .or_default()
            .insert(sample.session_id.clone());
        let key = match state.breakdown {
            UsageBreakdown::Model => sample.model.clone(),
            UsageBreakdown::Day => format_day(sample.timestamp),
        };
        breakdown.entry(key).or_default().add(sample, model_pricing);
        let bucket = usize::try_from((sample.timestamp - start) / bucket_seconds)
            .unwrap_or_default()
            .min(bucket_count.saturating_sub(1));
        let bucket_value = match state.metric {
            UsageMetric::Cost => model_pricing.map_or(0.0, |price| sample_cost(sample, price)),
            UsageMetric::Tokens => sample.processed_tokens() as f64,
        };
        bars[bucket] += bucket_value;
        provider_bars
            .entry(sample.provider)
            .or_insert_with(|| vec![0.0; bucket_count])[bucket] += bucket_value;
        if let Some(price) = model_pricing {
            cache_savings +=
                sample.cache_read_tokens as f64 * (price.input - price.cache_read) / 1_000_000.0;
        }
    }
    let providers = provider_totals
        .into_iter()
        .map(|(provider, totals)| {
            let sessions = provider_sessions.get(&provider).map_or(0, BTreeSet::len);
            (provider, (totals, sessions))
        })
        .collect();
    let mut breakdown: Vec<_> = breakdown.into_iter().collect();
    breakdown.sort_by(|left, right| {
        right
            .1
            .known_cost
            .total_cmp(&left.1.known_cost)
            .then_with(|| right.1.tokens().cmp(&left.1.tokens()))
    });
    UsageProjection {
        now,
        start,
        total,
        sessions: session_ids.len(),
        providers,
        breakdown,
        bars,
        provider_bars,
        cache_savings,
    }
}

fn pricing_for(model: &str, config: &UsageConfig) -> Option<UsageModelPricing> {
    config.pricing.get(model).copied().or_else(|| {
        config
            .pricing
            .iter()
            .filter(|(prefix, _)| model.starts_with(prefix.as_str()))
            .max_by_key(|(prefix, _)| prefix.len())
            .map(|(_, pricing)| *pricing)
    })
}

fn sample_cost(sample: &UsageSample, pricing: UsageModelPricing) -> f64 {
    (sample.input_tokens as f64 * pricing.input
        + sample.output_tokens as f64 * pricing.output
        + sample.cache_write_tokens as f64 * pricing.cache_write
        + sample.cache_read_tokens as f64 * pricing.cache_read)
        / 1_000_000.0
}

fn render_header(
    state: &UsageViewState,
    area: Rect,
    now: i64,
    palette: &Palette,
    frame: &mut Frame,
) {
    let title = usage_header_title(state.range, now);
    frame.render_widget(
        Paragraph::new(title).style(
            Style::default()
                .fg(palette.text)
                .add_modifier(Modifier::BOLD),
        ),
        area,
    );
    for hit in header_hit_areas(area) {
        let (label, active) = match hit.target {
            UsageHitTarget::Cost => ("[Cost]", state.metric == UsageMetric::Cost),
            UsageHitTarget::Tokens => ("[Tokens]", state.metric == UsageMetric::Tokens),
            UsageHitTarget::Hours24 => ("[Past 24h]", state.range == UsageRange::Hours24),
            UsageHitTarget::Days7 => ("[7 days]", state.range == UsageRange::Days7),
            UsageHitTarget::Days30 => ("[30 days]", state.range == UsageRange::Days30),
            UsageHitTarget::Days90 => ("[90 days]", state.range == UsageRange::Days90),
            UsageHitTarget::Rescan => ("⟳", state.scanning),
            UsageHitTarget::Model | UsageHitTarget::Day => continue,
        };
        let style = if active {
            Style::default()
                .fg(palette.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette.subtext0)
        };
        frame.render_widget(Paragraph::new(label).style(style), hit.rect);
    }
}

fn usage_header_title(range: UsageRange, now: i64) -> String {
    let start = now.saturating_sub(range.seconds());
    if range == UsageRange::Hours24 {
        format!("Usage / {} to {}", format_hour(start), format_hour(now))
    } else {
        format!("Usage / {} to {}", format_day(start), format_day(now))
    }
}

fn render_summary(
    projection: &UsageProjection,
    metric: UsageMetric,
    area: Rect,
    palette: &Palette,
    frame: &mut Frame,
) {
    let total = match metric {
        UsageMetric::Cost => projection.total.cost_label(),
        UsageMetric::Tokens => format_tokens(projection.total.tokens()),
    };
    let mut lines = vec![
        Line::styled(
            total,
            Style::default()
                .fg(palette.text)
                .add_modifier(Modifier::BOLD),
        ),
        Line::styled(
            format!("{} sessions · API est.", projection.sessions),
            Style::default().fg(palette.subtext0),
        ),
    ];
    for provider in [UsageProvider::ClaudeCode, UsageProvider::Codex] {
        let (totals, sessions) = projection
            .providers
            .get(&provider)
            .copied()
            .unwrap_or_default();
        let share = if totals.priced_samples == 0 {
            "—".to_string()
        } else if projection.total.known_cost > 0.0 {
            let prefix = if totals.priced_samples < totals.samples {
                "~"
            } else {
                ""
            };
            format!(
                "{prefix}{:.0}%",
                totals.known_cost * 100.0 / projection.total.known_cost
            )
        } else {
            "0%".to_string()
        };
        lines.push(Line::from(vec![
            Span::styled("● ", Style::default().fg(provider_color(provider, palette))),
            Span::styled(
                format!(
                    "{} {sessions} sessions   {}",
                    provider_label(provider),
                    totals.cost_label()
                ),
                Style::default().fg(palette.text),
            ),
        ]));
        lines.push(Line::styled(
            format!(
                "{share} of cost · {} tokens",
                format_tokens(totals.tokens())
            ),
            Style::default().fg(palette.subtext0),
        ));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

fn render_chart(
    projection: &UsageProjection,
    metric: UsageMetric,
    area: Rect,
    palette: &Palette,
    frame: &mut Frame,
) {
    if area.height < 3 || area.width < 8 {
        return;
    }
    let hourly =
        projection.bars.len() == 24 && projection.now - projection.start <= 24 * 60 * 60 + 1;
    let title = match (metric, hourly) {
        (UsageMetric::Cost, true) => "Hourly cost",
        (UsageMetric::Tokens, true) => "Hourly tokens",
        (UsageMetric::Cost, false) => "Daily cost",
        (UsageMetric::Tokens, false) => "Daily tokens",
    };
    if projection.provider_bars.len() > 1
        && area.height
            >= u16::try_from(projection.provider_bars.len().saturating_add(3)).unwrap_or(u16::MAX)
    {
        render_provider_chart(projection, hourly, title, area, palette, frame);
        return;
    }

    let max_columns = usize::from(area.width);
    let visible = chart_values(&projection.bars, max_columns);
    let tallest = visible.iter().copied().fold(0.0_f64, f64::max);
    let bars: String = visible
        .iter()
        .map(|value| super::bar::bar_glyph(*value, tallest))
        .collect();
    let middle = projection
        .start
        .saturating_add((projection.now - projection.start) / 2);
    let minimum_label_width = if hourly { 17 } else { 30 };
    let label_width = if visible.len() >= minimum_label_width {
        u16::try_from(visible.len().saturating_add(2))
            .unwrap_or(area.width)
            .min(area.width)
    } else {
        area.width
    };
    let labels = if hourly {
        axis_labels(
            &format_hour(projection.start),
            &format_hour(middle),
            &format_hour(projection.now),
            label_width,
        )
    } else {
        axis_labels(
            &format_day(projection.start),
            &format_day(middle),
            &format_day(projection.now),
            label_width,
        )
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(
                title,
                Style::default()
                    .fg(palette.subtext0)
                    .add_modifier(Modifier::BOLD),
            ),
            Line::styled(bars, Style::default().fg(palette.accent)),
            Line::styled(labels, Style::default().fg(palette.subtext0)),
        ]),
        area,
    );
}

fn render_provider_chart(
    projection: &UsageProjection,
    hourly: bool,
    title: &str,
    area: Rect,
    palette: &Palette,
    frame: &mut Frame,
) {
    let chart_width = usize::from(area.width.saturating_sub(2));
    let visible = projection
        .provider_bars
        .iter()
        .map(|(provider, values)| (*provider, chart_values(values, chart_width)))
        .collect::<Vec<_>>();
    let tallest = visible
        .iter()
        .flat_map(|(_, values)| values.iter().copied())
        .fold(0.0_f64, f64::max);
    let legend = visible
        .iter()
        .enumerate()
        .flat_map(|(index, (provider, _))| {
            let separator = (index > 0).then(|| Span::raw("  "));
            separator.into_iter().chain(std::iter::once(Span::styled(
                format!(
                    "{} {}",
                    provider_series_marker(*provider),
                    provider_label(*provider)
                ),
                Style::default().fg(provider_color(*provider, palette)),
            )))
        })
        .collect::<Vec<_>>();
    let mut lines = vec![
        Line::styled(
            title,
            Style::default()
                .fg(palette.subtext0)
                .add_modifier(Modifier::BOLD),
        ),
        Line::from(legend),
    ];
    for (provider, values) in visible {
        let bars = values
            .iter()
            .map(|value| super::bar::bar_glyph(*value, tallest))
            .collect::<String>();
        lines.push(Line::from(vec![
            Span::styled(
                format!("{} ", provider_series_marker(provider)),
                Style::default().fg(provider_color(provider, palette)),
            ),
            Span::styled(bars, Style::default().fg(provider_color(provider, palette))),
        ]));
    }
    let middle = projection
        .start
        .saturating_add((projection.now - projection.start) / 2);
    let labels = if hourly {
        axis_labels(
            &format_hour(projection.start),
            &format_hour(middle),
            &format_hour(projection.now),
            u16::try_from(chart_width).unwrap_or(area.width),
        )
    } else {
        axis_labels(
            &format_day(projection.start),
            &format_day(middle),
            &format_day(projection.now),
            u16::try_from(chart_width).unwrap_or(area.width),
        )
    };
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(labels, Style::default().fg(palette.subtext0)),
    ]));
    frame.render_widget(Paragraph::new(lines), area);
}

fn render_totals(projection: &UsageProjection, area: Rect, palette: &Palette, frame: &mut Frame) {
    let cached = projection.total.cache_read;
    let uncached = projection
        .total
        .input
        .saturating_add(projection.total.cache_write);
    let lines = if area.height >= 3 {
        vec![
            Line::styled(
                format!(
                    "Totals  processed {} · cached {}",
                    format_tokens(projection.total.tokens()),
                    format_tokens(cached),
                ),
                Style::default().fg(palette.text),
            ),
            Line::styled(
                format!(
                    "        uncached {} · output {}",
                    format_tokens(uncached),
                    format_tokens(projection.total.output),
                ),
                Style::default().fg(palette.text),
            ),
            Line::styled(
                format!(
                    "        cache savings ${:.2}",
                    projection.cache_savings.max(0.0)
                ),
                Style::default().fg(palette.subtext0),
            ),
        ]
    } else {
        vec![
            Line::styled(
                format!(
                    "Totals  processed {} │ cached {} │ uncached {} │ output {}",
                    format_tokens(projection.total.tokens()),
                    format_tokens(cached),
                    format_tokens(uncached),
                    format_tokens(projection.total.output),
                ),
                Style::default().fg(palette.text),
            ),
            Line::styled(
                format!(
                    "        cache savings ${:.2}",
                    projection.cache_savings.max(0.0)
                ),
                Style::default().fg(palette.subtext0),
            ),
        ]
    };
    frame.render_widget(Paragraph::new(lines), area);
}

fn render_breakdown(
    projection: &UsageProjection,
    state: &UsageViewState,
    area: Rect,
    palette: &Palette,
    frame: &mut Frame,
) {
    let model_style = toggle_style(state.breakdown == UsageBreakdown::Model, palette);
    let day_style = toggle_style(state.breakdown == UsageBreakdown::Day, palette);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "Breakdown",
                Style::default()
                    .fg(palette.text)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
        ])),
        area,
    );
    for hit in breakdown_hit_areas(area) {
        match hit.target {
            UsageHitTarget::Model => {
                frame.render_widget(Paragraph::new("[Model]").style(model_style), hit.rect)
            }
            UsageHitTarget::Day => {
                frame.render_widget(Paragraph::new("[Day]").style(day_style), hit.rect)
            }
            _ => {}
        }
    }
    let width = usize::from(area.width);
    let name_width = width.saturating_sub(34).max(10);
    let mut lines = vec![Line::styled(
        format!(
            "{:<name_width$}  {:>10}  {:>7}  {:>10}",
            "", "cost", "share", "tokens"
        ),
        Style::default().fg(palette.subtext0),
    )];
    for (name, totals) in projection
        .breakdown
        .iter()
        .take(usize::from(area.height.saturating_sub(2)))
    {
        let share = if totals.priced_samples == 0 || projection.total.known_cost <= 0.0 {
            "—".to_string()
        } else {
            let prefix = if totals.priced_samples < totals.samples {
                "~"
            } else {
                ""
            };
            format!(
                "{prefix}{:.1}%",
                totals.known_cost * 100.0 / projection.total.known_cost
            )
        };
        lines.push(Line::styled(
            format!(
                "{:<name_width$}  {:>10}  {:>7}  {:>10}",
                truncate(name, name_width),
                format_cost(totals.cost()),
                share,
                format_tokens(totals.tokens()),
            ),
            Style::default().fg(palette.text),
        ));
    }
    frame.render_widget(
        Paragraph::new(lines),
        Rect::new(
            area.x,
            area.y.saturating_add(1),
            area.width,
            area.height.saturating_sub(1),
        ),
    );
}

fn render_footer(area: Rect, palette: &Palette, frame: &mut Frame) {
    let text = if area.height >= 2 {
        vec![
            Line::raw(" c/t cost/tokens   1/7/3/9 range"),
            Line::raw(" m/d breakdown   r rescan   Esc close"),
        ]
    } else {
        vec![Line::raw(
            " c/t cost/tokens   1/7/3/9 range   m/d breakdown   r rescan   Esc close",
        )]
    };
    frame.render_widget(
        Paragraph::new(text).style(Style::default().fg(palette.subtext0)),
        area,
    );
}

fn toggle_style(active: bool, palette: &Palette) -> Style {
    if active {
        Style::default()
            .fg(palette.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(palette.subtext0)
    }
}

fn provider_label(provider: UsageProvider) -> &'static str {
    provider.label()
}

fn provider_color(provider: UsageProvider, palette: &Palette) -> ratatui::style::Color {
    let colors = [palette.peach, palette.blue, palette.green, palette.mauve];
    colors[provider.series_index() % colors.len()]
}

fn provider_series_marker(provider: UsageProvider) -> char {
    const MARKERS: [char; 4] = ['◆', '●', '▲', '■'];
    MARKERS[provider.series_index() % MARKERS.len()]
}

fn format_cost(value: Option<f64>) -> String {
    value.map_or_else(|| "—".to_string(), |value| format!("${value:.2}"))
}

fn chart_values(values: &[f64], max_columns: usize) -> Vec<f64> {
    if values.len() <= max_columns {
        return values.to_vec();
    }
    let mut columns = vec![0.0; max_columns];
    for (index, value) in values.iter().enumerate() {
        let column = index.saturating_mul(max_columns) / values.len();
        columns[column.min(max_columns.saturating_sub(1))] += value;
    }
    columns
}

fn format_tokens(value: u64) -> String {
    match value {
        value if value >= 1_000_000_000 => format!("{:.2}B", value as f64 / 1_000_000_000.0),
        value if value >= 1_000_000 => format!("{:.2}M", value as f64 / 1_000_000.0),
        value if value >= 1_000 => format!("{:.1}K", value as f64 / 1_000.0),
        value => value.to_string(),
    }
}

fn axis_labels(start: &str, middle: &str, end: &str, width: u16) -> String {
    let width = usize::from(width);
    if width <= start.len().saturating_add(end.len()).saturating_add(1) {
        return truncate(&format!("{start} {end}"), width);
    }
    let mut output = vec![' '; width];
    place_label(&mut output, start, 0);
    place_label(&mut output, middle, width.saturating_sub(middle.len()) / 2);
    place_label(&mut output, end, width.saturating_sub(end.len()));
    output.into_iter().collect()
}

fn place_label(output: &mut [char], label: &str, start: usize) {
    for (index, character) in label.chars().enumerate() {
        if let Some(slot) = output.get_mut(start.saturating_add(index)) {
            *slot = character;
        }
    }
}

fn format_hour(timestamp: i64) -> String {
    let hour = timestamp.rem_euclid(24 * 60 * 60) / (60 * 60);
    format!("{hour:02}:00")
}

fn format_day(timestamp: i64) -> String {
    let (year, month, day) = civil_from_days(timestamp.div_euclid(24 * 60 * 60));
    format!("{year:04}-{month:02}-{day:02}")
}

// Howard Hinnant's civil-from-days algorithm, with Unix day zero as 1970-01-01.
fn civil_from_days(days_since_epoch: i64) -> (i64, i64, i64) {
    let days = days_since_epoch + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += if month <= 2 { 1 } else { 0 };
    (year, month, day)
}

fn truncate(value: &str, width: usize) -> String {
    let mut chars = value.chars();
    let mut result: String = chars.by_ref().take(width).collect();
    if chars.next().is_some() && width > 0 {
        result.pop();
        result.push('…');
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_usage::{
        AccountUsage, ProviderUsageSnapshot, QuotaWindow, UsageProvider, UsageSample, UsageSnapshot,
    };
    use ratatui::{backend::TestBackend, Terminal};

    fn fixture() -> UsageSnapshot {
        UsageSnapshot {
            samples: vec![
                UsageSample {
                    provider: UsageProvider::ClaudeCode,
                    session_id: "claude-session".into(),
                    timestamp: 1_787_992_800,
                    model: "claude-sonnet-5".into(),
                    input_tokens: 1_000_000,
                    output_tokens: 100_000,
                    cache_write_tokens: 200_000,
                    cache_read_tokens: 3_000_000,
                },
                UsageSample {
                    provider: UsageProvider::Codex,
                    session_id: "codex-session".into(),
                    timestamp: 1_787_906_400,
                    model: "unknown-local-model".into(),
                    input_tokens: 500_000,
                    output_tokens: 50_000,
                    cache_write_tokens: 0,
                    cache_read_tokens: 1_000_000,
                },
            ],
            files_seen: 2,
            files_read: 2,
        }
    }

    fn render_at(width: u16, height: u16) -> String {
        render_snapshot_at(width, height, fixture())
    }

    fn render_snapshot_at(width: u16, height: u16, snapshot: UsageSnapshot) -> String {
        render_snapshot_with_provider_usage_at(
            width,
            height,
            snapshot,
            ProviderUsageSnapshot::default(),
        )
    }

    fn render_snapshot_with_provider_usage_at(
        width: u16,
        height: u16,
        snapshot: UsageSnapshot,
        provider_usage: ProviderUsageSnapshot,
    ) -> String {
        let mut app = AppState::test_new();
        app.status_now_unix = Some(1_787_992_841);
        app.usage_snapshot = Some(snapshot);
        app.usage_view = Some(UsageViewState::new(app.usage_snapshot.clone()));
        app.provider_usage = provider_usage;
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| render(&app, frame.area(), frame))
            .expect("draw");
        terminal
            .backend()
            .buffer()
            .content()
            .chunks(usize::from(width))
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn populated_provider_snapshot_renders_subscription_windows_resets_and_credits() {
        let now = 1_787_992_841;
        let mut provider_usage = ProviderUsageSnapshot::with_primary_accounts(
            AccountUsage {
                five_hour: Some(QuotaWindow {
                    used_percent: 31,
                    resets_at: Some(now + 720),
                }),
                seven_day: Some(QuotaWindow {
                    used_percent: 52,
                    resets_at: Some(now + 9_900),
                }),
                ..AccountUsage::default()
            },
            AccountUsage {
                five_hour: Some(QuotaWindow {
                    used_percent: 6,
                    resets_at: Some(now + 720),
                }),
                seven_day: Some(QuotaWindow {
                    used_percent: 19,
                    resets_at: Some(now + 313_200),
                }),
                credits: Some(1_927.95),
                ..AccountUsage::default()
            },
            AccountUsage {
                five_hour: Some(QuotaWindow {
                    used_percent: 0,
                    resets_at: None,
                }),
                seven_day: Some(QuotaWindow {
                    used_percent: 24,
                    resets_at: Some(now + 9_900),
                }),
                ..AccountUsage::default()
            },
        );
        provider_usage
            .primary_usage_mut(crate::provider_usage::QuotaProvider::Agy)
            .five_hour = Some(QuotaWindow {
            used_percent: 1,
            resets_at: Some(now + 720),
        });

        let text =
            render_snapshot_with_provider_usage_at(120, 40, fixture(), provider_usage.clone());
        let narrow =
            render_snapshot_with_provider_usage_at(80, 24, fixture(), provider_usage.clone());
        for expected in ["Kimi", "Antigravity", "5h 1% · 12m"] {
            assert!(
                narrow.contains(expected),
                "missing {expected:?} at 80x24\n{narrow}"
            );
        }
        // Antigravity takes two chart rows on narrow screens; a slightly taller one keeps the legend.
        let taller = render_snapshot_with_provider_usage_at(80, 26, fixture(), provider_usage);
        for expected in ["Antigravity", "◆ Claude Code  ● Codex"] {
            assert!(
                taller.contains(expected),
                "missing {expected:?} at 80x26\n{taller}"
            );
        }
        for expected in [
            "Claude Code",
            "5h 31% · 12m",
            "week 52% · 2h45",
            "Codex",
            "5h 6% · 12m",
            "week 19% · 3d15h",
            "credits: 1927.95",
            "Kimi",
            "5h 0% · —",
            "week 24% · 2h45",
            "Antigravity",
            "5h 1% · 12m",
        ] {
            assert!(text.contains(expected), "missing {expected:?}\n{text}");
        }
    }

    #[test]
    fn claude_cost_and_remaining_minutes_render_or_keep_placeholders() {
        let now = 1_787_992_841;
        let claude = AccountUsage {
            five_hour: Some(QuotaWindow {
                used_percent: 31,
                resets_at: Some(now + 720),
            }),
            cost_usd: Some(56.68),
            remaining_minutes: Some(66),
            ..AccountUsage::default()
        };
        let text = render_snapshot_with_provider_usage_at(
            120,
            40,
            fixture(),
            ProviderUsageSnapshot::with_primary_accounts(
                claude,
                AccountUsage::default(),
                AccountUsage::default(),
            ),
        );
        assert!(text.contains("cost: $56.68 · 66m left"), "{text}");

        let text = render_snapshot_with_provider_usage_at(
            120,
            40,
            fixture(),
            ProviderUsageSnapshot::with_primary_accounts(
                AccountUsage {
                    five_hour: Some(QuotaWindow {
                        used_percent: 31,
                        resets_at: Some(now + 720),
                    }),
                    ..AccountUsage::default()
                },
                AccountUsage::default(),
                AccountUsage::default(),
            ),
        );
        assert!(text.contains("cost: — · —"), "{text}");
    }

    #[test]
    fn empty_provider_snapshot_keeps_all_providers_and_placeholder_windows_visible() {
        for (width, height) in [(120, 40), (56, 23)] {
            let text = render_snapshot_with_provider_usage_at(
                width,
                height,
                fixture(),
                ProviderUsageSnapshot::default(),
            );

            for provider in ["Claude Code", "Codex", "Kimi"] {
                assert!(
                    text.contains(provider),
                    "missing {provider:?} at {width}x{height}\n{text}"
                );
            }
            assert_eq!(
                text.matches("5h — · —").count(),
                3,
                "{width}x{height}\n{text}"
            );
            assert_eq!(
                text.matches("week — · —").count(),
                3,
                "{width}x{height}\n{text}"
            );
        }
    }

    #[test]
    fn usage_layout_places_chart_beside_wide_and_below_narrow_summary() {
        let wide = layout(Rect::new(0, 0, 120, 40), false);
        assert_eq!(wide.chart.y, wide.summary.y);
        let narrow = layout(Rect::new(0, 0, 80, 24), false);
        assert!(narrow.chart.y > narrow.summary.y);
    }

    #[test]
    fn usage_chart_scales_with_block_glyphs_and_three_axis_labels_at_both_widths() {
        for (width, height) in [(120, 40), (80, 24)] {
            let text = render_at(width, height);
            assert!(text.chars().any(crate::ui::bar::is_bar_glyph));
            assert!(
                text.matches("2026-").count() >= 3,
                "{width}x{height}\n{text}"
            );
            assert!(text.contains("unknown-local-model"));
            assert!(text.contains('—'));
        }
    }

    #[test]
    fn injected_records_render_t3_usage_header_at_both_sizes() {
        for (width, height) in [(120, 40), (80, 24)] {
            let text = render_at(width, height);
            assert!(
                text.contains("Usage / 2026-07-30 to 2026-08-29"),
                "missing date breadcrumb at {width}x{height}\n{text}"
            );
            for control in [
                "[Cost]",
                "[Tokens]",
                "[Past 24h]",
                "[7 days]",
                "[30 days]",
                "[90 days]",
                "⟳",
            ] {
                assert!(
                    text.contains(control),
                    "missing {control:?} at {width}x{height}\n{text}"
                );
            }
        }
    }

    #[test]
    fn injected_provider_records_render_t3_legend_lines() {
        let mut snapshot = fixture();
        snapshot.samples[1].model = "gpt-5.6-sol".into();
        let text = render_snapshot_at(120, 40, snapshot);
        for line in [
            "● Claude Code 1 sessions   $6.15",
            "83% of cost · 4.30M tokens",
            "● Codex 1 sessions   $1.25",
            "17% of cost · 1.55M tokens",
        ] {
            assert!(text.contains(line), "missing legend line {line:?}\n{text}");
        }
        let lines = text.lines().collect::<Vec<_>>();
        let claude = lines
            .iter()
            .position(|line| line.trim_start().starts_with("│● Claude Code"))
            .expect("Claude legend");
        let codex = lines
            .iter()
            .position(|line| line.trim_start().starts_with("│● Codex"))
            .expect("Codex legend");
        assert!(claude < codex, "provider order\n{text}");
    }

    #[test]
    fn usage_header_title_names_each_selected_range() {
        for (range, expected) in [
            (UsageRange::Hours24, "Usage / 08:00 to 08:00"),
            (UsageRange::Days7, "Usage / 2026-08-22 to 2026-08-29"),
            (UsageRange::Days30, "Usage / 2026-07-30 to 2026-08-29"),
            (UsageRange::Days90, "Usage / 2026-05-31 to 2026-08-29"),
        ] {
            assert_eq!(usage_header_title(range, 1_787_992_841), expected);
        }
    }

    #[test]
    fn two_provider_records_render_stacked_series_with_legend_without_a_pty() {
        for (width, height) in [(120, 40), (80, 24)] {
            let text = render_at(width, height);
            assert!(
                text.contains("◆ Claude Code  ● Codex"),
                "missing provider legend at {width}x{height}\n{text}"
            );
            assert!(
                text.lines().any(|line| line.contains("◆ ▁")),
                "missing Claude series at {width}x{height}\n{text}"
            );
            assert!(
                text.lines().any(|line| line.contains("● ▁")),
                "missing Codex series at {width}x{height}\n{text}"
            );
        }
    }

    #[test]
    fn newly_collected_provider_record_gets_its_own_chart_series_without_a_pty() {
        let mut snapshot = fixture();
        snapshot.samples.push(UsageSample {
            provider: UsageProvider::TestCollected,
            session_id: "test-collected-session".into(),
            timestamp: 1_787_820_000,
            model: "test-collected-model".into(),
            input_tokens: 250_000,
            output_tokens: 25_000,
            cache_write_tokens: 0,
            cache_read_tokens: 500_000,
        });

        let text = render_snapshot_at(120, 40, snapshot);
        assert!(
            text.contains("◆ Claude Code  ● Codex  ▲ Test Collected"),
            "missing dynamic provider legend\n{text}"
        );
        assert!(
            text.lines().any(|line| line.contains("▲ ▁")),
            "missing new provider series\n{text}"
        );
    }

    #[test]
    fn one_provider_record_keeps_aggregate_chart_fallback_without_a_pty() {
        let mut snapshot = fixture();
        snapshot
            .samples
            .retain(|sample| sample.provider == UsageProvider::ClaudeCode);
        let text = render_snapshot_at(120, 40, snapshot);
        assert!(text.contains("Daily cost"), "{text}");
        assert!(text.chars().any(crate::ui::bar::is_bar_glyph), "{text}");
        assert!(!text.contains("◆ Claude Code  ● Codex"), "{text}");
        assert!(text.matches("2026-").count() >= 3, "{text}");
    }

    #[test]
    fn narrow_totals_and_footer_keep_every_required_label() {
        let text = render_at(56, 23);
        for label in [
            "processed",
            "cached",
            "uncached",
            "output",
            "cache savings",
            "m/d breakdown",
            "r rescan",
            "Esc close",
        ] {
            assert!(text.contains(label), "missing {label:?}\n{text}");
        }
    }

    #[test]
    fn chart_condenses_overflow_without_dropping_early_buckets() {
        let mut values = vec![0.0; 90];
        values[0] = 1.0;
        values[89] = 2.0;
        let columns = chart_values(&values, 45);
        assert_eq!(columns.len(), 45);
        assert_eq!(columns[0], 1.0);
        assert_eq!(columns[44], 2.0);
    }

    #[test]
    fn mixed_pricing_is_rendered_as_a_lower_bound() {
        let text = render_at(120, 40);
        assert!(text.contains("≥$6.15"), "{text}");
        assert!(text.contains("unknown-local-model"));
        assert!(text.contains("—"));
    }

    #[test]
    fn every_usage_toggle_has_a_nonempty_mouse_target() {
        let targets: BTreeSet<_> = hit_areas(Rect::new(0, 0, 120, 40), false)
            .into_iter()
            .filter(|hit| hit.rect.width > 0 && hit.rect.height > 0)
            .map(|hit| hit.target)
            .collect();
        assert_eq!(targets.len(), 9);
    }

    #[test]
    fn range_metric_and_breakdown_projection_uses_requested_buckets() {
        let snapshot = fixture();
        let mut state = UsageViewState::new(Some(snapshot.clone()));
        state.range = UsageRange::Hours24;
        state.metric = UsageMetric::Tokens;
        state.breakdown = UsageBreakdown::Day;
        let projected = project(&snapshot, &state, &UsageConfig::default(), 1_787_992_841);
        assert_eq!(projected.bars.len(), 24);
        assert_eq!(projected.sessions, 1);
        assert_eq!(projected.breakdown.len(), 1);
    }

    #[test]
    fn usage_projection_aggregates_provider_model_day_and_all_pricing_buckets() {
        let snapshot = fixture();
        let state = UsageViewState::new(Some(snapshot.clone()));
        let config = UsageConfig::default();
        let projected = project(&snapshot, &state, &config, 1_787_992_841);
        assert_eq!(projected.sessions, 2);
        assert_eq!(projected.providers.len(), 2);
        assert_eq!(projected.breakdown.len(), 2);
        let claude = projected
            .providers
            .get(&UsageProvider::ClaudeCode)
            .expect("Claude aggregate")
            .0;
        let expected = 1.0 * 3.0 + 0.1 * 15.0 + 0.2 * 3.75 + 3.0 * 0.30;
        assert!((claude.known_cost - expected).abs() < 0.000_001);
        let unknown = projected
            .breakdown
            .iter()
            .find(|(model, _)| model == "unknown-local-model")
            .map(|(_, totals)| *totals)
            .expect("unknown model row");
        assert_eq!(unknown.cost(), None);
        assert!(unknown.tokens() > 0);

        let mut by_day = state;
        by_day.breakdown = UsageBreakdown::Day;
        let projected = project(&snapshot, &by_day, &config, 1_787_992_841);
        assert_eq!(projected.breakdown.len(), 2);
    }

    #[test]
    fn every_range_allocates_one_time_bucket_per_hour_or_day() {
        let snapshot = fixture();
        for (range, expected) in [
            (UsageRange::Hours24, 24),
            (UsageRange::Days7, 7),
            (UsageRange::Days30, 30),
            (UsageRange::Days90, 90),
        ] {
            let mut state = UsageViewState::new(Some(snapshot.clone()));
            state.range = range;
            assert_eq!(
                project(&snapshot, &state, &UsageConfig::default(), 1_787_992_841)
                    .bars
                    .len(),
                expected
            );
        }
    }

    #[test]
    fn first_empty_cache_render_says_scanning() {
        let mut app = AppState::test_new();
        app.usage_view = Some(UsageViewState::new(None));
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| render(&app, frame.area(), frame))
            .expect("draw");
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("scanning…"));
        assert!(text.contains('⟳'));
    }
}
