use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::Span,
    Frame,
};

pub(crate) mod add_project;
mod aloop_run;
mod command_palette;
mod dialogs;
pub(crate) mod dock;
#[path = "ui/dock/context.rs"]
mod dock_context;
#[path = "ui/dock/scratchpad.rs"]
mod dock_scratchpad;
#[path = "ui/dock/shortcuts.rs"]
mod dock_shortcuts;
pub(crate) mod dropdown;
pub(crate) mod goals;
mod home;
pub(crate) mod hyperspace;
mod icons;
mod inbox;
mod keybind_help;
pub(crate) mod loop_runs;
mod markdown;
mod menus;
mod mobile;
mod navigator;
pub(crate) mod notepad;
pub(crate) mod notepad_agent;
pub(crate) mod notepad_usage;
mod onboarding;
mod panes;
pub(crate) mod pomodoro;
pub(crate) mod pr_actions;
mod release_notes;
mod scrollbar;
mod settings;
pub(crate) mod sidebar;
pub(crate) use dock::symphony::dashboard_link_rect as dock_symphony_dashboard_link_rect;
pub(crate) use dock::symphony::dashboard_url as dock_symphony_dashboard_url;
/// Exposed so the inbox tests can assert the queue and the sidebar answer the
/// blocked question identically. Both production paths consume the pane's
/// canonical agent projection.
#[cfg(test)]
pub(crate) use sidebar::entry_is_blocked;
pub(crate) mod status;
mod symphony;
mod tab_surface;
mod tabs;
mod tooltip;
pub(crate) use tooltip::hovered_control_at;
pub(crate) mod agent_picker;
mod bar;
pub(crate) mod board;
pub(crate) mod text;
pub(crate) mod ticket_actions;
pub(crate) mod usage;
mod user_actions;
mod widgets;
mod work_link_picker;
pub(crate) mod work_list_detail;
mod work_status;
pub(crate) mod work_view;

use self::add_project::render_add_project_overlay;
use self::agent_picker::render_agent_picker;
use self::command_palette::render_command_palette;
use self::dialogs::{
    render_confirm_close_overlay, render_new_linked_worktree_overlay,
    render_open_existing_worktree_overlay, render_remove_worktree_overlay, render_rename_overlay,
};
use self::dock::render_dock;
use self::keybind_help::render_keybind_help_overlay;
use self::loop_runs::render_loop_run_history;
use self::menus::{
    render_context_menu, render_copy_mode_overlay, render_global_launcher_menu,
    render_navigate_overlay, render_prefix_overlay, render_resize_overlay,
};
use self::mobile::{
    compute_mobile_header_hit_areas, is_mobile_width, mobile_switcher_max_scroll_for_height,
    mobile_toast_banner_rect, render_mobile_header, render_mobile_panel,
    render_mobile_toast_banner,
};
use self::navigator::render_navigator_overlay;
pub(crate) use self::onboarding::onboarding_welcome_continue_rect;
use self::onboarding::render_onboarding_overlay;
pub(crate) use self::panes::popup_pane_rects;
use self::panes::{render_empty, render_popup_pane, resize_popup_pane};
pub(crate) use self::release_notes::{
    product_announcement_display_lines, release_notes_close_button_rect,
    release_notes_display_lines, release_notes_wrapped_line_count, PRODUCT_ANNOUNCEMENT_MODAL_SIZE,
    RELEASE_NOTES_MODAL_SIZE,
};
use self::release_notes::{render_product_announcement_overlay, render_release_notes_overlay};
pub(crate) use self::scrollbar::{
    pane_scrollbar_rect, release_notes_scrollbar_rect, scrollbar_offset_from_drag_row,
    scrollbar_offset_from_row, scrollbar_thumb_grab_offset, should_show_scrollbar,
};
use self::settings::render_settings_overlay;
pub(crate) use self::sidebar::compute_tab_card_areas;
#[cfg(test)]
pub(crate) use self::sidebar::sidebar_object_menu_layout as sidebar_object_menu_layout_for_test;
#[cfg(test)]
pub(crate) use self::sidebar::tests::sidebar_work_item_fixture;
#[cfg(test)]
pub(crate) use self::sidebar::SPACES_SECTION_TITLE;
#[cfg(test)]
pub(crate) use self::sidebar::{compute_agent_card_areas, workspace_drop_indicator_row};
pub(crate) use self::sidebar::{
    compute_sidebar_section_header_areas, initial_collapsed_sidebar_groups,
};
use self::sidebar::{
    render_sidebar, render_sidebar_collapsed, render_sidebar_filter_menu,
    render_sidebar_group_menu, render_sidebar_new_menu, render_sidebar_new_thread,
    render_sidebar_object_menu, render_sidebar_pod_picker, render_sidebar_project_menu,
    render_sidebar_settled_menu, render_sidebar_snooze_menu, render_sidebar_sort_menu,
    render_sidebar_subgroup_picker,
};
#[cfg(test)]
pub(crate) use self::status::focused_context as focused_status_context_for_test;
use self::status::{
    config_diagnostic_marker_rect, copy_feedback_rect, render_config_diagnostic,
    render_copy_feedback, render_status_bar, render_toast_notification, toast_notification_rect,
};
use self::symphony::render as render_symphony;
pub(crate) use self::tab_surface::{
    compute_tab_surface, render_tab_surface, resize_tab_surface, TabSurfaceLayout,
};
use self::tabs::{render_git_menu, render_tab_action_buttons, render_tab_bar};
use self::tooltip::render_hover_tooltip;
use self::usage::render as render_usage;
use self::user_actions::render_add_action_overlay;
use self::work_link_picker::render_work_link_picker;
use self::work_view::render as render_work_view;

pub(crate) use self::sidebar::local_agent_panel_identities;
pub(crate) use self::{
    dialogs::{
        confirm_close_button_rects, confirm_close_popup_rect, new_linked_worktree_button_rects,
        new_linked_worktree_inner_rect, open_existing_worktree_button_rects,
        open_existing_worktree_inner_rect, open_existing_worktree_max_visible_rows,
        open_existing_worktree_visible_start, remove_worktree_button_rects,
        remove_worktree_popup_rect, rename_button_rects,
    },
    settings::{
        general_row_offsets, general_row_scroll, settings_areas, settings_button_rects,
        settings_nav_scroll, settings_popup_height, settings_show_primary_action,
        SETTINGS_POPUP_WIDTH,
    },
    sidebar::{
        agent_counts_by_workspace, agent_panel_entries, all_agent_panel_entries,
        collapsed_sidebar_row_scroll, collapsed_sidebar_scroll_for_target,
        collapsed_sidebar_sections, collapsed_sidebar_toggle_rect, compute_sidebar_row_areas,
        compute_workspace_card_areas, expanded_sidebar_toggle_rect, needs_you_more_at,
        needs_you_row_at, normalized_workspace_scroll, relative_agent_navigation_entry,
        remote_agent_panel_entries, remote_agent_panel_entries_at, remote_agent_row_at,
        repo_group_focus_plan, sidebar_agent_run_at, sidebar_aloop_target_at,
        sidebar_dim_header_at, sidebar_filter_anchor_rect, sidebar_filter_menu_layout,
        sidebar_filter_options, sidebar_group_menu_layout, sidebar_group_mode_anchor_rect_for_app,
        sidebar_header_new_menu_rect, sidebar_header_new_thread_rect, sidebar_header_overflow_rect,
        sidebar_header_search_rect_for_app, sidebar_header_star_filter_rect,
        sidebar_missive_copy_url, sidebar_nested_header_at, sidebar_new_menu_layout,
        sidebar_new_thread_layout, sidebar_new_thread_matches, sidebar_object_action_at,
        sidebar_object_at, sidebar_object_menu_item_at, sidebar_object_menu_items,
        sidebar_pod_header_at, sidebar_pod_member_at, sidebar_project_anchor_rect,
        sidebar_project_menu_layout, sidebar_project_menu_matches, sidebar_pull_request_actions,
        sidebar_pull_request_key, sidebar_row_index_for_workspace, sidebar_row_scroll_for_target,
        sidebar_rows, sidebar_separator_col, sidebar_settled_menu_layout, sidebar_show_more_at,
        sidebar_show_more_key, sidebar_snooze_menu_layout, sidebar_symphony_job_at,
        sidebar_thread_entries, sidebar_ticket_action_entries, sidebar_ticket_target,
        sidebar_unassigned_spawn_at, sidebar_work_group_activation, workspace_agent_chevron_rect,
        workspace_drop_slots, workspace_list_entries, workspace_list_entries_expanded,
        workspace_list_rect_for_app, workspace_list_scroll_metrics, workspace_list_scrollbar_rect,
        workspace_parent_group_state, AgentPanelEntry, AgentPanelLocalIdentity, NeedsYouTarget,
        PodTarget, RemoteAgentPanelEntry, SidebarFilterOption, SidebarObjectMenuItem, SidebarRow,
        WorkspaceListEntry, SETTLED_MENU_LABELS,
    },
};

#[cfg(test)]
pub(crate) use self::sidebar::compute_remote_agent_row_areas;
#[cfg(test)]
pub(crate) use self::sidebar::{sidebar_group_mode_anchor_rect, sidebar_header_search_rect};
#[cfg(test)]
pub(crate) use self::tabs::REPO_EDITOR_BUTTON_WIDTH;
pub(crate) use self::{
    keybind_help::keybind_help_lines,
    mobile::{
        mobile_switcher_areas, mobile_switcher_max_scroll, mobile_switcher_target_at,
        mobile_switcher_workspace_doc_range, MobileSwitcherTarget,
    },
    panes::{apply_pane_chrome, pane_inner_rect, pane_is_scrolled_back},
    tab_surface::{tab_surface_cursor, tab_surface_hyperlinks, TabSurfaceView},
    tabs::{
        compute_tab_bar_view_with_icons, tab_action_fallback_hit_areas, tab_bar_content_area,
        visible_user_actions, TabActionVisibility,
    },
    widgets::{centered_popup_rect, modal_stack_areas},
};
use crate::app::state::{
    ClientInputOwner, ClientOverlay, InputOwner, Palette, ServerInputOwner, ViewLayout,
};
use crate::app::{AppState, Mode};
use crate::terminal::TerminalRuntimeRegistry;

const COLLAPSED_WIDTH: u16 = 4; // num + space + dot + separator
pub(crate) const DOCK_COLLAPSED_WIDTH: u16 = 1;
pub(crate) const DOCK_DEFAULT_WIDTH: u16 = 32;
const EMPTY_DOCK_PREFERRED_WIDTH: u16 = 80;
pub(crate) const DOCK_MIN_WIDTH: u16 = 18;
/// Generous because the dock holds an editor: the ceiling that matters is the one
/// the layout already enforces -- the terminal keeps `DOCK_MIN_TERMINAL_WIDTH` no
/// matter what is stored here.
pub(crate) const DOCK_MAX_WIDTH: u16 = 160;
pub(crate) const DOCK_MIN_TERMINAL_WIDTH: u16 = 10;

/// Compute view geometry and reconcile pane sizes.
/// Called before render to separate mutation from drawing.
#[cfg_attr(not(test), allow(dead_code))]
pub fn compute_view(app: &mut AppState, area: Rect) {
    let terminal_runtimes = TerminalRuntimeRegistry::new();
    compute_view_with_runtime_registry(app, &terminal_runtimes, area);
}

pub fn compute_view_with_runtime_registry(
    app: &mut AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    area: Rect,
) {
    compute_view_internal(
        app,
        terminal_runtimes,
        area,
        true,
        crate::kitty_graphics::HostCellSize::default(),
    );
}

pub fn compute_view_with_cell_size(
    app: &mut AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    area: Rect,
    cell_size: crate::kitty_graphics::HostCellSize,
) {
    compute_view_internal(app, terminal_runtimes, area, true, cell_size);
}

/// Compute view geometry for a client-sized render without resizing pane runtimes.
///
/// This is used by the headless server when a non-foreground client needs its
/// own frame size while the shared pane runtimes stay pinned to the foreground
/// client.
pub(crate) fn compute_view_without_resizing_panes(
    app: &mut AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    area: Rect,
) {
    compute_view_internal(
        app,
        terminal_runtimes,
        area,
        false,
        crate::kitty_graphics::HostCellSize::default(),
    );
}

fn resize_background_tab_panes_to_area(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    terminal_area: Rect,
    cell_size: crate::kitty_graphics::HostCellSize,
) {
    for (ws_idx, ws) in app.workspaces.iter().enumerate() {
        for (tab_idx, tab) in ws.tabs.iter().enumerate() {
            if app.active == Some(ws_idx) && tab_idx == ws.active_tab_index() {
                continue;
            }
            resize_tab_surface(app, terminal_runtimes, tab, terminal_area, cell_size);
        }
    }
}

fn resize_background_tab_panes_for_desktop(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    main_area: Rect,
    cell_size: crate::kitty_graphics::HostCellSize,
) {
    for (ws_idx, ws) in app.workspaces.iter().enumerate() {
        let (_, terminal_area) = desktop_tab_bar_and_terminal_area(app, ws, main_area);
        for (tab_idx, tab) in ws.tabs.iter().enumerate() {
            if app.active == Some(ws_idx) && tab_idx == ws.active_tab_index() {
                continue;
            }
            resize_tab_surface(app, terminal_runtimes, tab, terminal_area, cell_size);
        }
    }
}

fn desktop_tab_bar_and_terminal_area(
    app: &AppState,
    ws: &crate::workspace::Workspace,
    main_area: Rect,
) -> (Rect, Rect) {
    let hide_single_tab_bar = app.hide_tab_bar_when_single_tab && ws.tabs.len() == 1;
    if hide_single_tab_bar || main_area.height <= 1 {
        return (Rect::default(), main_area);
    }
    match app.tab_bar_position {
        crate::config::TabBarPositionConfig::Top => {
            let [tab_bar_rect, terminal_area] =
                Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(main_area);
            (tab_bar_rect, terminal_area)
        }
        crate::config::TabBarPositionConfig::Bottom => {
            let [terminal_area, tab_bar_rect] =
                Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(main_area);
            (tab_bar_rect, terminal_area)
        }
        // An empty rect is the established "no tab row" shape here: the mode bar
        // checks the height and hit-testing is rect-based, so nothing downstream
        // needs to learn about hiding.
        crate::config::TabBarPositionConfig::Hidden => (Rect::default(), main_area),
    }
}

fn compute_view_internal(
    app: &mut AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    area: Rect,
    resize_panes: bool,
    cell_size: crate::kitty_graphics::HostCellSize,
) {
    compute_view_internal_at(
        app,
        terminal_runtimes,
        area,
        resize_panes,
        cell_size,
        std::time::Instant::now(),
        crate::app::settled::unix_seconds(std::time::SystemTime::now()),
    );
}

#[cfg(test)]
pub(crate) fn compute_view_at(
    app: &mut AppState,
    area: Rect,
    observed_at: std::time::Instant,
    observed_unix_s: u64,
) {
    compute_view_internal_at(
        app,
        &TerminalRuntimeRegistry::new(),
        area,
        true,
        crate::kitty_graphics::HostCellSize::default(),
        observed_at,
        observed_unix_s,
    );
}

fn compute_view_internal_at(
    app: &mut AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    area: Rect,
    resize_panes: bool,
    cell_size: crate::kitty_graphics::HostCellSize,
    observed_at: std::time::Instant,
    observed_unix_s: u64,
) {
    app.view_observed_at = observed_at;
    app.refresh_board_agent_lines(terminal_runtimes, observed_unix_s);
    app.view_observed_unix_s = observed_unix_s;
    app.reconcile_sidebar_presentation();
    app.reconcile_dock_context_tabs();
    if !app.dock_collapsed {
        if let Some(object) = app.dock_object_preview.clone() {
            let surface = object.surface;
            app.open_dock_object(object, crate::app::state::DockTabOrigin::User);
            app.dock_pr_focused = surface == crate::app::DockSurface::Pr;
            app.dock_linear_focused = surface == crate::app::DockSurface::Linear;
        }
    }
    if uses_mobile_layout(app, area) {
        compute_mobile_view(app, terminal_runtimes, area, resize_panes, cell_size);
        if sidebar::sidebar_areas_menu_layout(app, area).is_none() {
            app.sidebar_areas_menu_selected = None;
        }
        if sidebar::window_cycle_mode_menu_layout(app, area).is_none() {
            app.window_cycle_menu_open = false;
        }
        return;
    }

    // Full-width top status row (tmux-parity): spans the whole client, with
    // sidebar + tabs below. Reserve it before the horizontal split so the bar
    // is never squeezed into the main column only.
    let (status_bar_rect, body_area) = if app.status_bar_enabled && area.height > 1 {
        let [status_bar_rect, body_area] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(area);
        (status_bar_rect, body_area)
    } else {
        (Rect::default(), area)
    };

    let sidebar_w = if app.sidebar_collapsed {
        match app.sidebar_collapsed_mode {
            crate::config::SidebarCollapsedModeConfig::Compact => COLLAPSED_WIDTH,
            crate::config::SidebarCollapsedModeConfig::Hidden => 0,
        }
    } else {
        app.sidebar_width
            .clamp(app.sidebar_min_width, app.sidebar_max_width)
    };

    let available_after_sidebar = body_area.width.saturating_sub(sidebar_w);
    let main_view_active = app.symphony_detail.is_some()
        || app.board_view.is_some()
        || app.loop_run_history_detail.is_some()
        || app.aloop_run_detail.is_some()
        || app.usage_view.is_some()
        || app.work_view.is_some()
        || app.dock_object_preview.is_some()
        || app.home.is_some()
        || app.inbox.is_some();
    let dock_w = if app.dock_collapsed {
        DOCK_COLLAPSED_WIDTH
    } else if available_after_sidebar < DOCK_MIN_WIDTH + DOCK_MIN_TERMINAL_WIDTH {
        app.dock_collapsed = true;
        DOCK_COLLAPSED_WIDTH
    } else if app.dock_maximized {
        // Maximised, the dock owns the whole main area; the sidebar keeps its
        // width so the session stays navigable.
        available_after_sidebar
    } else {
        let requested_width =
            if app.dock_open_surfaces.is_empty() && !main_view_active && area.width > 80 {
                app.dock_width.max(EMPTY_DOCK_PREFERRED_WIDTH)
            } else {
                app.dock_width
            };
        requested_width
            .clamp(DOCK_MIN_WIDTH, DOCK_MAX_WIDTH)
            .min(available_after_sidebar.saturating_sub(DOCK_MIN_TERMINAL_WIDTH))
    };

    let [sidebar_area, main_area, dock_area] = Layout::horizontal([
        Constraint::Length(sidebar_w),
        Constraint::Min(0),
        Constraint::Length(dock_w),
    ])
    .areas(body_area);

    let (tab_bar_rect, terminal_area) = app
        .active
        .and_then(|i| app.workspaces.get(i))
        .map(|ws| desktop_tab_bar_and_terminal_area(app, ws, main_area))
        .unwrap_or((Rect::default(), main_area));

    if !app.sidebar_collapsed {
        app.workspace_scroll = normalized_workspace_scroll(app, sidebar_area, app.workspace_scroll);
        if let Some(active) = app.take_pending_workspace_reveal() {
            app.ensure_workspace_visible_in_sidebar(active, sidebar_area);
        }
    } else {
        let (ws_area, _, _) = collapsed_sidebar_sections(sidebar_area);
        app.workspace_scroll = collapsed_sidebar_row_scroll(app, ws_area);
        if let Some(active) = app.take_pending_workspace_reveal() {
            if let Some(target) = sidebar_row_index_for_workspace(app, active) {
                app.workspace_scroll =
                    collapsed_sidebar_scroll_for_target(app, ws_area, app.workspace_scroll, target);
            }
        }
    }

    let workspace_card_areas = if app.sidebar_collapsed {
        Vec::new()
    } else {
        compute_workspace_card_areas(app, sidebar_area)
    };

    let visible_user_actions = tabs::visible_user_actions(app);
    let tab_bar_view = app
        .active
        .and_then(|ws_idx| app.workspaces.get(ws_idx))
        .map(|ws| {
            compute_tab_bar_view_with_icons(
                ws,
                &app.terminals,
                tab_bar_content_area(app, tab_bar_rect),
                app.tab_scroll,
                app.tab_scroll_follow_active,
                app.mouse_capture,
                &visible_user_actions,
                tabs::TabActionVisibility::from_state(app),
                app.nerd_font,
            )
        })
        .unwrap_or_default();
    app.tab_scroll = tab_bar_view.scroll;
    // A hidden tab row leaves the toggles homeless; the status row takes them.
    let (
        repo_editor_button_hit_area,
        add_action_button_hit_area,
        user_action_hit_areas,
        git_menu_button_hit_area,
        pane_toggle_below_hit_area,
        pane_toggle_right_hit_area,
        // `+ Action` is always present when the row is drawn, so it is the sentinel
        // for "the tab row hosts the buttons"; the other controls are optional.
    ) = if tab_bar_view.add_action_button_hit_area.width > 0 {
        (
            tab_bar_view.repo_editor_button_hit_area,
            tab_bar_view.add_action_button_hit_area,
            tab_bar_view.user_action_hit_areas.clone(),
            tab_bar_view.git_menu_button_hit_area,
            tab_bar_view.pane_toggle_below_hit_area,
            tab_bar_view.pane_toggle_right_hit_area,
        )
    } else if app.active.is_some() {
        tabs::tab_action_fallback_hit_areas(
            status_bar_rect,
            app.mouse_capture,
            &visible_user_actions,
            tabs::TabActionVisibility::from_state(app),
        )
    } else {
        (
            Rect::default(),
            Rect::default(),
            Vec::new(),
            Rect::default(),
            Rect::default(),
            Rect::default(),
        )
    };

    let git_menu_layout = (app.server_mode() == Mode::GitMenu).then(|| {
        let in_git_repo = dock::chooser::focused_in_git_repo(app);
        tabs::git_menu_dropdown_layout(
            git_menu_button_hit_area,
            area,
            app.git_menu.highlighted,
            app.status_git_ahead_behind,
            in_git_repo,
        )
    });
    let git_menu_layout = git_menu_layout.flatten();
    let git_menu_popup_rect = git_menu_layout
        .as_ref()
        .map(|layout| layout.rect)
        .unwrap_or_default();
    let git_menu_first_visible = git_menu_layout
        .as_ref()
        .map(|layout| layout.first_visible)
        .unwrap_or_default();
    let git_menu_row_hit_areas = git_menu_layout
        .map(|layout| {
            (0..layout.visible_rows)
                .map(|offset| {
                    Rect::new(
                        layout.list_rect.x,
                        layout.list_rect.y + offset as u16,
                        layout.list_rect.width,
                        1,
                    )
                })
                .collect()
        })
        .unwrap_or_default();

    let TabSurfaceLayout {
        pane_infos,
        split_borders,
    } = compute_tab_surface(
        app,
        terminal_runtimes,
        terminal_area,
        resize_panes,
        cell_size,
    );
    if resize_panes {
        resize_background_tab_panes_for_desktop(app, terminal_runtimes, main_area, cell_size);
        resize_popup_pane(app, terminal_runtimes, terminal_area, cell_size);
    }

    let toast_hit_area = app
        .toast
        .as_ref()
        .map(|toast| {
            toast_notification_rect(
                area,
                toast,
                app.config_diagnostic.is_some(),
                toast.position.unwrap_or(app.toast_config.herdr.position),
            )
        })
        .unwrap_or_default();

    let agent_card_areas = if app.sidebar_collapsed {
        Vec::new()
    } else {
        sidebar::compute_agent_card_areas(app, sidebar_area)
    };
    let tab_card_areas = if app.sidebar_collapsed {
        Vec::new()
    } else {
        sidebar::compute_tab_card_areas(app, sidebar_area)
    };
    let sidebar_hover_targets = if app.sidebar_collapsed {
        Vec::new()
    } else {
        sidebar::compute_sidebar_hover_targets(app, sidebar_area)
    };
    let sidebar_footer_work_hit_area = if app.sidebar_collapsed {
        Rect::default()
    } else {
        sidebar::sidebar_footer_work_hit_area(sidebar_area)
    };
    let sidebar_footer_settings_hit_area = if app.sidebar_collapsed {
        Rect::default()
    } else {
        sidebar::sidebar_footer_settings_hit_area(sidebar_area)
    };
    let sidebar_footer_usage_hit_area = if app.sidebar_collapsed {
        Rect::default()
    } else {
        sidebar::sidebar_footer_usage_hit_area(sidebar_area)
    };
    let usage_hit_areas = if app.usage_view.is_some() {
        usage::hit_areas(terminal_area)
    } else {
        Vec::new()
    };
    let sidebar_footer_ticket_hit_area = if app.sidebar_collapsed {
        Rect::default()
    } else {
        sidebar::sidebar_footer_ticket_hit_area(sidebar_area)
    };
    let sidebar_footer_missive_hit_area = if app.sidebar_collapsed {
        Rect::default()
    } else {
        sidebar::sidebar_footer_missive_hit_area(sidebar_area)
    };
    let sidebar_footer_refresh_hit_area = if app.sidebar_collapsed {
        Rect::default()
    } else {
        sidebar::sidebar_footer_refresh_hit_area(sidebar_area)
    };
    let sidebar_footer_board_hit_area = if app.sidebar_collapsed {
        Rect::default()
    } else {
        sidebar::sidebar_footer_board_hit_area(sidebar_area)
    };
    let notepad_rect = sidebar::sidebar_notepad_rect(app, sidebar_area);
    let notepad_tab_hit_areas = notepad::notepad_tab_hit_areas(app, notepad_rect);
    // The agent tab's rows live on the view so a click resolves to the exact
    // row the operator saw. Deriving them takes a snapshot of the focused
    // pane's agent state, so it only happens while the tab is showing.
    let (notepad_agent_rows, notepad_agent_max_scroll) = if app.notepad.agent_tab
        && notepad_rect.height > 1
    {
        let body = notepad::notepad_body_rect(notepad_rect);
        let visible = usize::from(body.height).max(1);
        let (rows, max_scroll) =
            notepad_agent::agent_rows_window(app, body.width, app.notepad.agent_scroll, visible);
        app.notepad.agent_scroll = app.notepad.agent_scroll.min(max_scroll);
        (rows, max_scroll)
    } else {
        app.notepad.agent_scroll = 0;
        (Vec::new(), 0)
    };
    let (notepad_usage_rows, notepad_usage_max_scroll) = if app.notepad.usage_tab
        && notepad_rect.height > 1
    {
        let body = notepad::notepad_body_rect(notepad_rect);
        let visible = usize::from(body.height).max(1);
        let (rows, max_scroll) =
            notepad_usage::usage_rows_window(app, body.width, app.notepad.usage_scroll, visible);
        app.notepad.usage_scroll = app.notepad.usage_scroll.min(max_scroll);
        (rows, max_scroll)
    } else {
        app.notepad.usage_scroll = 0;
        (Vec::new(), 0)
    };
    let pomodoro_hit_area = pomodoro::pomodoro_hit_area(app, sidebar_area);
    let notification_hit_area = pomodoro::notification_hit_area(app, sidebar_area);
    let window_cycle_mode_hit_area = pomodoro::window_cycle_mode_hit_area(app, sidebar_area);
    let sidebar_areas_hit_area = pomodoro::sidebar_areas_hit_area(app, sidebar_area);
    let hyperspace_rect = sidebar::sidebar_animation_rect(app, sidebar_area);
    let hyperspace_pause_hit_area = hyperspace::pause_hit_area(app, hyperspace_rect);
    // The caret has to stay inside the rows the panel actually got, which is
    // only known once the sidebar geometry above resolved.
    app.notepad
        .sync_scroll(notepad::notepad_body_rect(notepad_rect).height);
    let visible_agent_activity_instants =
        sidebar::visible_tab_activity_instants_from(app, terminal_runtimes, &tab_card_areas);
    let visible_notepad_agent_ages = if app.notepad.agent_tab {
        let body = notepad::notepad_body_rect(notepad_rect);
        notepad_agent_rows
            .iter()
            .take(usize::from(body.height))
            .filter_map(|row| row.observed_at)
            .filter_map(|observed_at| {
                let observed_unix_s = observed_at
                    .duration_since(std::time::SystemTime::UNIX_EPOCH)
                    .ok()?
                    .as_secs();
                Some(std::time::Duration::from_secs(
                    app.view_observed_unix_s.saturating_sub(observed_unix_s),
                ))
            })
            .collect()
    } else {
        Vec::new()
    };
    let DockGeometry {
        handle: dock_handle_rect,
        divider: dock_divider_rect,
        tab_bar: dock_tab_bar_rect,
        tabs: dock_tab_hit_areas,
        close: dock_tab_close_rect,
        plus: dock_plus_rect,
        maximize: dock_maximize_rect,
        auto_open: dock_auto_open_rect,
        body: dock_body_rect,
    } = dock_geometry(
        dock_area,
        app.dock_collapsed,
        &(0..app.dock_open_surfaces.len())
            .map(|index| app.dock_tab_label(index))
            .collect::<Vec<_>>(),
        app.active_dock_tab_index(),
    );
    let dock_surface_card_hit_areas = if app.dock_collapsed || app.dock_tab.is_some() {
        Vec::new()
    } else {
        dock::chooser_card_hit_areas_for_count(
            dock_body_rect,
            area.width,
            dock::chooser::entries(app, true).len(),
        )
    };
    let (
        dock_home_section_hit_areas,
        dock_home_tab_hit_areas,
        dock_home_tab_keys,
        dock_home_detail_tab_hit_areas,
    ) = if !app.dock_collapsed && app.dock_tab == Some(crate::app::DockSurface::Home) {
        let projection = app.dock_home_projection();
        let layouts = match app.dock_home_section {
            crate::app::state::DockHomeSection::Prs => {
                dock::home_tab_layouts(app, &projection, dock_body_rect)
            }
            crate::app::state::DockHomeSection::Tickets => {
                dock::home_ticket_tab_layouts(app, &projection, dock_body_rect)
            }
            crate::app::state::DockHomeSection::XPolls => {
                dock::home_poll_tab_layouts(app, &projection, dock_body_rect)
            }
        };
        let hit_areas = layouts.tabs.iter().map(|(_, area)| *area).collect();
        let keys = layouts
            .tabs
            .iter()
            .filter_map(|(index, _)| match app.dock_home_section {
                crate::app::state::DockHomeSection::Prs => {
                    projection.rows.get(*index).map(|row| row.key.clone())
                }
                crate::app::state::DockHomeSection::Tickets => projection
                    .ticket_rows
                    .get(*index)
                    .map(|row| row.key.clone()),
                crate::app::state::DockHomeSection::XPolls => {
                    projection.poll_rows.get(*index).map(|row| row.key.clone())
                }
            })
            .collect();
        (
            dock::home_section_layouts(dock_body_rect).to_vec(),
            hit_areas,
            keys,
            if app.dock_home_section == crate::app::state::DockHomeSection::Prs {
                dock::home_detail_tab_layouts(dock_body_rect).to_vec()
            } else {
                Vec::new()
            },
        )
    } else {
        (Vec::new(), Vec::new(), Vec::new(), Vec::new())
    };

    let home_row_hit_areas = if app.home.is_some() {
        let queue = app.home_attention_agents();
        home::row_hit_areas(app, &queue, terminal_area)
    } else {
        Vec::new()
    };
    let home_hit_areas = if app.home.is_some() {
        let queue = app.home_attention_agents();
        home::home_hit_areas(app, &queue, terminal_area)
    } else {
        Vec::new()
    };
    let dock_file_row_hit_areas =
        if !app.dock_collapsed && app.dock_tab == Some(crate::app::DockSurface::Files) {
            dock::files::row_hit_areas(app, dock_body_rect)
        } else {
            Vec::new()
        };
    let (dock_files_refresh_rect, dock_files_sort_rect) =
        if !app.dock_collapsed && app.dock_tab == Some(crate::app::DockSurface::Files) {
            dock::files::header_hit_areas(app, dock_body_rect)
        } else {
            (Rect::default(), Rect::default())
        };
    let editor_preview_area =
        if !app.dock_collapsed && app.dock_tab == Some(crate::app::DockSurface::Editor) {
            dock_body_rect
        } else {
            terminal_area
        };
    let (editor_preview_refresh_rect, editor_preview_open_rect) =
        dock::editor::preview_action_hit_areas(app, editor_preview_area);
    let dock_agent_row_hit_areas =
        if !app.dock_collapsed && app.dock_tab == Some(crate::app::DockSurface::Agents) {
            dock::agents::row_hit_areas(app, dock_body_rect)
        } else {
            Vec::new()
        };
    let dock_host_row_hit_areas =
        if !app.dock_collapsed && app.dock_tab == Some(crate::app::DockSurface::Hosts) {
            dock::hosts::row_hit_areas(app, dock_body_rect)
        } else {
            Vec::new()
        };

    let add_action_layout = if app.server_mode() == Mode::AddAction {
        user_actions::add_action_layout(area)
    } else {
        user_actions::AddActionLayout::default()
    };
    let add_project_layout = add_project::add_project_layout(
        area,
        app.home.as_ref().and_then(|home| home.add_project.as_ref()),
    );
    app.view = crate::app::ViewState {
        layout: ViewLayout::Desktop,
        status_bar_rect,
        sidebar_rect: sidebar_area,
        sidebar_footer_settings_hit_area,
        sidebar_footer_work_hit_area,
        sidebar_footer_usage_hit_area,
        usage_hit_areas,
        sidebar_footer_ticket_hit_area,
        sidebar_footer_missive_hit_area,
        notepad_rect,
        notepad_tab_hit_areas,
        notepad_agent_rows,
        notepad_agent_max_scroll,
        notepad_usage_rows,
        notepad_usage_max_scroll,
        pomodoro_hit_area,
        notification_hit_area,
        window_cycle_mode_hit_area,
        sidebar_areas_hit_area,
        hyperspace_rect,
        hyperspace_pause_hit_area,
        sidebar_footer_refresh_hit_area,
        sidebar_footer_board_hit_area,
        workspace_card_areas,
        agent_card_areas,
        sidebar_hover_targets,
        visible_agent_activity_instants,
        visible_notepad_agent_ages,
        tab_bar_rect,
        tab_hit_areas: tab_bar_view.tab_hit_areas,
        tab_scroll_left_hit_area: tab_bar_view.scroll_left_hit_area,
        tab_scroll_right_hit_area: tab_bar_view.scroll_right_hit_area,
        new_tab_hit_area: tab_bar_view.new_tab_hit_area,
        repo_editor_button_hit_area,
        add_action_button_hit_area,
        user_action_hit_areas,
        add_action_close_hit_area: add_action_layout.close,
        add_action_field_hit_areas: add_action_layout.fields,
        add_action_cancel_hit_area: add_action_layout.cancel,
        add_action_save_hit_area: add_action_layout.save,
        add_project_layout,
        git_menu_button_hit_area,
        git_menu_popup_rect,
        git_menu_first_visible,
        git_menu_row_hit_areas,
        pane_toggle_below_hit_area,
        pane_toggle_right_hit_area,
        terminal_area,
        work_context_link_rows: {
            let mut rows =
                if !app.dock_collapsed && app.dock_tab == Some(crate::app::DockSurface::Context) {
                    dock_context::context_link_rows(app, dock_body_rect)
                } else {
                    Vec::new()
                };
            // The notepad strip's Context tab renders the same surface in the
            // sidebar, so its link rows register their own geometry.
            if app.notepad.context_active {
                rows.extend(dock_context::context_link_rows(
                    app,
                    notepad::notepad_body_rect(notepad_rect),
                ));
            }
            rows
        },
        status_buttons: Vec::new(),
        status_work_links: Vec::new(),
        status_segments: Vec::new(),
        status_segment_hit_areas: Vec::new(),
        focused_remote_host: None,
        scratchpad_link_rows: if !app.dock_collapsed
            && app.dock_tab == Some(crate::app::DockSurface::Scratchpad)
        {
            dock_scratchpad::scratchpad_link_rows(app, dock_body_rect)
        } else {
            Vec::new()
        },
        mobile_header_rect: Rect::default(),
        mobile_menu_hit_area: Rect::default(),
        config_diagnostic_hit_area: config_diagnostic_marker_rect(area),
        toast_hit_area,
        home_row_hit_areas,
        home_hit_areas,
        pane_infos,
        split_borders,
        dock_rect: dock_area,
        dock_handle_rect,
        dock_divider_rect,
        dock_tab_bar_rect,
        dock_tab_hit_areas,
        dock_tab_close_rect,
        dock_plus_rect,
        dock_maximize_rect,
        dock_auto_open_rect,
        dock_surface_card_hit_areas,
        dock_surface_menu_layout: None,
        dock_home_section_hit_areas,
        dock_home_tab_hit_areas,
        dock_home_tab_keys,
        dock_home_detail_tab_hit_areas,
        dock_file_row_hit_areas,
        dock_files_refresh_rect,
        dock_files_sort_rect,
        editor_preview_refresh_rect,
        editor_preview_open_rect,
        dock_agent_row_hit_areas,
        dock_host_row_hit_areas,
        dock_body_rect,
    };
    if sidebar::sidebar_areas_menu_layout(app, area).is_none() {
        app.sidebar_areas_menu_selected = None;
    }
    if sidebar::window_cycle_mode_menu_layout(app, area).is_none() {
        app.window_cycle_menu_open = false;
    }
    // The menu anchors on the `+`, so its geometry needs the strip already
    // stored on the view.
    app.view.dock_surface_menu_layout = dock::chooser_menu_layout(app, dock_area);
    // The links follow the title, and the title starts at the sidebar's right
    // edge, so they are laid out only once this frame's sidebar is on the view.
    // The row is fitted once here: the status segments, the title, and the
    // links share one layout pass, and render draws what this stored.
    app.view.focused_remote_host = status::resolve_focused_remote_host(app, terminal_runtimes);
    if status_bar_is_renderable(app, area) {
        app.view.status_buttons = status::status_buttons(app, status_bar_rect);
        app.view.status_segments = status::fitted_status_segments(app, status_bar_rect);
        app.view.status_segment_hit_areas = status::status_segment_hit_areas(app, status_bar_rect);
        app.view.status_work_links = status::status_work_links(app, status_bar_rect);
    } else {
        app.view.status_buttons = Vec::new();
        app.view.status_segments = Vec::new();
        app.view.status_segment_hit_areas = Vec::new();
        app.view.status_work_links = Vec::new();
    }
    app.sync_copy_mode_search_geometry();
}

fn uses_mobile_layout(app: &AppState, area: Rect) -> bool {
    is_mobile_width(area, app.mobile_width_threshold)
        || (app.status_bar_enabled
            && usize::from(area.width) < status::minimum_required_status_width(app))
}

/// Tab-strip and body geometry of the dock. Collapsed, the dock is nothing but
/// its handle, so everything else comes back empty.
pub(crate) struct DockGeometry {
    pub handle: Rect,
    pub divider: Rect,
    pub tab_bar: Rect,
    pub tabs: Vec<Rect>,
    pub close: Rect,
    pub plus: Rect,
    pub maximize: Rect,
    pub auto_open: Rect,
    pub body: Rect,
}

impl DockGeometry {
    fn empty(handle: Rect) -> Self {
        Self {
            handle,
            divider: Rect::default(),
            tab_bar: Rect::default(),
            tabs: Vec::new(),
            close: Rect::default(),
            plus: Rect::default(),
            maximize: Rect::default(),
            auto_open: Rect::default(),
            body: Rect::default(),
        }
    }
}

fn dock_geometry(
    area: Rect,
    collapsed: bool,
    labels: &[String],
    active_index: Option<usize>,
) -> DockGeometry {
    if area.width == 0 || area.height == 0 {
        return DockGeometry::empty(Rect::default());
    }

    let handle = Rect::new(area.x + area.width - 1, area.y, 1, area.height);
    if collapsed || area.width < 3 || area.height < 2 {
        return DockGeometry::empty(handle);
    }

    let divider = Rect::new(area.x, area.y, 1, area.height);
    let content = Rect::new(area.x + 1, area.y, area.width - 2, area.height);
    let [tab_bar, body] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(content);

    // Width the strip wants: every open tab, the trailing `+`, and the two
    // right-edge toggles. Each toggle only claims its columns when the tabs do
    // not need them, and the maximise glyph keeps the outer cell so its
    // position does not move when the auto-open toggle drops out.
    let wanted: u16 = labels
        .iter()
        .enumerate()
        .map(|(index, label)| dock::tab_width_label(label, active_index == Some(index)))
        .fold(dock::PLUS_WIDTH, u16::saturating_add);
    let (strip, maximize, auto_open) = if tab_bar.width >= wanted.saturating_add(4) {
        (
            Rect::new(tab_bar.x, tab_bar.y, tab_bar.width - 4, 1),
            Rect::new(tab_bar.right() - 1, tab_bar.y, 1, 1),
            Rect::new(tab_bar.right() - 3, tab_bar.y, 1, 1),
        )
    } else if tab_bar.width >= wanted.saturating_add(2) {
        (
            Rect::new(tab_bar.x, tab_bar.y, tab_bar.width - 2, 1),
            Rect::new(tab_bar.right() - 1, tab_bar.y, 1, 1),
            Rect::default(),
        )
    } else {
        (tab_bar, Rect::default(), Rect::default())
    };

    // The hit areas come from the same layout the strip is drawn from, so the
    // `+` cell a click lands in is the cell the glyph occupies.
    let dock::StripLayout { tabs, close, plus } =
        dock::strip_layout_labels(strip, labels, active_index);

    DockGeometry {
        handle,
        divider,
        tab_bar,
        tabs,
        close,
        plus,
        maximize,
        auto_open,
        body,
    }
}

pub(crate) fn horizontal_tab_hit_areas(tab_bar: Rect, widths: &[u16]) -> Vec<Rect> {
    let count = widths.len();
    if count == 0 || tab_bar.width == 0 || tab_bar.height == 0 {
        return Vec::new();
    }
    let wanted: u16 = widths.iter().copied().fold(0u16, u16::saturating_add);
    if wanted <= tab_bar.width {
        let mut constraints: Vec<Constraint> =
            widths.iter().copied().map(Constraint::Length).collect();
        // Without a trailing filler the layout stretches the final tab over the
        // slack, giving it a hit area far wider than its label.
        constraints.push(Constraint::Min(0));
        let areas = Layout::horizontal(constraints).split(tab_bar);
        return areas[..count].to_vec();
    }
    let ratio = u32::try_from(count).unwrap_or(1).max(1);
    Layout::horizontal(
        (0..count)
            .map(|_| Constraint::Ratio(1, ratio))
            .collect::<Vec<_>>(),
    )
    .split(tab_bar)
    .to_vec()
}

pub(crate) fn status_bar_is_renderable(app: &AppState, area: Rect) -> bool {
    app.status_bar_enabled && area.height > 1 && !uses_mobile_layout(app, area)
}

fn compute_mobile_view(
    app: &mut AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    area: Rect,
    resize_panes: bool,
    cell_size: crate::kitty_graphics::HostCellSize,
) {
    let header_h = area.height.min(2);
    let (header_rect, terminal_area) = if area.height > header_h {
        let [header_rect, terminal_area] =
            Layout::vertical([Constraint::Length(header_h), Constraint::Min(1)]).areas(area);
        (header_rect, terminal_area)
    } else {
        (area, Rect::default())
    };

    if app.server_mode() == Mode::Navigate {
        let switcher_viewport_h = area.height.saturating_sub(header_h + 1);
        let max_scroll = mobile_switcher_max_scroll_for_height(app, switcher_viewport_h);
        app.mobile_switcher_scroll = app.mobile_switcher_scroll.min(max_scroll);
    }

    let TabSurfaceLayout {
        pane_infos,
        split_borders,
    } = compute_tab_surface(
        app,
        terminal_runtimes,
        terminal_area,
        resize_panes,
        cell_size,
    );
    if resize_panes {
        resize_background_tab_panes_to_area(app, terminal_runtimes, terminal_area, cell_size);
        resize_popup_pane(app, terminal_runtimes, terminal_area, cell_size);
    }
    let header_hits = compute_mobile_header_hit_areas(app, header_rect);

    let toast_hit_area = app
        .toast
        .as_ref()
        .map(|_| mobile_toast_banner_rect(area, app.config_diagnostic.is_some()))
        .unwrap_or_default();

    let home_row_hit_areas = if app.home.is_some() {
        let queue = app.home_attention_agents();
        home::row_hit_areas(app, &queue, terminal_area)
    } else {
        Vec::new()
    };
    let home_hit_areas = if app.home.is_some() {
        let queue = app.home_attention_agents();
        home::home_hit_areas(app, &queue, terminal_area)
    } else {
        Vec::new()
    };
    let (editor_preview_refresh_rect, editor_preview_open_rect) =
        dock::editor::preview_action_hit_areas(app, terminal_area);

    app.view = crate::app::ViewState {
        layout: ViewLayout::Mobile,
        status_bar_rect: Rect::default(),
        sidebar_rect: Rect::default(),
        sidebar_footer_settings_hit_area: Rect::default(),
        sidebar_footer_work_hit_area: Rect::default(),
        sidebar_footer_usage_hit_area: Rect::default(),
        usage_hit_areas: Vec::new(),
        sidebar_footer_ticket_hit_area: Rect::default(),
        sidebar_footer_missive_hit_area: Rect::default(),
        notepad_rect: Rect::default(),
        notepad_tab_hit_areas: Vec::new(),
        notepad_agent_rows: Vec::new(),
        notepad_agent_max_scroll: 0,
        notepad_usage_rows: Vec::new(),
        notepad_usage_max_scroll: 0,
        pomodoro_hit_area: Rect::default(),
        notification_hit_area: Rect::default(),
        window_cycle_mode_hit_area: Rect::default(),
        sidebar_areas_hit_area: Rect::default(),
        hyperspace_rect: Rect::default(),
        hyperspace_pause_hit_area: Rect::default(),
        sidebar_footer_refresh_hit_area: Rect::default(),
        sidebar_footer_board_hit_area: Rect::default(),
        workspace_card_areas: Vec::new(),
        agent_card_areas: Vec::new(),
        sidebar_hover_targets: Vec::new(),
        visible_agent_activity_instants: Vec::new(),
        visible_notepad_agent_ages: Vec::new(),
        tab_bar_rect: Rect::default(),
        tab_hit_areas: Vec::new(),
        tab_scroll_left_hit_area: Rect::default(),
        tab_scroll_right_hit_area: Rect::default(),
        new_tab_hit_area: Rect::default(),
        repo_editor_button_hit_area: Rect::default(),
        add_action_button_hit_area: Rect::default(),
        user_action_hit_areas: Vec::new(),
        add_action_close_hit_area: Rect::default(),
        add_action_field_hit_areas: Vec::new(),
        add_action_cancel_hit_area: Rect::default(),
        add_action_save_hit_area: Rect::default(),
        add_project_layout: add_project::add_project_layout(
            area,
            app.home.as_ref().and_then(|home| home.add_project.as_ref()),
        ),
        git_menu_button_hit_area: Rect::default(),
        git_menu_popup_rect: Rect::default(),
        git_menu_first_visible: 0,
        git_menu_row_hit_areas: Vec::new(),
        pane_toggle_below_hit_area: Rect::default(),
        pane_toggle_right_hit_area: Rect::default(),
        terminal_area,
        work_context_link_rows: Vec::new(),
        status_buttons: Vec::new(),
        status_work_links: Vec::new(),
        status_segments: Vec::new(),
        status_segment_hit_areas: Vec::new(),
        focused_remote_host: None,
        scratchpad_link_rows: Vec::new(),
        mobile_header_rect: header_rect,
        mobile_menu_hit_area: header_hits.menu,
        config_diagnostic_hit_area: config_diagnostic_marker_rect(terminal_area),
        toast_hit_area,
        home_row_hit_areas,
        home_hit_areas,
        pane_infos,
        split_borders,
        dock_rect: Rect::default(),
        dock_handle_rect: Rect::default(),
        dock_divider_rect: Rect::default(),
        dock_tab_bar_rect: Rect::default(),
        dock_tab_hit_areas: Vec::new(),
        dock_tab_close_rect: Rect::default(),
        dock_plus_rect: Rect::default(),
        dock_maximize_rect: Rect::default(),
        dock_auto_open_rect: Rect::default(),
        dock_surface_card_hit_areas: Vec::new(),
        dock_surface_menu_layout: None,
        dock_home_section_hit_areas: Vec::new(),
        dock_home_tab_hit_areas: Vec::new(),
        dock_home_tab_keys: Vec::new(),
        dock_home_detail_tab_hit_areas: Vec::new(),
        dock_file_row_hit_areas: Vec::new(),
        dock_files_refresh_rect: Rect::default(),
        dock_files_sort_rect: Rect::default(),
        editor_preview_refresh_rect,
        editor_preview_open_rect,
        dock_agent_row_hit_areas: Vec::new(),
        dock_host_row_hit_areas: Vec::new(),
        dock_body_rect: Rect::default(),
    };
    if app.server_mode() == Mode::Navigate {
        if let Some(active) = app.take_pending_workspace_reveal() {
            app.ensure_mobile_workspace_visible(active);
        }
        let viewport = mobile_switcher_areas(app).viewport;
        app.view.visible_agent_activity_instants =
            mobile::visible_tab_activity_instants_from(app, terminal_runtimes, viewport);
    }
    app.sync_copy_mode_search_geometry();
}

/// Render the UI — reads AppState but does not mutate it.
#[cfg_attr(not(test), allow(dead_code))]
pub fn render(app: &AppState, frame: &mut Frame) {
    let terminal_runtimes = TerminalRuntimeRegistry::new();
    render_with_runtime_registry(app, &terminal_runtimes, frame);
}

pub fn render_with_runtime_registry(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
) {
    render_with_runtime_registry_for_owner(app, terminal_runtimes, frame, app.input_owner());
}

pub(crate) fn render_with_runtime_registry_for_owner(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
    input_owner: InputOwner,
) {
    render_with_runtime_registry_inner(app, terminal_runtimes, frame, input_owner);
}

fn render_with_runtime_registry_inner(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
    input_owner: InputOwner,
) {
    let tab_bar_area = app.view.tab_bar_rect;
    let terminal_area = app.view.terminal_area;

    if app.view.layout != ViewLayout::Mobile {
        render_status_bar(app, frame, app.view.status_bar_rect);
    }

    render_navigation_chrome(app, terminal_runtimes, frame);
    if app.view.layout != ViewLayout::Mobile {
        render_tab_bar(app, frame, tab_bar_area);
    }
    if app.view.layout != ViewLayout::Mobile {
        render_tab_action_buttons(app, frame);
    }
    match app.terminal_area_surface() {
        crate::app::state::TerminalAreaSurface::Board => {
            board::render(app, terminal_area, frame);
        }
        crate::app::state::TerminalAreaSurface::EditorPreview => {
            dock::editor::render_editor_preview(app, frame, terminal_area);
        }
        crate::app::state::TerminalAreaSurface::Symphony(detail) => render_symphony(
            &app.palette,
            &detail.snapshot,
            detail.selected,
            terminal_area,
            detail.observed_at,
            frame,
        ),
        crate::app::state::TerminalAreaSurface::LoopRunHistory(detail) => {
            render_loop_run_history(
                &app.palette,
                &detail.history,
                &detail.loop_id,
                terminal_area,
                detail.observed_at,
                frame,
            );
        }
        crate::app::state::TerminalAreaSurface::AloopRunLog(detail) => {
            aloop_run::render_aloop_run_log(
                &app.palette,
                &detail.loop_name,
                &detail.host,
                &detail.run,
                terminal_area,
                frame,
            );
        }
        crate::app::state::TerminalAreaSurface::Usage => render_usage(app, terminal_area, frame),
        crate::app::state::TerminalAreaSurface::Work => {
            render_work_view(app, terminal_area, frame);
        }
        crate::app::state::TerminalAreaSurface::DockObjectPreview => {
            dock::render_object_preview(app, frame, terminal_area);
        }
        crate::app::state::TerminalAreaSurface::Home => {
            let queue = app.home_attention_agents();
            let counts = app.home_counts(&queue);
            home::render_home(app, terminal_runtimes, &queue, counts, terminal_area, frame);
        }
        crate::app::state::TerminalAreaSurface::Inbox(inbox) => {
            let queue = app.blocked_agents();
            inbox::render_inbox(
                app,
                terminal_runtimes,
                inbox.current(&queue),
                queue.len(),
                inbox.deferred_count(&queue),
                terminal_area,
                frame,
            );
        }
        crate::app::state::TerminalAreaSurface::Tab => {
            render_tab_surface(app, terminal_runtimes, app.view.tab_surface(), frame);
        }
        crate::app::state::TerminalAreaSurface::Empty => render_empty(app, frame, terminal_area),
    }

    if app.view.layout != ViewLayout::Mobile {
        render_dock(app, terminal_runtimes, frame);
    }

    // Ambient notifications sit above panes, but below interactive overlays.
    render_notifications(app, frame, terminal_area);
    render_popup_pane(app, terminal_runtimes, frame, terminal_area);

    let mode_bar_area = if app.view.layout == ViewLayout::Desktop
        && app.tab_bar_position == crate::config::TabBarPositionConfig::Bottom
        && tab_bar_area.height > 0
    {
        tab_bar_area
    } else {
        terminal_area
    };

    match input_owner {
        InputOwner::Client(ClientInputOwner::Overlay(overlay)) => match overlay {
            ClientOverlay::RenameWorkspace
            | ClientOverlay::RenameTab
            | ClientOverlay::RenamePane => render_rename_overlay(app, frame, frame.area()),
            ClientOverlay::NewLinkedWorktree => {
                render_new_linked_worktree_overlay(app, frame, frame.area())
            }
            ClientOverlay::OpenExistingWorktree => {
                render_open_existing_worktree_overlay(app, frame, frame.area())
            }
            ClientOverlay::ConfirmRemoveWorktree => {
                render_remove_worktree_overlay(app, frame, frame.area())
            }
            ClientOverlay::ConfirmClose => {
                render_confirm_close_overlay(app, terminal_runtimes, frame, terminal_area)
            }
            ClientOverlay::ContextMenu => render_context_menu(app, frame),
            ClientOverlay::None => {}
        },
        InputOwner::Client(ClientInputOwner::SnoozeMenu) => render_sidebar_snooze_menu(app, frame),
        InputOwner::Client(ClientInputOwner::SnoozeTime) => {
            render_rename_overlay(app, frame, frame.area())
        }
        InputOwner::Client(
            ClientInputOwner::SettledMenu | ClientInputOwner::SettledDeleteConfirm,
        ) => render_sidebar_settled_menu(app, frame),
        InputOwner::Client(ClientInputOwner::AgentPicker) => {
            render_agent_picker(app, frame, frame.area())
        }
        InputOwner::Client(ClientInputOwner::SidebarGroupMenu) => {
            render_sidebar_group_menu(app, frame)
        }
        InputOwner::Client(ClientInputOwner::SidebarFilterMenu) => {
            render_sidebar_filter_menu(app, frame)
        }
        InputOwner::Client(ClientInputOwner::SidebarNewMenu) => render_sidebar_new_menu(app, frame),
        InputOwner::Client(ClientInputOwner::SidebarNewThread) => {
            render_sidebar_new_thread(app, frame)
        }
        InputOwner::Client(ClientInputOwner::SidebarProjectMenu) => {
            render_sidebar_project_menu(app, frame)
        }
        InputOwner::Client(ClientInputOwner::SidebarObjectMenu) => {
            render_sidebar_object_menu(app, frame)
        }
        InputOwner::Client(ClientInputOwner::SidebarSortMenu) => {
            render_sidebar_sort_menu(app, frame)
        }
        InputOwner::Client(ClientInputOwner::SidebarSubgroupPicker) => {
            render_sidebar_subgroup_picker(app, frame)
        }
        InputOwner::Client(ClientInputOwner::SidebarPodPicker) => {
            render_sidebar_pod_picker(app, frame)
        }
        InputOwner::Client(ClientInputOwner::PrActionConfirmation) => {
            pr_actions::render_confirmation(app, frame, frame.area())
        }
        // The dock owns this menu and renders it as part of `render_dock`.
        InputOwner::Client(ClientInputOwner::DockSurfaceMenu) => {}
        InputOwner::AddProject => render_add_project_overlay(app, frame),
        InputOwner::Server(owner) => match owner {
            ServerInputOwner::Onboarding => render_onboarding_overlay(app, frame, frame.area()),
            ServerInputOwner::ReleaseNotes => {
                render_release_notes_overlay(app, frame, frame.area())
            }
            ServerInputOwner::ProductAnnouncement => {
                render_product_announcement_overlay(app, frame, frame.area())
            }
            ServerInputOwner::Navigate if app.view.layout == ViewLayout::Mobile => {
                render_mobile_panel(app, terminal_runtimes, frame, frame.area())
            }
            ServerInputOwner::Navigate => render_navigate_overlay(app, frame, mode_bar_area),
            ServerInputOwner::Prefix => render_prefix_overlay(app, frame, mode_bar_area),
            ServerInputOwner::Copy => render_copy_mode_overlay(app, frame, mode_bar_area),
            ServerInputOwner::Resize => render_resize_overlay(app, frame, mode_bar_area),
            ServerInputOwner::GitMenu => render_git_menu(app, frame),
            ServerInputOwner::AddAction => render_add_action_overlay(app, frame),
            ServerInputOwner::Settings => render_settings_overlay(app, frame, frame.area()),
            ServerInputOwner::GlobalMenu => render_global_launcher_menu(app, frame),
            ServerInputOwner::KeybindHelp => render_keybind_help_overlay(app, frame),
            ServerInputOwner::Navigator => render_navigator_overlay(app, terminal_runtimes, frame),
            ServerInputOwner::CommandPalette => render_command_palette(app, frame),
            ServerInputOwner::WorkLinkPicker => render_work_link_picker(app, frame, frame.area()),
        },
        InputOwner::Pomodoro
        | InputOwner::Popup
        | InputOwner::Surface(_)
        | InputOwner::Notepad
        | InputOwner::Dock(_)
        | InputOwner::Sidebar
        | InputOwner::Pane
        | InputOwner::None => {}
    }
    render_hover_tooltip(app, frame);
    if input_owner == InputOwner::Notepad {
        notepad::render_notepad_caret(app, frame);
    }
    // Last, and over everything: a due break reminder outranks whatever the
    // operator was looking at, which is the point of it.
    pomodoro::render_overlay(app, frame, frame.area());
}

fn render_navigation_chrome(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
) {
    if app.view.layout == ViewLayout::Mobile {
        render_mobile_header(app, terminal_runtimes, frame, app.view.mobile_header_rect);
    } else if app.view.sidebar_rect.width > 0 {
        if app.sidebar_collapsed {
            render_sidebar_collapsed(app, frame, app.view.sidebar_rect);
        } else {
            render_sidebar(app, terminal_runtimes, frame, app.view.sidebar_rect);
        }
    }
}

fn render_notifications(app: &AppState, frame: &mut Frame, terminal_area: Rect) {
    let has_config_diagnostic = app.config_diagnostic.is_some();
    if let Some(message) = &app.config_diagnostic {
        let diagnostic_area = if app.view.layout == ViewLayout::Mobile {
            terminal_area
        } else {
            frame.area()
        };
        render_config_diagnostic(
            frame,
            diagnostic_area,
            message,
            app.hovered_control == Some(crate::app::state::ControlId::ConfigDiagnostic),
            &app.palette,
        );
    }
    let mut copy_feedback_offset = u16::from(has_config_diagnostic);
    let mut toast_rect = None;
    if let Some(toast) = &app.toast {
        if app.view.layout == ViewLayout::Mobile {
            render_mobile_toast_banner(
                frame,
                frame.area(),
                toast,
                has_config_diagnostic,
                &app.palette,
            );
        } else {
            render_toast_notification(
                frame,
                frame.area(),
                toast,
                has_config_diagnostic,
                toast.position.unwrap_or(app.toast_config.herdr.position),
                &app.palette,
            );
            toast_rect = Some(toast_notification_rect(
                frame.area(),
                toast,
                has_config_diagnostic,
                toast.position.unwrap_or(app.toast_config.herdr.position),
            ));
        }
        if app.view.layout == ViewLayout::Mobile {
            toast_rect = Some(mobile_toast_banner_rect(
                frame.area(),
                has_config_diagnostic,
            ));
        }
    }
    if let Some(feedback) = &app.copy_feedback {
        let area = if app.view.layout == ViewLayout::Mobile {
            frame.area()
        } else {
            terminal_area
        };
        if let Some(toast_rect) = toast_rect {
            copy_feedback_offset = copy_feedback_offset_for_toast(
                area,
                feedback,
                copy_feedback_offset,
                app.toast_config.clipboard.position,
                toast_rect,
            );
        }
        render_copy_feedback(
            frame,
            area,
            feedback,
            copy_feedback_offset,
            app.toast_config.clipboard.position,
            &app.palette,
        );
    }
}

fn copy_feedback_offset_for_toast(
    area: Rect,
    feedback: &crate::app::state::CopyFeedback,
    base_offset: u16,
    position: crate::config::ToastClipboardPosition,
    toast_rect: Rect,
) -> u16 {
    let feedback_rect = copy_feedback_rect(area, feedback, base_offset, position);
    if rects_overlap(feedback_rect, toast_rect) {
        base_offset.saturating_add(toast_rect.height)
    } else {
        base_offset
    }
}

fn rects_overlap(a: Rect, b: Rect) -> bool {
    a.x < b.x.saturating_add(b.width)
        && b.x < a.x.saturating_add(a.width)
        && a.y < b.y.saturating_add(b.height)
        && b.y < a.y.saturating_add(a.height)
}

/// Opaque backdrop for a modal that has to be answered before work resumes.
/// `dim_background` only adds the DIM modifier, which several terminals render
/// as no change at all, so a blocking prompt gets a real veil instead: the
/// cells behind it are blanked so the work underneath cannot be read past the
/// dialog.
fn veil_background(frame: &mut Frame, area: Rect, palette: &Palette) {
    // The 16-colour theme leaves `surface0` as the terminal's own background,
    // which would make the veil invisible; `surface1` is a concrete colour in
    // every bundled theme.
    let bg = match palette.surface0 {
        Color::Reset => palette.surface1,
        color => color,
    };
    let style = Style::default().bg(bg).fg(palette.overlay0);
    let buf = frame.buffer_mut();
    for y in area.y..area.y.saturating_add(area.height) {
        for x in area.x..area.x.saturating_add(area.width) {
            let cell = &mut buf[(x, y)];
            cell.reset();
            cell.set_style(style);
        }
    }
}

fn dim_background(frame: &mut Frame, area: Rect) {
    let buf = frame.buffer_mut();
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            let cell = &mut buf[(x, y)];
            cell.set_style(cell.style().add_modifier(Modifier::DIM));
        }
    }
}

/// Floating overlay for navigate mode — appears at bottom of terminal area.
fn _build_hints(items: &[(&str, &str)], key_style: Style, dim_style: Style) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    spans.push(Span::raw(" "));
    for (i, (k, desc)) in items.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ", dim_style));
        }
        spans.push(Span::styled(k.to_string(), key_style));
        spans.push(Span::styled(format!(" {desc}"), dim_style));
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::keybind_help::keybind_help_groups;
    use super::scrollbar::scrollbar_thumb;
    use super::*;
    use crate::{app::state::ViewLayout, layout::PaneInfo, workspace::Workspace};
    use ratatui::style::Color;
    use ratatui::{backend::TestBackend, Terminal};
    use std::path::PathBuf;

    use super::widgets::contrast_ratio;

    fn themed_app(theme: &str) -> crate::app::state::AppState {
        let mut app = crate::app::state::AppState::test_new();
        let mut ws = Workspace::test_new("space");
        ws.test_add_tab(Some("home"));
        ws.test_add_tab(Some("dock"));
        app.workspaces = vec![ws];
        app.active = Some(0);
        app.palette =
            crate::app::state::Palette::from_name(theme).expect("built-in theme resolves");
        app.theme_name = theme.to_string();
        app
    }

    /// A cell that only carries the right half of a double-width glyph is
    /// never painted on its own, so it keeps ratatui's default style.
    fn is_wide_glyph_continuation(
        buffer: &ratatui::buffer::Buffer,
        rect: Rect,
        x: u16,
        y: u16,
    ) -> bool {
        x > rect.x && self::text::display_width(buffer[(x - 1, y)].symbol()) > 1
    }

    fn chrome_rects(app: &crate::app::state::AppState) -> [Rect; 2] {
        [app.view.sidebar_rect, app.view.tab_bar_rect]
    }

    /// The regression this pins: herdr painted its tab bar from the palette but
    /// left the sidebar — the widest chrome surface — on the terminal's own
    /// background. Pin a light theme in front of a dark terminal and the
    /// sidebar became #4b535d text on near-black: rendered, unreadable. Chrome
    /// must carry its own background so a mismatched theme can only look wrong.
    #[test]
    fn themed_chrome_never_borrows_the_terminal_background() {
        let mut violations: Vec<String> = Vec::new();
        for theme in crate::app::state::THEME_NAMES {
            // The `terminal` palette opts into the terminal's colors by design.
            if *theme == "terminal" {
                continue;
            }
            let mut app = themed_app(theme);
            let area = Rect::new(0, 0, 80, 24);
            compute_view(&mut app, area);
            let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
            terminal.draw(|frame| render(&app, frame)).unwrap();
            let buffer = terminal.backend().buffer();
            for rect in chrome_rects(&app) {
                for y in rect.y..rect.y + rect.height {
                    for x in rect.x..rect.x + rect.width {
                        if is_wide_glyph_continuation(buffer, rect, x, y) {
                            continue;
                        }
                        if buffer[(x, y)].style().bg == Some(Color::Reset) {
                            violations.push(format!(
                                "{theme}: chrome cell ({x},{y}) fell back to the terminal background"
                            ));
                        }
                    }
                }
            }
        }
        assert!(violations.is_empty(), "{}", violations.join("\n"));
    }

    #[test]
    fn selected_sidebar_view_row_paints_the_full_popup_width() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("space")];
        app.active = Some(0);
        app.selected = 0;
        app.sidebar_width = 65;
        app.sidebar_max_width = 65;
        app.sidebar_group_menu_open = true;
        app.palette = crate::app::state::Palette::from_name("github-light-high-contrast")
            .expect("built-in theme resolves");
        let area = Rect::new(0, 0, 140, 24);
        compute_view(&mut app, area);
        let layout = sidebar_group_menu_layout(&app, area).expect("view dropdown fits");

        let mut terminal =
            Terminal::new(TestBackend::new(area.width, area.height)).expect("test terminal");
        terminal
            .draw(|frame| render(&app, frame))
            .expect("render full frame");
        let selected_row = layout.list_rect.y;
        let buffer = terminal.backend().buffer();

        for x in layout.list_rect.x..layout.list_rect.right() {
            assert_eq!(
                buffer[(x, selected_row)].style().bg,
                Some(app.palette.surface1),
                "selected view row cell at x={x} kept the popup background"
            );
        }
    }

    /// Guards the invisibility class, not dimness: text the same color as the
    /// surface it sits on, which is how a mismatched palette used to hide the
    /// sidebar and how a white-on-white style hides a selected tab. The dimmest
    /// built-in chrome measures 2.07:1 today (catppuccin-latte's inactive tab
    /// label), so the floor sits just below it — the selected tab, which has to
    /// be found at a glance, is held to the WCAG 4.5:1 text bar instead.
    #[test]
    fn themed_chrome_keeps_a_contrast_floor() {
        const CHROME_FLOOR: f64 = 1.8;
        const ACTIVE_TAB_FLOOR: f64 = 4.5;
        let mut violations: Vec<String> = Vec::new();
        for theme in crate::app::state::THEME_NAMES {
            if *theme == "terminal" {
                continue;
            }
            let mut app = themed_app(theme);
            let area = Rect::new(0, 0, 80, 24);
            compute_view(&mut app, area);
            let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
            terminal.draw(|frame| render(&app, frame)).unwrap();
            let buffer = terminal.backend().buffer();
            let section_header_rows =
                compute_sidebar_section_header_areas(&app, app.view.sidebar_rect)
                    .into_iter()
                    .map(|header| header.rect.y)
                    .collect::<std::collections::HashSet<_>>();

            let active_tab = app
                .active
                .and_then(|idx| app.workspaces.get(idx))
                .map(|ws| ws.active_tab)
                .and_then(|idx| app.view.tab_hit_areas.get(idx).copied())
                .expect("fixture has an active tab");

            for rect in chrome_rects(&app) {
                for y in rect.y..rect.y + rect.height {
                    for x in rect.x..rect.x + rect.width {
                        let cell = &buffer[(x, y)];
                        if cell.symbol().trim().is_empty()
                            || cell.symbol() == "─"
                            || section_header_rows.contains(&y)
                        {
                            continue;
                        }
                        let (Some(fg), Some(bg)) = (cell.style().fg, cell.style().bg) else {
                            continue;
                        };
                        let Some(ratio) = contrast_ratio(fg, bg) else {
                            continue;
                        };
                        let floor = if active_tab.contains(ratatui::layout::Position::new(x, y)) {
                            ACTIVE_TAB_FLOOR
                        } else {
                            CHROME_FLOOR
                        };
                        if ratio < floor {
                            violations.push(format!(
                                "{theme}: {:?} at ({x},{y}) renders {fg:?} on {bg:?} at {ratio:.2}:1, floor {floor:.1}:1",
                                cell.symbol()
                            ));
                        }
                    }
                }
            }
        }
        assert!(violations.is_empty(), "{}", violations.join("\n"));
    }

    #[test]
    fn copy_feedback_offset_only_increases_when_toast_rect_overlaps() {
        let area = Rect::new(0, 0, 80, 24);
        let feedback = crate::app::state::CopyFeedback {
            message: "copied to clipboard".into(),
        };
        let toast = crate::app::state::ToastNotification {
            kind: crate::app::state::ToastKind::Finished,
            title: "pi finished".into(),
            context: "workspace · 1".into(),
            position: None,
            target: None,
        };

        let bottom_right_toast = toast_notification_rect(
            area,
            &toast,
            false,
            crate::config::ToastHerdrPosition::BottomRight,
        );
        assert_eq!(
            copy_feedback_offset_for_toast(
                area,
                &feedback,
                0,
                crate::config::ToastClipboardPosition::TopCenter,
                bottom_right_toast,
            ),
            0
        );

        let bottom_center_toast = Rect::new(28, 21, 24, 3);
        assert_eq!(
            copy_feedback_offset_for_toast(
                area,
                &feedback,
                0,
                crate::config::ToastClipboardPosition::BottomCenter,
                bottom_center_toast,
            ),
            bottom_center_toast.height
        );
    }

    #[test]
    fn workspace_creation_dialog_renders_new_workspace_title() {
        let mut app = crate::app::state::AppState::test_new();
        app.open_client_overlay(crate::app::state::ClientOverlay::RenameWorkspace);
        app.pending_workspace_create_cwd = Some("/tmp/project".into());
        app.name_input = "project".into();

        let area = Rect::new(0, 0, 80, 20);
        compute_view(&mut app, area);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal.draw(|frame| render(&app, frame)).unwrap();
        let screen = (0..area.height)
            .map(|row| buffer_row_text(terminal.backend().buffer(), area, row))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(screen.contains("new workspace"), "{screen}");
        assert!(screen.contains("project"), "{screen}");
    }

    #[test]
    fn home_render_dispatch_wins_over_an_inbox_that_is_also_open() {
        let mut app = crate::app::state::AppState::test_new();
        app.home = Some(crate::app::home::HomeState::default());
        app.inbox = Some(crate::app::inbox::InboxState::default());

        let area = Rect::new(0, 0, 80, 20);
        compute_view(&mut app, area);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal.draw(|frame| render(&app, frame)).unwrap();
        let screen = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(screen.contains("nothing is waiting on you"), "{screen}");
        assert!(!screen.contains("nothing is blocked"), "{screen}");
    }

    #[test]
    fn a_collapsed_dock_leaves_the_layout_exactly_as_it_was() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.set_server_mode(Mode::Terminal);
        let area = Rect::new(0, 0, 80, 20);

        compute_view(&mut app, area);

        let body = Rect::new(0, 1, 80, 19);
        let [sidebar, old_main] =
            Layout::horizontal([Constraint::Length(26), Constraint::Min(1)]).areas(body);
        let [old_tab_bar, old_terminal] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(old_main);

        assert!(app.dock_collapsed);
        assert_eq!(app.view.sidebar_rect, sidebar);
        assert_eq!(app.view.tab_bar_rect.x, old_tab_bar.x);
        assert_eq!(app.view.tab_bar_rect.y, old_tab_bar.y);
        assert_eq!(app.view.tab_bar_rect.height, old_tab_bar.height);
        assert_eq!(
            app.view.tab_bar_rect.width + DOCK_COLLAPSED_WIDTH,
            old_tab_bar.width
        );
        assert_eq!(app.view.terminal_area.x, old_terminal.x);
        assert_eq!(app.view.terminal_area.y, old_terminal.y);
        assert_eq!(app.view.terminal_area.height, old_terminal.height);
        assert_eq!(
            app.view.terminal_area.width + DOCK_COLLAPSED_WIDTH,
            old_terminal.width
        );
        assert_eq!(app.view.dock_rect, Rect::new(79, 1, 1, 19));
        assert_eq!(app.view.dock_handle_rect, app.view.dock_rect);
    }

    #[test]
    fn a_small_terminal_refuses_to_open_the_dock_before_clipping_the_terminal() {
        let mut app = crate::app::state::AppState::test_new();
        app.mobile_width_threshold = 0;
        app.status_bar_enabled = false;
        app.dock_collapsed = false;
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.set_server_mode(Mode::Terminal);

        compute_view(&mut app, Rect::new(0, 0, 45, 12));

        assert!(app.dock_collapsed);
        assert_eq!(app.view.dock_rect.width, DOCK_COLLAPSED_WIDTH);
        assert!(app.view.terminal_area.width >= 1);
    }

    /// Screen size, open surfaces and the strip row the dock draws for them.
    fn strip_row(width: u16, height: u16, open: &[crate::app::DockSurface]) -> (String, AppState) {
        let mut app = crate::app::state::AppState::test_new();
        app.dock_collapsed = false;
        app.dock_open_surfaces = open.to_vec();
        app.dock_tab = open.first().copied();
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.set_server_mode(Mode::Terminal);
        let area = Rect::new(0, 0, width, height);

        compute_view(&mut app, area);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");
        terminal
            .draw(|frame| render(&app, frame))
            .expect("render dock");
        let buffer = terminal.backend().buffer().clone();
        let strip = app.view.dock_tab_bar_rect;
        let row = (strip.x..strip.right())
            .map(|col| buffer[(col, strip.y)].symbol())
            .collect::<String>();
        (row, app)
    }

    fn three_and_seven_tabs() -> [Vec<crate::app::DockSurface>; 2] {
        use crate::app::DockSurface;
        [
            vec![DockSurface::Home, DockSurface::Editor, DockSurface::Files],
            vec![
                DockSurface::Home,
                DockSurface::Editor,
                DockSurface::Shortcuts,
                DockSurface::Context,
                DockSurface::Scratchpad,
                DockSurface::Files,
                DockSurface::Diff,
            ],
        ]
    }

    #[test]
    fn the_dock_strip_keeps_one_space_between_tab_labels() {
        for (width, height) in [(80u16, 24u16), (120, 40)] {
            for open in three_and_seven_tabs() {
                let (row, app) = strip_row(width, height, &open);
                let rendered = app
                    .dock_open_surfaces
                    .iter()
                    .copied()
                    .zip(app.view.dock_tab_hit_areas.iter())
                    .filter(|(_, area)| area.width > 0)
                    .map(|(surface, area)| (surface, *area))
                    .collect::<Vec<_>>();
                assert!(!rendered.is_empty(), "{width}x{height}: no tab drawn");
                for (surface, area) in rendered {
                    let start = usize::from(area.x - app.view.dock_tab_bar_rect.x);
                    let cells = row.chars().skip(start).take(usize::from(area.width));
                    let drawn = cells.collect::<String>();
                    let label = surface
                        .label()
                        .chars()
                        .take(usize::from(area.width.saturating_sub(1)))
                        .collect::<String>();
                    assert!(
                        drawn.starts_with(&label),
                        "{width}x{height}: {drawn:?} does not start with {label:?}"
                    );
                    assert!(
                        drawn.ends_with(' '),
                        "{width}x{height}: {drawn:?} has no separating space in {row:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_plus_hit_area_is_the_cell_the_strip_draws_the_plus_in() {
        for (width, height) in [(80u16, 24u16), (120, 40)] {
            for open in three_and_seven_tabs() {
                let (row, app) = strip_row(width, height, &open);
                let plus = app.view.dock_plus_rect;
                assert_eq!(plus.width, crate::ui::dock::PLUS_WIDTH);
                assert_eq!(plus.y, app.view.dock_tab_bar_rect.y);
                let cell = usize::from(plus.x - app.view.dock_tab_bar_rect.x);
                assert_eq!(
                    row.chars().nth(cell),
                    Some('+'),
                    "{width}x{height}: the + is not at {plus:?} in {row:?}"
                );
                assert_eq!(
                    row.matches('+').count(),
                    1,
                    "{width}x{height}: more than one + in {row:?}"
                );
                assert!(app.on_dock_plus(plus.x, plus.y));
                // The tab left of the `+` must not claim its cell.
                assert_eq!(app.dock_tab_at(plus.x, plus.y), None);
            }
        }
    }

    /// The auto-open toggle sits inside the strip, left of the maximise glyph,
    /// and is clickable there. Maximise keeps the outer cell either way so its
    /// position does not move when the strip is too narrow for both.
    #[test]
    fn the_auto_open_toggle_sits_left_of_maximise_and_is_clickable() {
        let (row, app) = strip_row(120, 40, &[crate::app::DockSurface::Files]);
        let toggle = app.view.dock_auto_open_rect;
        let maximize = app.view.dock_maximize_rect;
        assert_eq!(toggle.width, 1);
        assert_eq!(toggle.y, app.view.dock_tab_bar_rect.y);
        assert_eq!(toggle.x + 2, maximize.x);
        let cell = usize::from(toggle.x - app.view.dock_tab_bar_rect.x);
        assert_eq!(
            row.chars().nth(cell),
            Some('◧'),
            "the toggle is not at {toggle:?} in {row:?}"
        );
        assert!(app.on_dock_auto_open(toggle.x, toggle.y));
        assert!(!app.on_dock_maximize(toggle.x, toggle.y));
        assert_eq!(app.dock_tab_at(toggle.x, toggle.y), None);

        // A collapsed dock exposes neither control.
        let mut collapsed = app;
        collapsed.dock_collapsed = true;
        assert!(!collapsed.on_dock_auto_open(toggle.x, toggle.y));
    }

    #[test]
    fn f20_hovered_object_tab_renders_its_full_title_below_the_strip() {
        use crate::app::state::{DockObjectRef, DockTabBinding, DockTabOrigin};
        use crate::app::DockSurface;

        let mut app = crate::app::state::AppState::test_new();
        app.dock_collapsed = false;
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.set_server_mode(Mode::Terminal);
        compute_view(&mut app, Rect::new(0, 0, 120, 40));

        let key = "https://mail.missiveapp.com/#inbox/conversations/abc".to_string();
        app.dock_open_surfaces = vec![DockSurface::Missive];
        app.dock_tab = Some(DockSurface::Missive);
        app.dock_active_tab_index = Some(0);
        app.hovered_control = Some(crate::app::state::ControlId::DockTab(0));
        app.hover_tooltip_visible = true;
        app.dock_tab_bindings = vec![Some(DockTabBinding {
            object: DockObjectRef {
                surface: DockSurface::Missive,
                key: key.clone(),
            },
            origin: DockTabOrigin::Context,
        })];
        app.view.dock_tab_hit_areas = vec![Rect::new(
            app.view.dock_tab_bar_rect.x,
            app.view.dock_tab_bar_rect.y,
            14,
            1,
        )];
        app.work_index_snapshot = Some(crate::work_index::Snapshot {
            items: Vec::new(),
            conversations: vec![crate::work_index::MissiveConversation {
                id: "abc".into(),
                subject: "Full conversation subject".into(),
                app_url: key,
                web_url: String::new(),
                team: None,
                assignees: Vec::new(),
                last_activity_at: None,
                closed: false,
                labels: Vec::new(),
                pane_bound: true,
                messages: Vec::new(),
                notes: Vec::new(),
                drafts: Vec::new(),
                posts: Vec::new(),
            }],
            missive_users: Vec::new(),
            unavailable: None,
            observed_at: std::time::SystemTime::now(),
        });

        let mut terminal = Terminal::new(TestBackend::new(120, 40)).expect("test terminal");
        terminal.draw(|frame| render(&app, frame)).expect("render");
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(rendered.contains("Full conversation subject"));
    }

    #[test]
    fn a_scrolled_strip_gives_hidden_tabs_no_hit_area() {
        use crate::app::DockSurface;
        let open = three_and_seven_tabs()[1].clone();
        let mut app = crate::app::state::AppState::test_new();
        app.dock_collapsed = false;
        app.dock_open_surfaces = open.clone();
        app.dock_tab = Some(DockSurface::Diff);
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.set_server_mode(Mode::Terminal);

        compute_view(&mut app, Rect::new(0, 0, 120, 40));

        let hidden = app.view.dock_tab_hit_areas[0];
        assert_eq!(hidden.width, 0, "the first tab scrolled out of the strip");
        assert_eq!(
            app.dock_tab_at(hidden.x, app.view.dock_tab_bar_rect.y),
            None
        );
        let active = app.view.dock_tab_hit_areas[open.len() - 1];
        assert!(active.width > 0, "the active tab stays visible");
        assert!(active.right() <= app.view.dock_plus_rect.x);
    }

    #[test]
    fn the_dock_strip_ends_with_a_plus_and_a_maximise_glyph() {
        let mut app = crate::app::state::AppState::test_new();
        app.dock_collapsed = false;
        app.dock_open_surfaces = vec![crate::app::DockSurface::Home];
        app.dock_tab = Some(crate::app::DockSurface::Home);
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.set_server_mode(Mode::Terminal);

        compute_view(&mut app, Rect::new(0, 0, 120, 40));

        let tabs = app.view.dock_tab_hit_areas.clone();
        assert_eq!(tabs.len(), app.dock_open_surfaces.len());
        assert!(app.view.dock_plus_rect.x >= tabs.last().expect("a tab").right());
        assert!(app.view.dock_maximize_rect.x >= app.view.dock_plus_rect.right());
        assert!(app.view.dock_maximize_rect.right() <= app.view.dock_rect.right());
        // The close glyph sits inside the active tab, after its label.
        let active = tabs.first().copied().expect("home is active");
        assert_eq!(app.view.dock_tab_close_rect.y, active.y);
        assert!(app.view.dock_tab_close_rect.x > active.x);
        assert!(app.view.dock_tab_close_rect.right() <= active.right());
    }

    #[test]
    fn an_empty_dock_lays_out_the_card_grid_instead_of_tabs() {
        let mut app = crate::app::state::AppState::test_new();
        app.dock_collapsed = false;
        app.dock_open_surfaces.clear();
        app.dock_tab = None;
        app.dock_chooser_focused = true;
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.set_server_mode(Mode::Terminal);
        let area = Rect::new(0, 0, 120, 40);

        compute_view(&mut app, area);
        assert!(app.view.dock_tab_hit_areas.is_empty());
        assert_eq!(
            app.view.dock_surface_card_hit_areas.len(),
            crate::app::DockSurface::CARDS.len()
        );

        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal.draw(|frame| render(&app, frame)).unwrap();
        let screen = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(screen.contains("Open a surface"));
        for title in [
            "Terminal",
            "Files",
            "Diff",
            "Pull request",
            "Linear",
            "Missive",
        ] {
            assert!(screen.contains(title), "missing {title}");
        }
        assert!(screen.contains("Choose what to show in the right panel."));
    }

    #[test]
    fn empty_dock_grid_is_single_column_at_80_and_two_columns_at_120() {
        for (width, height, two_columns) in [(80, 24, false), (120, 40, true)] {
            let mut app = crate::app::state::AppState::test_new();
            app.dock_collapsed = false;
            app.workspaces = vec![Workspace::test_new("one")];
            app.active = Some(0);
            app.set_server_mode(Mode::Terminal);

            compute_view(&mut app, Rect::new(0, 0, width, height));
            let cards = &app.view.dock_surface_card_hit_areas;
            assert!(cards.len() >= 2);
            assert_eq!(cards[0].y == cards[1].y, two_columns, "width {width}");
            assert_eq!(cards[0].x == cards[1].x, !two_columns, "width {width}");
        }
    }

    #[test]
    fn a_maximised_dock_takes_the_main_area_and_a_restore_gives_it_back() {
        let mut app = crate::app::state::AppState::test_new();
        app.dock_collapsed = false;
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.set_server_mode(Mode::Terminal);
        let area = Rect::new(0, 0, 120, 40);

        compute_view(&mut app, area);
        let normal = app.view.dock_rect.width;

        app.dock_maximized = true;
        compute_view(&mut app, area);
        let maximised = app.view.dock_rect.width;
        assert!(maximised > normal);
        assert_eq!(
            app.view.dock_rect.right(),
            area.right(),
            "the dock still ends at the screen edge"
        );

        app.dock_maximized = false;
        compute_view(&mut app, area);
        assert_eq!(app.view.dock_rect.width, normal);
    }

    #[test]
    fn an_open_dock_exposes_every_named_tab_and_a_body_area() {
        let mut app = crate::app::state::AppState::test_new();
        app.dock_collapsed = false;
        // Home is the default tab now; this case still asserts the editor body.
        app.dock_tab = Some(crate::app::DockSurface::Editor);
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.set_server_mode(Mode::Terminal);
        let area = Rect::new(0, 0, 120, 20);

        compute_view(&mut app, area);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal.draw(|frame| render(&app, frame)).unwrap();
        let screen = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert_eq!(
            app.view.dock_tab_hit_areas.len(),
            app.dock_open_surfaces.len()
        );
        assert!(app.view.dock_body_rect.height > 0);
        for tab in app.dock_open_surfaces.iter().copied() {
            assert!(
                screen.contains(tab.label()),
                "missing {} in {screen:?}",
                tab.label()
            );
        }
        assert!(screen.contains("focus an agent first"));
    }

    #[test]
    fn dock_home_hit_areas_follow_rendered_tabs_and_home_visibility() {
        let mut app = crate::app::state::AppState::test_new();
        app.mobile_width_threshold = 0;
        app.dock_collapsed = false;
        app.dock_tab = Some(crate::app::DockSurface::Home);
        app.workspaces = vec![Workspace::test_new("review")];
        app.active = Some(0);
        app.set_server_mode(Mode::Terminal);
        app.ensure_test_terminals();
        let pane_id = app.workspaces[0].focused_pane_id().expect("pane");
        let terminal_id = app.workspaces[0]
            .terminal_id(pane_id)
            .cloned()
            .expect("terminal");
        app.terminals
            .get_mut(&terminal_id)
            .expect("terminal state")
            .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                pr_urls: Some(vec!["https://github.com/herdrdev/herdr/pull/125".into()]),
                work_title: Some("dock home".into()),
                role: Some(crate::work_context::PaneWorkRole::Review),
                active_owner: Some(true),
                ..Default::default()
            })
            .expect("work context");
        let screen = Rect::new(0, 0, 120, 20);

        compute_view(&mut app, screen);
        let projection = app.dock_home_projection();
        assert_eq!(
            app.view.dock_home_tab_hit_areas.len(),
            projection.rows.len()
        );
        assert_eq!(app.view.dock_home_tab_keys.len(), projection.rows.len());
        assert_eq!(app.view.dock_home_tab_keys[0], projection.rows[0].key);
        assert_eq!(app.view.dock_home_tab_hit_areas[0].height, 1);
        assert_eq!(
            app.view.dock_home_detail_tab_hit_areas.len(),
            crate::app::state::DockHomeDetailTab::ALL.len()
        );

        app.dock_collapsed = true;
        compute_view(&mut app, screen);
        assert!(app.view.dock_home_tab_hit_areas.is_empty());
        assert!(app.view.dock_home_tab_keys.is_empty());
        assert!(app.view.dock_home_detail_tab_hit_areas.is_empty());

        app.dock_collapsed = false;
        app.dock_tab = Some(crate::app::DockSurface::Editor);
        compute_view(&mut app, screen);
        assert!(app.view.dock_home_tab_hit_areas.is_empty());
        assert!(app.view.dock_home_tab_keys.is_empty());
    }

    #[tokio::test]
    async fn focused_pane_cursor_wins_during_terminal_render() {
        let mut app = crate::app::state::AppState::test_new();
        let mut ws = Workspace::test_new("test");
        let first_pane = ws.tabs[0].root_pane;
        let second_pane = ws.test_split(ratatui::layout::Direction::Horizontal);

        ws.insert_test_runtime(
            first_pane,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(20, 5, b"left"),
        );
        ws.insert_test_runtime(
            second_pane,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(20, 5, b"r\r\nb"),
        );
        ws.tabs[0].layout.focus_pane(first_pane);

        app.workspaces = vec![ws];
        app.active = Some(0);
        app.selected = 0;
        app.set_server_mode(Mode::Terminal);

        compute_view(&mut app, Rect::new(0, 0, 80, 20));
        let focused = app
            .view
            .pane_infos
            .iter()
            .find(|info| info.id == first_pane)
            .expect("focused pane info");

        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(&app, frame)).unwrap();

        terminal
            .backend_mut()
            .assert_cursor_position((focused.inner_rect.x + 4, focused.inner_rect.y));
    }

    #[test]
    fn mobile_width_uses_header_and_full_width_terminal() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.set_server_mode(Mode::Terminal);

        compute_view(&mut app, Rect::new(0, 0, 44, 20));

        assert_eq!(app.view.layout, ViewLayout::Mobile);
        assert_eq!(app.view.sidebar_rect, Rect::default());
        assert_eq!(app.view.tab_bar_rect, Rect::default());
        assert_eq!(app.view.mobile_header_rect, Rect::new(0, 0, 44, 2));
        assert_eq!(app.view.terminal_area, Rect::new(0, 2, 44, 18));
        assert_eq!(app.view.mobile_menu_hit_area.height, 2);
        assert_eq!(
            app.view.mobile_menu_hit_area.x + app.view.mobile_menu_hit_area.width,
            44
        );
    }

    #[test]
    fn config_diagnostic_renders_as_a_three_cell_marker_when_idle() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.set_server_mode(Mode::Terminal);
        app.config_diagnostic = Some("config.toml:100:10; herdr config check".into());

        let area = Rect::new(0, 0, 44, 20);
        compute_view(&mut app, area);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal.draw(|frame| render(&app, frame)).unwrap();
        let row = buffer_row_text(terminal.backend().buffer(), area, app.view.terminal_area.y);
        let marker = app.view.config_diagnostic_hit_area;
        let marker_text = (marker.x..marker.right())
            .map(|x| terminal.backend().buffer()[(x, marker.y)].symbol())
            .collect::<String>();

        assert_eq!(marker.width, 3);
        assert_eq!(marker.right(), app.view.terminal_area.right());
        assert_eq!(marker_text, " ! ");
        assert!(!row.contains("config.toml:100:10"), "{row}");
        assert!(!row.contains("herdr config check"), "{row}");
    }

    #[test]
    fn hovering_config_diagnostic_marker_renders_the_full_message() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.set_server_mode(Mode::Terminal);
        app.config_diagnostic = Some("config.toml:100:10; herdr config check".into());

        let area = Rect::new(0, 0, 44, 20);
        compute_view(&mut app, area);
        app.hovered_control = Some(crate::app::state::ControlId::ConfigDiagnostic);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal.draw(|frame| render(&app, frame)).unwrap();
        let row = buffer_row_text(terminal.backend().buffer(), area, app.view.terminal_area.y);

        assert!(row.contains("config.toml:100:10"), "{row}");
        assert!(row.contains("herdr config check"), "{row}");
    }

    #[test]
    fn desktop_toast_hit_area_uses_full_frame_not_terminal_area() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.set_server_mode(Mode::Terminal);
        app.toast_config.herdr.position = crate::config::ToastHerdrPosition::TopLeft;
        app.toast = Some(crate::app::state::ToastNotification {
            kind: crate::app::state::ToastKind::Finished,
            title: "pi finished".into(),
            context: "one".into(),
            position: None,
            target: None,
        });

        compute_view(&mut app, Rect::new(0, 0, 100, 20));

        assert_eq!(app.view.layout, ViewLayout::Desktop);
        assert!(app.view.terminal_area.x > 0);
        assert_eq!(app.view.toast_hit_area.x, 0);
        assert_eq!(app.view.toast_hit_area.y, 0);
    }

    #[test]
    fn desktop_toast_hit_area_still_offsets_for_config_diagnostic() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.set_server_mode(Mode::Terminal);
        app.config_diagnostic = Some("config warning".into());
        app.toast_config.herdr.position = crate::config::ToastHerdrPosition::TopLeft;
        app.toast = Some(crate::app::state::ToastNotification {
            kind: crate::app::state::ToastKind::Finished,
            title: "pi finished".into(),
            context: "one".into(),
            position: None,
            target: None,
        });

        compute_view(&mut app, Rect::new(0, 0, 100, 20));

        assert_eq!(app.view.toast_hit_area.x, 0);
        assert_eq!(app.view.toast_hit_area.y, 1);
    }

    #[test]
    fn configured_mobile_width_threshold_controls_layout_switch() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.set_server_mode(Mode::Terminal);

        compute_view(&mut app, Rect::new(0, 0, 80, 20));
        assert_eq!(app.view.layout, ViewLayout::Desktop);

        app.mobile_width_threshold = 90;
        compute_view(&mut app, Rect::new(0, 0, 80, 20));
        assert_eq!(app.view.layout, ViewLayout::Mobile);
        assert_eq!(app.view.mobile_header_rect, Rect::new(0, 0, 80, 2));
        assert_eq!(app.view.terminal_area, Rect::new(0, 2, 80, 18));
    }

    #[test]
    fn desktop_tab_bar_position_controls_geometry_and_mode_bar_placement() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.set_server_mode(Mode::Prefix);

        compute_view(&mut app, Rect::new(0, 0, 80, 20));
        assert_eq!(app.view.tab_bar_rect, Rect::new(26, 1, 53, 1));
        assert_eq!(app.view.terminal_area, Rect::new(26, 2, 53, 18));

        app.tab_bar_position = crate::config::TabBarPositionConfig::Bottom;
        compute_view(&mut app, Rect::new(0, 0, 80, 20));
        assert_eq!(app.view.terminal_area, Rect::new(26, 1, 53, 18));
        assert_eq!(app.view.tab_bar_rect, Rect::new(26, 19, 53, 1));
        assert!(app.view.tab_hit_areas.iter().all(|rect| rect.y == 19));
        assert_eq!(app.view.new_tab_hit_area.y, 19);

        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
        terminal.draw(|frame| render(&app, frame)).unwrap();
        let mode_row = buffer_row_text(
            terminal.backend().buffer(),
            app.view.tab_bar_rect,
            app.view.tab_bar_rect.y,
        );
        assert!(mode_row.contains("PREFIX"), "{mode_row}");
    }

    #[test]
    fn hide_tab_bar_when_single_tab_toggles_geometry_with_tab_count() {
        let mut app = crate::app::state::AppState::test_new();
        app.hide_tab_bar_when_single_tab = true;
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.set_server_mode(Mode::Terminal);

        compute_view(&mut app, Rect::new(0, 0, 80, 20));
        let single_tab_terminal_area = app.view.terminal_area;
        // Full-width status bar occupies row 0; chrome starts at y=1.
        assert_eq!(app.view.status_bar_rect, Rect::new(0, 0, 80, 1));
        assert_eq!(app.view.tab_bar_rect, Rect::default());
        assert_eq!(single_tab_terminal_area, Rect::new(26, 1, 53, 19));
        assert!(app.view.tab_hit_areas.is_empty());
        assert_eq!(app.view.new_tab_hit_area, Rect::default());

        app.workspaces[0].test_add_tab(Some("logs"));
        compute_view(&mut app, Rect::new(0, 0, 80, 20));

        assert_eq!(app.view.status_bar_rect, Rect::new(0, 0, 80, 1));
        assert_eq!(app.view.tab_bar_rect, Rect::new(26, 1, 53, 1));
        assert_eq!(app.view.terminal_area, Rect::new(26, 2, 53, 18));
        assert_eq!(app.view.tab_hit_areas.len(), 2);
        assert!(app.view.tab_hit_areas.iter().all(|rect| rect.width > 0));
        assert!(app.view.new_tab_hit_area.width > 0);

        assert!(app.workspaces[0].close_tab(1));
        compute_view(&mut app, Rect::new(0, 0, 80, 20));

        assert_eq!(app.view.terminal_area, single_tab_terminal_area);
        assert_eq!(app.view.tab_bar_rect, Rect::default());
        assert!(app.view.tab_hit_areas.is_empty());
        assert_eq!(app.view.new_tab_hit_area, Rect::default());
    }

    /// The pull button and the two pane toggles duplicate keybindings and eat
    /// tab-title width, so they are opt-in. Their hit areas must collapse, not
    /// merely go undrawn, or the row still reserves their columns.
    #[test]
    fn the_optional_top_right_controls_are_absent_by_default() {
        let defaults = crate::config::Config::default();
        assert!(!defaults.ui.show_pull_button);
        assert!(!defaults.ui.show_pane_toggle_buttons);

        let mut app = crate::app::state::AppState::test_new();
        app.tab_bar_position = crate::config::TabBarPositionConfig::Top;
        app.mouse_capture = true;
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.set_server_mode(Mode::Terminal);
        app.ensure_test_terminals();

        compute_view(&mut app, Rect::new(0, 0, 120, 40));

        assert_eq!(app.view.git_menu_button_hit_area, Rect::default());
        assert_eq!(app.view.pane_toggle_below_hit_area, Rect::default());
        assert_eq!(app.view.pane_toggle_right_hit_area, Rect::default());
        // `+ Action` is not optional and still anchors the row.
        assert!(app.view.add_action_button_hit_area.width > 0);
    }

    /// Each control is independent, so the mixed combinations have to place the
    /// remaining ones correctly too, on the tab row and on the status row.
    #[test]
    fn each_optional_control_hides_independently_on_both_host_rows() {
        for on_tab_row in [true, false] {
            for (pull, toggles) in [(true, false), (false, true)] {
                let mut app = crate::app::state::AppState::test_new();
                app.tab_bar_position = if on_tab_row {
                    crate::config::TabBarPositionConfig::Top
                } else {
                    crate::config::TabBarPositionConfig::Hidden
                };
                app.mouse_capture = true;
                app.show_pull_button = pull;
                app.show_pane_toggle_buttons = toggles;
                app.workspaces = vec![Workspace::test_new("one")];
                app.active = Some(0);
                app.selected = 0;
                app.set_server_mode(Mode::Terminal);
                app.ensure_test_terminals();

                compute_view(&mut app, Rect::new(0, 0, 120, 40));

                let label = format!("tab_row={on_tab_row} pull={pull} toggles={toggles}");
                let menu = app.view.git_menu_button_hit_area;
                let below = app.view.pane_toggle_below_hit_area;
                let right = app.view.pane_toggle_right_hit_area;
                let add = app.view.add_action_button_hit_area;
                assert!(add.width > 0, "{label}");
                assert_eq!(menu.width > 0, pull, "{label}");
                assert_eq!(below.width > 0, toggles, "{label}");
                assert_eq!(right.width > 0, toggles, "{label}");
                // Every drawn control shares the row and never overlaps.
                let mut spans: Vec<(u16, u16)> = [add, menu, below, right]
                    .into_iter()
                    .filter(|rect| rect.width > 0)
                    .map(|rect| {
                        assert_eq!(rect.y, add.y, "{label}");
                        (rect.x, rect.x + rect.width)
                    })
                    .collect();
                spans.sort_unstable();
                for pair in spans.windows(2) {
                    assert!(pair[0].1 <= pair[1].0, "{label} overlapping {spans:?}");
                }
            }
        }
    }

    /// Rendering a hidden pull button must not repair its now-invalid menu.
    /// Config application owns that transition before the next frame.
    #[test]
    fn losing_the_pull_button_does_not_close_an_open_git_menu_during_render() {
        let mut app = crate::app::state::AppState::test_new();
        app.tab_bar_position = crate::config::TabBarPositionConfig::Top;
        app.mouse_capture = true;
        app.show_pull_button = true;
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.set_server_mode(Mode::Terminal);
        app.ensure_test_terminals();
        compute_view(&mut app, Rect::new(0, 0, 120, 40));

        app.set_server_mode(Mode::GitMenu);
        compute_view(&mut app, Rect::new(0, 0, 120, 40));
        assert_eq!(app.server_mode(), Mode::GitMenu);
        assert!(app.view.git_menu_popup_rect.width > 0);

        app.show_pull_button = false;
        compute_view(&mut app, Rect::new(0, 0, 120, 40));

        assert_eq!(app.server_mode(), Mode::GitMenu);
    }

    /// A narrow client that cannot fit the button must not close a menu another
    /// client is looking at, and the mobile layout must not skip the close.
    #[test]
    fn client_geometry_does_not_decide_whether_the_git_menu_closes() {
        let mut app = crate::app::state::AppState::test_new();
        app.tab_bar_position = crate::config::TabBarPositionConfig::Top;
        app.mouse_capture = true;
        app.show_pull_button = true;
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.set_server_mode(Mode::GitMenu);
        app.ensure_test_terminals();

        // Too narrow for the action row, and narrow enough for mobile layout.
        compute_view(&mut app, Rect::new(0, 0, 40, 20));
        assert_eq!(app.view.git_menu_button_hit_area, Rect::default());
        assert_eq!(
            app.server_mode(),
            Mode::GitMenu,
            "geometry alone must not close it"
        );

        app.show_pull_button = false;
        compute_view(&mut app, Rect::new(0, 0, 40, 20));
        assert_eq!(app.server_mode(), Mode::GitMenu);
    }

    /// Hiding the two controls must give their columns back to the tabs rather
    /// than leave a gap at the right edge.
    #[test]
    fn hiding_the_optional_controls_reclaims_their_columns() {
        let build = |show: bool| {
            let mut app = crate::app::state::AppState::test_new();
            app.tab_bar_position = crate::config::TabBarPositionConfig::Top;
            app.mouse_capture = true;
            app.show_pull_button = show;
            app.show_pane_toggle_buttons = show;
            app.workspaces = vec![Workspace::test_new("one")];
            app.active = Some(0);
            app.selected = 0;
            app.set_server_mode(Mode::Terminal);
            app.ensure_test_terminals();
            compute_view(&mut app, Rect::new(0, 0, 120, 40));
            app
        };

        let shown = build(true);
        let hidden = build(false);
        let right_edge = |app: &crate::app::state::AppState| {
            app.view.add_action_button_hit_area.x + app.view.add_action_button_hit_area.width
        };
        assert!(
            right_edge(&hidden) > right_edge(&shown),
            "hidden {} vs shown {}",
            right_edge(&hidden),
            right_edge(&shown)
        );
    }

    /// The buttons were only ever computed for the tab row, which the shipped
    /// default config does not draw: `tab_bar_position` defaults to `hidden`.
    /// Every unit test used `AppState::test_new()`, whose tab row is `Top`, so
    /// the whole feature was invisible at runtime while the tests were green.
    #[test]
    fn opted_in_pane_toggles_fall_back_to_the_status_row_under_the_default_config() {
        assert_eq!(
            crate::config::Config::default().ui.tab_bar_position,
            crate::config::TabBarPositionConfig::Hidden,
            "the default config hides the tab row; the toggles must survive that"
        );

        for (width, height) in [(120u16, 40u16), (80, 24)] {
            let mut app = crate::app::state::AppState::test_new();
            let defaults = crate::config::Config::default();
            app.tab_bar_position = defaults.ui.tab_bar_position;
            app.hide_tab_bar_when_single_tab = defaults.ui.hide_tab_bar_when_single_tab;
            app.mouse_capture = defaults.ui.mouse_capture;
            app.show_pull_button = true;
            app.show_pane_toggle_buttons = true;
            app.workspaces = vec![Workspace::test_new("one")];
            app.active = Some(0);
            app.selected = 0;
            app.set_server_mode(Mode::Terminal);

            compute_view(&mut app, Rect::new(0, 0, width, height));

            assert_eq!(app.view.tab_bar_rect, Rect::default(), "{width}x{height}");
            let status = app.view.status_bar_rect;
            let git = app.view.git_menu_button_hit_area;
            let below = app.view.pane_toggle_below_hit_area;
            let right = app.view.pane_toggle_right_hit_area;
            assert_eq!(
                git,
                Rect::new(status.x + status.width - 16, status.y, 10, 1)
            );
            assert_eq!(
                below,
                Rect::new(status.x + status.width - 6, status.y, 3, 1)
            );
            assert_eq!(right, Rect::new(below.x + 3, status.y, 3, 1));

            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| render(&app, frame)).unwrap();
            let row = buffer_row_text(terminal.backend().buffer(), status, status.y);
            assert!(
                row.contains(tabs::PANE_TOGGLE_BELOW_GLYPH),
                "{width}x{height} status row: {row:?}"
            );
            assert!(
                row.contains(tabs::PANE_TOGGLE_RIGHT_GLYPH),
                "{width}x{height} status row: {row:?}"
            );
            assert!(
                row.contains("⇣ Pull ▾"),
                "{width}x{height} status row: {row:?}"
            );
        }
    }

    #[test]
    fn compute_view_records_git_menu_button_popup_and_row_hit_areas() {
        for (width, height) in [(120u16, 40u16), (80, 24)] {
            let mut app = crate::app::state::AppState::test_new();
            let defaults = crate::config::Config::default();
            app.tab_bar_position = defaults.ui.tab_bar_position;
            app.mouse_capture = true;
            app.show_pull_button = true;
            app.workspaces = vec![Workspace::test_new("one")];
            app.active = Some(0);
            app.ensure_test_terminals();
            let pane_id = app.workspaces[0].focused_pane_id().expect("focused pane");
            let terminal_id = app.workspaces[0]
                .terminal_id(pane_id)
                .expect("terminal")
                .clone();
            let cwd = app
                .terminals
                .get(&terminal_id)
                .expect("terminal state")
                .cwd
                .clone();
            app.git_root_for_cwd.insert(cwd.clone(), Some(cwd));

            compute_view(&mut app, Rect::new(0, 0, width, height));
            app.status_git_cwd = app.status_focused_cwd.clone();
            app.status_git_ahead_behind = Some((0, 1));
            app.set_server_mode(Mode::GitMenu);

            compute_view(&mut app, Rect::new(0, 0, width, height));

            assert_eq!(app.view.git_menu_button_hit_area.width, 10);
            assert_eq!(
                app.view.git_menu_popup_rect.y,
                app.view.git_menu_button_hit_area.y + 1
            );
            assert_eq!(app.view.git_menu_row_hit_areas.len(), 5);
            assert!(app
                .view
                .git_menu_row_hit_areas
                .iter()
                .all(|row| app.view.git_menu_popup_rect.contains((row.x, row.y).into())));
        }
    }

    /// Keyboard-only sessions keep a clean row: no hit areas and no glyphs.
    #[test]
    fn pane_toggles_stay_absent_without_mouse_capture() {
        let mut app = crate::app::state::AppState::test_new();
        app.tab_bar_position = crate::config::TabBarPositionConfig::Hidden;
        app.mouse_capture = false;
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.set_server_mode(Mode::Terminal);

        compute_view(&mut app, Rect::new(0, 0, 120, 40));

        assert_eq!(app.view.pane_toggle_below_hit_area, Rect::default());
        assert_eq!(app.view.pane_toggle_right_hit_area, Rect::default());
        assert_eq!(app.view.git_menu_button_hit_area, Rect::default());

        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        terminal.draw(|frame| render(&app, frame)).unwrap();
        let status = app.view.status_bar_rect;
        let row = buffer_row_text(terminal.backend().buffer(), status, status.y);
        assert!(!row.contains(tabs::PANE_TOGGLE_BELOW_GLYPH), "{row:?}");
        assert!(!row.contains(tabs::PANE_TOGGLE_RIGHT_GLYPH), "{row:?}");
    }

    #[test]
    fn a_hidden_tab_bar_gives_the_row_back_to_the_terminal() {
        let mut app = crate::app::state::AppState::test_new();
        app.tab_bar_position = crate::config::TabBarPositionConfig::Hidden;
        app.workspaces = vec![Workspace::test_new("one")];
        app.workspaces[0].test_add_tab(Some("logs"));
        app.active = Some(0);
        app.selected = 0;
        app.set_server_mode(Mode::Prefix);

        compute_view(&mut app, Rect::new(0, 0, 80, 20));

        // Two tabs, so this is the hiding of the row itself rather than the
        // pre-existing single-tab case.
        assert_eq!(app.workspaces[0].tabs.len(), 2);
        assert_eq!(app.view.tab_bar_rect, Rect::default());
        assert_eq!(app.view.terminal_area, Rect::new(26, 1, 53, 19));
        assert!(app.view.tab_hit_areas.is_empty());
        assert_eq!(app.view.new_tab_hit_area, Rect::default());

        // The mode bar has no tab row to borrow, so it falls back to the
        // terminal area and still renders.
        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
        terminal.draw(|frame| render(&app, frame)).unwrap();
        let mode_row = buffer_row_text(
            terminal.backend().buffer(),
            app.view.terminal_area,
            app.view.terminal_area.bottom() - 1,
        );
        assert!(mode_row.contains("PREFIX"), "{mode_row}");
    }

    #[test]
    fn bottom_tab_bar_still_hides_when_single_tab() {
        let mut app = crate::app::state::AppState::test_new();
        app.hide_tab_bar_when_single_tab = true;
        app.tab_bar_position = crate::config::TabBarPositionConfig::Bottom;
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.set_server_mode(Mode::Prefix);

        compute_view(&mut app, Rect::new(0, 0, 80, 20));
        assert_eq!(app.view.tab_bar_rect, Rect::default());
        assert_eq!(app.view.terminal_area, Rect::new(26, 1, 53, 19));

        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
        terminal.draw(|frame| render(&app, frame)).unwrap();
        let mode_row = buffer_row_text(
            terminal.backend().buffer(),
            app.view.terminal_area,
            app.view.terminal_area.y + app.view.terminal_area.height - 1,
        );
        assert!(mode_row.contains("PREFIX"), "{mode_row}");
    }

    #[tokio::test]
    async fn hide_tab_bar_when_single_tab_resizes_background_tabs_per_workspace() {
        let mut app = crate::app::state::AppState::test_new();
        app.hide_tab_bar_when_single_tab = true;

        let mut one_tab_workspace = Workspace::test_new("one");
        let one_tab_pane = one_tab_workspace.tabs[0].root_pane;
        let one_tab_runtime = crate::terminal::TerminalRuntime::test_with_screen_bytes(10, 5, b"");
        one_tab_workspace.tabs[0]
            .runtimes
            .insert(one_tab_pane, one_tab_runtime);

        let mut two_tab_workspace = Workspace::test_new("two");
        let background_tab = two_tab_workspace.test_add_tab(Some("logs"));
        let two_tab_pane = two_tab_workspace.tabs[background_tab].root_pane;
        let two_tab_runtime = crate::terminal::TerminalRuntime::test_with_screen_bytes(10, 5, b"");
        two_tab_workspace.tabs[background_tab]
            .runtimes
            .insert(two_tab_pane, two_tab_runtime);

        app.workspaces = vec![one_tab_workspace, two_tab_workspace];
        app.active = Some(0);
        app.selected = 0;
        app.set_server_mode(Mode::Terminal);

        compute_view(&mut app, Rect::new(0, 0, 80, 20));

        let one_tab_size = app.workspaces[0].tabs[0].runtimes[&one_tab_pane].current_size();
        let two_tab_size =
            app.workspaces[1].tabs[background_tab].runtimes[&two_tab_pane].current_size();
        // Status bar reserves one full-width row; single-tab workspaces reclaim the tab bar.
        assert_eq!(one_tab_size, (19, 52));
        assert_eq!(two_tab_size, (18, 52));
    }

    #[tokio::test]
    async fn mobile_background_tabs_use_mobile_terminal_area() {
        let mut app = crate::app::state::AppState::test_new();

        let mut workspace = Workspace::test_new("mobile");
        let background_tab = workspace.test_add_tab(Some("logs"));
        let background_pane = workspace.tabs[background_tab].root_pane;
        let runtime = crate::terminal::TerminalRuntime::test_with_screen_bytes(10, 5, b"");
        workspace.tabs[background_tab]
            .runtimes
            .insert(background_pane, runtime);

        app.workspaces = vec![workspace];
        app.active = Some(0);
        app.selected = 0;
        app.set_server_mode(Mode::Terminal);

        compute_view(&mut app, Rect::new(0, 0, 44, 20));

        assert_eq!(app.view.layout, ViewLayout::Mobile);
        assert_eq!(app.view.terminal_area, Rect::new(0, 2, 44, 18));
        assert_eq!(
            app.workspaces[0].tabs[background_tab].runtimes[&background_pane].current_size(),
            (18, 43)
        );
    }

    #[test]
    fn product_announcement_renders_above_config_diagnostic() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.set_server_mode(Mode::ProductAnnouncement);
        app.product_announcement = Some(crate::app::state::ProductAnnouncementState {
            version: "0.6.0".into(),
            id: "keybinding-v2".into(),
            title: "Keybinding syntax changed".into(),
            body: "### Update\n- Body".into(),
            scroll: 0,
            preview: false,
        });
        app.config_diagnostic = Some(
            "unsafe direct keybinding: keys.new_workspace = \"n\"\nunsafe direct keybinding: keys.new_tab = \"c\""
                .into(),
        );

        let area = Rect::new(0, 0, 44, 20);
        compute_view(&mut app, area);

        let backend = TestBackend::new(area.width, area.height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(&app, frame)).unwrap();
        let buffer = terminal.backend().buffer();

        let popup = centered_popup_rect(
            area,
            PRODUCT_ANNOUNCEMENT_MODAL_SIZE.0,
            PRODUCT_ANNOUNCEMENT_MODAL_SIZE.1,
        )
        .expect("announcement popup");
        let title_row = popup.y + 1;
        let row = buffer_row_text(buffer, Rect::new(0, title_row, area.width, 1), title_row);

        assert!(row.contains("Keybinding syntax changed"));
        assert!(!row.contains("config warning"));
    }

    #[test]
    fn compute_view_clamps_sidebar_width_to_configured_max() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.set_server_mode(Mode::Terminal);
        app.sidebar_max_width = 120;
        app.sidebar_width = 999;

        compute_view(&mut app, Rect::new(0, 0, 160, 20));

        assert_eq!(app.view.sidebar_rect.width, 120);
    }

    #[test]
    fn compute_view_clamps_sidebar_width_to_configured_min() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.set_server_mode(Mode::Terminal);
        app.sidebar_min_width = 22;
        app.sidebar_width = 5;

        compute_view(&mut app, Rect::new(0, 0, 100, 20));

        assert_eq!(app.view.sidebar_rect.width, 22);
    }

    #[test]
    fn hidden_collapsed_sidebar_uses_full_width_terminal_area() {
        let mut app = crate::app::state::AppState::test_new();
        app.sidebar_collapsed = true;
        app.sidebar_collapsed_mode = crate::config::SidebarCollapsedModeConfig::Hidden;
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.set_server_mode(Mode::Terminal);

        compute_view(&mut app, Rect::new(0, 0, 80, 20));

        // Status bar is full-width on row 0; sidebar/tabs/terminal sit below.
        assert_eq!(app.view.status_bar_rect, Rect::new(0, 0, 80, 1));
        assert_eq!(app.view.sidebar_rect, Rect::new(0, 1, 0, 19));
        assert_eq!(app.view.tab_bar_rect, Rect::new(0, 1, 79, 1));
        assert_eq!(app.view.terminal_area, Rect::new(0, 2, 79, 18));
        assert!(app.view.workspace_card_areas.is_empty());

        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(&app, frame)).unwrap();
    }

    #[test]
    fn desktop_status_bar_spans_full_width_above_sidebar() {
        // AC1 provenance is enforced by the merge-parent check; AC2 covers its geometry.
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.set_server_mode(Mode::Terminal);
        app.sidebar_width = 26;

        compute_view(&mut app, Rect::new(0, 0, 100, 24));

        assert_eq!(app.view.status_bar_rect, Rect::new(0, 0, 100, 1));
        assert_eq!(app.view.sidebar_rect.y, 1);
        assert_eq!(app.view.sidebar_rect.height, 23);
        assert!(app.view.sidebar_rect.x == 0);
        assert_eq!(app.view.sidebar_rect.width, 26);
        // Terminal/tab chrome never occupy the status-bar row.
        assert!(app.view.terminal_area.y >= 1);
        assert!(app.view.tab_bar_rect.y >= 1 || app.view.tab_bar_rect == Rect::default());
    }

    #[test]
    fn disabled_status_bar_returns_its_row_to_the_body() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.set_server_mode(Mode::Terminal);
        app.sidebar_width = 26;
        app.status_bar_enabled = false;

        compute_view(&mut app, Rect::new(0, 0, 100, 24));

        assert_eq!(app.view.status_bar_rect, Rect::default());
        assert_eq!(app.view.sidebar_rect.y, 0);
        assert_eq!(app.view.sidebar_rect.height, 24);
    }

    #[test]
    fn status_view_computation_keeps_the_scheduled_focused_cwd_projection() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        let projected = PathBuf::from("/scheduled/focused");
        app.status_focused_cwd = Some(projected.clone());

        compute_view(&mut app, Rect::new(0, 0, 100, 24));

        assert_eq!(app.status_focused_cwd, Some(projected));
    }

    #[test]
    fn desktop_status_bar_renders_only_branch_device_and_metrics_segments() {
        let mut app = crate::app::state::AppState::test_new();
        app.nerd_font = false;
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.set_server_mode(Mode::Terminal);
        app.sidebar_width = 26;

        app.status_focused_cwd = Some(PathBuf::from("/repo"));
        app.status_git_cwd = app.status_focused_cwd.clone();
        app.status_git_branch = Some("feature/native-status".into());

        // Wide enough that every contracted field fits.
        let width = 220u16;
        compute_view(&mut app, Rect::new(0, 0, width, 24));
        assert_eq!(app.view.status_bar_rect, Rect::new(0, 0, width, 1));

        let backend = TestBackend::new(width, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(&app, frame)).unwrap();
        let row0 = buffer_row_text(
            terminal.backend().buffer(),
            app.view.status_bar_rect,
            app.view.status_bar_rect.y,
        );
        // AC4: fixture metrics provide deterministic segment-parity evidence.
        assert!(
            row0.contains("testhost"),
            "status bar missing hostname: {row0:?}"
        );
        // The branch is gone from the row by design; the device and the
        // resource columns are what the contract now guarantees.
        assert!(!row0.contains("feature/native-stat"), "{row0:?}");
        assert!(
            row0.contains("CPU \u{2581}") && row0.contains("MEM \u{2584}"),
            "status bar missing resource metrics: {row0:?}"
        );
        assert!(!row0.contains("/repo"), "{row0:?}");
        assert!(!row0.contains("Herdr v"), "{row0:?}");
        assert!(!row0.contains("testuser"), "{row0:?}");
        for hidden_identity in ["session:", "workspace:", "tab:", "pane:"] {
            assert!(!row0.contains(hidden_identity), "{row0:?}");
        }
        assert!(!row0.contains("↓"), "{row0:?}");
        assert!(!row0.contains("↑"), "{row0:?}");
        let first_content = row0
            .char_indices()
            .find_map(|(index, ch)| (!ch.is_whitespace()).then_some(index))
            .expect("status row content");
        assert!(
            first_content > 0,
            "status row is not right-aligned: {row0:?}"
        );
    }

    #[test]
    fn status_bar_keeps_memory_and_cpu_legible_at_120_columns() {
        // AC3/AC7: the ordinary-width renderer retains both required metric segments.
        let mut app = crate::app::state::AppState::test_new();
        app.nerd_font = false;
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.selected = 0;
        app.set_server_mode(Mode::Terminal);

        let width = 120;
        compute_view(&mut app, Rect::new(0, 0, width, 24));
        let backend = TestBackend::new(width, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(&app, frame)).unwrap();
        let row0 = buffer_row_text(
            terminal.backend().buffer(),
            app.view.status_bar_rect,
            app.view.status_bar_rect.y,
        );
        assert!(row0.contains("MEM \u{2584}"), "{row0:?}");
        assert!(row0.contains("CPU \u{2581}"), "{row0:?}");
        assert!(!row0.contains("Herdr v"), "{row0:?}");
    }

    #[test]
    fn status_bar_visual_evidence_covers_wide_and_120_column_layouts() {
        let mut app = crate::app::state::AppState::test_new();
        app.nerd_font = false;
        app.workspaces = vec![
            Workspace::test_new("focused"),
            Workspace::test_new("queued"),
        ];
        app.active = Some(0);
        app.selected = 0;
        app.set_server_mode(Mode::Terminal);
        app.sidebar_width = 26;

        let mut rendered = Vec::new();
        for (width, height) in [(220, 8), (120, 12)] {
            compute_view(&mut app, Rect::new(0, 0, width, height));
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal.draw(|frame| render(&app, frame)).unwrap();
            let row0 = buffer_row_text(
                terminal.backend().buffer(),
                app.view.status_bar_rect,
                app.view.status_bar_rect.y,
            );
            assert!(row0.contains("MEM \u{2584}"), "{row0:?}");
            assert!(row0.contains("CPU \u{2581}"), "{row0:?}");
            assert!(!row0.contains("Herdr v"), "{row0:?}");
            rendered.push((
                "sampled metrics",
                width,
                height,
                terminal.backend().buffer().clone(),
            ));
        }

        app.status_metrics = None;
        let width = 120;
        let height = 4;
        compute_view(&mut app, Rect::new(0, 0, width, height));
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(&app, frame)).unwrap();
        let row0 = buffer_row_text(
            terminal.backend().buffer(),
            app.view.status_bar_rect,
            app.view.status_bar_rect.y,
        );
        assert!(row0.contains("MEM -"), "{row0:?}");
        assert!(row0.contains("CPU -"), "{row0:?}");
        assert!(!row0.contains("Herdr v"), "{row0:?}");
        rendered.push((
            "unavailable fallback",
            width,
            height,
            terminal.backend().buffer().clone(),
        ));

        let Ok(path) = std::env::var("HERDR_STATUS_EVIDENCE_HTML") else {
            return;
        };
        let mut html = String::from(
            "<!doctype html><meta charset=\"utf-8\"><title>Herdr native status row</title>\
             <style>body{margin:0;padding:24px;background:#11111b;color:#cdd6f4;\
             font-family:\"JetBrainsMono Nerd Font Mono\",monospace}h1{font-size:20px}\
             h2{font-size:14px;color:#a6adc8;margin:24px 0 8px}.terminal{display:grid;\
             width:max-content;font-size:12px;line-height:16px;box-shadow:0 0 0 1px #45475a}\
             .cell{width:1ch;height:16px;white-space:pre;overflow:visible}</style>\
             <h1>Native full-width desktop status row</h1>",
        );
        for (label, width, height, buffer) in rendered {
            html.push_str(&format!(
                "<h2>{width} columns × {height} rows — {label}</h2>\
                 <div class=\"terminal\" style=\"grid-template-columns:repeat({width},1ch)\">"
            ));
            for cell in buffer.content() {
                let symbol = cell
                    .symbol()
                    .replace('&', "&amp;")
                    .replace('<', "&lt;")
                    .replace('>', "&gt;");
                html.push_str(&format!(
                    "<span class=\"cell\" style=\"color:{};background:{}\">{symbol}</span>",
                    color_css(cell.fg, "#cdd6f4"),
                    color_css(cell.bg, "#11111b"),
                ));
            }
            html.push_str("</div>");
        }
        std::fs::write(path, html).expect("write status-row visual evidence");
    }

    #[test]
    fn collapsed_sidebar_status_geometry_keeps_active_workspace_foreground_emphasis() {
        // AC2/AC7: collapsed desktop content remains aligned below status row
        // 0 and uses foreground emphasis without a selection fill.
        let mut app = crate::app::state::AppState::test_new();
        app.sidebar_collapsed = true;
        app.workspaces = vec![Workspace::test_new("one"), Workspace::test_new("two")];
        app.active = Some(1);
        app.selected = 0;
        app.set_server_mode(Mode::Terminal);

        compute_view(&mut app, Rect::new(0, 0, 80, 20));

        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(&app, frame)).unwrap();
        let buffer = terminal.backend().buffer();

        let (ws_area, _, _) = collapsed_sidebar_sections(app.view.sidebar_rect);
        let active_row_index = sidebar::sidebar_rows(&app)
            .iter()
            .position(|row| matches!(row, sidebar::SidebarRow::Workspace { ws_idx: 1, .. }))
            .expect("active workspace row");
        let active_row = ws_area.y + active_row_index as u16;
        let active_style = buffer[(ws_area.x, active_row)].style();

        assert_eq!(active_style.fg, Some(app.palette.text));
        assert!(active_style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(active_style.bg, Some(app.palette.sidebar_background()));
    }

    #[test]
    fn expanded_sidebar_workspace_rows_omit_redundant_branch_line() {
        let mut app = crate::app::state::AppState::test_new();
        app.nerd_font = false;
        let mut ws = Workspace::test_new("one");
        let repo = temp_git_repo("main");
        ws.identity_cwd = repo.clone();
        let root_pane = ws.tabs[0].root_pane;
        ws.refresh_git_ahead_behind();

        app.workspaces = vec![ws];
        app.ensure_test_terminals();
        let root_terminal_id = app.workspaces[0].tabs[0].panes[&root_pane]
            .attached_terminal_id
            .clone();
        app.terminals.get_mut(&root_terminal_id).unwrap().cwd = repo.clone();
        app.selected = 0;
        app.set_server_mode(Mode::Navigate);

        compute_view(&mut app, Rect::new(0, 0, 80, 20));

        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(&app, frame)).unwrap();
        let buffer = terminal.backend().buffer();

        let card = app.view.workspace_card_areas[0].rect;
        let line1 = buffer_row_text(buffer, card, card.y);
        assert!(line1.starts_with(" ▾ one (0/1)"), "{line1:?}");
        assert!(!line1.contains("1 one"));
        assert_eq!(card.height, 1);
        assert!(!line1.contains("main"));

        std::fs::remove_dir_all(repo).ok();
    }

    #[test]
    fn tab_bar_dims_auto_named_tabs_and_emphasizes_custom_tabs() {
        let mut app = crate::app::state::AppState::test_new();
        let mut ws = Workspace::test_new("test");
        let custom_tab = ws.test_add_tab(Some("logs"));
        ws.switch_tab(custom_tab);

        app.workspaces = vec![ws];
        app.active = Some(0);
        app.selected = 0;
        app.set_server_mode(Mode::Terminal);

        compute_view(&mut app, Rect::new(0, 0, 80, 20));

        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(&app, frame)).unwrap();
        let buffer = terminal.backend().buffer();

        let auto_rect = app.view.tab_hit_areas[0];
        let custom_rect = app.view.tab_hit_areas[1];
        let auto_style = buffer[(auto_rect.x + 1, auto_rect.y)].style();
        let custom_style = buffer[(custom_rect.x + 1, custom_rect.y)].style();

        assert_eq!(auto_style.fg, Some(app.palette.overlay0));
        assert!(auto_style.add_modifier.contains(Modifier::DIM));
        assert_eq!(custom_style.fg, Some(app.palette.panel_bg));
        assert!(custom_style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn tab_bar_uses_surface_dim_when_panel_background_resets() {
        let mut app = crate::app::state::AppState::test_new();
        let mut ws = Workspace::test_new("test");
        let custom_tab = ws.test_add_tab(Some("logs"));
        ws.switch_tab(custom_tab);

        app.palette.panel_bg = Color::Reset;
        app.workspaces = vec![ws];
        app.active = Some(0);
        app.selected = 0;
        app.set_server_mode(Mode::Terminal);

        compute_view(&mut app, Rect::new(0, 0, 80, 20));

        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(&app, frame)).unwrap();
        let buffer = terminal.backend().buffer();

        let custom_rect = app.view.tab_hit_areas[1];
        let custom_style = buffer[(custom_rect.x + 1, custom_rect.y)].style();

        assert_eq!(custom_style.bg, Some(app.palette.accent));
        assert_eq!(custom_style.fg, Some(app.palette.surface_dim));
        assert!(custom_style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn new_tab_button_tracks_rightmost_tab_when_tabs_fit() {
        let mut app = crate::app::state::AppState::test_new();
        let mut ws = Workspace::test_new("test");
        ws.test_add_tab(Some("logs"));

        app.workspaces = vec![ws];
        app.active = Some(0);
        app.selected = 0;
        app.set_server_mode(Mode::Terminal);

        compute_view(&mut app, Rect::new(0, 0, 80, 20));

        let last_visible = app
            .view
            .tab_hit_areas
            .iter()
            .rev()
            .find(|rect| rect.width > 0)
            .copied()
            .expect("last visible tab");

        assert_eq!(
            app.view.new_tab_hit_area.x,
            last_visible.x + last_visible.width
        );
    }

    #[test]
    fn tab_bar_shows_scroll_controls_when_tabs_overflow() {
        let mut app = crate::app::state::AppState::test_new();
        let mut ws = Workspace::test_new("test");
        for name in ["alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta"] {
            ws.test_add_tab(Some(name));
        }

        app.workspaces = vec![ws];
        app.active = Some(0);
        app.selected = 0;
        app.set_server_mode(Mode::Terminal);
        app.tab_scroll_follow_active = false;
        app.tab_scroll = 2;

        compute_view(&mut app, Rect::new(0, 0, 65, 20));

        assert!(app.view.tab_scroll_left_hit_area.width > 0);
        assert!(app.view.tab_scroll_right_hit_area.width > 0);
        assert_eq!(app.view.tab_hit_areas[0].width, 0);
        assert_eq!(app.view.tab_hit_areas[1].width, 0);
        assert!(app.view.tab_hit_areas[2].width > 0);
        assert!(app.view.new_tab_hit_area.width > 0);

        let last_visible = app
            .view
            .tab_hit_areas
            .iter()
            .rev()
            .find(|rect| rect.width > 0)
            .copied()
            .expect("last visible tab");

        assert_eq!(
            app.view.tab_scroll_right_hit_area.x,
            last_visible.x + last_visible.width
        );
        assert_eq!(
            app.view.new_tab_hit_area.x,
            app.view.tab_scroll_right_hit_area.x + app.view.tab_scroll_right_hit_area.width
        );
    }

    #[test]
    fn tab_bar_clamps_manual_scroll_at_last_visible_tab() {
        let mut app = crate::app::state::AppState::test_new();
        let mut ws = Workspace::test_new("test");
        for name in [
            "one", "two", "three", "four", "five", "six", "seven", "eight",
        ] {
            ws.test_add_tab(Some(name));
        }

        app.workspaces = vec![ws];
        app.active = Some(0);
        app.selected = 0;
        app.set_server_mode(Mode::Terminal);
        app.tab_scroll_follow_active = false;
        app.tab_scroll = usize::MAX;

        compute_view(&mut app, Rect::new(0, 0, 65, 20));

        let last_idx = app.workspaces[0].tabs.len() - 1;
        assert!(app.view.tab_hit_areas[last_idx].width > 0);
        let clamped_scroll = app.tab_scroll;

        app.scroll_tabs_right();

        assert_eq!(app.tab_scroll, clamped_scroll);
        assert!(app.view.tab_hit_areas[last_idx].width > 0);
    }

    #[test]
    fn pane_scrollbar_rect_uses_reserved_rightmost_column() {
        let info = PaneInfo {
            id: crate::layout::PaneId::from_raw(1),
            rect: Rect::new(0, 0, 12, 8),
            inner_rect: Rect::new(1, 1, 9, 6),
            scrollbar_rect: Some(Rect::new(10, 1, 1, 6)),
            borders: ratatui::widgets::Borders::ALL,
            is_focused: true,
        };

        assert_eq!(pane_scrollbar_rect(&info), Some(Rect::new(10, 1, 1, 6)));
    }

    #[tokio::test]
    async fn compute_view_reserves_terminal_column_when_pane_scrollbar_is_visible() {
        let mut app = crate::app::state::AppState::test_new();
        let mut ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        ws.insert_test_runtime(
            pane_id,
            crate::terminal::TerminalRuntime::test_with_scrollback_bytes(
                12,
                4,
                4096,
                b"000000000000\r\n111111111111\r\n222222222222\r\n333333333333\r\n444444444444\r\n",
            ),
        );

        app.workspaces = vec![ws];
        app.active = Some(0);
        app.selected = 0;

        compute_view(&mut app, Rect::new(0, 0, 40, 12));

        let info = app.view.pane_infos.first().expect("pane info");
        assert_eq!(info.inner_rect.width + 1, app.view.terminal_area.width);
        assert_eq!(
            info.scrollbar_rect,
            Some(Rect::new(
                info.inner_rect.x + info.inner_rect.width,
                info.inner_rect.y,
                1,
                info.inner_rect.height,
            ))
        );
    }

    #[test]
    fn scrollbar_stays_hidden_without_scrollback() {
        let metrics = crate::pane::ScrollMetrics {
            offset_from_bottom: 0,
            max_offset_from_bottom: 0,
            viewport_rows: 5,
        };

        assert!(!should_show_scrollbar(metrics));
    }

    #[test]
    fn scrollbar_shows_with_scrollback() {
        let metrics = crate::pane::ScrollMetrics {
            offset_from_bottom: 0,
            max_offset_from_bottom: 20,
            viewport_rows: 5,
        };

        assert!(should_show_scrollbar(metrics));
    }

    #[test]
    fn scrollbar_thumb_reaches_bottom_when_scrolled_to_bottom() {
        let metrics = crate::pane::ScrollMetrics {
            offset_from_bottom: 0,
            max_offset_from_bottom: 20,
            viewport_rows: 5,
        };
        let track = Rect::new(9, 4, 1, 5);

        let thumb = scrollbar_thumb(metrics, track).expect("thumb");
        assert_eq!(thumb.top + thumb.len, track.y + track.height);
    }

    #[test]
    fn scrollbar_offset_mapping_hits_top_middle_and_bottom() {
        let metrics = crate::pane::ScrollMetrics {
            offset_from_bottom: 0,
            max_offset_from_bottom: 20,
            viewport_rows: 5,
        };
        let track = Rect::new(9, 4, 1, 5);

        assert_eq!(scrollbar_offset_from_row(metrics, track, 4), 20);
        assert_eq!(scrollbar_offset_from_row(metrics, track, 6), 10);
        assert_eq!(scrollbar_offset_from_row(metrics, track, 8), 0);
    }

    #[test]
    fn dragging_from_current_thumb_row_preserves_offset() {
        let metrics = crate::pane::ScrollMetrics {
            offset_from_bottom: 7,
            max_offset_from_bottom: 20,
            viewport_rows: 5,
        };
        let track = Rect::new(9, 4, 1, 8);
        let thumb = scrollbar_thumb(metrics, track).expect("thumb");
        let row = thumb.top + thumb.len / 2;
        let grab = scrollbar_thumb_grab_offset(metrics, track, row).expect("grab");

        assert_eq!(scrollbar_offset_from_drag_row(metrics, track, row, grab), 7);
    }

    fn buffer_row_text(buffer: &ratatui::buffer::Buffer, area: Rect, row: u16) -> String {
        (area.x..area.x + area.width)
            .map(|x| buffer[(x, row)].symbol())
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    fn color_css(color: Color, reset: &'static str) -> String {
        match color {
            Color::Reset => reset.into(),
            Color::Black => "#45475a".into(),
            Color::Red => "#f38ba8".into(),
            Color::Green => "#a6e3a1".into(),
            Color::Yellow => "#f9e2af".into(),
            Color::Blue => "#89b4fa".into(),
            Color::Magenta => "#cba6f7".into(),
            Color::Cyan => "#94e2d5".into(),
            Color::Gray => "#bac2de".into(),
            Color::DarkGray => "#585b70".into(),
            Color::LightRed => "#eba0ac".into(),
            Color::LightGreen => "#a6e3a1".into(),
            Color::LightYellow => "#f9e2af".into(),
            Color::LightBlue => "#89dceb".into(),
            Color::LightMagenta => "#f5c2e7".into(),
            Color::LightCyan => "#89dceb".into(),
            Color::White => "#cdd6f4".into(),
            Color::Rgb(red, green, blue) => format!("#{red:02x}{green:02x}{blue:02x}"),
            Color::Indexed(index) => format!("color-mix(in srgb, #cdd6f4 {}%, #11111b)", index),
        }
    }

    fn temp_git_repo(branch: &str) -> std::path::PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("unix time")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("herdr-ui-test-{unique}"));
        std::fs::create_dir_all(root.join(".git")).expect("create .git dir");
        std::fs::write(
            root.join(".git/HEAD"),
            format!("ref: refs/heads/{branch}\n"),
        )
        .expect("write HEAD");
        root
    }

    #[test]
    fn prefix_mode_renders_prefix_indicator() {
        let mut app = crate::app::state::AppState::test_new();
        app.set_server_mode(Mode::Prefix);
        app.view.terminal_area = ratatui::layout::Rect::new(0, 0, 60, 4);
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(60, 4))
            .expect("test terminal");

        terminal
            .draw(|frame| render_prefix_overlay(&app, frame, app.view.terminal_area))
            .expect("draw prefix overlay");

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("PREFIX"));
    }

    #[test]
    fn keybind_help_shows_unset_for_optional_actions() {
        let app = crate::app::state::AppState::test_new();
        let groups = keybind_help_groups(&app);

        let workspace_tab = groups
            .iter()
            .find(|(name, _)| *name == "workspaces / tabs")
            .expect("workspace tab group")
            .1
            .clone();
        let panes = groups
            .iter()
            .find(|(name, _)| *name == "panes")
            .expect("panes group")
            .1
            .clone();

        assert!(workspace_tab
            .iter()
            .any(|(key, label)| key == "unset" && label.as_ref() == "previous workspace"));
        assert!(workspace_tab
            .iter()
            .any(|(key, label)| key == "unset" && label.as_ref() == "next workspace"));
        assert!(workspace_tab
            .iter()
            .any(|(key, label)| key == "unset" && label.as_ref() == "previous agent"));
        assert!(workspace_tab
            .iter()
            .any(|(key, label)| key == "unset" && label.as_ref() == "next agent"));
        assert!(workspace_tab
            .iter()
            .any(|(key, label)| key == "unset" && label.as_ref() == "focus agent 1-9"));
        assert!(workspace_tab
            .iter()
            .any(|(key, label)| key == "unset" && label.as_ref() == "switch workspace 1-9"));
        assert!(panes
            .iter()
            .any(|(key, label)| key == "prefix+h" && label.as_ref() == "focus pane left"));
        assert!(panes
            .iter()
            .any(|(key, label)| key == "prefix+j" && label.as_ref() == "focus pane down"));
        assert!(panes
            .iter()
            .any(|(key, label)| key == "prefix+k" && label.as_ref() == "focus pane up"));
        assert!(panes
            .iter()
            .any(|(key, label)| key == "prefix+l" && label.as_ref() == "focus pane right"));
        // Both routes are listed: the dock is reachable by prefix and by shorthand.
        assert!(panes
            .iter()
            .any(|(key, label)| key == "prefix+shift+e / ctrl+alt+d"
                && label.as_ref() == "toggle dock"));
    }

    #[test]
    fn keybind_help_shows_custom_command_descriptions() {
        let mut app = crate::app::state::AppState::test_new();
        app.keybinds.custom_commands = vec![
            crate::config::CustomCommandKeybind {
                bindings: crate::config::ActionKeybinds::prefix("alt+g"),
                label: "prefix+alt+g".to_string(),
                command: "lazygit".to_string(),
                action: crate::config::CustomCommandAction::Pane,
                description: Some("open lazygit".to_string()),
                width: None,
                height: None,
            },
            crate::config::CustomCommandKeybind {
                bindings: crate::config::ActionKeybinds::prefix("alt+h"),
                label: "prefix+alt+h".to_string(),
                command: "echo hello".to_string(),
                action: crate::config::CustomCommandAction::Shell,
                description: None,
                width: None,
                height: None,
            },
        ];

        let groups = keybind_help_groups(&app);
        let custom = groups
            .iter()
            .find(|(name, _)| *name == "custom")
            .expect("custom group")
            .1
            .clone();
        assert!(custom
            .iter()
            .any(|(key, label)| key == "prefix+alt+g" && label.as_ref() == "open lazygit"));
        assert!(custom
            .iter()
            .any(|(key, label)| key == "prefix+alt+h" && label.as_ref() == "custom command"));

        let rendered_help = keybind_help_lines(&app)
            .into_iter()
            .flat_map(|(_, line)| line.spans)
            .map(|span| span.content.into_owned())
            .collect::<Vec<_>>()
            .join("");
        assert!(rendered_help.contains("open lazygit"));
        assert!(rendered_help.contains("custom command"));
    }

    #[test]
    fn keybind_help_compacts_multiple_indexed_ranges() {
        let config: crate::config::Config = toml::from_str(
            r#"
[keys]
switch_tab = ["prefix+1..9", "alt+1..9"]
switch_workspace = "ctrl+1..9"
"#,
        )
        .expect("config parses");

        let mut app = crate::app::state::AppState::test_new();
        app.keybinds = config.keybinds();

        let workspace_tab = keybind_help_groups(&app)
            .into_iter()
            .find(|(name, _)| *name == "workspaces / tabs")
            .expect("workspace tab group")
            .1;

        let switch_tab_key = workspace_tab
            .iter()
            .find(|(_, label)| label.as_ref() == "switch tab 1-9")
            .map(|(key, _)| key.as_str())
            .expect("switch tab help entry");
        let switch_workspace_key = workspace_tab
            .iter()
            .find(|(_, label)| label.as_ref() == "switch workspace 1-9")
            .map(|(key, _)| key.as_str())
            .expect("switch workspace help entry");

        assert_eq!(switch_tab_key, "prefix+1..9 / alt+1..9");
        assert_eq!(switch_workspace_key, "ctrl+1..9");
    }
}
