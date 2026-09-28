use ratatui::{
    layout::{Alignment, Constraint, Layout, Margin, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::app::{
    home::{HomeCounts, HomeFocus, HomePicker, HomeState, HomeTarget},
    inbox::BlockedAgent,
    state::{HomeHitArea, HomeHitTarget},
    AppState,
};
use crate::ui::dropdown::{
    hit_test as dropdown_hit_test, layout_dropdown, DropdownLayout, DropdownSpec,
};
use crate::ui::text::{display_width, display_width_u16, truncate_end};

/// How long this agent has been waiting, in the sidebar's own age vocabulary.
fn waited_label(agent: &BlockedAgent) -> String {
    match agent.blocked_since {
        Some(since) => crate::activity_age::coarse_label(Some(since), std::time::Instant::now()),
        // The transition was never observed. Saying so beats inventing a duration.
        None => "—".to_string(),
    }
}

/// Red blockers and lower-tier attention waits on the left, fleet size right.
///
/// Blocked leads with the strongest marker. Lower-tier attention follows in
/// amber so the two kinds remain distinct at a glance.
fn header_line(app: &AppState, counts: HomeCounts, width: u16) -> Line<'static> {
    let blocked = format!(" ● {} blocked", counts.blocked);
    let attention = format!(" · {} attention", counts.attention);
    let right = format!("{} agents · {} spaces ", counts.agents, counts.spaces,);
    let show_attention =
        display_width(&blocked) + display_width(&attention) + display_width(&right)
            <= width as usize;
    let attention_width = if show_attention {
        display_width(&attention)
    } else {
        0
    };
    let gap = (width as usize)
        .saturating_sub(display_width(&blocked) + attention_width + display_width(&right));
    let mut spans = vec![Span::styled(
        blocked,
        Style::default()
            // The palette reserves `red` for needs-attention/blocked, and
            // the sidebar already says blocked in it. Accent is the generic
            // highlight colour and read as "selected", not "waiting".
            .fg(if counts.blocked > 0 {
                app.palette.red
            } else {
                app.palette.overlay0
            })
            .add_modifier(Modifier::BOLD),
    )];
    if show_attention {
        spans.push(Span::styled(
            attention,
            Style::default()
                .fg(if counts.attention > 0 {
                    app.palette.peach
                } else {
                    app.palette.overlay0
                })
                .add_modifier(Modifier::BOLD),
        ));
    }
    spans.push(Span::raw(" ".repeat(gap)));
    spans.push(Span::styled(
        right,
        Style::default().fg(app.palette.overlay0),
    ));
    Line::from(spans)
}

/// Narrowest title fragment worth keeping the provider cell for.
const MIN_ROW_TITLE_WIDTH: usize = 6;

/// `○ workspace        session title                 cc >_  18m`
///
/// Dot, title and provider come from the sidebar row for the same pane, so the
/// two lists read the same way. A pane the sidebar cannot resolve keeps the
/// queue's own label and the empty waiting dot.
fn agent_line(
    app: &AppState,
    agent: &BlockedAgent,
    cells: Option<&crate::ui::sidebar::AgentRowCells>,
    selected: bool,
    width: u16,
) -> Line<'static> {
    let attention_color = match agent.attention_tier {
        crate::terminal::state::AttentionTier::Blocked => app.palette.red,
        crate::terminal::state::AttentionTier::Attention => app.palette.peach,
        crate::terminal::state::AttentionTier::None => app.palette.subtext0,
    };
    let dot = cells.map_or("○", |cells| cells.dot.as_str());
    let dot_color = cells.map_or(attention_color, |cells| cells.dot_color);
    let title = cells.map_or(agent.agent_label.as_str(), |cells| cells.title.as_str());
    let provider = cells.map_or("", |cells| cells.provider.as_str());
    let provider_color = cells.map_or(app.palette.overlay0, |cells| cells.provider_color);

    let lead = format!(" {dot} ");
    if (width as usize) <= display_width(&lead) {
        return Line::from(Span::styled(
            truncate_end(&lead, width as usize),
            Style::default().fg(dot_color),
        ));
    }
    let age = waited_label(agent);
    let available = (width as usize).saturating_sub(display_width(&lead) + 1);
    // The provider's own name, without subagent counts or the background-shell
    // marker, which yield before it does.
    let bare_provider = provider.split(['+', ' ']).next().unwrap_or_default();
    // The repo row contract keeps, in order, the dot, a title fragment and the
    // provider; workspace, shell details and age give way first on a narrow Home.
    let (label_width, provider_text, show_age) = [
        (true, provider, true),
        (false, provider, true),
        (false, bare_provider, true),
        (false, bare_provider, false),
    ]
    .into_iter()
    .find_map(|(workspace, provider_text, age_shown)| {
        let provider_width = if provider_text.is_empty() {
            0
        } else {
            display_width(provider_text) + 2
        };
        let age_width = if age_shown {
            display_width(&age) + 1
        } else {
            0
        };
        let rest = available.saturating_sub(provider_width + age_width);
        let label_width = if workspace { (rest / 3).min(16) } else { 0 };
        let title_width = rest.saturating_sub(if workspace { label_width + 1 } else { 0 });
        (title_width >= MIN_ROW_TITLE_WIDTH).then_some((label_width, provider_text, age_shown))
    })
    .unwrap_or((0, "", false));
    let provider_cell = if provider_text.is_empty() {
        String::new()
    } else {
        format!(" {provider_text} ")
    };
    let age_cell = if show_age {
        format!(" {age}")
    } else {
        String::new()
    };
    let workspace = truncate_end(&agent.workspace_label, label_width);
    let workspace_pad = label_width.saturating_sub(display_width(&workspace));
    let workspace_cell = if label_width == 0 {
        String::new()
    } else {
        format!("{workspace}{} ", " ".repeat(workspace_pad))
    };
    let title_width = available.saturating_sub(
        display_width(&workspace_cell) + display_width(&provider_cell) + display_width(&age_cell),
    );
    // Like the sidebar, a PR or ticket prefix goes before the title it names.
    let title = if display_width(title) > title_width {
        crate::ui::sidebar::title_without_object_identifier(title).unwrap_or(title)
    } else {
        title
    };
    let title = truncate_end(title, title_width);
    let title_pad = title_width.saturating_sub(display_width(&title));
    // A filled row is the cursor, the same surface the sidebar uses.
    let base = if selected {
        Style::default().bg(app.palette.surface0)
    } else {
        Style::default()
    };
    let text_style = if selected {
        base.fg(app.palette.text).add_modifier(Modifier::BOLD)
    } else {
        base.fg(attention_color)
    };
    let age_style = base.fg(if selected {
        app.palette.overlay1
    } else {
        app.palette.overlay0
    });
    Line::from(vec![
        Span::styled(" ", base),
        Span::styled(
            dot.to_string(),
            base.fg(dot_color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" ", base),
        Span::styled(workspace_cell, text_style),
        Span::styled(format!("{title}{}", " ".repeat(title_pad)), text_style),
        Span::styled(provider_cell, base.fg(provider_color)),
        Span::styled(format!("{age_cell} "), age_style),
    ])
}

fn truncate(s: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if s.chars().count() <= width {
        return s.to_string();
    }
    let mut out: String = s.chars().take(width.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn empty_line(app: &AppState) -> Line<'static> {
    Line::from(Span::styled(
        " nothing is waiting on you",
        Style::default()
            .fg(app.palette.overlay0)
            .add_modifier(Modifier::DIM),
    ))
}

fn hint_line(
    app: &AppState,
    hidden_above: usize,
    hidden_below: usize,
    composer_visible: bool,
    lens_visible: bool,
) -> Line<'static> {
    let mut hint = if composer_visible {
        " ↑↓ browse · tab focus · ⏎ dispatch · esc back".to_string()
    } else if lens_visible {
        " ↑↓ browse · tab reply · ⏎ jump · d detach · esc closes".to_string()
    } else {
        " ↑↓ browse · ⏎ jump · esc closes".to_string()
    };
    // Only mention what is off-screen when something is, so a list that fits
    // carries no chrome about scrolling.
    if hidden_above + hidden_below > 0 {
        hint.push_str(&format!(" · {} more", hidden_above + hidden_below));
    }
    Line::from(Span::styled(
        hint,
        Style::default()
            .fg(app.palette.overlay0)
            .add_modifier(Modifier::DIM),
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ComposerBands {
    /// The centred headline above the card.
    headline: Rect,
    /// The card drawn around the composer, border included.
    frame: Rect,
    prompt: Rect,
    /// Agent, model, effort and access, with the submit glyph carved off the right.
    chips: Rect,
    submit: Rect,
    /// The rule between the pickers and the workspace row, drawn across the
    /// card so it meets the border on both sides.
    divider: Rect,
    /// Workspace on the left, ref on the right.
    bottom: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LensBands {
    frame: Rect,
    title: Rect,
    output: Rect,
    reply: Rect,
    new_task: Rect,
    detach: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HomeBands {
    header: Rect,
    gap: Rect,
    body: Rect,
    composer: Option<ComposerBands>,
    lens: Option<LensBands>,
    hint: Rect,
}

/// The bands home draws into: header, gap, queue, composer, hint.
///
/// Shared with hit-testing so a click can never land on a row the renderer put
/// somewhere else.
/// The composer card has two border rows and three control rows around its
/// prompt. Short frames fall back to the previous one-row prompt.
const NORMAL_COMPOSER_CARD_ROWS: u16 = 11;
const COMPACT_COMPOSER_CARD_ROWS: u16 = 6;
const TALL_COMPOSER_PROMPT_ROWS: u16 = 15;
const COMPOSER_NON_PROMPT_ROWS: u16 = 5;
const TALL_COMPOSER_CARD_ROWS: u16 = TALL_COMPOSER_PROMPT_ROWS + COMPOSER_NON_PROMPT_ROWS;
/// The card is a reading surface, not a pane: past this it stops being one
/// glance and the headline drifts away from the prompt it introduces.
/// The composer card clamps to this width and centres in whatever is left.
///
/// Wide enough that the chip row still reads as one line once the provider,
/// model, effort, access, project and repository chips are all present; a
/// narrower terminal simply gets the full width it has.
const HOME_CARD_MAX_WIDTH: u16 = 96;
/// `[ ↵ ]`, right-aligned on the picker row.
const SUBMIT_GLYPH: &str = "[ ↵ ]";

/// Rows the queue needs, clamped so the trailing bands always fit.
fn body_rows(area: Rect, queue_rows: usize, trailing: u16) -> u16 {
    let available = area
        .height
        .saturating_sub(2)
        .saturating_sub(trailing)
        .max(1);
    // At least one row, so the "nothing is waiting on you" line keeps its place.
    u16::try_from(queue_rows.max(1))
        .unwrap_or(u16::MAX)
        .clamp(1, available)
}

/// Return the card and prompt heights that fit while leaving one queue row and
/// the headline, header, gap, and hint in place.
fn composer_dimensions(area_height: u16, picker_clearance: u16) -> (u16, u16) {
    let available_card_rows = area_height.saturating_sub(COMPOSER_NON_PROMPT_ROWS);
    let card_rows = if available_card_rows < NORMAL_COMPOSER_CARD_ROWS {
        COMPACT_COMPOSER_CARD_ROWS
    } else {
        available_card_rows.min(TALL_COMPOSER_CARD_ROWS)
    }
    .saturating_sub(picker_clearance)
    .max(COMPACT_COMPOSER_CARD_ROWS);
    (
        card_rows,
        card_rows.saturating_sub(COMPOSER_NON_PROMPT_ROWS).max(1),
    )
}

fn bands_for(
    area: Rect,
    lens_requested: bool,
    queue_rows: usize,
    picker_clearance: u16,
) -> HomeBands {
    if lens_requested && area.height >= crate::app::home::HOME_LENS_MIN_HEIGHT {
        let [header, gap, body, frame, _slack, hint] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(body_rows(area, queue_rows, 7)),
            Constraint::Length(6),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .areas(area);
        let inner = frame.inner(Margin::new(1, 1));
        let [title, output, reply] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .areas(inner);
        let detach_width = 12.min(inner.width);
        let detach = Rect::new(
            inner.right().saturating_sub(detach_width),
            title.y,
            detach_width,
            title.height,
        );
        let action_gap = u16::from(inner.width > detach_width);
        let new_task_width = 12.min(
            inner
                .width
                .saturating_sub(detach_width)
                .saturating_sub(action_gap),
        );
        let new_task = Rect::new(
            detach.x.saturating_sub(action_gap + new_task_width),
            title.y,
            new_task_width,
            title.height,
        );
        let actions_width = detach_width
            .saturating_add(action_gap)
            .saturating_add(new_task_width);
        let title = Rect::new(
            title.x,
            title.y,
            title.width.saturating_sub(actions_width.saturating_add(1)),
            title.height,
        );
        HomeBands {
            header,
            gap,
            body,
            composer: None,
            lens: Some(LensBands {
                frame,
                title,
                output,
                reply,
                new_task,
                detach,
            }),
            hint,
        }
    } else if !lens_requested && area.height >= crate::app::home::HOME_COMPOSER_MIN_HEIGHT {
        let (card_rows, prompt_rows) = composer_dimensions(area.height, picker_clearance);
        // The composer sits directly under the queue; the slack goes below it,
        // so a short queue no longer strands the prompt at the pane floor.
        let [header, gap, body, headline, card_row, _slack, hint] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(body_rows(area, queue_rows, card_rows + 2)),
            Constraint::Length(1),
            Constraint::Length(card_rows),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .areas(area);
        // The card is centred rather than stretched: the prompt is one column
        // of text, and a full-width box puts its pickers at opposite ends of a
        // wide pane.
        let card_width = area.width.min(HOME_CARD_MAX_WIDTH);
        let frame = Rect::new(
            area.x + (area.width.saturating_sub(card_width)) / 2,
            card_row.y,
            card_width,
            card_row.height,
        );
        let inner = frame.inner(Margin::new(1, 1));
        let [prompt, chips_row, divider_row, bottom] = Layout::vertical([
            Constraint::Length(prompt_rows),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(inner);
        let submit_width = display_width_u16(SUBMIT_GLYPH).min(chips_row.width);
        let submit = Rect::new(
            chips_row.right().saturating_sub(submit_width),
            chips_row.y,
            submit_width,
            chips_row.height,
        );
        let chips = Rect::new(
            chips_row.x,
            chips_row.y,
            chips_row
                .width
                .saturating_sub(submit_width.saturating_add(u16::from(submit_width > 0))),
            chips_row.height,
        );
        // The rule spans the card, not the inner area, so it lands on the
        // border cells the block already drew.
        let divider = Rect::new(frame.x, divider_row.y, frame.width, 1);
        HomeBands {
            header,
            gap,
            body,
            composer: Some(ComposerBands {
                headline,
                frame,
                prompt,
                chips,
                submit,
                divider,
                bottom,
            }),
            lens: None,
            hint,
        }
    } else {
        let [header, gap, body, hint] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .areas(area);
        HomeBands {
            header,
            gap,
            body,
            composer: None,
            lens: None,
            hint,
        }
    }
}

/// Composer bands for a chip-row test: the bottom row is wide enough that the
/// start-in target keeps its place there and the chips stay on the picker row.
#[cfg(test)]
fn chip_row_bands(chips: Rect) -> ComposerBands {
    ComposerBands {
        headline: Rect::new(chips.x, chips.y, chips.width, 1),
        frame: chips,
        prompt: chips,
        chips,
        submit: Rect::new(chips.right(), chips.y, 0, 1),
        divider: chips,
        bottom: Rect::new(chips.x, chips.y, u16::MAX / 2, 1),
    }
}

#[cfg(test)]
fn bands(area: Rect, queue_rows: usize) -> HomeBands {
    bands_for(area, false, queue_rows, 0)
}

fn lens_requested(app: &AppState, queue: &[BlockedAgent]) -> bool {
    app.home.as_ref().is_some_and(|home| {
        !queue.is_empty()
            && home.current(queue).is_some()
            && matches!(home.focus, None | Some(HomeFocus::Reply))
    })
}

fn home_bands(app: &AppState, queue: &[BlockedAgent], area: Rect) -> HomeBands {
    let lens_requested = lens_requested(app, queue);
    let layout = bands_for(area, lens_requested, queue.len(), 0);
    let Some(home) = app.home.as_ref() else {
        return layout;
    };
    let Some(composer) = layout.composer else {
        return layout;
    };
    if home.picker.is_none() {
        return layout;
    }
    let Some(field) = home
        .focus
        .and_then(|focus| composer_field_rect(app, home, composer, focus))
    else {
        return layout;
    };
    if field.y != composer.bottom.y || picker_layout(app, home, composer, area).is_some() {
        return layout;
    }

    // A filtered picker anchored to the bottom row needs one more row than the
    // card's bottom border provides: its filter plus at least one result.
    let adjusted = bands_for(area, lens_requested, queue.len(), 1);
    let Some(composer) = adjusted.composer else {
        return layout;
    };
    if picker_layout(app, home, composer, area).is_some() {
        adjusted
    } else {
        layout
    }
}

fn chip_specs(
    app: &AppState,
    home: &HomeState,
    composer: ComposerBands,
) -> Vec<(HomeFocus, String)> {
    let mut specs = vec![
        (HomeFocus::Agent, format!("{} ▾", home.profile().label)),
        (HomeFocus::Model, format!("{} ▾", home.model_display_name())),
    ];
    if let Some(effort) = &home.effort {
        specs.push((HomeFocus::Effort, format!("{effort} ▾")));
    }
    if let Some(access) = home.access {
        specs.push((HomeFocus::Access, format!("{} ▾", access.label())));
    }
    if let Some(context) = &home.context_window {
        specs.push((HomeFocus::Context, format!("{context} ▾")));
    }
    if home.project_visible() {
        if let Some(project) = home.project() {
            specs.push((HomeFocus::Project, format!("{} ▾", project.label)));
        }
    }
    if home.repo_visible() {
        specs.push((HomeFocus::Repo, format!("{} ▾", home.repo_label())));
    }
    if home.machine_visible() {
        if let Some(machine) = home.machine() {
            specs.push((HomeFocus::Machine, format!("{} ▾", machine.name)));
        }
    }
    if target_in_chip_row(app, home, composer) {
        specs.push((HomeFocus::Target, target_chip_label(app, home)));
    }
    specs
}

fn picker_row_label(label: &str, width: usize) -> String {
    if display_width(label) <= width {
        return label.to_string();
    }
    let compact = label.strip_prefix("Claude ").unwrap_or(label);
    if display_width(compact) <= width {
        return compact.to_string();
    }
    if width == 0 {
        return String::new();
    }

    let (body, suffix) = compact
        .strip_suffix(" ▾")
        .map(|body| (body, " ▾"))
        .unwrap_or((compact, ""));
    let suffix_width = display_width(suffix);
    if width <= suffix_width + 1 {
        return "…".to_string();
    }
    let words_width = width - suffix_width - 1;
    let mut words = String::new();
    for word in body.split_whitespace() {
        let separator = usize::from(!words.is_empty());
        if display_width(&words) + separator + display_width(word) > words_width {
            break;
        }
        if separator > 0 {
            words.push(' ');
        }
        words.push_str(word);
    }
    if words.is_empty() {
        format!("…{suffix}")
    } else {
        format!("{words}…{suffix}")
    }
}

fn chip_rects(app: &AppState, home: &HomeState, composer: ComposerBands) -> Vec<(HomeFocus, Rect)> {
    let area = composer.chips;
    let specs = chip_specs(app, home, composer);
    let preferred_widths = specs
        .iter()
        .map(|(_, label)| display_width(label))
        .collect::<Vec<_>>();
    let minimum_widths = specs
        .iter()
        .map(|(focus, _)| match focus {
            HomeFocus::Agent
            | HomeFocus::Model
            | HomeFocus::Effort
            | HomeFocus::Access
            | HomeFocus::Context
            | HomeFocus::Target => MINIMUM_CHIP_WIDTH,
            _ => 0,
        })
        .collect::<Vec<_>>();
    let gaps = specs.len().saturating_sub(1);
    let preferred_total = preferred_widths.iter().sum::<usize>();
    let gap = if preferred_total.saturating_add(gaps.saturating_mul(3)) <= area.width as usize {
        3
    } else {
        1
    };
    let mut widths = preferred_widths.clone();
    let available = (area.width as usize).saturating_sub(gaps.saturating_mul(gap));
    if preferred_total > available {
        widths = minimum_widths
            .iter()
            .scan(available, |remaining, minimum| {
                let width = (*minimum).min(*remaining);
                *remaining = remaining.saturating_sub(width);
                Some(width)
            })
            .collect();
        let mut remaining = available.saturating_sub(widths.iter().sum::<usize>());
        while remaining > 0 {
            let mut grew = false;
            for (width, preferred) in widths.iter_mut().zip(&preferred_widths) {
                if *width < *preferred && remaining > 0 {
                    *width += 1;
                    remaining -= 1;
                    grew = true;
                }
            }
            if !grew {
                break;
            }
        }
    }
    let mut x = area.x;
    let right = area.right();
    specs
        .into_iter()
        .zip(widths)
        .filter_map(|((focus, _), requested_width)| {
            if x >= right {
                return None;
            }
            let width = u16::try_from(requested_width)
                .unwrap_or(u16::MAX)
                .min(right - x);
            if width == 0 {
                return None;
            }
            let rect = Rect::new(x, area.y, width, area.height);
            x = x.saturating_add(width).saturating_add(gap as u16);
            Some((focus, rect))
        })
        .collect()
}

/// The bottom-row fields, outermost first on each side.
///
/// Workspace and ref are the two the design names and keep the row's ends; the
/// start-in target sits between them while it fits and moves up to the picker
/// row when it does not, so neither named field is squeezed for it.
fn secondary_specs(
    app: &AppState,
    home: &HomeState,
    composer: ComposerBands,
) -> Vec<(HomeFocus, String)> {
    let mut specs = vec![(HomeFocus::Workspace, workspace_label(home))];
    if !target_in_chip_row(app, home, composer) {
        specs.push((HomeFocus::Target, target_chip_label(app, home)));
    }
    specs.push((HomeFocus::Ref, pr_ref_label(app, home)));
    specs
}

fn target_chip_label(app: &AppState, home: &HomeState) -> String {
    format!("{} ▾", target_label(app, &home.target))
}

/// Gap between the bottom-row fields.
const BOTTOM_ROW_GAP: usize = 2;

/// `… ▾`: the narrowest a picker chip can be and still read as one.
const MINIMUM_CHIP_WIDTH: usize = 4;

/// Whether the target has to give up its place between workspace and ref.
///
/// It only moves when the picker row can actually hold it: a row too narrow for
/// a fourth chip would leave the field with a single cell, which is worse than
/// an elided label between the two fields the design names.
fn target_in_chip_row(app: &AppState, home: &HomeState, composer: ComposerBands) -> bool {
    let wanted = display_width(&workspace_label(home))
        + display_width(&target_chip_label(app, home))
        + display_width(&pr_ref_label(app, home))
        + BOTTOM_ROW_GAP * 2;
    if wanted <= composer.bottom.width as usize {
        return false;
    }
    let chips = 2
        + usize::from(home.effort.is_some())
        + usize::from(home.access.is_some())
        + usize::from(home.context_window.is_some());
    let minimum = (chips + 1) * MINIMUM_CHIP_WIDTH + chips;
    minimum <= composer.chips.width as usize
}

fn pr_ref_label(app: &AppState, home: &HomeState) -> String {
    match home.pr.as_ref() {
        Some(pr) => format!("⑂ #{} · ⎇ {} ▾", pr.number, ref_label(app)),
        None => format!("⎇ {} ▾", ref_label(app)),
    }
}

fn workspace_label(home: &HomeState) -> String {
    if home.pending_dispatch.is_some() {
        "creating worktree…".into()
    } else {
        format!("{} ▾", home.workspace.label())
    }
}

fn ref_label(app: &AppState) -> String {
    app.home_ref_label()
}

fn secondary_rects(
    app: &AppState,
    home: &HomeState,
    composer: ComposerBands,
) -> Vec<(HomeFocus, Rect)> {
    let area = composer.bottom;
    if area.width == 0 {
        return Vec::new();
    }
    let specs = secondary_specs(app, home, composer);
    let mut widths = specs
        .iter()
        .map(|(_, label)| display_width(label))
        .collect::<Vec<_>>();
    let gaps = specs.len().saturating_sub(1) * BOTTOM_ROW_GAP;
    let mut total = widths.iter().sum::<usize>() + gaps;
    // Shrink from the middle outwards: the two fields the design names lose
    // their labels last.
    let shrink_order = if specs.len() == 3 {
        vec![1usize, 2, 0]
    } else {
        vec![1usize, 0]
    };
    for index in shrink_order {
        if total <= area.width as usize {
            break;
        }
        let Some(width) = widths.get_mut(index) else {
            continue;
        };
        let taken = (total - area.width as usize).min(*width);
        *width -= taken;
        total -= taken;
    }
    let widths = widths
        .into_iter()
        .map(|width| u16::try_from(width).unwrap_or(u16::MAX))
        .collect::<Vec<_>>();
    let last = specs.len() - 1;
    let mut rects = Vec::with_capacity(specs.len());
    if widths[0] > 0 {
        rects.push((
            specs[0].0,
            Rect::new(area.x, area.y, widths[0], area.height),
        ));
    }
    if widths[last] > 0 {
        rects.push((
            specs[last].0,
            Rect::new(
                area.right().saturating_sub(widths[last]),
                area.y,
                widths[last],
                area.height,
            ),
        ));
    }
    // Whatever slack is left lands around the middle field rather than at
    // either edge.
    if specs.len() == 3 && widths[1] > 0 {
        let left = area
            .x
            .saturating_add(widths[0])
            .saturating_add(BOTTOM_ROW_GAP as u16);
        let right = area
            .right()
            .saturating_sub(widths[last])
            .saturating_sub(BOTTOM_ROW_GAP as u16);
        let span = right.saturating_sub(left);
        let x = left.saturating_add(span.saturating_sub(widths[1]) / 2);
        rects.push((specs[1].0, Rect::new(x, area.y, widths[1], area.height)));
    }
    rects
}

/// `What should we build in <name>?`, with the name underlined.
///
/// The name is the directory field: it is what the thread is about, so it
/// carries the picker instead of a second folder chip in the bottom row.
const HEADLINE_PREFIX: &str = "What should we build in ";

fn headline_name(app: &AppState, width: u16) -> String {
    truncate_end(
        &app.home_headline_name(),
        (width as usize).saturating_sub(display_width(HEADLINE_PREFIX) + 1),
    )
}

/// The cells the underlined name occupies, given the centred headline.
fn headline_name_rect(app: &AppState, headline: Rect) -> Option<Rect> {
    if headline.width == 0 || headline.height == 0 {
        return None;
    }
    let name = headline_name(app, headline.width);
    let name_width = display_width_u16(&name);
    if name_width == 0 {
        return None;
    }
    let line_width = display_width_u16(HEADLINE_PREFIX)
        .saturating_add(name_width)
        .saturating_add(1);
    let left = headline
        .x
        .saturating_add(headline.width.saturating_sub(line_width) / 2)
        .saturating_add(display_width_u16(HEADLINE_PREFIX));
    if left >= headline.right() {
        return None;
    }
    Some(Rect::new(
        left,
        headline.y,
        name_width.min(headline.right() - left),
        1,
    ))
}

fn headline_line(app: &AppState, width: u16, focused: bool) -> Line<'static> {
    let name = headline_name(app, width);
    let name_style = Style::default()
        .fg(app.palette.text)
        .add_modifier(Modifier::UNDERLINED);
    let name_style = if focused {
        name_style
            .bg(app.palette.surface1)
            .add_modifier(Modifier::BOLD)
    } else {
        name_style
    };
    Line::from(vec![
        Span::styled(HEADLINE_PREFIX, Style::default().fg(app.palette.subtext0)),
        Span::styled(name, name_style),
        Span::styled("?", Style::default().fg(app.palette.subtext0)),
    ])
}

/// `├───┤` across the card, meeting the border cells the block drew.
fn divider_line(width: u16) -> String {
    match width {
        0 => String::new(),
        1 => "├".to_string(),
        _ => format!("├{}┤", "─".repeat(usize::from(width - 2))),
    }
}

/// Wrap the prompt to the box and report where the cursor sits.
///
/// Newlines are hard breaks, everything else wraps at the column, and only the
/// last `height` lines survive so a long prompt scrolls its tail into view the
/// way typing into a box does.
fn prompt_layout(
    prompt: &str,
    width: u16,
    height: u16,
    focused: bool,
) -> (Vec<String>, Option<(u16, u16)>) {
    if width == 0 || height == 0 {
        return (Vec::new(), None);
    }
    let width = usize::from(width);
    let mut lines: Vec<String> = vec![String::new()];
    for character in prompt.chars() {
        if character == '\n' {
            lines.push(String::new());
            continue;
        }
        let cell = display_width(&character.to_string());
        let current = lines
            .last_mut()
            .unwrap_or_else(|| unreachable!("never empty"));
        if display_width(current) + cell > width {
            lines.push(character.to_string());
        } else {
            current.push(character);
        }
    }
    // The cursor is a cell, so it needs room of its own on the last line.
    let cursor_column = display_width(lines.last().map(String::as_str).unwrap_or_default());
    let cursor_column = if cursor_column >= width {
        lines.push(String::new());
        0
    } else {
        cursor_column
    };
    if focused {
        if let Some(last) = lines.last_mut() {
            last.push('█');
        }
    }
    let first = lines.len().saturating_sub(usize::from(height));
    let cursor_row = lines.len().saturating_sub(first + 1);
    let visible = lines.split_off(first);
    let cursor = focused.then(|| {
        (
            u16::try_from(cursor_column).unwrap_or(u16::MAX),
            u16::try_from(cursor_row).unwrap_or(u16::MAX),
        )
    });
    (visible, cursor)
}

fn target_label(app: &AppState, target: &HomeTarget) -> String {
    match target {
        HomeTarget::NewSpace => "new space".into(),
        HomeTarget::Existing(id) => app
            .workspaces
            .iter()
            .find(|workspace| workspace.id == *id)
            .map(|workspace| format!("space {}", workspace.id))
            .unwrap_or_else(|| "missing space".into()),
    }
}

/// A lane's remaining plan quota, as the picker row shows it: the 5h window
/// then the 7d one, in the same fill-column vocabulary the status bar uses.
///
/// The number is spelled out only once a window is nearly spent. Below that a
/// column height is the whole answer, and a percentage on every row would bury
/// the lane names the picker exists to show. A lane with no collected window
/// renders nothing rather than a zero, because absent and idle are different.
fn lane_quota_summary(
    profile: &crate::app::launch_profiles::LaunchProfile,
    app: &AppState,
) -> Option<String> {
    let usage = profile.quota?.usage(&app.provider_usage);
    if usage.is_empty() {
        return None;
    }
    let mut text = String::new();
    for window in [usage.five_hour, usage.seven_day].into_iter().flatten() {
        if !text.is_empty() {
            text.push(' ');
        }
        text.push(crate::ui::status::fill_glyph(window.used_percent));
        if window.used_percent >= LANE_QUOTA_ESCALATION_PERCENT {
            text.push_str(&window.used_percent.to_string());
        }
    }
    (!text.is_empty()).then_some(text)
}

/// Above this the picker spells the percentage out beside the column.
const LANE_QUOTA_ESCALATION_PERCENT: u8 = 88;

fn picker_labels(app: &AppState, home: &HomeState, picker: HomePicker) -> Vec<String> {
    match picker {
        HomePicker::Agent => home
            .profiles()
            .iter()
            .map(|profile| match lane_quota_summary(profile, app) {
                Some(quota) => format!("{}  {quota}", profile.label),
                None => profile.label.clone(),
            })
            .collect(),
        HomePicker::Model => home
            .model_options()
            .iter()
            .map(|model| model.display_name.clone())
            .collect(),
        HomePicker::Effort => home.effort_options().to_vec(),
        HomePicker::Access => home
            .access_options()
            .iter()
            .map(|access| access.label().to_string())
            .collect(),
        HomePicker::Context => home
            .context_options()
            .iter()
            .map(|context| (*context).to_string())
            .collect(),
        HomePicker::Project => home
            .projects()
            .iter()
            .map(|project| project.label.clone())
            .collect(),
        HomePicker::Repo => home
            .repo_options()
            .iter()
            .map(|repo| repo.name.clone())
            .collect(),
        HomePicker::Machine => home
            .machines()
            .iter()
            .map(|machine| machine.name.clone())
            .collect(),
        HomePicker::Directory => match home.browse.as_ref() {
            // Browsing replaces the options with what is under the typed path.
            Some(browse) => browse.children.clone(),
            None => app
                .home_directory_picker_options()
                .iter()
                .map(crate::app::home::HomeDirectoryOption::label)
                .collect(),
        },
        HomePicker::Workspace => app
            .home_workspace_options()
            .iter()
            .map(crate::app::home::HomeWorkspace::label)
            .collect(),
        HomePicker::Ref => app
            .home_ref_options()
            .iter()
            .map(crate::app::home_refs::HomeRef::display_label)
            .collect(),
        HomePicker::Target => app
            .home_target_options()
            .iter()
            .map(|target| target_label(app, target))
            .collect(),
    }
}

fn picker_matches<'a>(
    app: &AppState,
    home: &HomeState,
    picker: HomePicker,
    labels: &'a [String],
) -> Vec<(usize, &'a str)> {
    match picker {
        HomePicker::Directory if home.browse.is_some() => labels
            .iter()
            .enumerate()
            .map(|(index, label)| (index, label.as_str()))
            .collect(),
        HomePicker::Directory => home.directory_filter.matches(labels),
        HomePicker::Ref => {
            let names = app
                .home_ref_options()
                .iter()
                .map(|git_ref| git_ref.name.clone())
                .collect::<Vec<_>>();
            home.ref_filter
                .matches(&names)
                .into_iter()
                .filter_map(|(index, _)| labels.get(index).map(|label| (index, label.as_str())))
                .collect()
        }
        _ => labels
            .iter()
            .enumerate()
            .map(|(index, label)| (index, label.as_str()))
            .collect(),
    }
}

fn composer_field_rect(
    app: &AppState,
    home: &HomeState,
    composer: ComposerBands,
    focus: HomeFocus,
) -> Option<Rect> {
    if focus == HomeFocus::Prompt {
        return (composer.prompt.width > 0).then_some(composer.prompt);
    }
    // The directory is the headline, not a field in the card.
    if focus == HomeFocus::Directory {
        return headline_name_rect(app, composer.headline);
    }
    secondary_rects(app, home, composer)
        .into_iter()
        .chain(chip_rects(app, home, composer))
        .find_map(|(field_focus, rect)| (field_focus == focus).then_some(rect))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LensSnapshot {
    title: String,
    output: String,
    reason: Option<String>,
}

fn lens_snapshot(
    app: &AppState,
    terminal_runtimes: &crate::terminal::TerminalRuntimeRegistry,
    queue: &[BlockedAgent],
    output_lines: u16,
) -> Option<LensSnapshot> {
    let home = app.home.as_ref()?;
    let agent = home.current(queue)?;
    let title = format!(
        "{} · {}:p{}",
        agent.agent_label,
        agent.workspace_label,
        agent.pane_id.raw()
    );
    let Some(terminal) = app.terminals.get(&agent.terminal_id) else {
        return Some(LensSnapshot {
            title,
            output: String::new(),
            reason: Some("pane is no longer available".into()),
        });
    };
    let Some(pane) = app
        .workspaces
        .get(agent.ws_idx)
        .and_then(|workspace| workspace.pane_state(agent.pane_id))
    else {
        return Some(LensSnapshot {
            title,
            output: String::new(),
            reason: Some("pane is no longer available".into()),
        });
    };
    if !pane.agent_projection(terminal).needs_human_attention() {
        return Some(LensSnapshot {
            title,
            output: String::new(),
            reason: Some("pane is no longer waiting on you".into()),
        });
    }
    let Some(runtime) =
        app.runtime_for_pane_in_workspace(terminal_runtimes, agent.ws_idx, agent.pane_id)
    else {
        return Some(LensSnapshot {
            title,
            output: String::new(),
            reason: Some("pane output is unavailable".into()),
        });
    };
    // Keep the lens on the same read contract as `pane.read`. Recent is the
    // normal tail; visible is the API's bounded fallback for a pane with no
    // retained history yet.
    let recent = crate::app::read_terminal_snapshot(
        runtime,
        crate::api::schema::ReadSource::Recent,
        crate::api::schema::ReadFormat::Text,
        Some(u32::from(output_lines.max(1))),
    );
    let snapshot = if recent.text.trim().is_empty() {
        crate::app::read_terminal_snapshot(
            runtime,
            crate::api::schema::ReadSource::Visible,
            crate::api::schema::ReadFormat::Text,
            Some(u32::from(output_lines.max(1))),
        )
    } else {
        recent
    };
    let pending_draft = app
        .pending_human_drafts
        .get(&agent.pane_id)
        .is_some_and(|draft| !draft.is_empty());
    let reason = home
        .reply_error
        .clone()
        .or_else(|| pending_draft.then_some("human draft pending · clear it in the pane".into()));
    let reason = reason.or_else(|| {
        snapshot
            .text
            .trim()
            .is_empty()
            .then_some("no output yet".into())
    });
    Some(LensSnapshot {
        title,
        output: snapshot.text,
        reason,
    })
}

fn render_lens(
    app: &AppState,
    terminal_runtimes: &crate::terminal::TerminalRuntimeRegistry,
    queue: &[BlockedAgent],
    bands: LensBands,
    frame: &mut Frame,
) {
    let Some(home) = app.home.as_ref() else {
        return;
    };
    let Some(snapshot) = lens_snapshot(app, terminal_runtimes, queue, bands.output.height) else {
        return;
    };
    let border = Style::default().fg(app.palette.surface_dim);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_style(border)
            .style(Style::default().bg(app.palette.panel_bg)),
        bands.frame,
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            truncate(&snapshot.title, bands.title.width as usize),
            Style::default()
                .fg(app.palette.text)
                .add_modifier(Modifier::BOLD),
        )))
        .style(Style::default().bg(app.palette.panel_bg)),
        bands.title,
    );
    let mut output = snapshot.output;
    if let Some(reason) = snapshot.reason {
        output = if output.is_empty() {
            reason
        } else {
            format!("{reason}\n{output}")
        };
    }
    frame.render_widget(
        Paragraph::new(output).style(
            Style::default()
                .fg(app.palette.overlay1)
                .add_modifier(Modifier::DIM),
        ),
        bands.output,
    );
    let focused = home.focus == Some(HomeFocus::Reply);
    let reply = if home.reply.is_empty() {
        "type a reply".to_string()
    } else {
        home.reply.clone()
    };
    let suffix = if focused { "_" } else { "" };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("> {reply}{suffix}"),
            if focused {
                Style::default()
                    .fg(app.palette.text)
                    .bg(app.palette.surface1)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(app.palette.subtext0)
            },
        )))
        .style(Style::default().bg(app.palette.panel_bg)),
        bands.reply,
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            truncate_end("new task [n]", bands.new_task.width as usize),
            Style::default().fg(app.palette.accent),
        )))
        .style(Style::default().bg(app.palette.panel_bg)),
        bands.new_task,
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            truncate("detach [d] →", bands.detach.width as usize),
            Style::default().fg(app.palette.accent),
        )))
        .style(Style::default().bg(app.palette.panel_bg)),
        bands.detach,
    );
}

/// The dropdown's own input line: a fuzzy filter, or the browsed path.
fn picker_filter_text(home: &HomeState, picker: HomePicker) -> String {
    match (picker, home.browse.as_ref()) {
        (HomePicker::Directory, Some(browse)) => format!(" › {}_", browse.input),
        (HomePicker::Directory, None) => format!(" / {}_", home.directory_filter.query),
        _ => format!(" / {}_", home.ref_filter.query),
    }
}

fn picker_layout(
    app: &AppState,
    home: &HomeState,
    composer: ComposerBands,
    area: Rect,
) -> Option<DropdownLayout> {
    let picker = home.picker?;
    let field = composer_field_rect(app, home, composer, home.focus?)?;
    let labels = picker_labels(app, home, picker);
    let browsing = picker == HomePicker::Directory && home.browse.is_some();
    // A path with no matching children still has to show its input line.
    if (labels.is_empty() && !browsing) || area.width == 0 || area.height == 0 {
        return None;
    }
    // The hint line is the last row of every home layout. A picker anchored on
    // the headline is tall enough to reach it, so the popup clamps one row
    // early rather than drawing over the only instructions on screen.
    let area = Rect::new(area.x, area.y, area.width, area.height - 1);
    let has_filter = matches!(picker, HomePicker::Directory | HomePicker::Ref);
    let matches = picker_matches(app, home, picker, &labels);
    let filter_width = if has_filter {
        display_width(&picker_filter_text(home, picker)).saturating_add(1)
    } else {
        0
    };
    let min_width = labels
        .iter()
        .map(|label| display_width(label))
        .max()
        .unwrap_or(0)
        .saturating_add(2)
        .max(filter_width)
        .try_into()
        .unwrap_or(u16::MAX)
        .min(area.width);
    layout_dropdown(
        &DropdownSpec {
            anchor: field,
            item_count: matches.len(),
            selected: match picker {
                HomePicker::Directory => home.directory_filter.selected,
                HomePicker::Ref => home.ref_filter.selected,
                _ => home.picker_selected,
            },
            has_filter,
            max_rows: labels.len().max(1),
            min_width,
        },
        area,
    )
}

#[cfg(test)]
fn picker_popup_rect(
    app: &AppState,
    home: &HomeState,
    composer: ComposerBands,
    area: Rect,
) -> Option<Rect> {
    picker_layout(app, home, composer, area).map(|layout| layout.rect)
}

fn picker_viewport(
    app: &AppState,
    home: &HomeState,
    composer: ComposerBands,
    area: Rect,
) -> Option<DropdownLayout> {
    picker_layout(app, home, composer, area)
}

fn queue_row_hit_areas(
    app: &AppState,
    queue: &[BlockedAgent],
    area: Rect,
    body: Rect,
) -> Vec<(usize, Rect)> {
    let Some(home) = app.home.as_ref() else {
        return Vec::new();
    };
    if queue.is_empty() || area.width == 0 || area.height < 4 {
        return Vec::new();
    }
    let visible = body.height as usize;
    let scroll = home.scroll(queue, visible);
    (scroll..queue.len().min(scroll + visible))
        .enumerate()
        .map(|(offset, index)| {
            (
                index,
                Rect::new(body.x, body.y + offset as u16, body.width, 1),
            )
        })
        .collect()
}

/// One `(queue index, rect)` per row home is currently showing.
///
/// Empty when home is closed or the queue is, so the click handler needs no
/// separate "is home open" test.
pub(super) fn row_hit_areas(
    app: &AppState,
    queue: &[BlockedAgent],
    area: Rect,
) -> Vec<(usize, Rect)> {
    queue_row_hit_areas(app, queue, area, home_bands(app, queue, area).body)
}

pub(super) fn home_hit_areas(
    app: &AppState,
    queue: &[BlockedAgent],
    area: Rect,
) -> Vec<HomeHitArea> {
    let Some(home) = app.home.as_ref() else {
        return Vec::new();
    };
    let layout = home_bands(app, queue, area);
    let mut hits = Vec::new();
    if let Some(composer) = layout.composer {
        if let Some(dropdown) = picker_viewport(app, home, composer, area) {
            for offset in 0..dropdown.visible_rows {
                let y = dropdown.list_rect.y + offset as u16;
                let Some(index) = dropdown_hit_test(&dropdown, dropdown.list_rect.x, y) else {
                    continue;
                };
                hits.push(HomeHitArea {
                    target: HomeHitTarget::PickerOption(index),
                    rect: Rect::new(dropdown.list_rect.x, y, dropdown.list_rect.width, 1),
                });
            }
        }
        hits.extend(
            queue_row_hit_areas(app, queue, area, layout.body)
                .into_iter()
                .map(|(index, rect)| HomeHitArea {
                    target: HomeHitTarget::QueueRow(index),
                    rect,
                }),
        );
        if composer.prompt.width > 0 {
            hits.push(HomeHitArea {
                target: HomeHitTarget::Prompt,
                rect: composer.prompt,
            });
        }
        // The underlined name in the headline is the directory field.
        if let Some(rect) = headline_name_rect(app, composer.headline) {
            hits.push(HomeHitArea {
                target: HomeHitTarget::Directory,
                rect,
            });
        }
        for (focus, rect) in chip_rects(app, home, composer)
            .into_iter()
            .chain(secondary_rects(app, home, composer))
        {
            hits.push(HomeHitArea {
                target: match focus {
                    HomeFocus::Agent => HomeHitTarget::Agent,
                    HomeFocus::Model => HomeHitTarget::Model,
                    HomeFocus::Effort => HomeHitTarget::Effort,
                    HomeFocus::Access => HomeHitTarget::Access,
                    HomeFocus::Context => HomeHitTarget::Context,
                    HomeFocus::Project => HomeHitTarget::Project,
                    HomeFocus::Repo => HomeHitTarget::Repo,
                    HomeFocus::Directory => HomeHitTarget::Directory,
                    HomeFocus::Machine => HomeHitTarget::Machine,
                    HomeFocus::Workspace => HomeHitTarget::Workspace,
                    HomeFocus::Ref => HomeHitTarget::Ref,
                    HomeFocus::Target => HomeHitTarget::Target,
                    HomeFocus::Reply | HomeFocus::Prompt => continue,
                },
                rect,
            });
        }
    } else if let Some(lens) = layout.lens {
        hits.extend(
            queue_row_hit_areas(app, queue, area, layout.body)
                .into_iter()
                .map(|(index, rect)| HomeHitArea {
                    target: HomeHitTarget::QueueRow(index),
                    rect,
                }),
        );
        if lens.reply.width > 0 {
            hits.push(HomeHitArea {
                target: HomeHitTarget::Reply,
                rect: lens.reply,
            });
        }
        if lens.new_task.width > 0 {
            hits.push(HomeHitArea {
                target: HomeHitTarget::NewTask,
                rect: lens.new_task,
            });
        }
        if lens.detach.width > 0 {
            hits.push(HomeHitArea {
                target: HomeHitTarget::Detach,
                rect: lens.detach,
            });
        }
    } else {
        hits.extend(
            queue_row_hit_areas(app, queue, area, layout.body)
                .into_iter()
                .map(|(index, rect)| HomeHitArea {
                    target: HomeHitTarget::QueueRow(index),
                    rect,
                }),
        );
    }
    hits
}

pub(super) fn render_home(
    app: &AppState,
    terminal_runtimes: &crate::terminal::TerminalRuntimeRegistry,
    queue: &[BlockedAgent],
    counts: HomeCounts,
    area: Rect,
    frame: &mut Frame,
) {
    let layout = home_bands(app, queue, area);
    let HomeBands {
        header,
        body,
        composer,
        lens,
        hint,
        ..
    } = layout;

    frame.render_widget(Paragraph::new(header_line(app, counts, area.width)), header);

    let visible = body.height as usize;
    let scroll = app
        .home
        .as_ref()
        .map(|home| home.scroll(queue, visible))
        .unwrap_or(0);
    let selected = app
        .home
        .as_ref()
        .map(|home| home.selected(queue))
        .unwrap_or(0);

    let entries = if queue.is_empty() {
        Vec::new()
    } else {
        crate::ui::sidebar::all_agent_panel_entries(app)
    };
    let lines: Vec<Line<'static>> = if queue.is_empty() {
        vec![empty_line(app)]
    } else {
        queue
            .iter()
            .enumerate()
            .skip(scroll)
            .take(visible)
            .map(|(idx, agent)| {
                let cells = entries
                    .iter()
                    .find(|entry| {
                        entry
                            .local_target()
                            .is_some_and(|target| target.pane_id == agent.pane_id)
                    })
                    .map(|entry| {
                        crate::ui::sidebar::agent_row_cells(entry, &app.palette, app.nerd_font)
                    });
                agent_line(app, agent, cells.as_ref(), idx == selected, body.width)
            })
            .collect()
    };
    frame.render_widget(Paragraph::new(lines), body);

    if let Some(composer) = composer {
        let Some(home) = app.home.as_ref() else {
            return;
        };
        // The card, drawn first so the input rows sit inside it. Focused
        // borders take the accent so the composer reads as active.
        let focused = home.focus.is_some();
        frame.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(if focused {
                    app.palette.accent
                } else {
                    app.palette.surface_dim
                }))
                .style(Style::default().bg(app.palette.panel_bg)),
            composer.frame,
        );
        frame.render_widget(
            Paragraph::new(headline_line(
                app,
                composer.headline.width,
                home.focus == Some(HomeFocus::Directory),
            ))
            .alignment(Alignment::Center),
            composer.headline,
        );

        let focused_prompt = home.focus == Some(HomeFocus::Prompt);
        let prompt_style = if focused_prompt {
            Style::default()
                .fg(app.palette.text)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(app.palette.subtext0)
        };
        // Wrapping is done here rather than by `Wrap` so the cursor cell is the
        // same computation as the text it follows: a cursor a `Paragraph`
        // placed differently would sit on the wrong row.
        let (prompt_lines, cursor) = prompt_layout(
            &home.prompt,
            composer.prompt.width,
            composer.prompt.height,
            focused_prompt,
        );
        let placeholder = home.prompt.is_empty() && !focused_prompt;
        let prompt_style = if placeholder {
            Style::default()
                .fg(app.palette.overlay0)
                .add_modifier(Modifier::DIM)
        } else {
            prompt_style
        };
        let prompt_lines = if placeholder {
            vec!["type a prompt".to_string()]
        } else {
            prompt_lines
        };
        frame.render_widget(
            Paragraph::new(
                prompt_lines
                    .into_iter()
                    .map(|line| Line::from(Span::styled(line, prompt_style)))
                    .collect::<Vec<_>>(),
            )
            .style(Style::default().bg(app.palette.panel_bg)),
            composer.prompt,
        );
        if let Some((column, row)) = cursor {
            let x = composer.prompt.x.saturating_add(column);
            let y = composer.prompt.y.saturating_add(row);
            if x < composer.prompt.right() && y < composer.prompt.bottom() {
                frame.set_cursor_position((x, y));
            }
        }

        let chip_rects = chip_rects(app, home, composer);
        // The separator lives in the gap, so it never eats a chip's own width
        // or its click target.
        for pair in chip_rects.windows(2) {
            let separator_x = pair[0].1.right().saturating_add(1);
            if separator_x < pair[1].1.x && separator_x < composer.chips.right() {
                frame.render_widget(
                    Paragraph::new("│").style(
                        Style::default()
                            .fg(app.palette.surface_dim)
                            .bg(app.palette.panel_bg),
                    ),
                    Rect::new(separator_x, composer.chips.y, 1, 1),
                );
            }
        }
        if composer.submit.width > 0 {
            frame.render_widget(
                Paragraph::new(truncate_end(SUBMIT_GLYPH, composer.submit.width as usize)).style(
                    Style::default()
                        .fg(if focused_prompt {
                            app.palette.accent
                        } else {
                            app.palette.overlay0
                        })
                        .bg(app.palette.panel_bg),
                ),
                composer.submit,
            );
        }
        frame.render_widget(
            Paragraph::new(divider_line(composer.divider.width))
                .style(Style::default().fg(app.palette.surface_dim)),
            composer.divider,
        );

        for (focus, rect) in chip_rects {
            let label = chip_specs(app, home, composer)
                .into_iter()
                .find_map(|(candidate, label)| (candidate == focus).then_some(label))
                .unwrap_or_default();
            let style = if home.focus == Some(focus) {
                Style::default()
                    .fg(app.palette.text)
                    .bg(app.palette.surface1)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(app.palette.subtext0)
            };
            frame.render_widget(
                Paragraph::new(picker_row_label(&label, rect.width as usize)).style(style),
                rect,
            );
        }

        for (focus, rect) in secondary_rects(app, home, composer) {
            let label = secondary_specs(app, home, composer)
                .into_iter()
                .find_map(|(candidate, label)| (candidate == focus).then_some(label))
                .unwrap_or_default();
            let style = if home.focus == Some(focus) {
                Style::default()
                    .fg(app.palette.text)
                    .bg(app.palette.surface1)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(app.palette.subtext0)
            };
            frame.render_widget(
                Paragraph::new(truncate_end(&label, rect.width as usize)).style(style),
                rect,
            );
        }

        if let Some(dropdown) = picker_viewport(app, home, composer, area) {
            if let Some(picker) = home.picker {
                let labels = picker_labels(app, home, picker);
                let matches = picker_matches(app, home, picker, &labels);
                let selected = match picker {
                    HomePicker::Directory => home.directory_filter.selected,
                    HomePicker::Ref => home.ref_filter.selected,
                    _ => home.picker_selected,
                };
                if let Some(filter_rect) = dropdown.filter_rect {
                    let query = picker_filter_text(home, picker);
                    frame.render_widget(
                        Paragraph::new(truncate_end(&query, filter_rect.width as usize)).style(
                            Style::default()
                                .fg(app.palette.text)
                                .bg(app.palette.surface1)
                                .add_modifier(Modifier::BOLD),
                        ),
                        filter_rect,
                    );
                }
                let lines = matches
                    .into_iter()
                    .enumerate()
                    .skip(dropdown.first_visible)
                    .take(dropdown.visible_rows)
                    .map(|(match_index, (_, label))| {
                        let style = if match_index == selected {
                            Style::default()
                                .fg(app.palette.text)
                                .bg(app.palette.surface1)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default()
                                .fg(app.palette.subtext0)
                                .bg(app.palette.panel_bg)
                        };
                        Line::from(Span::styled(format!(" {label}"), style))
                    })
                    .collect::<Vec<_>>();
                frame.render_widget(
                    Paragraph::new(lines).style(Style::default().bg(app.palette.panel_bg)),
                    dropdown.list_rect,
                );
            }
        } else if let Some(picker) = home.picker {
            if !picker_labels(app, home, picker).is_empty() {
                if let Some(field) = home
                    .focus
                    .and_then(|focus| composer_field_rect(app, home, composer, focus))
                {
                    frame.render_widget(
                        Paragraph::new(" no room below").style(
                            Style::default()
                                .fg(app.palette.red)
                                .bg(app.palette.panel_bg),
                        ),
                        field,
                    );
                }
            }
        }
    }

    if let Some(lens) = lens {
        render_lens(app, terminal_runtimes, queue, lens, frame);
    }

    let hidden_below = queue.len().saturating_sub(scroll + visible);
    frame.render_widget(
        Paragraph::new(hint_line(
            app,
            scroll,
            hidden_below,
            composer.is_some(),
            lens.is_some(),
        )),
        hint,
    );
    if let (Some(composer), Some(error)) = (
        composer,
        app.home.as_ref().and_then(|home| {
            home.browse
                .as_ref()
                .and_then(|browse| browse.error.as_deref())
                .or(home.dispatch_error.as_deref())
        }),
    ) {
        let error_row = Rect::new(
            composer.frame.x,
            composer.frame.bottom(),
            composer.frame.width,
            1,
        );
        if error_row.bottom() <= area.bottom() {
            frame.render_widget(
                Paragraph::new(truncate_end(error, error_row.width as usize))
                    .style(Style::default().fg(app.palette.red)),
                error_row,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::PaneId;
    use crate::terminal::{TerminalId, TerminalRuntime};
    use crate::workspace::Workspace;
    use ratatui::{backend::TestBackend, buffer::Buffer, Terminal};

    fn blocked(index: usize) -> BlockedAgent {
        BlockedAgent {
            ws_idx: 0,
            pane_id: PaneId::alloc(),
            terminal_id: TerminalId::alloc(),
            workspace_label: format!("ws{index}"),
            agent_label: format!("agent{index}"),
            blocked_since: None,
            seq: None,
            attention_tier: crate::terminal::state::AttentionTier::Blocked,
        }
    }

    fn draw_home(app: &AppState, queue: &[BlockedAgent], area: Rect) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).expect("term");
        terminal
            .draw(|frame| {
                render_home(
                    app,
                    &crate::terminal::TerminalRuntimeRegistry::new(),
                    queue,
                    HomeCounts {
                        blocked: queue.len(),
                        attention: 0,
                        agents: queue.len(),
                        spaces: 1,
                    },
                    area,
                    frame,
                );
            })
            .expect("render home");
        terminal.backend().buffer().clone()
    }

    fn row_text(buffer: &Buffer, area: Rect, row: u16) -> String {
        (area.x..area.x + area.width)
            .map(|column| buffer[(column, row)].symbol())
            .collect()
    }

    fn app_with_lens_screen(output: &[u8]) -> AppState {
        let mut app = AppState::test_new();
        let mut workspace = Workspace::test_new("lens");
        let pane_id = workspace.tabs[0].root_pane;
        workspace.insert_test_runtime(
            pane_id,
            TerminalRuntime::test_with_scrollback_bytes(80, 8, 1024, output),
        );
        let terminal_id = workspace.tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        app.terminals
            .get_mut(&terminal_id)
            .expect("test terminal")
            .set_raw_agent_state_for_test(crate::detect::AgentState::Blocked);
        let mut home = HomeState::default();
        home.focus = None;
        app.home = Some(home);
        app
    }

    #[test]
    fn home_rows_use_the_sidebar_dot_session_title_and_provider() {
        let mut app = AppState::test_new();
        app.nerd_font = false;
        let mut workspace = Workspace::test_new("herdr");
        workspace.tabs[0].custom_name = Some("Critical action links".into());
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let pane_id = app.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.terminals.get_mut(&terminal_id).expect("test terminal");
        terminal.detected_agent = Some(crate::detect::Agent::Claude);
        terminal.set_raw_agent_state_for_test(crate::detect::AgentState::Blocked);
        let mut home = HomeState::default();
        home.focus = None;
        app.home = Some(home);
        let queue = app.home_attention_agents();
        assert_eq!(queue.len(), 1);
        let area = Rect::new(0, 0, 80, 6);

        let buffer = draw_home(&app, &queue, area);
        let row = row_text(&buffer, area, 2);

        assert!(row.contains("Critical action links"), "{row}");
        assert!(row.contains(" cc "), "{row}");
        assert!(!row.contains("claude"), "{row}");
        assert_eq!(buffer[(1, 2)].symbol(), "○");
        assert_eq!(buffer[(1, 2)].fg, app.palette.red);
    }

    #[test]
    fn home_rows_fit_their_width_with_wide_titles_and_narrow_frames() {
        let app = AppState::test_new();
        let agent = blocked(0);
        let cells = crate::ui::sidebar::AgentRowCells {
            dot: "○".into(),
            dot_color: app.palette.red,
            title: "界界界界界界界界界界".into(),
            provider: "cc".into(),
            provider_color: app.palette.peach,
        };
        let mut aged = agent.clone();
        aged.blocked_since = Some(std::time::Instant::now());
        let narrow = agent_line(&app, &aged, Some(&cells), false, 18);
        let text: String = narrow
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(display_width(&text), 18, "{text:?}");
        assert!(text.contains('界') && text.contains("cc"), "{text:?}");
        let decorated = crate::ui::sidebar::AgentRowCells {
            provider: "cc+2 >_".into(),
            title: "#159 · sample-pr".into(),
            ..cells.clone()
        };
        let narrow = agent_line(&app, &aged, Some(&decorated), false, 18);
        let text: String = narrow
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(display_width(&text), 18, "{text:?}");
        assert!(
            text.contains(" cc ") && text.contains("sampl") && !text.contains("#159"),
            "{text:?}"
        );
        for width in 0u16..=80 {
            let line = agent_line(&app, &agent, Some(&cells), false, width);
            let text: String = line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect();
            assert_eq!(
                display_width(&text),
                width as usize,
                "width {width}: {text:?}"
            );
            assert!(
                width < 18 || text.contains('界'),
                "width {width} lost the title: {text:?}"
            );
        }
        let wide = agent_line(&app, &agent, Some(&cells), false, 40);
        let text: String = wide
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(text.contains(" cc "), "{text:?}");
    }

    #[test]
    fn home_renders_blocked_rows_and_marks_the_selected_row_with_bold_text() {
        let mut app = AppState::test_new();
        app.home = Some(crate::app::home::HomeState::default());
        let queue = vec![blocked(0), blocked(1)];
        let area = Rect::new(0, 0, 60, 6);

        let buffer = draw_home(&app, &queue, area);

        assert!(row_text(&buffer, area, 2).contains("agent0"));
        assert!(row_text(&buffer, area, 3).contains("agent1"));
        assert_eq!(buffer[(1, 2)].symbol(), "○");
        assert_eq!(buffer[(1, 3)].symbol(), "○");
        assert_eq!(
            buffer[(3, 2)].style().add_modifier(Modifier::BOLD),
            buffer[(3, 2)].style()
        );
        assert_ne!(
            buffer[(3, 3)].style().add_modifier(Modifier::BOLD),
            buffer[(3, 3)].style()
        );
    }

    #[test]
    fn the_blocked_count_is_drawn_in_the_needs_attention_colour() {
        let mut app = AppState::test_new();
        app.home = Some(crate::app::home::HomeState::default());
        let queue = vec![blocked(0)];
        let area = Rect::new(0, 0, 60, 6);

        let buffer = draw_home(&app, &queue, area);

        // The palette reserves `red` for blocked; `accent` is the generic
        // highlight and reads as "selected" rather than "waiting".
        assert_eq!(buffer[(1, 0)].style().fg, Some(app.palette.red));
        assert_ne!(app.palette.red, app.palette.accent);

        // With nothing waiting the count goes quiet rather than staying loud.
        let buffer = draw_home(&app, &[], area);
        assert_eq!(buffer[(1, 0)].style().fg, Some(app.palette.overlay0));
    }

    #[test]
    fn narrow_header_drops_attention_before_the_existing_fleet_summary() {
        let app = AppState::test_new();
        let counts = HomeCounts {
            blocked: 1,
            attention: 2,
            agents: 3,
            spaces: 4,
        };
        let text = |width| {
            header_line(&app, counts, width)
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        };

        let narrow = text(40);
        assert!(!narrow.contains("attention"));
        assert!(narrow.ends_with("3 agents · 4 spaces "));
        assert!(text(80).contains("2 attention"));
    }

    #[test]
    fn the_selected_row_is_filled_across_its_full_width_not_only_emboldened() {
        let mut app = AppState::test_new();
        app.home = Some(crate::app::home::HomeState::default());
        let queue = vec![blocked(0), blocked(1)];
        let area = Rect::new(0, 0, 60, 6);

        let buffer = draw_home(&app, &queue, area);

        // Every cell of the cursor row carries the surface, so the cursor is
        // legible as a band rather than as a weight difference.
        for column in area.x..area.right() {
            assert_eq!(
                buffer[(column, 2)].style().bg,
                Some(app.palette.surface0),
                "column {column} of the selected row is not filled"
            );
        }
        assert_ne!(buffer[(1, 3)].style().bg, Some(app.palette.surface0));
    }

    #[test]
    fn home_row_hit_areas_line_up_with_the_rows_that_were_drawn() {
        let mut app = AppState::test_new();
        app.home = Some(crate::app::home::HomeState::default());
        let queue = vec![blocked(0), blocked(1)];
        let area = Rect::new(0, 0, 60, 6);

        let buffer = draw_home(&app, &queue, area);
        let hits = row_hit_areas(&app, &queue, area);

        assert_eq!(hits.len(), 2);
        for (index, rect) in &hits {
            assert_eq!(rect.height, 1);
            assert!(
                row_text(&buffer, area, rect.y).contains(&format!("agent{index}")),
                "hit area for row {index} is not where agent{index} was drawn"
            );
        }
    }

    #[test]
    fn home_offers_no_hit_areas_when_it_is_closed_or_has_nothing_to_show() {
        let mut app = AppState::test_new();
        let queue = vec![blocked(0)];
        let area = Rect::new(0, 0, 60, 6);

        // Closed: the click handler needs no separate "is home open" test.
        assert!(row_hit_areas(&app, &queue, area).is_empty());

        app.home = Some(crate::app::home::HomeState::default());
        assert!(row_hit_areas(&app, &[], area).is_empty());
        // Too short to have a body band at all.
        assert!(row_hit_areas(&app, &queue, Rect::new(0, 0, 60, 3)).is_empty());
    }

    #[test]
    fn a_scrolled_home_reports_hit_areas_for_the_rows_actually_on_screen() {
        let mut app = AppState::test_new();
        let mut home = crate::app::home::HomeState::default();
        let queue: Vec<BlockedAgent> = (0..10).map(blocked).collect();
        home.select(9);
        app.home = Some(home);
        let area = Rect::new(0, 0, 60, 6);

        let buffer = draw_home(&app, &queue, area);
        let hits = row_hit_areas(&app, &queue, area);

        // Body is 3 rows tall, and the cursor is on the last agent, so the
        // reported indices are the tail of the queue rather than its head.
        assert_eq!(hits.len(), 3);
        assert_eq!(
            hits.iter().map(|(index, _)| *index).collect::<Vec<_>>(),
            vec![7, 8, 9]
        );
        for (index, rect) in &hits {
            assert!(row_text(&buffer, area, rect.y).contains(&format!("agent{index}")));
        }
    }

    #[test]
    fn an_empty_home_queue_renders_the_waiting_message_instead_of_a_blank_body() {
        let mut app = AppState::test_new();
        app.home = Some(crate::app::home::HomeState::default());
        let area = Rect::new(0, 0, 60, 6);

        let buffer = draw_home(&app, &[], area);
        let text = buffer
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(text.contains("nothing is waiting on you"), "{text:?}");
    }

    #[test]
    fn a_scrolled_home_queue_renders_only_the_rows_in_the_body_from_the_scroll_offset() {
        let mut app = AppState::test_new();
        let queue: Vec<_> = (0..8).map(blocked).collect();
        let mut home = crate::app::home::HomeState::default();
        for _ in 0..6 {
            home.select_next(&queue);
        }
        app.home = Some(home);
        let area = Rect::new(0, 0, 60, 6);

        let buffer = draw_home(&app, &queue, area);
        let body_rows: Vec<String> = (2..5).map(|row| row_text(&buffer, area, row)).collect();

        assert!(body_rows[0].contains("agent4"), "{body_rows:?}");
        assert!(body_rows[1].contains("agent5"), "{body_rows:?}");
        assert!(body_rows[2].contains("agent6"), "{body_rows:?}");
        assert!(body_rows.iter().all(|row| !row.contains("agent3")));
        assert!(body_rows.iter().all(|row| !row.contains("agent7")));
    }

    #[test]
    fn a_one_cell_home_area_does_not_panic_or_write_outside_its_width() {
        let mut app = AppState::test_new();
        app.home = Some(crate::app::home::HomeState::default());
        let queue = vec![blocked(0)];
        let area = Rect::new(0, 0, 1, 6);

        let buffer = draw_home(&app, &queue, area);

        assert_eq!(*buffer.area(), area);
        for row in 0..area.height {
            assert_eq!(row_text(&buffer, area, row).chars().count(), 1);
        }
    }

    #[test]
    fn the_composer_is_drawn_as_a_card_around_its_tall_prompt() {
        let mut app = AppState::test_new();
        app.home = Some(HomeState::default());
        let queue = vec![blocked(0)];
        let area = Rect::new(0, 0, 60, 25);
        let layout = bands(area, queue.len());
        let composer = layout.composer.expect("composer should fit");
        let buffer = draw_home(&app, &queue, area);

        assert_eq!(composer.frame.height, TALL_COMPOSER_CARD_ROWS);
        assert_eq!(composer.prompt.height, TALL_COMPOSER_PROMPT_ROWS);
        assert_eq!(buffer[(composer.frame.x, composer.frame.y)].symbol(), "┌");
        assert_eq!(
            buffer[(composer.frame.right() - 1, composer.frame.y)].symbol(),
            "┐"
        );
        assert_eq!(
            buffer[(composer.frame.x, composer.frame.bottom() - 1)].symbol(),
            "└"
        );
        // The prompt and three control rows sit inside the border, not on it.
        assert!(composer.prompt.y > composer.frame.y);
        assert!(composer.bottom.y < composer.frame.bottom() - 1);
        // The divider meets the border on both sides.
        assert_eq!(
            buffer[(composer.divider.x, composer.divider.y)].symbol(),
            "├"
        );
        assert_eq!(
            buffer[(composer.divider.right() - 1, composer.divider.y)].symbol(),
            "┤"
        );
        // And the card follows the headline, which follows the queue rather
        // than the pane floor.
        assert_eq!(composer.headline.y, layout.body.bottom());
        assert_eq!(composer.frame.y, composer.headline.bottom());
        assert!(composer.frame.bottom() <= layout.hint.y);
    }

    #[test]
    fn dropdowns_open_downward_from_their_field() {
        let mut app = AppState::test_new();
        let mut home = HomeState::default();
        home.focus = Some(HomeFocus::Access);
        home.picker = Some(HomePicker::Access);
        app.home = Some(home);
        let queue = [blocked(0)];
        let area = Rect::new(0, 0, 60, 24);
        let layout = bands(area, queue.len());
        let composer = layout.composer.expect("composer should fit");
        let home = app.home.as_ref().expect("home");
        let field =
            composer_field_rect(&app, home, composer, HomeFocus::Access).expect("access field");
        let popup = picker_popup_rect(&app, home, composer, area).expect("an open picker");

        assert_eq!(
            popup.y,
            field.bottom(),
            "dropdown top must match field bottom"
        );
        assert!(popup.bottom() <= area.bottom());
    }

    #[test]
    fn workspace_dropdown_opens_downward_and_renders_fixed_options_first() {
        let mut app = AppState::test_new();
        let home = HomeState::test_with_focus(HomeFocus::Workspace);
        app.home = Some(home);
        app.home_open_picker(HomePicker::Workspace);
        let queue = [blocked(0)];
        let area = Rect::new(0, 0, 100, 40);
        let layout = bands(area, queue.len());
        let composer = layout.composer.expect("composer should fit");
        let home = app.home.as_ref().expect("home");
        let field = composer_field_rect(&app, home, composer, HomeFocus::Workspace)
            .expect("workspace field");
        let popup = picker_popup_rect(&app, home, composer, area).expect("workspace picker");
        let buffer = draw_home(&app, &queue, area);

        assert_eq!(popup.y, field.bottom());
        assert!(popup.bottom() <= area.bottom());
        assert!(row_text(&buffer, popup, popup.y).contains("⌂ Current checkout"));
        assert!(row_text(&buffer, popup, popup.y + 1).contains("⎇ New worktree"));
    }

    #[test]
    fn composer_card_shows_pull_request_context() {
        let mut app = AppState::test_new();
        let mut home = HomeState::test_with_prompt("continue the review");
        home.pr = Some(crate::app::home::HomePrContext {
            url: "https://github.com/owner/repo/pull/42".into(),
            number: 42,
            repo: "owner/repo".into(),
        });
        app.home = Some(home);
        let area = Rect::new(0, 0, 100, 20);
        let composer = bands(area, 0).composer.expect("composer");

        let buffer = draw_home(&app, &[], area);

        assert!(row_text(&buffer, composer.frame, composer.bottom.y).contains("⑂ #42"));
    }

    #[test]
    fn composer_renders_worktree_progress_and_inline_failure() {
        let mut app = AppState::test_new();
        let mut home = HomeState::test_with_prompt("keep this prompt");
        home.pending_dispatch = Some(crate::app::home::HomeDispatchPlan {
            agent: crate::detect::Agent::Codex,
            model: "gpt-5.3-codex".into(),
            effort: Some("high".into()),
            directory: "/repo/herdr".into(),
            workspace: crate::app::home::HomeWorkspace::NewWorktree,
            git_ref: None,
            pr: None,
            ticket: None,
            missive: None,
            work_context_patch: crate::work_context::PaneWorkContextPatch::default(),
            target: HomeTarget::NewSpace,
            prompt: home.prompt.clone(),
            argv: vec!["codex".into(), "keep this prompt".into()],
            env: Vec::new(),
            remote: None,
        });
        app.home = Some(home);
        let queue = [blocked(0)];
        let area = Rect::new(0, 0, 100, 20);
        let composer = bands(area, queue.len()).composer.expect("composer");

        let creating = draw_home(&app, &queue, area);
        assert!(
            row_text(&creating, composer.frame, composer.bottom.y).contains("creating worktree…")
        );

        let home = app.home.as_mut().expect("home");
        home.pending_dispatch = None;
        home.dispatch_error = Some("branch already exists".into());
        let failed = draw_home(&app, &queue, area);
        assert!(row_text(&failed, composer.frame, composer.frame.bottom())
            .contains("branch already exists"));
    }

    #[test]
    fn composer_inset_matches_queue_content_and_click_targets() {
        let mut app = AppState::test_new();
        app.home = Some(HomeState::default());
        let queue = vec![blocked(0)];
        let area = Rect::new(0, 0, 100, 12);
        let layout = bands(area, queue.len());
        let composer = layout.composer.expect("composer should fit");
        let home = app.home.as_ref().expect("home");
        let buffer = draw_home(&app, &queue, area);
        let hits = home_hit_areas(&app, &queue, area);

        assert_eq!(buffer[(area.x + 1, layout.body.y)].symbol(), "○");
        assert_eq!(composer.prompt.x, composer.frame.x + 1);
        assert_eq!(composer.chips.x, composer.frame.x + 1);
        assert_eq!(composer.bottom.x, composer.frame.x + 1);
        let wide_chip_rects = chip_rects(&app, home, composer);
        assert!(wide_chip_rects
            .windows(2)
            .all(|pair| pair[0].1.right() + 3 == pair[1].1.x));
        let chip_row = row_text(&buffer, composer.frame, composer.chips.y);
        assert!(
            chip_row.contains("claude ▾ │ Claude Opus 5 ▾ │ auto ▾ │ bypass ▾ │ 1M ▾"),
            "the picker row should read agent │ model │ effort │ access │ context: {chip_row:?}"
        );
        assert!(
            chip_row.ends_with("[ ↵ ]│"),
            "the submit glyph is right-aligned inside the card: {chip_row:?}"
        );
        assert_eq!(
            hits.iter()
                .find(|hit| hit.target == HomeHitTarget::Prompt)
                .map(|hit| hit.rect),
            Some(composer.prompt)
        );
        assert_eq!(
            hits.iter()
                .find(|hit| hit.target == HomeHitTarget::Target)
                .map(|hit| hit.rect),
            secondary_rects(&app, home, composer)
                .into_iter()
                .find_map(|(focus, rect)| (focus == HomeFocus::Target).then_some(rect))
        );
        for (focus, rect) in chip_rects(&app, home, composer)
            .into_iter()
            .chain(secondary_rects(&app, home, composer))
        {
            let target = match focus {
                HomeFocus::Agent => HomeHitTarget::Agent,
                HomeFocus::Model => HomeHitTarget::Model,
                HomeFocus::Effort => HomeHitTarget::Effort,
                HomeFocus::Access => HomeHitTarget::Access,
                HomeFocus::Context => HomeHitTarget::Context,
                HomeFocus::Project => HomeHitTarget::Project,
                HomeFocus::Repo => HomeHitTarget::Repo,
                HomeFocus::Directory => HomeHitTarget::Directory,
                HomeFocus::Machine => HomeHitTarget::Machine,
                HomeFocus::Workspace => HomeHitTarget::Workspace,
                HomeFocus::Ref => HomeHitTarget::Ref,
                HomeFocus::Target => HomeHitTarget::Target,
                HomeFocus::Reply | HomeFocus::Prompt => continue,
            };
            assert_eq!(
                hits.iter()
                    .find(|hit| hit.target == target)
                    .map(|hit| hit.rect),
                Some(rect),
                "hit area for {focus:?} drifted from its chip"
            );
        }
    }

    #[test]
    fn open_picker_options_use_the_same_popup_rows_as_the_click_targets() {
        let mut app = AppState::test_new();
        let mut home = HomeState::default();
        home.focus = Some(HomeFocus::Agent);
        home.picker = Some(HomePicker::Agent);
        app.home = Some(home);
        let queue = vec![blocked(0)];
        let area = Rect::new(0, 0, 100, 12);
        let layout = bands(area, queue.len());
        let composer = layout.composer.expect("composer should fit");
        let home = app.home.as_ref().expect("home");
        let popup = picker_popup_rect(&app, home, composer, area).expect("picker should fit");
        let option_hits: Vec<_> = home_hit_areas(&app, &queue, area)
            .into_iter()
            .filter(|hit| matches!(hit.target, HomeHitTarget::PickerOption(_)))
            .collect();

        assert_eq!(option_hits.len(), 2);
        for (offset, hit) in option_hits.iter().enumerate() {
            assert_eq!(
                hit.rect,
                Rect::new(popup.x, popup.y + offset as u16, popup.width, 1)
            );
        }
    }

    #[test]
    fn a_short_home_frame_omits_the_composer_without_panic_or_overlap() {
        let mut app = AppState::test_new();
        app.home = Some(HomeState::default());
        let queue = vec![blocked(0)];
        let area = Rect::new(0, 0, 60, 6);

        let buffer = draw_home(&app, &queue, area);
        let hits = home_hit_areas(&app, &queue, area);
        let text = buffer
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(bands(area, queue.len()).composer.is_none());
        assert!(hits
            .iter()
            .all(|hit| matches!(hit.target, HomeHitTarget::QueueRow(_))));
        assert!(!text.contains("type a prompt"), "{text:?}");
    }

    #[test]
    fn a_compact_composer_keeps_one_prompt_row_without_overlapping_queue_or_hint() {
        let area = Rect::new(0, 0, 60, crate::app::home::HOME_COMPOSER_MIN_HEIGHT);
        let layout = bands(area, 1);
        let composer = layout.composer.expect("compact composer should fit");

        assert_eq!(composer.frame.height, COMPACT_COMPOSER_CARD_ROWS);
        assert_eq!(composer.prompt.height, 1);
        assert_eq!(layout.body.bottom(), composer.headline.y);
        assert_eq!(composer.headline.bottom(), composer.frame.y);
        assert!(composer.frame.bottom() <= layout.hint.y);
        assert_eq!(layout.hint.bottom(), area.bottom());
    }

    #[test]
    fn the_compact_composer_still_fits_at_the_old_normal_threshold() {
        let old_normal_threshold = crate::app::home::HOME_COMPOSER_MIN_HEIGHT + 2;
        let area = Rect::new(0, 0, 60, old_normal_threshold);
        let layout = bands(area, 1);
        let composer = layout.composer.expect("compact composer should fit");

        assert_eq!(composer.frame.height, COMPACT_COMPOSER_CARD_ROWS);
        assert_eq!(composer.prompt.height, 1);
        assert!(composer.frame.bottom() <= layout.hint.y);
    }

    #[test]
    fn the_prompt_hit_area_matches_the_drawn_rows_at_each_composer_height() {
        let mut app = AppState::test_new();
        app.home = Some(HomeState::default());
        let queue = vec![blocked(0)];
        for (height, expected_card_rows, expected_prompt_rows) in [
            (25, TALL_COMPOSER_CARD_ROWS, TALL_COMPOSER_PROMPT_ROWS),
            (20, 15, 10),
            (16, NORMAL_COMPOSER_CARD_ROWS, 6),
            (15, COMPACT_COMPOSER_CARD_ROWS, 1),
            (11, COMPACT_COMPOSER_CARD_ROWS, 1),
        ] {
            let area = Rect::new(0, 0, 60, height);
            let layout = bands(area, queue.len());
            let composer = layout.composer.expect("composer should fit");
            let buffer = draw_home(&app, &queue, area);
            let hit = home_hit_areas(&app, &queue, area)
                .into_iter()
                .find(|hit| hit.target == HomeHitTarget::Prompt)
                .expect("prompt hit area");

            assert_eq!(composer.frame.height, expected_card_rows);
            assert_eq!(composer.prompt.height, expected_prompt_rows);
            assert_eq!(hit.rect, composer.prompt);
            assert!(composer.frame.bottom() <= layout.hint.y);
            assert_eq!(buffer.area(), &area);
        }
    }

    #[test]
    fn a_wrapped_unicode_prompt_keeps_its_tail_and_cursor_visible() {
        let mut app = AppState::test_new();
        let mut home = HomeState::default();
        home.prompt =
            "开头一 开头二 开头三 开头四 开头五 开头六 开头七 开头八 开头九 开头十 开始 重构用户认证模块 继续检查边界 继续确认溢出滚动 最后尾部"
                .into();
        app.home = Some(home);
        let queue = vec![blocked(0)];
        let area = Rect::new(0, 0, 20, 17);
        let composer = bands(area, queue.len()).composer.expect("normal composer");
        let buffer = draw_home(&app, &queue, area);
        let prompt_text = (composer.prompt.y..composer.prompt.bottom())
            .map(|row| row_text(&buffer, area, row))
            .collect::<String>();
        let compact_text = prompt_text
            .chars()
            .filter(|character| !matches!(character, ' ' | '│'))
            .collect::<String>();

        assert!(compact_text.contains("尾部█"), "{prompt_text:?}");
        assert!(!compact_text.contains("开头一"), "{prompt_text:?}");
    }

    #[test]
    fn home_tab_order_includes_effort_and_supported_context_options() {
        let mut home = HomeState::default();
        // Haiku carries no 1M window, so this pass covers the shorter cycle.
        home.set_model("claude-haiku-4-5-20251001");
        let claude_order = [
            HomeFocus::Agent,
            HomeFocus::Model,
            HomeFocus::Effort,
            HomeFocus::Access,
            HomeFocus::Directory,
            HomeFocus::Workspace,
            HomeFocus::Ref,
            HomeFocus::Target,
            HomeFocus::Prompt,
        ];
        for expected in claude_order {
            home.move_focus(false);
            assert_eq!(home.focus, Some(expected));
        }

        home.set_model("claude-fable-5-1");
        home.focus = Some(HomeFocus::Effort);
        home.move_focus(false);
        assert_eq!(home.focus, Some(HomeFocus::Access));
        home.move_focus(false);
        assert_eq!(home.focus, Some(HomeFocus::Context));
        home.move_focus(false);
        assert_eq!(home.focus, Some(HomeFocus::Directory));
        home.move_focus(true);
        assert_eq!(home.focus, Some(HomeFocus::Context));

        home.set_agent(crate::detect::Agent::Codex);
        home.focus = Some(HomeFocus::Model);
        home.move_focus(false);
        assert_eq!(home.focus, Some(HomeFocus::Effort));
        home.move_focus(false);
        assert_eq!(home.focus, Some(HomeFocus::Access));
        home.move_focus(false);
        assert_eq!(home.focus, Some(HomeFocus::Directory));
        home.move_focus(true);
        assert_eq!(home.focus, Some(HomeFocus::Access));
    }

    #[test]
    fn home_tab_order_includes_each_agents_supported_effort_options() {
        let mut home = HomeState::default();
        home.focus = Some(HomeFocus::Model);
        home.move_focus(false);
        assert_eq!(home.focus, Some(HomeFocus::Effort));

        home.set_model("claude-fable-5-1");
        home.focus = Some(HomeFocus::Effort);
        home.move_focus(false);
        assert_eq!(home.focus, Some(HomeFocus::Access));
        home.move_focus(false);
        assert_eq!(home.focus, Some(HomeFocus::Context));

        home.set_agent(crate::detect::Agent::Codex);
        home.focus = Some(HomeFocus::Model);
        home.move_focus(false);
        assert_eq!(home.focus, Some(HomeFocus::Effort));
        home.move_focus(false);
        assert_eq!(home.focus, Some(HomeFocus::Access));
        home.move_focus(false);
        assert_eq!(home.focus, Some(HomeFocus::Directory));
    }

    #[test]
    fn narrow_composer_collapses_chip_gaps_and_keeps_click_targets_in_bounds() {
        let mut app = AppState::test_new();
        let mut home = HomeState::default();
        // A model without a 1M window, so this stays the four-chip row the
        // elision assertions below are written against.
        home.set_model("claude-haiku-4-5-20251001");
        app.home = Some(home);
        let queue = vec![blocked(0)];
        let area = Rect::new(0, 0, 24, 12);
        let composer = bands(area, queue.len())
            .composer
            .expect("composer should fit");
        let home = app.home.as_ref().expect("home");
        let rects = chip_rects(&app, home, composer);
        let buffer = draw_home(&app, &queue, area);
        let chip_row = row_text(&buffer, area, composer.chips.y);

        // The row keeps all four pickers, elided rather than dropped.
        assert_eq!(rects.len(), 4);
        assert_eq!(chip_row.matches('…').count(), 4, "{chip_row:?}");
        assert!(chip_row.matches("… ▾").count() >= 3, "{chip_row:?}");
        assert!(!chip_row.contains("clau"), "{chip_row:?}");
        assert!(!chip_row.contains("defa"), "{chip_row:?}");
        assert_eq!(rects[0].1.right() + 1, rects[1].1.x);
        assert!(rects.windows(2).all(|pair| pair[0].1.right() < pair[1].1.x));
        assert!(rects.iter().all(|(_, rect)| rect.right() <= area.right()));

        let hits = home_hit_areas(&app, &queue, area);
        for (focus, rect) in rects {
            let target = match focus {
                HomeFocus::Agent => HomeHitTarget::Agent,
                HomeFocus::Model => HomeHitTarget::Model,
                HomeFocus::Effort => HomeHitTarget::Effort,
                HomeFocus::Access => HomeHitTarget::Access,
                HomeFocus::Context => HomeHitTarget::Context,
                HomeFocus::Project => HomeHitTarget::Project,
                HomeFocus::Repo => HomeHitTarget::Repo,
                HomeFocus::Directory => HomeHitTarget::Directory,
                HomeFocus::Machine => HomeHitTarget::Machine,
                HomeFocus::Workspace => HomeHitTarget::Workspace,
                HomeFocus::Ref => HomeHitTarget::Ref,
                HomeFocus::Target => HomeHitTarget::Target,
                HomeFocus::Reply | HomeFocus::Prompt => continue,
            };
            assert_eq!(
                hits.iter()
                    .find(|hit| hit.target == target)
                    .map(|hit| hit.rect),
                Some(rect)
            );
        }
    }

    #[test]
    fn a_long_picker_keeps_selection_visible_and_uses_absolute_hit_indexes() {
        let mut app = AppState::test_new();
        for index in 0..12 {
            let mut workspace = Workspace::test_new(&format!("picker-{index}"));
            workspace.identity_cwd = format!("/tmp/home-picker-{index}").into();
            app.workspaces.push(workspace);
        }
        let mut home = HomeState::default();
        home.focus = Some(HomeFocus::Directory);
        home.picker = Some(HomePicker::Directory);
        home.directory_filter.selected = app.home_directory_options().len().saturating_sub(1);
        let selected = home.directory_filter.selected;
        app.home = Some(home);
        let queue = vec![blocked(0)];
        let area = Rect::new(0, 0, 50, 12);
        let composer = bands(area, queue.len()).composer.expect("composer");
        let home = app.home.as_ref().expect("home");
        let dropdown = picker_viewport(&app, home, composer, area).expect("picker");
        let popup = dropdown.rect;
        let start = dropdown.first_visible;
        let hits = home_hit_areas(&app, &queue, area);
        let option_hits = hits
            .iter()
            .filter_map(|hit| match hit.target {
                HomeHitTarget::PickerOption(index) => Some((index, hit.rect)),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert!(start > 0);
        assert_eq!(option_hits.first().map(|(index, _)| *index), Some(start));
        assert_eq!(option_hits.last().map(|(index, _)| *index), Some(selected));
        assert_eq!(option_hits.len(), dropdown.visible_rows);
        let field = composer_field_rect(&app, home, composer, HomeFocus::Directory)
            .expect("directory field");
        assert_eq!(popup.y, field.bottom());
        assert!(popup.bottom() <= area.bottom());
    }

    #[test]
    fn directory_picker_filter_narrows_and_accepts_absolute_index() {
        let mut app = AppState::test_new();
        app.workspaces.clear();
        for (id, directory) in [
            ("first", "/tmp/absolute-first-match"),
            ("second", "/tmp/absolute-second-match"),
        ] {
            let mut workspace = Workspace::test_new(id);
            workspace.identity_cwd = directory.into();
            app.workspaces.push(workspace);
        }
        app.home = Some(HomeState::default());
        app.home_open_picker(HomePicker::Directory);
        for character in "absolutematch".chars() {
            app.home_push_picker_filter(character);
        }

        let home = app.home.as_ref().expect("home");
        let labels = picker_labels(&app, home, HomePicker::Directory);
        let matches = picker_matches(&app, home, HomePicker::Directory, &labels);
        assert_eq!(
            matches
                .iter()
                .map(|(absolute_index, _)| *absolute_index)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(home.directory_filter.selected, 0);

        app.home_move_picker(1);
        app.home_accept_picker();

        let home = app.home.as_ref().expect("home");
        assert_eq!(
            home.directory,
            std::path::PathBuf::from("/tmp/absolute-second-match")
        );
        assert!(home.picker.is_none());
    }

    #[test]
    fn unicode_model_labels_do_not_push_effort_or_access_out_of_a_narrow_row() {
        let mut home = HomeState::default();
        home.set_model("claude-haiku-4-5-20251001");
        home.model = "模型六点一".into();
        let area = Rect::new(0, 0, 24, 1);
        let rects = chip_rects(&AppState::test_new(), &home, chip_row_bands(area));

        assert_eq!(rects.len(), 4);
        assert_eq!(
            rects.last().map(|(focus, _)| *focus),
            Some(HomeFocus::Access)
        );
        assert!(rects.windows(2).all(|pair| pair[0].1.right() < pair[1].1.x));
        assert!(rects.iter().all(|(_, rect)| rect.right() <= area.right()));
    }

    #[test]
    fn escape_leaves_the_composer_before_closing_home() {
        let mut home = HomeState::default();

        assert!(!home.close_composer_or_home());
        assert_eq!(home.focus, None);
        assert!(home.picker.is_none());
        assert!(home.close_composer_or_home());
    }

    #[test]
    fn typing_in_the_prompt_does_not_move_queue_selection() {
        let queue = vec![blocked(0), blocked(1)];
        let mut home = HomeState::default();
        home.select(1);

        home.append_prompt('j');

        assert_eq!(home.prompt, "j");
        assert_eq!(home.selected(&queue), 1);
    }

    #[test]
    fn the_lens_follows_the_queue_cursor() {
        let mut app = AppState::test_new();
        let queue = vec![blocked(0), blocked(1)];
        let mut home = HomeState::default();
        home.focus = None;
        app.home = Some(home);
        let runtimes = crate::terminal::TerminalRuntimeRegistry::new();

        let first = lens_snapshot(&app, &runtimes, &queue, 2).expect("first lens");
        app.home.as_mut().expect("home").select(1);
        let second = lens_snapshot(&app, &runtimes, &queue, 2).expect("second lens");

        assert!(first.title.contains("agent0"));
        assert!(second.title.contains("agent1"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn the_lens_renders_the_tail_returned_by_pane_read() {
        let app = app_with_lens_screen(b"old line\r\nretry cap\r\nwhich do you want?\r\n");
        let queue = app.home_attention_agents();
        let area = Rect::new(0, 0, 90, 12);
        let runtimes = crate::terminal::TerminalRuntimeRegistry::new();
        let expected = lens_snapshot(&app, &runtimes, &queue, 2)
            .expect("lens")
            .output;
        let buffer = draw_home(&app, &queue, area);
        let rendered = buffer
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(expected.contains("which do you want?"), "{expected:?}");
        assert!(rendered.contains("which do you want?"), "{rendered:?}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn the_lens_renders_a_clickable_new_task_action() {
        let app = app_with_lens_screen(b"which do you want?\r\n");
        let queue = app.home_attention_agents();
        let area = Rect::new(0, 0, 90, 12);
        let buffer = draw_home(&app, &queue, area);
        let rendered = buffer
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        let action = home_hit_areas(&app, &queue, area)
            .into_iter()
            .find(|hit| hit.target == HomeHitTarget::NewTask)
            .expect("new task hit area");

        assert!(rendered.contains("new task [n]"), "{rendered:?}");
        assert_eq!(action.rect.height, 1);
        assert_eq!(action.rect.width, 12);
    }

    #[test]
    fn a_short_lens_frame_omits_the_lens_without_panic() {
        let mut app = AppState::test_new();
        let queue = vec![blocked(0)];
        let mut home = HomeState::default();
        home.focus = None;
        app.home = Some(home);
        let area = Rect::new(0, 0, 60, 6);

        let buffer = draw_home(&app, &queue, area);
        let layout = home_bands(&app, &queue, area);
        let hits = home_hit_areas(&app, &queue, area);

        assert!(layout.lens.is_none());
        assert!(hits
            .iter()
            .all(|hit| matches!(hit.target, HomeHitTarget::QueueRow(_))));
        assert_eq!(*buffer.area(), area);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_missing_or_empty_lens_degrades_to_a_reason() {
        let mut gone = AppState::test_new();
        let queue = vec![blocked(0)];
        let mut home = HomeState::default();
        home.focus = None;
        gone.home = Some(home);
        let runtimes = crate::terminal::TerminalRuntimeRegistry::new();

        assert_eq!(
            lens_snapshot(&gone, &runtimes, &queue, 2)
                .expect("lens")
                .reason
                .as_deref(),
            Some("pane is no longer available")
        );

        let empty = app_with_lens_screen(b"");
        let empty_queue = empty.home_attention_agents();
        assert_eq!(
            lens_snapshot(&empty, &runtimes, &empty_queue, 2)
                .expect("lens")
                .reason
                .as_deref(),
            Some("no output yet")
        );

        let mut no_longer_blocked = app_with_lens_screen(b"");
        let stale_queue = no_longer_blocked.home_attention_agents();
        no_longer_blocked
            .terminals
            .get_mut(&stale_queue[0].terminal_id)
            .expect("test terminal")
            .set_raw_agent_state_for_test(crate::detect::AgentState::Idle);
        assert_eq!(
            lens_snapshot(&no_longer_blocked, &runtimes, &stale_queue, 2)
                .expect("lens")
                .reason
                .as_deref(),
            Some("pane is no longer waiting on you")
        );
    }

    /// 1a-5: the card is centred and unclipped, and an open dropdown stays
    /// inside the main area — which is disjoint from the sidebar rect, so it
    /// can never draw over it.
    #[test]
    fn the_card_stays_centred_and_dropdowns_stay_inside_the_main_area_at_both_sizes() {
        for (columns, rows) in [(80u16, 24u16), (120, 40)] {
            let sidebar = Rect::new(0, 0, 24, rows);
            let area = Rect::new(sidebar.right(), 0, columns - sidebar.width, rows);
            let mut app = AppState::test_new();
            let mut home = HomeState::default();
            home.focus = Some(HomeFocus::Ref);
            home.picker = Some(HomePicker::Ref);
            home.ref_repo_root = Some("/repo".into());
            home.ref_directory = "/repo".into();
            app.home = Some(home);
            app.home_ref_cache.insert(
                "/repo".into(),
                crate::app::home_refs::parse_ref_cache(
                    "refs/heads/main\t1234567\t1\n",
                    "worktree /repo\nHEAD 1234567890\nbranch refs/heads/main\n\n",
                    "main\n",
                ),
            );
            let queue = vec![blocked(0)];
            let layout = home_bands(&app, &queue, area);
            let composer = layout
                .composer
                .unwrap_or_else(|| panic!("composer should fit at {columns}x{rows}"));

            // Centred, inside the area, and clear of the queue and the hint.
            assert!(composer.frame.width <= area.width);
            assert_eq!(
                composer.frame.x - area.x,
                area.right() - composer.frame.right(),
                "the card is off centre at {columns}x{rows}"
            );
            assert!(composer.frame.right() <= area.right());
            assert!(composer.headline.y >= layout.body.bottom());
            assert!(composer.frame.bottom() <= layout.hint.y);
            assert!(composer.bottom.right() <= composer.frame.right());
            assert!(composer.submit.right() <= composer.frame.right());

            let home = app.home.as_ref().expect("home");
            let dropdown = picker_viewport(&app, home, composer, area)
                .unwrap_or_else(|| panic!("ref dropdown should fit at {columns}x{rows}"));
            let field =
                composer_field_rect(&app, home, composer, HomeFocus::Ref).expect("ref field");
            assert_eq!(dropdown.rect.y, field.bottom(), "dropdowns open downward");
            assert!(
                dropdown.rect.x >= area.x,
                "dropdown crossed into the sidebar"
            );
            assert!(dropdown.rect.right() <= area.right());
            assert!(dropdown.rect.bottom() <= area.bottom());
            assert!(
                dropdown.rect.x >= sidebar.right(),
                "dropdown overlaps the sidebar at {columns}x{rows}"
            );

            // Nothing clips: the same geometry drawn into a buffer of exactly
            // that size writes every card row inside it.
            let origin = Rect::new(0, 0, area.width, area.height);
            let origin_composer = home_bands(&app, &queue, origin)
                .composer
                .unwrap_or_else(|| panic!("composer should fit at {columns}x{rows}"));
            let buffer = draw_home(&app, &queue, origin);
            assert_eq!(*buffer.area(), origin);
            assert!(row_text(&buffer, origin, origin_composer.headline.y)
                .contains("What should we build in"));
            assert!(
                row_text(&buffer, origin_composer.frame, origin_composer.bottom.y)
                    .contains("Current checkout"),
                "the workspace field clipped at {columns}x{rows}"
            );
        }
    }

    #[test]
    fn picker_labels_are_word_safe_at_80_and_120_columns() {
        for columns in [80u16, 120] {
            let area = Rect::new(0, 0, columns.saturating_sub(24), 24);
            let mut app = AppState::test_new();
            let mut home = HomeState::default();
            home.set_model("claude-fable-5-1");
            app.home = Some(home);
            let queue = vec![blocked(0)];
            let composer = bands(area, queue.len()).composer.expect("composer");
            let buffer = draw_home(&app, &queue, area);
            let row = row_text(&buffer, composer.frame, composer.chips.y);

            for label in ["Fable 5.1 ▾", "auto ▾", "bypass ▾", "1M ▾"] {
                assert!(
                    row.contains(label),
                    "picker row at {columns} columns: {row:?}"
                );
            }
            assert!(!row.to_ascii_lowercase().contains("fablet"), "{row:?}");
        }

        assert_eq!(picker_row_label("Claude Fable 5.1 ▾", 11), "Fable 5.1 ▾");
        assert_eq!(picker_row_label("Claude Fable 5.1 ▾", 8), "Fable… ▾");
    }

    /// G-7: the headline picker is tall enough to reach the bottom, so it
    /// clamps above the hint line instead of drawing over it.
    #[test]
    fn the_headline_dropdown_clamps_above_the_hint_line() {
        for (columns, rows) in [(80u16, 24u16), (120, 40)] {
            let mut app = AppState::test_new();
            app.home = Some(HomeState::test_with_focus(HomeFocus::Directory));
            for index in 0..40 {
                let mut workspace = Workspace::test_new(&format!("space-{index}"));
                workspace.identity_cwd =
                    std::path::PathBuf::from(format!("/tmp/t3-f2-many/checkout-{index}"));
                app.workspaces.push(workspace);
            }
            app.home_open_picker(HomePicker::Directory);
            let queue = [blocked(0)];
            let area = Rect::new(0, 0, columns, rows);
            let layout = bands(area, queue.len());
            let composer = layout.composer.expect("composer");
            let home = app.home.as_ref().expect("home");

            let dropdown = picker_viewport(&app, home, composer, area)
                .unwrap_or_else(|| panic!("directory dropdown at {columns}x{rows}"));
            assert!(
                dropdown.rect.bottom() <= layout.hint.y,
                "the dropdown covered the hint line at {columns}x{rows}: {:?}",
                dropdown.rect
            );
            assert!(
                dropdown.rect.y > composer.headline.y,
                "the dropdown opens below the headline, never above it"
            );
        }
    }

    #[test]
    fn model_picker_uses_display_names_instead_of_cli_ids() {
        let mut app = AppState::test_new();
        let mut home = HomeState::default();
        home.focus = Some(HomeFocus::Model);
        home.picker = Some(HomePicker::Model);
        app.home = Some(home);
        let labels = picker_labels(&app, app.home.as_ref().expect("home"), HomePicker::Model);

        assert_eq!(
            labels,
            [
                "default",
                "Claude Fable 5.1",
                "Claude Opus 5",
                "Claude Sonnet 5",
                "Claude Haiku 4.5",
            ]
        );
    }

    #[test]
    fn lane_picker_rows_carry_quota_and_stay_bare_without_a_snapshot() {
        use crate::provider_usage::QuotaWindow;

        let mut app = AppState::test_new();
        let home = HomeState::default();
        let bare = picker_labels(&app, &home, HomePicker::Agent);
        assert!(
            bare.iter().all(|label| !label.contains(' ')),
            "no collected usage must leave the lane names untouched: {bare:?}"
        );

        app.provider_usage
            .primary_usage_mut(crate::provider_usage::QuotaProvider::Claude)
            .five_hour = Some(QuotaWindow {
            used_percent: 40,
            resets_at: None,
        });
        app.provider_usage
            .primary_usage_mut(crate::provider_usage::QuotaProvider::Claude)
            .seven_day = Some(QuotaWindow {
            used_percent: 92,
            resets_at: None,
        });
        let claude = picker_labels(&app, &home, HomePicker::Agent)
            .into_iter()
            .find(|label| label.starts_with("claude"))
            .expect("a claude lane");
        // Only the window past the escalation point spells its number out.
        assert!(claude.ends_with("92"), "{claude}");
    }

    #[test]
    fn access_picker_uses_provider_options_and_hides_without_a_mapping() {
        let mut app = AppState::test_new();
        let mut home = HomeState::default();
        home.picker = Some(HomePicker::Access);
        assert_eq!(
            picker_labels(&app, &home, HomePicker::Access),
            ["default", "accept edits", "plan", "bypass"]
        );

        home.set_agent(crate::detect::Agent::Codex);
        assert_eq!(
            picker_labels(&app, &home, HomePicker::Access),
            ["read-only", "workspace-write", "full"]
        );

        home.set_agent(crate::detect::Agent::Gemini);
        app.home = Some(home);
        let composer = bands(Rect::new(0, 0, 96, 20), 0)
            .composer
            .expect("composer");
        let home = app.home.as_ref().expect("home");
        assert!(home.access_options().is_empty());
        assert!(!chip_specs(&app, home, composer)
            .iter()
            .any(|(focus, _)| *focus == HomeFocus::Access));
    }

    /// 1a-1: the card reads headline, prompt, pickers, divider, workspace/ref.
    #[test]
    fn the_card_renders_the_headline_pickers_divider_and_workspace_ref_row() {
        let mut app = AppState::test_new();
        app.home = Some(HomeState::default());
        let queue = vec![blocked(0)];
        let area = Rect::new(0, 0, 96, 20);
        let composer = bands(area, queue.len()).composer.expect("composer");
        let buffer = draw_home(&app, &queue, area);
        let card_row = |row: u16| row_text(&buffer, composer.frame, row);

        assert!(
            card_row(composer.headline.y).contains("What should we build in"),
            "{:?}",
            card_row(composer.headline.y)
        );
        assert!(card_row(composer.prompt.y).contains('█'));
        let chips = card_row(composer.chips.y);
        assert!(
            chips.contains("claude ▾ │ Claude Opus 5 ▾ │ auto ▾ │ bypass ▾ │ 1M ▾"),
            "{chips:?}"
        );
        assert!(chips.ends_with("[ ↵ ]│"), "{chips:?}");
        assert!(card_row(composer.divider.y).starts_with('├'));
        assert!(card_row(composer.divider.y).ends_with('┤'));
        let bottom = card_row(composer.bottom.y);
        assert!(bottom.contains("⌂ Current checkout ▾"), "{bottom:?}");
        assert!(bottom.contains("⎇ current branch ▾"), "{bottom:?}");
        let workspace = secondary_rects(&app, app.home.as_ref().expect("home"), composer)
            .into_iter()
            .find_map(|(focus, rect)| (focus == HomeFocus::Workspace).then_some(rect))
            .expect("workspace field");
        let git_ref = secondary_rects(&app, app.home.as_ref().expect("home"), composer)
            .into_iter()
            .find_map(|(focus, rect)| (focus == HomeFocus::Ref).then_some(rect))
            .expect("ref field");
        assert_eq!(workspace.x, composer.bottom.x, "workspace is leftmost");
        assert_eq!(git_ref.right(), composer.bottom.right(), "ref is rightmost");
    }

    /// 1a-2: the headline names the repo when a pane in that directory knows
    /// one, and the directory otherwise.
    #[test]
    fn the_headline_prefers_the_observed_repo_over_the_directory_basename() {
        let mut app = AppState::test_new();
        let mut workspace = Workspace::test_new("headline");
        workspace.identity_cwd = std::path::PathBuf::from("/tmp/t3-1a-checkout");
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let mut home = HomeState::default();
        home.directory = std::path::PathBuf::from("/tmp/t3-1a-checkout");
        app.home = Some(home);

        // No pane has observed a repo yet, so the directory names the work.
        assert_eq!(app.home_headline_name(), "t3-1a-checkout");

        let terminal_id = app.workspaces[0].tabs[0]
            .panes
            .values()
            .next()
            .expect("pane")
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&terminal_id)
            .expect("terminal")
            .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                repo: Some("matthias-scale/herdr".into()),
                ..Default::default()
            })
            .expect("declaring a repo on a test terminal");

        assert_eq!(app.home_headline_name(), "herdr");

        // A directory nothing is running in falls back to its basename again.
        app.home.as_mut().expect("home").directory = std::path::PathBuf::from("/tmp/elsewhere");
        assert_eq!(app.home_headline_name(), "elsewhere");
    }

    /// 1a-3: `Tab` and `Shift+Tab` reach the two new pickers in order.
    #[test]
    fn the_focus_cycle_reaches_workspace_and_ref_in_both_directions() {
        let mut home = HomeState::default();
        let forward = [
            HomeFocus::Agent,
            HomeFocus::Model,
            HomeFocus::Effort,
            HomeFocus::Access,
            HomeFocus::Context,
            HomeFocus::Directory,
            HomeFocus::Workspace,
            HomeFocus::Ref,
            HomeFocus::Target,
            HomeFocus::Prompt,
        ];
        for expected in forward {
            home.move_focus(false);
            assert_eq!(home.focus, Some(expected));
        }

        let backward = [
            HomeFocus::Target,
            HomeFocus::Ref,
            HomeFocus::Workspace,
            HomeFocus::Directory,
            HomeFocus::Context,
            HomeFocus::Access,
            HomeFocus::Effort,
            HomeFocus::Model,
            HomeFocus::Agent,
            HomeFocus::Prompt,
        ];
        for expected in backward {
            home.move_focus(true);
            assert_eq!(home.focus, Some(expected));
        }

        assert_eq!(
            HomePicker::for_focus(HomeFocus::Workspace),
            Some(HomePicker::Workspace)
        );
        assert_eq!(HomePicker::for_focus(HomeFocus::Ref), Some(HomePicker::Ref));
    }

    /// 1a-4: `Shift+Enter` puts a newline in the prompt and the prompt box
    /// shows it as a second line.
    #[test]
    fn a_newline_in_the_prompt_renders_as_a_second_line() {
        let mut app = AppState::test_new();
        let mut home = HomeState::default();
        home.append_prompt('a');
        home.append_prompt('\n');
        home.append_prompt('b');
        app.home = Some(home);
        let queue = vec![blocked(0)];
        let area = Rect::new(0, 0, 96, 20);
        let composer = bands(area, queue.len()).composer.expect("composer");
        let buffer = draw_home(&app, &queue, area);

        assert!(row_text(&buffer, composer.frame, composer.prompt.y).contains('a'));
        assert!(row_text(&buffer, composer.frame, composer.prompt.y + 1).contains("b█"));
    }

    #[test]
    fn the_workspace_picker_puts_current_then_new_first() {
        let mut app = AppState::test_new();
        app.home = Some(HomeState::default());

        app.home_open_picker(HomePicker::Workspace);
        assert_eq!(
            app.home.as_ref().and_then(|home| home.picker),
            Some(HomePicker::Workspace)
        );
        assert_eq!(
            &app.home_workspace_options()[..2],
            &[
                crate::app::home::HomeWorkspace::CurrentCheckout,
                crate::app::home::HomeWorkspace::NewWorktree,
            ]
        );
        assert!(app.home_ref_options().is_empty());
        assert_eq!(app.home_ref_label(), "current branch");
        app.home_accept_picker();
        assert!(app.home.as_ref().and_then(|home| home.picker).is_none());
    }

    #[test]
    fn composer_and_lens_precedence_is_exclusive_and_focus_driven() {
        let mut app = AppState::test_new();
        let queue = vec![blocked(0)];
        app.home = Some(HomeState::default());
        let area = Rect::new(0, 0, 90, 12);

        let composer = home_bands(&app, &queue, area);
        assert!(composer.composer.is_some());
        assert!(composer.lens.is_none());

        app.home.as_mut().expect("home").focus = None;
        let lens = home_bands(&app, &queue, area);
        assert!(lens.composer.is_none());
        assert!(lens.lens.is_some());

        app.home.as_mut().expect("home").focus = Some(HomeFocus::Agent);
        let composer_again = home_bands(&app, &queue, area);
        assert!(composer_again.composer.is_some());
        assert!(composer_again.lens.is_none());
    }

    #[test]
    fn the_headline_name_is_the_directory_field_and_opens_its_picker_below_the_headline() {
        let mut app = AppState::test_new();
        let mut home = HomeState::test_with_focus(HomeFocus::Directory);
        // Keep this layout test independent of how many linked worktrees the
        // repository running the test happens to have.
        home.directory = std::env::temp_dir();
        app.home = Some(home);
        app.home_open_picker(HomePicker::Directory);
        let queue = [blocked(0)];
        let area = Rect::new(0, 0, 100, 40);
        let layout = bands(area, queue.len());
        let composer = layout.composer.expect("composer should fit");
        let home = app.home.as_ref().expect("home");
        let field =
            composer_field_rect(&app, home, composer, HomeFocus::Directory).expect("name field");
        let popup = picker_popup_rect(&app, home, composer, area).expect("directory picker");
        let buffer = draw_home(&app, &queue, area);
        let hits = home_hit_areas(&app, &queue, area);

        // The field is the underlined name inside the headline, not a chip.
        assert_eq!(field.y, composer.headline.y);
        assert!(field.y < composer.frame.y);
        let headline = row_text(&buffer, area, composer.headline.y);
        let name = app.home_headline_name();
        assert!(headline.contains(&format!("What should we build in {name}?")));
        assert_eq!(
            row_text(&buffer, field, field.y).trim(),
            name,
            "the field covers exactly the name"
        );
        assert_eq!(
            hits.iter()
                .find(|hit| hit.target == HomeHitTarget::Directory)
                .map(|hit| hit.rect),
            Some(field),
            "clicking the name is what opens the picker"
        );

        // And the picker opens below the headline, never above it.
        assert_eq!(popup.y, field.bottom());
        assert!(popup.bottom() <= area.bottom());
        assert!(
            row_text(&buffer, popup, popup.y).contains('/'),
            "filter line"
        );
        assert!(
            (popup.y..popup.bottom()).any(
                |y| row_text(&buffer, popup, y).contains(crate::app::home::BROWSE_OPTION_LABEL)
            ),
            "the last option is browse"
        );
    }

    #[test]
    fn the_bottom_row_drops_the_folder_field_and_keeps_workspace_target_and_ref() {
        let mut app = AppState::test_new();
        app.home = Some(HomeState::default());
        let queue = [blocked(0)];
        for width in [80u16, 120] {
            let area = Rect::new(0, 0, width, 40);
            let composer = bands(area, queue.len()).composer.expect("composer");
            let home = app.home.as_ref().expect("home");
            let focuses = secondary_specs(&app, home, composer)
                .into_iter()
                .map(|(focus, _)| focus)
                .collect::<Vec<_>>();

            assert_eq!(
                focuses,
                vec![HomeFocus::Workspace, HomeFocus::Target, HomeFocus::Ref],
                "at {width} columns"
            );
            let bottom = row_text(&draw_home(&app, &queue, area), area, composer.bottom.y);
            assert!(bottom.contains("⌂ Current checkout ▾"), "{bottom:?}");
            assert!(bottom.contains("⎇ "), "{bottom:?}");
            assert!(
                !bottom.contains('~') && !bottom.contains("Folder"),
                "the folder field is gone: {bottom:?}"
            );
        }
    }

    #[test]
    fn a_narrow_bottom_row_moves_the_target_to_the_picker_row() {
        let mut app = AppState::test_new();
        app.home = Some(HomeState::default());
        let queue = [blocked(0)];
        let area = Rect::new(0, 0, 46, 40);
        let composer = bands(area, queue.len()).composer.expect("composer");
        let home = app.home.as_ref().expect("home");

        let bottom = secondary_specs(&app, home, composer)
            .into_iter()
            .map(|(focus, _)| focus)
            .collect::<Vec<_>>();
        let chips = chip_specs(&app, home, composer)
            .into_iter()
            .map(|(focus, _)| focus)
            .collect::<Vec<_>>();

        assert_eq!(bottom, vec![HomeFocus::Workspace, HomeFocus::Ref]);
        assert!(chips.contains(&HomeFocus::Target), "{chips:?}");
        assert_eq!(
            composer_field_rect(&app, home, composer, HomeFocus::Target).map(|rect| rect.y),
            Some(composer.chips.y),
            "the field follows the label it moved with"
        );
    }

    #[test]
    fn browse_replaces_the_filter_line_with_the_typed_path_and_lists_its_children() {
        let root =
            std::env::temp_dir().join(format!("herdr-home-browse-ui-{}", std::process::id()));
        std::fs::create_dir_all(root.join("alpha")).expect("fixture");
        std::fs::create_dir_all(root.join("beta")).expect("fixture");
        let mut app = AppState::test_new();
        let mut home = HomeState::test_with_focus(HomeFocus::Directory);
        home.directory = root.clone();
        app.home = Some(home);
        app.home_open_picker(HomePicker::Directory);
        if let Some(home) = app.home.as_mut() {
            home.browse = Some(crate::app::home::HomeBrowse::starting_at(&root));
        }
        let queue = [blocked(0)];
        let area = Rect::new(0, 0, 120, 40);
        let composer = bands(area, queue.len()).composer.expect("composer");
        let home = app.home.as_ref().expect("home");
        let popup = picker_popup_rect(&app, home, composer, area).expect("browse popup");
        let buffer = draw_home(&app, &queue, area);

        assert!(
            row_text(&buffer, popup, popup.y).contains('›'),
            "the filter line became a path input: {:?}",
            row_text(&buffer, popup, popup.y)
        );
        assert!(row_text(&buffer, popup, popup.y + 1).contains("alpha"));
        assert!(row_text(&buffer, popup, popup.y + 2).contains("beta"));

        let _ = std::fs::remove_dir_all(&root);
    }
}
