use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph},
    Frame,
};

use super::text::{display_width, display_width_u16, truncate_end};
use super::widgets::{panel_contrast_fg, readable_fg_on};
use crate::app::AppState;

/// The pin lives in the leading pad column every tab cell already renders, so
/// the affordance costs no width and never touches the label: a pinned tab
/// shows `*` where an unpinned one shows the blank that was always there. The
/// click target is that column either way -- an icon you can only hit once it
/// is already on would be no way to turn it on.
pub(crate) const PIN_GLYPH_ON: char = '*';
pub(crate) const PIN_GLYPH_OFF: char = ' ';

const MIN_TAB_WIDTH: u16 = 8;
/// One cell of padding on each side of the glyph, matching `+` and the scroll
/// arrows so the whole trailing control group has the same click rhythm.
pub(crate) const PANE_TOGGLE_BUTTON_WIDTH: u16 = 3;
pub(crate) const PANE_TOGGLE_BELOW_GLYPH: char = '\u{25ad}';
pub(crate) const PANE_TOGGLE_RIGHT_GLYPH: char = '\u{25af}';
pub(crate) const GIT_MENU_BUTTON_WIDTH: u16 = 10;
#[cfg(test)]
pub(crate) const REPO_EDITOR_BUTTON_WIDTH: u16 = 6;
#[cfg(test)]
pub(crate) const ADD_ACTION_BUTTON_WIDTH: u16 = 10;
pub(crate) const GIT_MENU_STATUS: &str = "⚠ Behind upstream. Pull first.";
pub(crate) const GIT_MENU_UNAVAILABLE: &str = "Not a git repository";
const NEW_TAB_WIDTH: u16 = 3;
const TAB_SCROLL_BUTTON_WIDTH: u16 = 3;
const ACTION_ROW_LEFT_BUDGET: u16 = 24;
const ZOOM_INDICATOR: &str = "ZOOM";
// The narrowest overflowing tab strip worth keeping interactive: one
// minimum-width tab, both scroll controls, and the new-tab control.
const MIN_TAB_STRIP_WIDTH: u16 =
    MIN_TAB_WIDTH + NEW_TAB_WIDTH + TAB_SCROLL_BUTTON_WIDTH.saturating_mul(2);

pub(crate) fn visible_user_actions(app: &AppState) -> Vec<(usize, String)> {
    let repo = app.focused_repo_slug();
    app.keybinds
        .user_actions
        .iter()
        .enumerate()
        .filter(|(_, action)| action.applies_to_repo(repo.as_deref()))
        .map(|(index, action)| (index, action.name.clone()))
        .collect()
}

#[derive(Debug, Clone, Default)]
pub(crate) struct TabBarView {
    pub scroll: usize,
    pub tab_hit_areas: Vec<Rect>,
    pub scroll_left_hit_area: Rect,
    pub scroll_right_hit_area: Rect,
    pub new_tab_hit_area: Rect,
    pub repo_editor_button_hit_area: Rect,
    pub add_action_button_hit_area: Rect,
    pub user_action_hit_areas: Vec<(usize, Rect)>,
    pub git_menu_button_hit_area: Rect,
    /// Splits the focused pane below, or closes the pane already below it.
    pub pane_toggle_below_hit_area: Rect,
    /// Splits the focused pane to the right, or closes the pane already there.
    pub pane_toggle_right_hit_area: Rect,
}

fn tab_width(
    ws: &crate::workspace::Workspace,
    terminals: &std::collections::HashMap<
        crate::terminal::TerminalId,
        crate::terminal::TerminalState,
    >,
    tab_idx: usize,
) -> u16 {
    display_width_u16(&tab_chrome_label(ws, terminals, tab_idx, u16::MAX as usize))
        .saturating_add(4)
        .max(MIN_TAB_WIDTH)
}

fn tab_width_with_icons(
    ws: &crate::workspace::Workspace,
    terminals: &std::collections::HashMap<
        crate::terminal::TerminalId,
        crate::terminal::TerminalState,
    >,
    tab_idx: usize,
    nerd_font: bool,
) -> u16 {
    if !nerd_font {
        return tab_width(ws, terminals, tab_idx);
    }
    display_width_u16(&tab_chrome_label_with_icons(
        ws,
        terminals,
        tab_idx,
        u16::MAX as usize,
        nerd_font,
    ))
    .saturating_add(4)
    .max(MIN_TAB_WIDTH)
}

pub(crate) fn fit_tab_display_projection(
    projection: crate::workspace::TabDisplayProjection,
    max_width: usize,
) -> String {
    fit_tab_display_projection_impl(projection, max_width, false)
}

pub(super) fn fit_tab_display_projection_with_icons(
    projection: crate::workspace::TabDisplayProjection,
    max_width: usize,
    nerd_font: bool,
) -> String {
    if !nerd_font {
        return fit_tab_display_projection(projection, max_width);
    }
    fit_tab_display_projection_impl(projection, max_width, nerd_font)
}

fn fit_tab_display_projection_impl(
    projection: crate::workspace::TabDisplayProjection,
    max_width: usize,
    nerd_font: bool,
) -> String {
    use crate::workspace::TabDisplayProjection;

    let candidates = match projection {
        TabDisplayProjection::Manual(name) | TabDisplayProjection::Fallback(name) => vec![name],
        TabDisplayProjection::Derived {
            agent,
            ticket,
            binding,
            title,
        } => {
            let full = [&ticket, &binding, &title]
                .into_iter()
                .filter_map(|part| part.as_deref())
                .collect::<Vec<_>>()
                .join(" · ");
            let mut candidates = Vec::new();
            if !full.is_empty() {
                candidates.push(full);
            }
            if let Some(ticket) = ticket {
                if !candidates.contains(&ticket) {
                    candidates.push(ticket);
                }
            }
            if let Some(binding) = binding {
                if !candidates.contains(&binding) {
                    candidates.push(binding);
                }
            }
            if let Some(title) = title {
                if !candidates.contains(&title) {
                    candidates.push(title);
                }
            }
            if let Some(agent) = agent {
                let agent = if nerd_font {
                    crate::ui::icons::agent_icon_for_name(&agent)
                        .unwrap_or(&agent)
                        .to_string()
                } else {
                    agent
                };
                if !candidates.contains(&agent) {
                    candidates.push(agent);
                }
            }
            candidates
        }
    };
    candidates
        .iter()
        .find(|candidate| display_width(candidate) <= max_width)
        .cloned()
        .unwrap_or_else(|| {
            truncate_end(
                candidates.last().map(String::as_str).unwrap_or(""),
                max_width,
            )
        })
}

pub(crate) fn tab_chrome_label(
    ws: &crate::workspace::Workspace,
    terminals: &std::collections::HashMap<
        crate::terminal::TerminalId,
        crate::terminal::TerminalState,
    >,
    tab_idx: usize,
    max_width: usize,
) -> String {
    tab_chrome_label_impl(ws, terminals, tab_idx, max_width, false)
}

fn tab_chrome_label_with_icons(
    ws: &crate::workspace::Workspace,
    terminals: &std::collections::HashMap<
        crate::terminal::TerminalId,
        crate::terminal::TerminalState,
    >,
    tab_idx: usize,
    max_width: usize,
    nerd_font: bool,
) -> String {
    if !nerd_font {
        return tab_chrome_label(ws, terminals, tab_idx, max_width);
    }
    tab_chrome_label_impl(ws, terminals, tab_idx, max_width, nerd_font)
}

fn tab_chrome_label_impl(
    ws: &crate::workspace::Workspace,
    terminals: &std::collections::HashMap<
        crate::terminal::TerminalId,
        crate::terminal::TerminalState,
    >,
    tab_idx: usize,
    max_width: usize,
    nerd_font: bool,
) -> String {
    let zoomed = ws.tabs.get(tab_idx).is_some_and(|tab| tab.zoomed);
    // Same budgeting rule as the zoom marker: the glyph is paid for out of the
    // label, never out of the cell, so a pin can never widen or shove a tab.
    let label_width = max_width.saturating_sub(if zoomed { 2 } else { 0 });
    let projection = ws.tab_display_projection(terminals, tab_idx);
    let keyword_icon = if nerd_font && !projection_agent_icon_fits(projection.as_ref(), label_width)
    {
        projection
            .as_ref()
            .and_then(projection_keyword_icon)
            .filter(|_| {
                projection
                    .as_ref()
                    .and_then(|projection| match projection {
                        crate::workspace::TabDisplayProjection::Manual(name)
                        | crate::workspace::TabDisplayProjection::Fallback(name) => {
                            crate::ui::icons::leading_label_icon(name)
                        }
                        crate::workspace::TabDisplayProjection::Derived { title, .. } => title
                            .as_deref()
                            .and_then(crate::ui::icons::leading_label_icon),
                    })
                    .is_none()
            })
            .filter(|icon| {
                let workspace_name = ws.display_name_from_terminals(terminals);
                let parent_icon = crate::ui::icons::keyword_icon(&workspace_name).unwrap_or("");
                crate::ui::icons::child_badge_icon(&workspace_name, parent_icon, icon).is_some()
            })
    } else {
        None
    };
    let title_width = label_width.saturating_sub(if keyword_icon.is_some() { 2 } else { 0 });
    let name = projection
        .map(|projection| fit_tab_display_projection_impl(projection, title_width, nerd_font))
        .unwrap_or_else(|| truncate_end(&(tab_idx + 1).to_string(), title_width));
    let name = if let Some(icon) = keyword_icon {
        format!("{icon} {name}")
    } else {
        name
    };
    if zoomed {
        format!("{name} Z")
    } else {
        name
    }
}

fn projection_keyword_icon(
    projection: &crate::workspace::TabDisplayProjection,
) -> Option<&'static str> {
    use crate::workspace::TabDisplayProjection;
    match projection {
        TabDisplayProjection::Manual(name) | TabDisplayProjection::Fallback(name) => {
            crate::ui::icons::keyword_icon(name)
        }
        TabDisplayProjection::Derived {
            ticket,
            binding,
            title,
            ..
        } => [ticket, binding, title]
            .into_iter()
            .filter_map(|part| part.as_deref())
            .find_map(crate::ui::icons::keyword_icon),
    }
}

fn projection_agent_icon_fits(
    projection: Option<&crate::workspace::TabDisplayProjection>,
    max_width: usize,
) -> bool {
    use crate::workspace::TabDisplayProjection;
    let Some(TabDisplayProjection::Derived {
        ticket,
        binding,
        title,
        agent: Some(agent),
        ..
    }) = projection
    else {
        return false;
    };
    let component_widths = [ticket, binding, title].map(|part| part.as_deref().map(display_width));
    let full_width = component_widths.iter().flatten().copied().sum::<usize>()
        + component_widths.iter().flatten().count().saturating_sub(1) * display_width(" · ");
    full_width > max_width
        && !component_widths
            .iter()
            .flatten()
            .any(|width| *width <= max_width)
        && crate::ui::icons::agent_icon_for_name(agent)
            .is_some_and(|icon| display_width(icon) <= max_width)
}

#[derive(Clone, Copy)]
struct VisibleStatusSegment<'a> {
    text: &'a str,
    accent: bool,
}

fn visible_status_segments(app: &AppState) -> Vec<VisibleStatusSegment<'_>> {
    let zoomed = app
        .active
        .and_then(|index| app.workspaces.get(index))
        .is_some_and(|workspace| workspace.zoomed);
    app.tab_bar_right
        .iter()
        .filter_map(|segment| match segment {
            crate::app::state::TabBarStatusSegment::Zoom if zoomed => Some(VisibleStatusSegment {
                text: ZOOM_INDICATOR,
                accent: true,
            }),
            crate::app::state::TabBarStatusSegment::Text(Some(text))
                if display_width_u16(text) > 0 =>
            {
                Some(VisibleStatusSegment {
                    text,
                    accent: false,
                })
            }
            crate::app::state::TabBarStatusSegment::Zoom
            | crate::app::state::TabBarStatusSegment::Text(_) => None,
        })
        .collect()
}

fn tab_bar_status_width(app: &AppState) -> u16 {
    let segments = visible_status_segments(app);
    let content_width = segments.iter().fold(0_u16, |width, segment| {
        width.saturating_add(display_width_u16(segment.text))
    });
    let separators = u16::try_from(segments.len().saturating_sub(1)).unwrap_or(u16::MAX);
    content_width
        .saturating_add(display_width_u16(&app.tab_bar_right_separator).saturating_mul(separators))
}

fn tab_bar_status_area(app: &AppState, area: Rect) -> Option<Rect> {
    let width = tab_bar_status_width(app);
    if width == 0 {
        return None;
    }
    let reserved = width.saturating_add(1);
    (area.width.saturating_sub(reserved) >= MIN_TAB_STRIP_WIDTH)
        .then(|| Rect::new(area.x + area.width.saturating_sub(width), area.y, width, 1))
}

pub(crate) fn tab_bar_content_area(app: &AppState, area: Rect) -> Rect {
    let reserved = tab_bar_status_area(app, area)
        .map(|status| status.width.saturating_add(1))
        .unwrap_or(0);
    Rect {
        width: area.width.saturating_sub(reserved),
        ..area
    }
}

fn layout_tab_hit_areas(
    ws: &crate::workspace::Workspace,
    terminals: &std::collections::HashMap<
        crate::terminal::TerminalId,
        crate::terminal::TerminalState,
    >,
    area: Rect,
    scroll: usize,
    nerd_font: bool,
) -> Vec<Rect> {
    let mut rects = vec![Rect::default(); ws.tabs.len()];
    if area.width == 0 || area.height == 0 {
        return rects;
    }

    let mut x = area.x;
    let right = area.x + area.width;
    for (idx, rect) in rects.iter_mut().enumerate().skip(scroll) {
        if x >= right {
            break;
        }
        let desired = tab_width_with_icons(ws, terminals, idx, nerd_font);
        let remaining = right.saturating_sub(x);
        let width = desired.min(remaining).max(1);
        *rect = Rect::new(x, area.y, width, 1);
        x = x.saturating_add(width + 1);
    }
    rects
}

fn centered_tab_scroll(
    ws: &crate::workspace::Workspace,
    terminals: &std::collections::HashMap<
        crate::terminal::TerminalId,
        crate::terminal::TerminalState,
    >,
    area: Rect,
    nerd_font: bool,
) -> usize {
    let mut best_scroll = ws.active_tab;
    let mut best_distance = u16::MAX;
    let viewport_center = area.x.saturating_mul(2).saturating_add(area.width);

    for scroll in 0..=ws.active_tab {
        let rects = layout_tab_hit_areas(ws, terminals, area, scroll, nerd_font);
        let Some(active_rect) = rects.get(ws.active_tab).copied() else {
            continue;
        };
        if active_rect.width == 0 {
            continue;
        }

        let active_center = active_rect
            .x
            .saturating_mul(2)
            .saturating_add(active_rect.width);
        let distance = active_center.abs_diff(viewport_center);
        if distance <= best_distance {
            best_distance = distance;
            best_scroll = scroll;
        }
    }

    best_scroll
}

fn trailing_tab_controls_x(tab_hit_areas: &[Rect], fallback_x: u16) -> u16 {
    tab_hit_areas
        .iter()
        .rev()
        .find(|rect| rect.width > 0)
        .map(|rect| rect.x + rect.width)
        .unwrap_or(fallback_x)
}

fn max_tab_scroll(
    ws: &crate::workspace::Workspace,
    terminals: &std::collections::HashMap<
        crate::terminal::TerminalId,
        crate::terminal::TerminalState,
    >,
    area: Rect,
    nerd_font: bool,
) -> usize {
    (0..ws.tabs.len())
        .find(|&scroll| {
            layout_tab_hit_areas(ws, terminals, area, scroll, nerd_font)
                .last()
                .is_some_and(|rect| rect.width > 0)
        })
        .unwrap_or(0)
}

fn active_tab_cell_width(ws: &crate::workspace::Workspace, view: &TabBarView) -> u16 {
    view.tab_hit_areas
        .get(ws.active_tab)
        .map(|rect| rect.width)
        .unwrap_or(0)
}

/// Which optional top-right controls the operator asked for.
///
/// Both are off by default, because they crowd the tab titles they sit beside.
/// The pane toggles duplicate the split keybindings. The pull button does not
/// duplicate anything: it is the only entry point to the git menu, so hiding it
/// hides that menu too.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct TabActionVisibility {
    pub git_menu: bool,
    pub pane_toggles: bool,
    /// Icon labels for the editor and add-action buttons; their hit areas
    /// follow the rendered label width.
    pub nerd_font: bool,
}

impl TabActionVisibility {
    pub(crate) fn from_state(app: &AppState) -> Self {
        Self {
            git_menu: app.show_pull_button,
            pane_toggles: app.show_pane_toggle_buttons,
            nerd_font: app.nerd_font,
        }
    }

    pub(crate) fn editor_width(self) -> u16 {
        display_width_u16(&crate::ui::icons::repo_editor_button_label(self.nerd_font))
    }

    pub(crate) fn add_action_width(self) -> u16 {
        display_width_u16(&crate::ui::icons::add_action_button_label(self.nerd_font))
    }

    fn git_menu_width(self) -> u16 {
        if self.git_menu {
            GIT_MENU_BUTTON_WIDTH
        } else {
            0
        }
    }

    fn pane_toggles_width(self) -> u16 {
        if self.pane_toggles {
            PANE_TOGGLE_BUTTON_WIDTH.saturating_mul(2)
        } else {
            0
        }
    }
}

#[cfg(test)]
pub(crate) fn compute_tab_bar_view(
    ws: &crate::workspace::Workspace,
    terminals: &std::collections::HashMap<
        crate::terminal::TerminalId,
        crate::terminal::TerminalState,
    >,
    area: Rect,
    current_scroll: usize,
    follow_active: bool,
    mouse_chrome: bool,
    user_actions: &[(usize, String)],
    visibility: TabActionVisibility,
) -> TabBarView {
    compute_tab_bar_view_with_icons(
        ws,
        terminals,
        area,
        current_scroll,
        follow_active,
        mouse_chrome,
        user_actions,
        visibility,
        false,
    )
}

pub(crate) fn compute_tab_bar_view_with_icons(
    ws: &crate::workspace::Workspace,
    terminals: &std::collections::HashMap<
        crate::terminal::TerminalId,
        crate::terminal::TerminalState,
    >,
    area: Rect,
    current_scroll: usize,
    follow_active: bool,
    mouse_chrome: bool,
    user_actions: &[(usize, String)],
    visibility: TabActionVisibility,
    nerd_font: bool,
) -> TabBarView {
    if !mouse_chrome || area.width == 0 || area.height == 0 {
        return compute_tab_bar_view_inner(
            ws,
            terminals,
            area,
            current_scroll,
            follow_active,
            mouse_chrome,
            nerd_font,
        );
    }

    // The buttons are the rightmost content, so they are paid for out of the
    // tab strip's width. When that would truncate the active tab title they are
    // dropped entirely rather than shown over a clipped label.
    let user_widths = action_button_widths(user_actions, area.width, visibility);
    let actions_width = action_controls_width(&user_widths, visibility);
    if area.width > actions_width {
        let tabs_area = Rect::new(
            area.x,
            area.y,
            area.width.saturating_sub(actions_width),
            area.height,
        );
        let mut view = compute_tab_bar_view_inner(
            ws,
            terminals,
            tabs_area,
            current_scroll,
            follow_active,
            true,
            nerd_font,
        );
        // The active tab may already be clipped by a narrow row; the buttons
        // only have to not make that worse.
        let full_view = compute_tab_bar_view_inner(
            ws,
            terminals,
            area,
            current_scroll,
            follow_active,
            true,
            nerd_font,
        );
        let budget = active_tab_cell_width(ws, &full_view).min(tab_width_with_icons(
            ws,
            terminals,
            ws.active_tab,
            nerd_font,
        ));
        if budget > 0 && active_tab_cell_width(ws, &view) >= budget {
            let editor_x = area.x + area.width - actions_width;
            view.repo_editor_button_hit_area =
                Rect::new(editor_x, area.y, visibility.editor_width(), 1);
            let add_x = editor_x + visibility.editor_width();
            view.add_action_button_hit_area =
                Rect::new(add_x, area.y, visibility.add_action_width(), 1);
            let mut action_x = add_x + visibility.add_action_width();
            view.user_action_hit_areas = user_actions
                .iter()
                .zip(user_widths.iter().copied())
                .map(|((index, _), width)| {
                    let rect = Rect::new(action_x, area.y, width, 1);
                    action_x = action_x.saturating_add(width);
                    (*index, rect)
                })
                .collect();
            let menu_x = action_x;
            if visibility.git_menu {
                view.git_menu_button_hit_area = Rect::new(menu_x, area.y, GIT_MENU_BUTTON_WIDTH, 1);
            }
            let below_x = menu_x + visibility.git_menu_width();
            if visibility.pane_toggles {
                view.pane_toggle_below_hit_area =
                    Rect::new(below_x, area.y, PANE_TOGGLE_BUTTON_WIDTH, 1);
                view.pane_toggle_right_hit_area = Rect::new(
                    below_x + PANE_TOGGLE_BUTTON_WIDTH,
                    area.y,
                    PANE_TOGGLE_BUTTON_WIDTH,
                    1,
                );
            }
            return view;
        }
    }

    compute_tab_bar_view_inner(
        ws,
        terminals,
        area,
        current_scroll,
        follow_active,
        true,
        nerd_font,
    )
}

/// Where the toggles go when the tab row cannot host them.
///
/// The tab row is optional and absent by default: `tab_bar_position` defaults
/// to `hidden`, and `hide_tab_bar_when_single_tab` removes it again for a
/// one-tab workspace. Anchoring the buttons to that row alone made them
/// unreachable in the configuration most sessions actually run, so they fall
/// back to the right end of the status row -- the same "title row" the design
/// draws them on -- and keep their geometry, hit testing and toggle semantics.
pub(crate) fn tab_action_fallback_hit_areas(
    status_bar_rect: Rect,
    mouse_chrome: bool,
    user_actions: &[(usize, String)],
    visibility: TabActionVisibility,
) -> (Rect, Rect, Vec<(usize, Rect)>, Rect, Rect, Rect) {
    let user_widths = action_button_widths(user_actions, status_bar_rect.width, visibility);
    let actions_width = action_controls_width(&user_widths, visibility);
    if !mouse_chrome || status_bar_rect.height == 0 || status_bar_rect.width <= actions_width {
        return (
            Rect::default(),
            Rect::default(),
            Vec::new(),
            Rect::default(),
            Rect::default(),
            Rect::default(),
        );
    }
    let editor_x = status_bar_rect.x + status_bar_rect.width - actions_width;
    let add_x = editor_x + visibility.editor_width();
    let mut action_x = add_x + visibility.add_action_width();
    let action_rects = user_actions
        .iter()
        .zip(user_widths.iter().copied())
        .map(|((index, _), width)| {
            let rect = Rect::new(action_x, status_bar_rect.y, width, 1);
            action_x = action_x.saturating_add(width);
            (*index, rect)
        })
        .collect();
    let menu_x = action_x;
    let below_x = menu_x + visibility.git_menu_width();
    let toggle = |x: u16| {
        if visibility.pane_toggles {
            Rect::new(x, status_bar_rect.y, PANE_TOGGLE_BUTTON_WIDTH, 1)
        } else {
            Rect::default()
        }
    };
    (
        Rect::new(editor_x, status_bar_rect.y, visibility.editor_width(), 1),
        Rect::new(add_x, status_bar_rect.y, visibility.add_action_width(), 1),
        action_rects,
        if visibility.git_menu {
            Rect::new(menu_x, status_bar_rect.y, GIT_MENU_BUTTON_WIDTH, 1)
        } else {
            Rect::default()
        },
        toggle(below_x),
        toggle(below_x + PANE_TOGGLE_BUTTON_WIDTH),
    )
}

fn action_button_widths(
    user_actions: &[(usize, String)],
    row_width: u16,
    visibility: TabActionVisibility,
) -> Vec<u16> {
    let fixed = visibility
        .editor_width()
        .saturating_add(visibility.add_action_width())
        .saturating_add(visibility.git_menu_width())
        .saturating_add(visibility.pane_toggles_width());
    let mut widths = user_actions
        .iter()
        .map(|(_, name)| display_width_u16(name).saturating_add(2).max(3))
        .collect::<Vec<_>>();
    let available = row_width.saturating_sub(fixed.saturating_add(ACTION_ROW_LEFT_BUDGET));
    while widths.iter().copied().sum::<u16>() > available {
        let Some(width) = widths.iter_mut().max() else {
            break;
        };
        if *width <= 3 {
            break;
        }
        *width -= 1;
    }
    widths
}

fn action_controls_width(user_widths: &[u16], visibility: TabActionVisibility) -> u16 {
    visibility
        .editor_width()
        .saturating_add(visibility.add_action_width())
        .saturating_add(user_widths.iter().copied().sum::<u16>())
        .saturating_add(visibility.git_menu_width())
        .saturating_add(visibility.pane_toggles_width())
}

/// The width the status row must keep clear when it hosts the toggles.
pub(crate) fn tab_action_status_bar_reserved_width(app: &AppState, status_bar_rect: Rect) -> u16 {
    let menu = app.view.git_menu_button_hit_area;
    let editor = app.view.repo_editor_button_hit_area;
    let add = app.view.add_action_button_hit_area;
    let below = app.view.pane_toggle_below_hit_area;
    let right = app.view.pane_toggle_right_hit_area;
    // `add` is the sentinel, not `menu`: the pull button and the pane toggles
    // are optional, so an absent one says nothing about where the row lives.
    if add.width == 0 || add.y != status_bar_rect.y || status_bar_rect.height == 0 {
        return 0;
    }
    editor
        .width
        .saturating_add(add.width)
        .saturating_add(
            app.view
                .user_action_hit_areas
                .iter()
                .map(|(_, rect)| rect.width)
                .sum::<u16>(),
        )
        .saturating_add(menu.width)
        .saturating_add(below.width)
        .saturating_add(right.width)
}

pub(crate) fn git_menu_status_visible(ahead_behind: Option<(usize, usize)>) -> bool {
    ahead_behind.is_some_and(|(_, behind)| behind > 0)
}

pub(crate) fn git_menu_dropdown_layout(
    anchor: Rect,
    area: Rect,
    selected: usize,
    ahead_behind: Option<(usize, usize)>,
    in_git_repo: bool,
) -> Option<crate::ui::dropdown::DropdownLayout> {
    let item_count = if in_git_repo {
        crate::app::state::GitAction::ALL.len() + usize::from(git_menu_status_visible(ahead_behind))
    } else {
        1
    };
    crate::ui::dropdown::layout_dropdown(
        &crate::ui::dropdown::DropdownSpec {
            anchor,
            item_count,
            selected: if in_git_repo {
                selected.min(crate::app::state::GitAction::ALL.len() - 1)
            } else {
                0
            },
            has_filter: false,
            max_rows: item_count,
            min_width: 34,
        },
        area,
    )
}

fn compute_tab_bar_view_inner(
    ws: &crate::workspace::Workspace,
    terminals: &std::collections::HashMap<
        crate::terminal::TerminalId,
        crate::terminal::TerminalState,
    >,
    area: Rect,
    current_scroll: usize,
    follow_active: bool,
    mouse_chrome: bool,
    nerd_font: bool,
) -> TabBarView {
    if area.width == 0 || area.height == 0 {
        return TabBarView::default();
    }

    if !mouse_chrome {
        let max_scroll = max_tab_scroll(ws, terminals, area, nerd_font);
        let scroll = if follow_active {
            centered_tab_scroll(ws, terminals, area, nerd_font).min(max_scroll)
        } else {
            current_scroll.min(max_scroll)
        };
        return TabBarView {
            scroll,
            tab_hit_areas: layout_tab_hit_areas(ws, terminals, area, scroll, nerd_font),
            scroll_left_hit_area: Rect::default(),
            scroll_right_hit_area: Rect::default(),
            new_tab_hit_area: Rect::default(),
            repo_editor_button_hit_area: Rect::default(),
            add_action_button_hit_area: Rect::default(),
            user_action_hit_areas: Vec::new(),
            git_menu_button_hit_area: Rect::default(),
            pane_toggle_below_hit_area: Rect::default(),
            pane_toggle_right_hit_area: Rect::default(),
        };
    }

    let area_right = area.x + area.width;
    let all_tabs_area = Rect::new(
        area.x,
        area.y,
        area.width.saturating_sub(NEW_TAB_WIDTH),
        area.height,
    );
    let all_tabs = layout_tab_hit_areas(ws, terminals, all_tabs_area, 0, nerd_font);
    let overflow = all_tabs.iter().any(|rect| rect.width == 0);
    if !overflow {
        let new_tab_x = trailing_tab_controls_x(&all_tabs, area.x);
        let new_tab_hit_area = Rect::new(
            new_tab_x,
            area.y,
            area_right.saturating_sub(new_tab_x).min(NEW_TAB_WIDTH),
            1,
        );
        return TabBarView {
            scroll: 0,
            tab_hit_areas: all_tabs,
            scroll_left_hit_area: Rect::default(),
            scroll_right_hit_area: Rect::default(),
            new_tab_hit_area,
            repo_editor_button_hit_area: Rect::default(),
            add_action_button_hit_area: Rect::default(),
            user_action_hit_areas: Vec::new(),
            git_menu_button_hit_area: Rect::default(),
            pane_toggle_below_hit_area: Rect::default(),
            pane_toggle_right_hit_area: Rect::default(),
        };
    }

    let left_hit_area = Rect::new(area.x, area.y, TAB_SCROLL_BUTTON_WIDTH.min(area.width), 1);
    let tab_area_x = left_hit_area.x + left_hit_area.width;
    let reserved_trailing_width = NEW_TAB_WIDTH.saturating_add(TAB_SCROLL_BUTTON_WIDTH);
    let tab_area_right = area_right.saturating_sub(reserved_trailing_width);
    let tab_area = Rect::new(
        tab_area_x,
        area.y,
        tab_area_right.saturating_sub(tab_area_x),
        area.height,
    );

    let max_scroll = max_tab_scroll(ws, terminals, tab_area, nerd_font);
    let scroll = if follow_active {
        centered_tab_scroll(ws, terminals, tab_area, nerd_font).min(max_scroll)
    } else {
        current_scroll.min(max_scroll)
    };
    let tab_hit_areas = layout_tab_hit_areas(ws, terminals, tab_area, scroll, nerd_font);
    let trailing_x = trailing_tab_controls_x(&tab_hit_areas, tab_area_x).min(tab_area_right);
    let right_hit_area = Rect::new(
        trailing_x,
        area.y,
        area_right
            .saturating_sub(trailing_x)
            .min(TAB_SCROLL_BUTTON_WIDTH),
        1,
    );
    let new_tab_x = right_hit_area.x + right_hit_area.width;
    let new_tab_hit_area = Rect::new(
        new_tab_x,
        area.y,
        area_right.saturating_sub(new_tab_x).min(NEW_TAB_WIDTH),
        1,
    );

    TabBarView {
        scroll,
        tab_hit_areas,
        scroll_left_hit_area: left_hit_area,
        scroll_right_hit_area: right_hit_area,
        new_tab_hit_area,
        repo_editor_button_hit_area: Rect::default(),
        add_action_button_hit_area: Rect::default(),
        user_action_hit_areas: Vec::new(),
        git_menu_button_hit_area: Rect::default(),
        pane_toggle_below_hit_area: Rect::default(),
        pane_toggle_right_hit_area: Rect::default(),
    }
}

fn tab_drop_indicator_x(
    app: &AppState,
    ws: &crate::workspace::Workspace,
    insert_idx: usize,
) -> Option<u16> {
    let mut visible_tabs = app
        .view
        .tab_hit_areas
        .iter()
        .enumerate()
        .filter(|(_, rect)| rect.width > 0);
    let first_visible = visible_tabs.clone().next()?;
    let last_visible = visible_tabs.next_back().unwrap_or(first_visible);

    if insert_idx == 0 {
        return Some(if first_visible.0 == 0 {
            first_visible.1.x
        } else {
            app.view.tab_scroll_left_hit_area.x + app.view.tab_scroll_left_hit_area.width
        });
    }

    if let Some((_, rect)) = app
        .view
        .tab_hit_areas
        .iter()
        .enumerate()
        .find(|(idx, rect)| *idx == insert_idx && rect.width > 0)
    {
        return Some(rect.x.saturating_sub(1));
    }

    if insert_idx >= ws.tabs.len() {
        return Some(if last_visible.0 + 1 >= ws.tabs.len() {
            last_visible.1.x + last_visible.1.width
        } else {
            app.view.tab_scroll_right_hit_area.x.saturating_sub(1)
        });
    }

    None
}

/// Drawn from the top-level render pass because these controls move to the
/// status row whenever the tab row is hidden.
pub(super) fn render_tab_action_buttons(app: &AppState, frame: &mut Frame) {
    let p = &app.palette;
    let icons = TabActionVisibility::from_state(app).nerd_font;
    let editor_rect = app.view.repo_editor_button_hit_area;
    if app.mouse_capture && editor_rect.width > 0 {
        let mut style = Style::default()
            .fg(readable_fg_on(p.surface0, &[p.overlay1, p.text]))
            .bg(p.surface0);
        if !app.repo_editor_available() {
            style = style.add_modifier(Modifier::DIM);
        }
        frame.render_widget(
            Paragraph::new(crate::ui::icons::repo_editor_button_label(icons)).style(style),
            editor_rect,
        );
    }

    let add_rect = app.view.add_action_button_hit_area;
    if app.mouse_capture && add_rect.width > 0 {
        let style = if app.server_mode() == crate::app::Mode::AddAction {
            Style::default()
                .fg(panel_contrast_fg(p))
                .bg(p.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(readable_fg_on(p.surface0, &[p.overlay1, p.text]))
                .bg(p.surface0)
        };
        frame.render_widget(
            Paragraph::new(crate::ui::icons::add_action_button_label(icons)).style(style),
            add_rect,
        );
    }

    for (index, rect) in &app.view.user_action_hit_areas {
        let Some(action) = app.keybinds.user_actions.get(*index) else {
            continue;
        };
        let label_width = rect.width.saturating_sub(2) as usize;
        let label = format!(" {} ", truncate_end(&action.name, label_width));
        frame.render_widget(
            Paragraph::new(label).style(
                Style::default()
                    .fg(readable_fg_on(p.surface0, &[p.overlay1, p.text]))
                    .bg(p.surface0),
            ),
            *rect,
        );
    }

    let menu_rect = app.view.git_menu_button_hit_area;
    if app.mouse_capture && menu_rect.width > 0 {
        let in_git_repo = crate::ui::dock::chooser::focused_in_git_repo(app);
        let open = app.server_mode() == crate::app::Mode::GitMenu;
        let label = if open && app.view.git_menu_popup_rect.width == 0 {
            " no room "
        } else {
            " ⇣ Pull ▾ "
        };
        let style = if !in_git_repo {
            Style::default()
                .fg(readable_fg_on(p.surface0, &[p.overlay1, p.text]))
                .bg(p.surface0)
                .add_modifier(Modifier::DIM)
        } else if open {
            Style::default()
                .fg(panel_contrast_fg(p))
                .bg(p.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(readable_fg_on(p.surface0, &[p.overlay1, p.text]))
                .bg(p.surface0)
        };
        frame.render_widget(Paragraph::new(label).style(style), menu_rect);
    }

    for (rect, glyph, direction) in [
        (
            app.view.pane_toggle_below_hit_area,
            PANE_TOGGLE_BELOW_GLYPH,
            crate::app::state::PaneToggleDirection::Below,
        ),
        (
            app.view.pane_toggle_right_hit_area,
            PANE_TOGGLE_RIGHT_GLYPH,
            crate::app::state::PaneToggleDirection::Right,
        ),
    ] {
        if !app.mouse_capture || rect.width == 0 {
            continue;
        }
        // Accent means "a pane is already there", so the same click closes it.
        let style = if app.pane_toggle_sibling(direction).is_some() {
            Style::default().fg(p.accent)
        } else {
            Style::default().fg(p.overlay1)
        };
        frame.render_widget(Paragraph::new(format!(" {glyph} ")).style(style), rect);
    }
}

pub(super) fn render_git_menu(app: &AppState, frame: &mut Frame) {
    if app.server_mode() != crate::app::Mode::GitMenu || app.view.git_menu_popup_rect.width == 0 {
        return;
    }

    frame.render_widget(Clear, app.view.git_menu_popup_rect);
    if !crate::ui::dock::chooser::focused_in_git_repo(app) {
        if let Some(rect) = app.view.git_menu_row_hit_areas.first().copied() {
            let style = Style::default()
                .fg(app.palette.overlay0)
                .bg(app.palette.panel_bg);
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    format!(" {GIT_MENU_UNAVAILABLE}"),
                    style,
                )))
                .style(Style::default().bg(app.palette.panel_bg)),
                rect,
            );
        }
        return;
    }
    let action_count = crate::app::state::GitAction::ALL.len();
    for (offset, rect) in app.view.git_menu_row_hit_areas.iter().copied().enumerate() {
        let index = app.view.git_menu_first_visible + offset;
        let (label, style) = if let Some(action) = crate::app::state::GitAction::ALL.get(index) {
            let style = if index == app.git_menu.highlighted {
                Style::default()
                    .fg(panel_contrast_fg(&app.palette))
                    .bg(app.palette.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(app.palette.text)
                    .bg(app.palette.panel_bg)
            };
            (action.label(), style)
        } else {
            debug_assert_eq!(index, action_count);
            (
                GIT_MENU_STATUS,
                Style::default()
                    .fg(app.palette.yellow)
                    .bg(app.palette.panel_bg),
            )
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(format!(" {label}"), style)))
                .style(Style::default().bg(app.palette.panel_bg)),
            rect,
        );
    }
}

pub(super) fn render_tab_bar(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let Some(active_ws_idx) = app.active else {
        return;
    };
    let Some(ws) = app.workspaces.get(active_ws_idx) else {
        return;
    };
    let p = &app.palette;

    frame.render_widget(
        Paragraph::new(" ".repeat(area.width as usize)).style(Style::default().bg(p.panel_bg)),
        area,
    );

    let first_visible_idx = app
        .view
        .tab_hit_areas
        .iter()
        .enumerate()
        .find(|(_, rect)| rect.width > 0)
        .map(|(idx, _)| idx);
    let last_visible_idx = app
        .view
        .tab_hit_areas
        .iter()
        .enumerate()
        .rev()
        .find(|(_, rect)| rect.width > 0)
        .map(|(idx, _)| idx);
    let can_scroll_left = app.view.tab_scroll_left_hit_area.width > 0 && app.tab_scroll > 0;
    let can_scroll_right = app.view.tab_scroll_right_hit_area.width > 0
        && last_visible_idx.is_some_and(|idx| idx + 1 < ws.tabs.len());

    if app.mouse_capture && app.view.tab_scroll_left_hit_area.width > 0 {
        let style = if can_scroll_left {
            Style::default().fg(p.overlay1).bg(p.surface0)
        } else {
            Style::default()
                .fg(readable_fg_on(
                    p.surface0,
                    &[p.overlay0, p.overlay1, p.text],
                ))
                .bg(p.surface0)
                .add_modifier(Modifier::DIM)
        };
        frame.render_widget(
            Paragraph::new(" < ").style(style),
            app.view.tab_scroll_left_hit_area,
        );
    }

    if app.mouse_capture && app.view.tab_scroll_right_hit_area.width > 0 {
        let style = if can_scroll_right {
            Style::default().fg(p.overlay1).bg(p.surface0)
        } else {
            Style::default()
                .fg(readable_fg_on(
                    p.surface0,
                    &[p.overlay0, p.overlay1, p.text],
                ))
                .bg(p.surface0)
                .add_modifier(Modifier::DIM)
        };
        frame.render_widget(
            Paragraph::new(" > ").style(style),
            app.view.tab_scroll_right_hit_area,
        );
    }

    for (idx, tab) in ws.tabs.iter().enumerate() {
        let Some(rect) = app.view.tab_hit_areas.get(idx).copied() else {
            break;
        };
        if rect.width == 0 {
            continue;
        }
        let active = idx == ws.active_tab;
        let style = if active {
            let base = Style::default()
                .fg(readable_fg_on(p.accent, &[panel_contrast_fg(p), p.text]))
                .bg(p.accent);
            if tab.is_auto_named() {
                base
            } else {
                base.add_modifier(Modifier::BOLD)
            }
        } else if tab.is_auto_named() {
            Style::default()
                .fg(p.overlay0)
                .bg(p.surface0)
                .add_modifier(Modifier::DIM)
        } else {
            Style::default().fg(p.overlay1).bg(p.surface0)
        };
        let width = rect.width as usize;
        let name = tab_chrome_label_with_icons(
            ws,
            &app.terminals,
            idx,
            width.saturating_sub(1),
            app.nerd_font,
        );
        let pin = if tab.pinned {
            PIN_GLYPH_ON
        } else {
            PIN_GLYPH_OFF
        };
        let padding = width.saturating_sub(1).saturating_sub(display_width(&name));
        let left = padding / 2;
        let text = format!(
            "{pin}{left_pad}{name}{right_pad}",
            left_pad = " ".repeat(left),
            right_pad = " ".repeat(padding - left),
        );
        frame.render_widget(Paragraph::new(text).style(style), rect);
    }

    if let Some(crate::app::state::DragState {
        target:
            crate::app::state::DragTarget::TabReorder {
                ws_idx,
                insert_idx: Some(insert_idx),
                ..
            },
    }) = &app.drag
    {
        if *ws_idx == active_ws_idx {
            if let Some(x) = tab_drop_indicator_x(app, ws, *insert_idx) {
                frame.buffer_mut()[(x.min(area.x + area.width.saturating_sub(1)), area.y)]
                    .set_symbol("│")
                    .set_style(Style::default().fg(p.accent));
            }
        }
    }

    if app.mouse_capture && app.view.new_tab_hit_area.width > 0 {
        frame.render_widget(
            Paragraph::new(" + ").style(Style::default().fg(p.overlay1)),
            app.view.new_tab_hit_area,
        );
    }

    if first_visible_idx.is_some_and(|idx| idx > 0) {
        let x = if app.mouse_capture && app.view.tab_scroll_left_hit_area.width > 0 {
            app.view.tab_scroll_left_hit_area.x + app.view.tab_scroll_left_hit_area.width
        } else {
            area.x
        };
        if x < area.x + area.width {
            frame.buffer_mut()[(x, area.y)]
                .set_symbol("…")
                .set_style(Style::default().fg(readable_fg_on(
                    p.surface0,
                    &[p.overlay0, p.overlay1, p.text],
                )));
        }
    }
    if last_visible_idx.is_some_and(|idx| idx + 1 < ws.tabs.len()) {
        let content = tab_bar_content_area(app, area);
        let content_right = content.x + content.width;
        let x = if app.mouse_capture && app.view.tab_scroll_right_hit_area.width > 0 {
            app.view.tab_scroll_right_hit_area.x.saturating_sub(1)
        } else {
            content_right.saturating_sub(1)
        };
        if x >= area.x && x < area.x + area.width {
            frame.buffer_mut()[(x, area.y)]
                .set_symbol("…")
                .set_style(Style::default().fg(readable_fg_on(
                    p.surface0,
                    &[p.overlay0, p.overlay1, p.text],
                )));
        }
    }

    if let Some(status_area) = tab_bar_status_area(app, area) {
        let segments = visible_status_segments(app);
        let separator_width = display_width_u16(&app.tab_bar_right_separator);
        let mut x = status_area.x;
        for (index, segment) in segments.iter().enumerate() {
            if index > 0 && separator_width > 0 {
                let rect = Rect::new(x, area.y, separator_width, 1);
                frame.render_widget(
                    Paragraph::new(app.tab_bar_right_separator.as_str())
                        .style(Style::default().fg(p.overlay0).bg(p.panel_bg)),
                    rect,
                );
                x = x.saturating_add(separator_width);
            }

            let width = display_width_u16(segment.text);
            let rect = Rect::new(x, area.y, width, 1);
            let style = if segment.accent {
                Style::default()
                    .fg(panel_contrast_fg(p))
                    .bg(p.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(p.overlay1).bg(p.panel_bg)
            };
            frame.render_widget(Paragraph::new(segment.text).style(style), rect);
            x = x.saturating_add(width);
        }
    }
}

#[cfg(test)]
mod tests {
    /// The layout tests below assert geometry, so they opt every optional
    /// control in. The hidden default has its own tests.
    fn all_tab_actions_visible() -> TabActionVisibility {
        TabActionVisibility {
            git_menu: true,
            pane_toggles: true,
            nerd_font: false,
        }
    }

    use super::*;
    use crate::app::state::AppState;
    use crate::workspace::Workspace;
    use ratatui::{backend::TestBackend, Terminal};

    fn buffer_row_text(buffer: &ratatui::buffer::Buffer, area: Rect, row: u16) -> String {
        (area.x..area.x + area.width)
            .map(|x| buffer[(x, row)].symbol())
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    fn set_focused_git_availability(app: &mut AppState, in_git_repo: bool) {
        if app.workspaces.is_empty() {
            app.workspaces = vec![Workspace::test_new("git-menu")];
            app.active = Some(0);
            app.ensure_test_terminals();
        }
        let workspace = &app.workspaces[app.active.expect("active workspace")];
        let pane_id = workspace.focused_pane_id().expect("focused pane");
        let terminal_id = workspace.terminal_id(pane_id).expect("terminal").clone();
        let cwd = app
            .terminals
            .get(&terminal_id)
            .expect("terminal state")
            .cwd
            .clone();
        app.git_root_for_cwd
            .insert(cwd.clone(), in_git_repo.then_some(cwd));
    }

    #[test]
    fn tab_bar_marks_zoomed_tabs_without_renaming_them() {
        let mut app = AppState::test_new();
        let mut ws = Workspace::test_new("test");
        ws.tabs[0].zoomed = true;
        let custom_tab = ws.test_add_tab(Some("test"));
        ws.tabs[custom_tab].zoomed = true;

        app.workspaces = vec![ws];
        app.active = Some(0);
        app.view.tab_bar_rect = Rect::new(0, 0, 30, 1);
        let view = compute_tab_bar_view(
            &app.workspaces[0],
            &app.terminals,
            app.view.tab_bar_rect,
            0,
            true,
            false,
            &[],
            all_tab_actions_visible(),
        );
        app.view.tab_hit_areas = view.tab_hit_areas;

        let backend = TestBackend::new(30, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_tab_bar(&app, frame, app.view.tab_bar_rect))
            .unwrap();

        let row = buffer_row_text(terminal.backend().buffer(), app.view.tab_bar_rect, 0);
        assert!(row.contains(" 1 Z"), "tab row: {row:?}");
        assert!(row.contains(" test Z"), "tab row: {row:?}");
        assert_eq!(
            app.workspaces[0]
                .tab_display_projection(&app.terminals, 0)
                .map(|projection| projection.full_label())
                .as_deref(),
            Some("1")
        );
        assert_eq!(
            app.workspaces[0]
                .tab_display_projection(&app.terminals, custom_tab)
                .map(|projection| projection.full_label())
                .as_deref(),
            Some("test")
        );
    }

    #[test]
    fn tab_bar_renders_ordered_status_entries_with_separator() {
        let mut app = AppState::test_new();
        let mut ws = Workspace::test_new("test");
        ws.tabs[0].zoomed = true;
        app.tab_bar_right = vec![
            crate::app::state::TabBarStatusSegment::Zoom,
            crate::app::state::TabBarStatusSegment::Text(Some("wintermute".into())),
            crate::app::state::TabBarStatusSegment::Text(Some("14:30".into())),
        ];
        app.tab_bar_right_separator = " · ".into();

        app.workspaces = vec![ws];
        app.active = Some(0);
        app.view.tab_bar_rect = Rect::new(0, 0, 60, 1);
        let content = tab_bar_content_area(&app, app.view.tab_bar_rect);
        let view = compute_tab_bar_view(
            &app.workspaces[0],
            &app.terminals,
            content,
            0,
            true,
            false,
            &[],
            all_tab_actions_visible(),
        );
        app.view.tab_hit_areas = view.tab_hit_areas.clone();

        let backend = TestBackend::new(60, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_tab_bar(&app, frame, app.view.tab_bar_rect))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let row = buffer_row_text(buffer, app.view.tab_bar_rect, 0);
        assert!(
            row.ends_with("ZOOM · wintermute · 14:30"),
            "tab row: {row:?}"
        );
        let status_x = 60 - display_width_u16("ZOOM · wintermute · 14:30");
        assert_eq!(buffer[(status_x, 0)].style().bg, Some(app.palette.accent));
        for rect in &view.tab_hit_areas {
            assert!(rect.x + rect.width <= content.x + content.width);
        }
    }

    #[test]
    fn hidden_status_entries_do_not_leave_dangling_separators() {
        let mut app = AppState::test_new();
        app.tab_bar_right = vec![
            crate::app::state::TabBarStatusSegment::Zoom,
            crate::app::state::TabBarStatusSegment::Text(None),
            crate::app::state::TabBarStatusSegment::Text(Some("wintermute".into())),
        ];
        app.tab_bar_right_separator = " | ".into();
        app.workspaces = vec![Workspace::test_new("test")];
        app.active = Some(0);
        app.view.tab_bar_rect = Rect::new(0, 0, 40, 1);
        let content = tab_bar_content_area(&app, app.view.tab_bar_rect);
        let view = compute_tab_bar_view(
            &app.workspaces[0],
            &app.terminals,
            content,
            0,
            true,
            false,
            &[],
            all_tab_actions_visible(),
        );
        app.view.tab_hit_areas = view.tab_hit_areas;

        let backend = TestBackend::new(40, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_tab_bar(&app, frame, app.view.tab_bar_rect))
            .unwrap();

        let row = buffer_row_text(terminal.backend().buffer(), app.view.tab_bar_rect, 0);
        assert!(row.ends_with("wintermute"), "tab row: {row:?}");
        assert!(!row.contains(" | "), "tab row: {row:?}");
    }

    #[test]
    fn status_reservation_keeps_a_minimum_width_tab_between_scroll_controls() {
        let mut app = AppState::test_new();
        app.tab_bar_right = vec![crate::app::state::TabBarStatusSegment::Text(Some(
            "x".into(),
        ))];
        let mut workspace = Workspace::test_new("test");
        workspace.test_add_tab(None);
        workspace.test_add_tab(None);
        app.workspaces = vec![workspace];
        app.active = Some(0);

        let too_narrow = Rect::new(0, 0, MIN_TAB_STRIP_WIDTH + 1, 1);
        assert_eq!(tab_bar_content_area(&app, too_narrow), too_narrow);

        let wide_enough = Rect::new(0, 0, MIN_TAB_STRIP_WIDTH + 2, 1);
        let content = tab_bar_content_area(&app, wide_enough);
        assert_eq!(content.width, MIN_TAB_STRIP_WIDTH);
        let view = compute_tab_bar_view(
            &app.workspaces[0],
            &app.terminals,
            content,
            0,
            true,
            true,
            &[],
            all_tab_actions_visible(),
        );
        assert!(view.tab_hit_areas[0].width >= MIN_TAB_WIDTH);
    }

    #[test]
    fn combined_status_entries_yield_to_tab_controls_on_narrow_rows() {
        let mut app = AppState::test_new();
        app.tab_bar_right = vec![
            crate::app::state::TabBarStatusSegment::Text(Some(
                "a-hostname-wider-than-the-whole-bar".into(),
            )),
            crate::app::state::TabBarStatusSegment::Text(Some("14:30".into())),
        ];
        app.workspaces = vec![Workspace::test_new("test")];
        app.active = Some(0);
        app.view.tab_bar_rect = Rect::new(0, 0, 30, 1);

        assert_eq!(
            tab_bar_content_area(&app, app.view.tab_bar_rect),
            app.view.tab_bar_rect
        );
        assert_eq!(tab_bar_status_area(&app, app.view.tab_bar_rect), None);

        let view = compute_tab_bar_view(
            &app.workspaces[0],
            &app.terminals,
            tab_bar_content_area(&app, app.view.tab_bar_rect),
            0,
            true,
            true,
            &[],
            all_tab_actions_visible(),
        );
        assert!(view.tab_hit_areas[0].width > 0);
        assert!(view.new_tab_hit_area.width > 0);
    }

    #[test]
    fn cjk_tab_labels_are_centered_by_display_width() {
        let mut app = AppState::test_new();
        let mut ws = Workspace::test_new("test");
        ws.tabs[0].set_custom_name("提交 notes 的反馈".into());

        app.workspaces = vec![ws];
        app.active = Some(0);
        app.view.tab_bar_rect = Rect::new(0, 0, 30, 1);
        let view = compute_tab_bar_view(
            &app.workspaces[0],
            &app.terminals,
            app.view.tab_bar_rect,
            0,
            true,
            false,
            &[],
            all_tab_actions_visible(),
        );
        app.view.tab_hit_areas = view.tab_hit_areas;

        let backend = TestBackend::new(30, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_tab_bar(&app, frame, app.view.tab_bar_rect))
            .unwrap();

        // 17 display columns + 4 padding: two columns each side, wide glyphs
        // starting right after the left padding.
        let rect = app.view.tab_hit_areas[0];
        assert_eq!(rect.width, 21);
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(rect.x, rect.y)].symbol(), " ");
        assert_eq!(buffer[(rect.x + 1, rect.y)].symbol(), " ");
        assert_eq!(buffer[(rect.x + 2, rect.y)].symbol(), "提");
        assert_eq!(buffer[(rect.x + rect.width - 2, rect.y)].symbol(), " ");
        assert_eq!(buffer[(rect.x + rect.width - 1, rect.y)].symbol(), " ");
    }

    #[test]
    fn tab_labels_are_centered_in_their_cells() {
        let mut app = AppState::test_new();
        let mut ws = Workspace::test_new("test");
        ws.tabs[0].set_custom_name("omarchy".into());

        app.workspaces = vec![ws];
        app.active = Some(0);
        app.view.tab_bar_rect = Rect::new(0, 0, 30, 1);
        let view = compute_tab_bar_view(
            &app.workspaces[0],
            &app.terminals,
            app.view.tab_bar_rect,
            0,
            true,
            false,
            &[],
            all_tab_actions_visible(),
        );
        app.view.tab_hit_areas = view.tab_hit_areas;

        let backend = TestBackend::new(30, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_tab_bar(&app, frame, app.view.tab_bar_rect))
            .unwrap();

        let rect = app.view.tab_hit_areas[0];
        let buffer = terminal.backend().buffer();
        let cell: String = (rect.x..rect.x + rect.width)
            .map(|x| buffer[(x, rect.y)].symbol())
            .collect();
        assert_eq!(cell, "  omarchy  ");
    }

    #[test]
    fn active_auto_named_tab_keeps_readable_weight() {
        let mut app = AppState::test_new();
        let ws = Workspace::test_new("test");

        app.workspaces = vec![ws];
        app.active = Some(0);
        app.view.tab_bar_rect = Rect::new(0, 0, 30, 1);
        let view = compute_tab_bar_view(
            &app.workspaces[0],
            &app.terminals,
            app.view.tab_bar_rect,
            0,
            true,
            false,
            &[],
            all_tab_actions_visible(),
        );
        app.view.tab_hit_areas = view.tab_hit_areas;

        let backend = TestBackend::new(30, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_tab_bar(&app, frame, app.view.tab_bar_rect))
            .unwrap();

        let tab_rect = app.view.tab_hit_areas[0];
        let style = terminal.backend().buffer()[(tab_rect.x + 1, tab_rect.y)].style();

        assert_eq!(style.bg, Some(app.palette.accent));
        assert!(!style.add_modifier.contains(Modifier::DIM));
        assert!(!style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn zoom_marker_counts_toward_tab_width() {
        let mut ws = Workspace::test_new("test");
        ws.tabs[0].set_custom_name("abcdefgh".into());
        ws.tabs[0].zoomed = true;

        assert_eq!(tab_width(&ws, &Default::default(), 0), 14);
    }

    #[test]
    fn tab_keyword_icon_is_skipped_for_workspace_keyword_and_when_nerd_font_is_off() {
        let mut ws = Workspace::test_new("test");
        ws.set_custom_name("workspace for Linear".into());
        ws.tabs[0].set_custom_name("Fix Sentry alerts".into());
        let terminals = std::collections::HashMap::new();
        assert_eq!(
            tab_chrome_label_with_icons(&ws, &terminals, 0, 80, true),
            "\u{F6008} Fix Sentry alerts"
        );

        ws.set_custom_name("Sentry workspace".into());
        assert_eq!(
            tab_chrome_label_with_icons(&ws, &terminals, 0, 80, true),
            "Fix Sentry alerts"
        );
        assert_eq!(
            tab_chrome_label_with_icons(&ws, &terminals, 0, 80, false),
            "Fix Sentry alerts"
        );
        ws.set_custom_name("workspace".into());
        assert_eq!(
            tab_chrome_label_with_icons(&ws, &terminals, 0, 10, true),
            "\u{F6008} Fix Sen…"
        );
    }

    #[test]
    fn tab_projection_prefers_detected_agent_title_and_degrades_by_width() {
        let mut app = AppState::test_new();
        let mut ws = Workspace::test_new("test");
        let other = ws.test_split(ratatui::layout::Direction::Horizontal);
        ws.tabs[0].layout.focus_pane(other);
        app.workspaces = vec![ws];
        app.active = Some(0);
        app.ensure_test_terminals();

        let root = app.workspaces[0].tabs[0].root_pane;
        let root_terminal = app.workspaces[0].terminal_id(root).cloned().unwrap();
        app.terminals
            .get_mut(&root_terminal)
            .unwrap()
            .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                ticket_ids: Some(vec!["SCA-1".into()]),
                work_title: Some("wrong pane".into()),
                ..Default::default()
            })
            .unwrap();

        let focused_terminal = app.workspaces[0].terminal_id(other).cloned().unwrap();
        let focused = app.terminals.get_mut(&focused_terminal).unwrap();
        focused.agent_name = Some("Claude".into());
        focused.detected_agent = Some(crate::detect::Agent::Claude);
        focused.set_terminal_title(Some("⠋ Add Subabe management token to Doppler".into()));
        focused
            .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                ticket_ids: Some(vec!["SCA-42".into()]),
                work_title: Some("Review If Context Correctly Enriched Herdr Plus".into()),
                ..Default::default()
            })
            .unwrap();

        assert_eq!(
            tab_chrome_label(&app.workspaces[0], &app.terminals, 0, 80),
            "SCA-42 · Add Subabe management token to Doppler"
        );
        assert_eq!(
            tab_chrome_label(&app.workspaces[0], &app.terminals, 0, 15),
            "SCA-42"
        );
        assert_eq!(
            tab_chrome_label(&app.workspaces[0], &app.terminals, 0, 8),
            "SCA-42"
        );

        app.workspaces[0].tabs[0].set_custom_name("手动名称".into());
        assert_eq!(
            tab_chrome_label(&app.workspaces[0], &app.terminals, 0, 7),
            "手动名…"
        );
    }

    #[test]
    fn fit_tab_display_projection_truncates_only_after_component_fallbacks() {
        use crate::workspace::TabDisplayProjection;

        let projection = TabDisplayProjection::Derived {
            agent: Some("Claude".into()),
            ticket: Some("SCA-42".into()),
            binding: None,
            title: Some("repair login regression".into()),
        };

        assert_eq!(
            fit_tab_display_projection(projection.clone(), 80),
            "SCA-42 · repair login regression"
        );
        assert_eq!(fit_tab_display_projection(projection.clone(), 8), "SCA-42");
        assert_eq!(fit_tab_display_projection(projection, 4), "Cla…");

        let narrow_projection = TabDisplayProjection::Derived {
            agent: Some("cx".into()),
            ticket: Some("LONG-TICKET".into()),
            binding: None,
            title: Some("task".into()),
        };
        assert_eq!(
            fit_tab_display_projection(narrow_projection.clone(), 4),
            "task"
        );
        assert_eq!(fit_tab_display_projection(narrow_projection, 2), "cx");
    }

    #[test]
    fn tab_agent_projection_uses_a_glyph_only_with_nerd_font_enabled() {
        use crate::workspace::TabDisplayProjection;

        let projection = TabDisplayProjection::Derived {
            agent: Some("Claude Code".into()),
            ticket: None,
            binding: None,
            title: None,
        };
        assert_eq!(
            fit_tab_display_projection_with_icons(projection.clone(), 1, true),
            "\u{EC82}"
        );
        assert_eq!(
            fit_tab_display_projection_with_icons(projection, 20, false),
            "Claude Code"
        );
    }

    #[test]
    fn derived_projection_never_prefixes_the_agent_onto_a_title() {
        use crate::workspace::TabDisplayProjection;

        let titled = TabDisplayProjection::Derived {
            agent: Some("claude".into()),
            ticket: None,
            binding: None,
            title: Some("Create Python Script".into()),
        };
        assert_eq!(
            fit_tab_display_projection(titled, 80),
            "Create Python Script"
        );

        let agent_only = TabDisplayProjection::Derived {
            agent: Some("codex".into()),
            ticket: None,
            binding: None,
            title: None,
        };
        assert_eq!(fit_tab_display_projection(agent_only, 80), "codex");
    }

    #[test]
    fn tab_projection_distinguishes_owner_history_and_unowned_pr() {
        let mut app = AppState::test_new();
        app.workspaces = vec![Workspace::test_new("owner"), Workspace::test_new("review")];
        app.ensure_test_terminals();
        let terminal_ids = app
            .workspaces
            .iter()
            .map(|workspace| {
                workspace
                    .terminal_id(workspace.tabs[0].root_pane)
                    .cloned()
                    .unwrap()
            })
            .collect::<Vec<_>>();
        for (terminal_id, role, active_owner) in [
            (
                &terminal_ids[0],
                crate::work_context::PaneWorkRole::Ship,
                true,
            ),
            (
                &terminal_ids[1],
                crate::work_context::PaneWorkRole::Review,
                false,
            ),
        ] {
            let context = crate::work_context::PaneWorkContext {
                pr_urls: vec!["https://github.com/o/r/pull/42".into()],
                role: Some(role),
                active_owner,
                ..Default::default()
            }
            .normalized_spawn_binding()
            .unwrap();
            app.terminals
                .get_mut(terminal_id)
                .unwrap()
                .replace_prevalidated_manual_work_context(context);
        }

        assert_eq!(
            tab_chrome_label(&app.workspaces[0], &app.terminals, 0, 80),
            "owner/ship"
        );
        assert_eq!(
            tab_chrome_label(&app.workspaces[1], &app.terminals, 0, 80),
            "history/review"
        );
        app.terminals
            .get_mut(&terminal_ids[0])
            .unwrap()
            .clear_active_work_owner();
        assert_eq!(
            tab_chrome_label(&app.workspaces[1], &app.terminals, 0, 80),
            "history/review · un-owned"
        );
    }

    #[test]
    fn plain_shell_tab_bar_uses_number_instead_of_cwd_title() {
        let mut app = AppState::test_new();
        let ws = Workspace::test_new("test");
        app.workspaces = vec![ws];
        app.ensure_test_terminals();
        let terminal_id = app.workspaces[0].tabs[0]
            .terminal_id(app.workspaces[0].tabs[0].layout.focused())
            .cloned()
            .unwrap();
        let terminal = app.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_terminal_title(Some("cx-personal-20260801-feature".into()));

        assert_eq!(
            tab_chrome_label(&app.workspaces[0], &app.terminals, 0, 80),
            "1"
        );
    }

    #[test]
    fn tab_width_uses_display_width_for_cjk_labels() {
        let mut ws = Workspace::test_new("test");
        ws.tabs[0].set_custom_name("提交 notes 的反馈".into());

        assert_eq!(
            tab_width(&ws, &Default::default(), 0),
            display_width_u16("提交 notes 的反馈") + 4
        );
    }

    #[test]
    fn tab_bar_renders_trailing_cjk_character() {
        let mut app = AppState::test_new();
        let mut ws = Workspace::test_new("test");
        ws.tabs[0].set_custom_name("提交 notes 的反馈".into());

        app.active = Some(0);
        app.workspaces = vec![ws];
        app.view.tab_bar_rect = Rect::new(0, 0, 30, 1);
        let view = compute_tab_bar_view(
            &app.workspaces[0],
            &app.terminals,
            app.view.tab_bar_rect,
            0,
            true,
            false,
            &[],
            all_tab_actions_visible(),
        );
        app.view.tab_hit_areas = view.tab_hit_areas;

        let backend = TestBackend::new(30, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_tab_bar(&app, frame, app.view.tab_bar_rect))
            .unwrap();

        let row = buffer_row_text(terminal.backend().buffer(), app.view.tab_bar_rect, 0);
        assert!(row.contains('馈'), "tab row: {row:?}");
    }

    fn tab_bar_view_at(width: u16, tab_name: &str) -> (crate::workspace::Workspace, TabBarView) {
        let app = AppState::test_new();
        let ws = Workspace::test_new(tab_name);
        let view = compute_tab_bar_view(
            &ws,
            &app.terminals,
            Rect::new(0, 0, width, 1),
            0,
            true,
            true,
            &[],
            all_tab_actions_visible(),
        );
        (ws, view)
    }

    #[test]
    fn pane_toggle_buttons_sit_at_the_right_end_of_the_tab_row() {
        for width in [80u16, 120] {
            let (_, view) = tab_bar_view_at(width, "test");
            assert_eq!(
                view.git_menu_button_hit_area,
                Rect::new(width - 16, 0, GIT_MENU_BUTTON_WIDTH, 1),
                "width {width}"
            );
            assert_eq!(
                view.repo_editor_button_hit_area.right(),
                view.add_action_button_hit_area.x,
                "width {width}"
            );
            assert_eq!(
                view.repo_editor_button_hit_area.width, REPO_EDITOR_BUTTON_WIDTH,
                "width {width}"
            );
            assert_eq!(
                view.pane_toggle_below_hit_area,
                Rect::new(width - 6, 0, 3, 1),
                "width {width}"
            );
            assert_eq!(
                view.pane_toggle_right_hit_area,
                Rect::new(width - 3, 0, 3, 1),
                "width {width}"
            );
            // The tab strip and the `+` button stay left of every action.
            assert!(view.new_tab_hit_area.x + view.new_tab_hit_area.width <= width - 16);
            assert!(view
                .tab_hit_areas
                .iter()
                .all(|rect| rect.x + rect.width <= width - 16));
        }
    }

    #[test]
    fn pane_toggle_buttons_are_hidden_when_the_row_cannot_spare_the_width() {
        let app = AppState::test_new();
        let mut ws = Workspace::test_new("test");
        let long_tab = ws.test_add_tab(Some("a-very-long-tab-title-here"));
        ws.active_tab = long_tab;
        let narrow = compute_tab_bar_view(
            &ws,
            &app.terminals,
            Rect::new(0, 0, 20, 1),
            0,
            true,
            true,
            &[],
            all_tab_actions_visible(),
        );
        assert_eq!(narrow.pane_toggle_below_hit_area, Rect::default());
        assert_eq!(narrow.pane_toggle_right_hit_area, Rect::default());
        assert_eq!(narrow.git_menu_button_hit_area, Rect::default());
        assert_eq!(narrow.repo_editor_button_hit_area, Rect::default());

        // The same long title with room to spare keeps the buttons.
        let wide = compute_tab_bar_view(
            &ws,
            &app.terminals,
            Rect::new(0, 0, 120, 1),
            0,
            true,
            true,
            &[],
            all_tab_actions_visible(),
        );
        assert_eq!(wide.pane_toggle_below_hit_area, Rect::new(114, 0, 3, 1));
        assert_eq!(wide.git_menu_button_hit_area, Rect::new(104, 0, 10, 1));

        // Without mouse chrome there are no clickable controls at all.
        let app = AppState::test_new();
        let ws = Workspace::test_new("test");
        let view = compute_tab_bar_view(
            &ws,
            &app.terminals,
            Rect::new(0, 0, 120, 1),
            0,
            true,
            false,
            &[],
            all_tab_actions_visible(),
        );
        assert_eq!(view.pane_toggle_below_hit_area, Rect::default());
        assert_eq!(view.pane_toggle_right_hit_area, Rect::default());
        assert_eq!(view.git_menu_button_hit_area, Rect::default());
        assert_eq!(view.repo_editor_button_hit_area, Rect::default());
    }

    #[test]
    fn pane_toggle_buttons_render_their_glyphs_and_accent_the_close_side() {
        let mut app = AppState::test_new();
        let mut ws = Workspace::test_new("test");
        ws.test_split(ratatui::layout::Direction::Vertical);
        let root = ws.tabs[0].root_pane;
        ws.tabs[0].layout.focus_pane(root);
        app.workspaces = vec![ws];
        app.active = Some(0);
        app.mouse_capture = true;
        app.view.tab_bar_rect = Rect::new(0, 0, 50, 1);
        let view = compute_tab_bar_view(
            &app.workspaces[0],
            &app.terminals,
            app.view.tab_bar_rect,
            0,
            true,
            true,
            &[],
            all_tab_actions_visible(),
        );
        app.view.tab_hit_areas = view.tab_hit_areas;
        app.view.new_tab_hit_area = view.new_tab_hit_area;
        app.view.pane_toggle_below_hit_area = view.pane_toggle_below_hit_area;
        app.view.pane_toggle_right_hit_area = view.pane_toggle_right_hit_area;

        let backend = TestBackend::new(50, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_tab_bar(&app, frame, app.view.tab_bar_rect);
                render_tab_action_buttons(&app, frame);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let row = buffer_row_text(buffer, app.view.tab_bar_rect, 0);
        assert!(row.contains(PANE_TOGGLE_BELOW_GLYPH), "tab row: {row:?}");
        assert!(row.contains(PANE_TOGGLE_RIGHT_GLYPH), "tab row: {row:?}");

        let below_style = buffer[(app.view.pane_toggle_below_hit_area.x + 1, 0)].style();
        let right_style = buffer[(app.view.pane_toggle_right_hit_area.x + 1, 0)].style();
        assert_eq!(below_style.fg, Some(app.palette.accent));
        assert_ne!(right_style.fg, Some(app.palette.accent));
    }

    #[test]
    fn git_menu_layout_opens_downward_and_adds_only_the_behind_status_row() {
        let anchor = Rect::new(70, 2, GIT_MENU_BUTTON_WIDTH, 1);
        let area = Rect::new(0, 0, 120, 40);

        for (ahead_behind, expected_rows) in [
            (None, 4),
            (Some((2, 0)), 4),
            (Some((0, 1)), 5),
            (Some((2, 3)), 5),
        ] {
            assert_eq!(
                git_menu_status_visible(ahead_behind),
                ahead_behind.is_some_and(|(_, behind)| behind > 0)
            );
            let layout = git_menu_dropdown_layout(anchor, area, 0, ahead_behind, true)
                .expect("menu fits below the tab row");
            assert_eq!(layout.rect.y, anchor.y + anchor.height);
            assert_eq!(layout.visible_rows, expected_rows);
        }

        let unavailable = git_menu_dropdown_layout(anchor, area, 3, Some((2, 3)), false)
            .expect("info row fits below the tab row");
        assert_eq!(unavailable.rect.y, anchor.y + anchor.height);
        assert_eq!(unavailable.visible_rows, 1);
    }

    #[test]
    fn git_menu_renders_actions_in_order_then_nonselectable_status() {
        let mut app = AppState::test_new();
        set_focused_git_availability(&mut app, true);
        app.set_server_mode(crate::app::Mode::GitMenu);
        app.view.git_menu_popup_rect = Rect::new(0, 1, 34, 5);
        app.view.git_menu_row_hit_areas = (1..=5).map(|y| Rect::new(0, y, 34, 1)).collect();
        app.status_git_ahead_behind = Some((0, 1));

        let backend = TestBackend::new(40, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render_git_menu(&app, frame)).unwrap();
        let buffer = terminal.backend().buffer();
        let rows = (1..=5)
            .map(|y| buffer_row_text(buffer, Rect::new(0, y, 34, 1), y))
            .collect::<Vec<_>>();
        assert!(rows[0].contains("Pull"), "{rows:?}");
        assert!(rows[1].contains("Commit"), "{rows:?}");
        assert!(rows[2].contains("Push"), "{rows:?}");
        assert!(rows[3].contains("Create PR"), "{rows:?}");
        assert!(rows[4].contains(GIT_MENU_STATUS), "{rows:?}");
        assert_ne!(buffer[(1, 5)].style().bg, Some(app.palette.accent));
    }

    #[test]
    fn git_menu_dims_button_and_renders_only_info_outside_repository() {
        let mut app = AppState::test_new();
        set_focused_git_availability(&mut app, false);
        app.mouse_capture = true;
        app.set_server_mode(crate::app::Mode::GitMenu);
        app.view.git_menu_button_hit_area = Rect::new(0, 0, GIT_MENU_BUTTON_WIDTH, 1);
        app.view.git_menu_popup_rect = Rect::new(0, 1, 34, 1);
        app.view.git_menu_row_hit_areas = vec![Rect::new(0, 1, 34, 1)];

        let backend = TestBackend::new(40, 3);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_tab_action_buttons(&app, frame);
                render_git_menu(&app, frame);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let button_style = buffer[(1, 0)].style();
        assert_eq!(
            button_style.fg,
            Some(readable_fg_on(
                app.palette.surface0,
                &[app.palette.overlay1, app.palette.text]
            ))
        );
        assert!(button_style.add_modifier.contains(Modifier::DIM));
        let row = buffer_row_text(buffer, Rect::new(0, 1, 34, 1), 1);
        assert!(row.contains(GIT_MENU_UNAVAILABLE), "menu row: {row:?}");
        assert!(!row.contains("Pull"), "menu row: {row:?}");
    }

    #[test]
    fn repo_editor_button_renders_before_add_action_and_dims_without_an_editor() {
        let mut app = AppState::test_new();
        app.nerd_font = false;
        app.mouse_capture = true;
        let ws = Workspace::test_new("test");
        let view = compute_tab_bar_view(
            &ws,
            &app.terminals,
            Rect::new(0, 0, 80, 1),
            0,
            true,
            true,
            &[],
            all_tab_actions_visible(),
        );
        app.view.repo_editor_button_hit_area = view.repo_editor_button_hit_area;
        app.view.add_action_button_hit_area = view.add_action_button_hit_area;
        assert_eq!(
            app.view.repo_editor_button_hit_area.right(),
            app.view.add_action_button_hit_area.x
        );

        let backend = TestBackend::new(80, 1);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| render_tab_action_buttons(&app, frame))
            .expect("draw disabled button");
        let cell = &terminal.backend().buffer()[(
            app.view.repo_editor_button_hit_area.x + 1,
            app.view.repo_editor_button_hit_area.y,
        )];
        assert_eq!(cell.symbol(), "n");
        assert!(cell.style().add_modifier.contains(Modifier::DIM));

        app.repo_editor_argv = Some(vec!["nvim".into()]);
        terminal
            .draw(|frame| render_tab_action_buttons(&app, frame))
            .expect("draw enabled button");
        let cell = &terminal.backend().buffer()[(
            app.view.repo_editor_button_hit_area.x + 1,
            app.view.repo_editor_button_hit_area.y,
        )];
        assert!(!cell.style().add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn user_action_buttons_have_ordered_hit_areas_at_required_widths() {
        let app = AppState::test_new();
        let ws = Workspace::test_new("test");
        let actions = vec![
            (0, "test".to_string()),
            (1, "a very long action name that must truncate".to_string()),
        ];

        for width in [80, 120] {
            let view = compute_tab_bar_view(
                &ws,
                &app.terminals,
                Rect::new(0, 0, width, 1),
                0,
                true,
                true,
                &actions,
                all_tab_actions_visible(),
            );
            assert_eq!(
                view.add_action_button_hit_area.width,
                ADD_ACTION_BUTTON_WIDTH
            );
            assert_eq!(
                view.repo_editor_button_hit_area.right(),
                view.add_action_button_hit_area.x
            );
            assert_eq!(view.user_action_hit_areas.len(), 2);
            assert_eq!(
                view.user_action_hit_areas[0].1.x,
                view.add_action_button_hit_area.right()
            );
            assert_eq!(
                view.user_action_hit_areas[1].1.right(),
                view.git_menu_button_hit_area.x
            );
            assert_eq!(view.pane_toggle_right_hit_area.right(), width);
            if width == 80 {
                assert!(
                    view.user_action_hit_areas[1].1.width
                        < display_width_u16(&actions[1].1).saturating_add(2)
                );
            } else {
                assert_eq!(
                    view.user_action_hit_areas[1].1.width,
                    display_width_u16(&actions[1].1).saturating_add(2)
                );
            }
        }
    }
}

#[cfg(test)]
mod action_icons {
    use super::*;
    use crate::app::state::AppState;
    use crate::workspace::Workspace;
    use ratatui::{backend::TestBackend, Terminal};

    /// Renders the tab row and returns the editor and add-action cells, each as
    /// the text inside its hit area plus that hit area's width.
    fn rendered_buttons(nerd_font: bool) -> [(String, u16, u16); 2] {
        let mut app = AppState::test_new();
        app.nerd_font = nerd_font;
        app.mouse_capture = true;
        app.workspaces = vec![Workspace::test_new("test")];
        app.active = Some(0);
        let area = Rect::new(0, 0, 80, 1);
        app.view.tab_bar_rect = area;
        let visibility = TabActionVisibility::from_state(&app);
        let view = compute_tab_bar_view_with_icons(
            &app.workspaces[0],
            &app.terminals,
            area,
            0,
            true,
            true,
            &[],
            visibility,
            nerd_font,
        );
        app.view.repo_editor_button_hit_area = view.repo_editor_button_hit_area;
        app.view.add_action_button_hit_area = view.add_action_button_hit_area;

        let mut terminal = Terminal::new(TestBackend::new(80, 1)).unwrap();
        terminal
            .draw(|frame| render_tab_action_buttons(&app, frame))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let cell_text = |rect: Rect| -> (String, u16, u16) {
            let text: String = (rect.x..rect.right())
                .map(|x| buffer[(x, 0)].symbol())
                .collect();
            (
                text.clone(),
                rect.width,
                crate::ui::text::display_width_u16(&text),
            )
        };
        [
            cell_text(view.repo_editor_button_hit_area),
            cell_text(view.add_action_button_hit_area),
        ]
    }

    #[test]
    fn nerd_font_on_renders_glyphs_with_matching_hit_areas() {
        let [editor, add] = rendered_buttons(true);
        assert_eq!(editor.0, " \u{F07C} ");
        assert_eq!(add.0, " \u{F0A00} ");
        assert_eq!(editor.1, editor.2);
        assert_eq!(add.1, add.2);
    }

    #[test]
    fn nerd_font_off_keeps_text_with_matching_hit_areas() {
        let [editor, add] = rendered_buttons(false);
        assert_eq!(editor.0, " nvim ");
        assert_eq!(add.0, " + Action ");
        assert_eq!(
            (editor.1, add.1),
            (REPO_EDITOR_BUTTON_WIDTH, ADD_ACTION_BUTTON_WIDTH)
        );
        assert_eq!(editor.1, editor.2);
        assert_eq!(add.1, add.2);
    }
}
