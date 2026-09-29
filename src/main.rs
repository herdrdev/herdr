use std::io;

use crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
    EnableFocusChange, EnableMouseCapture,
};
#[cfg(not(windows))]
use crossterm::event::{PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags};
use crossterm::execute;

pub(crate) const HERDR_ENV_VAR: &str = "HERDR_ENV";
pub(crate) const HERDR_ENV_VALUE: &str = "1";
const NESTED_HERDR_MESSAGES: [&str; 6] = [
    "inception detected. we need to go deeper... said no one ever.",
    "recursion is a pathway to many abilities some consider to be... unnatural.",
    "you were so preoccupied with whether you could, you didn't stop to think if you should. — dr. malcolm",
    "recursive herdring is disabled. somewhere, a call stack breathes a sigh of relief.",
    "recursive descent denied. there is, in fact, such a thing as too much herdr.",
    "recursion detected. base case not found. aborting.",
];

#[cfg(not(windows))]
fn push_keyboard_enhancement_flags() -> io::Result<()> {
    execute!(
        io::stdout(),
        PushKeyboardEnhancementFlags(crate::input::ime_compatible_keyboard_enhancement_flags())
    )
}

#[cfg(windows)]
fn push_keyboard_enhancement_flags() -> io::Result<()> {
    Ok(())
}

#[cfg(not(windows))]
fn pop_keyboard_enhancement_flags() -> io::Result<()> {
    execute!(io::stdout(), PopKeyboardEnhancementFlags)
}

#[cfg(windows)]
fn pop_keyboard_enhancement_flags() -> io::Result<()> {
    Ok(())
}

fn set_host_color_scheme_reports(enabled: bool) -> io::Result<()> {
    use std::io::Write;

    let sequence = if enabled {
        crate::terminal_theme::HOST_COLOR_SCHEME_REPORT_ENABLE_SEQUENCE
    } else {
        crate::terminal_theme::HOST_COLOR_SCHEME_REPORT_DISABLE_SEQUENCE
    };
    io::stdout().write_all(sequence.as_bytes())?;
    io::stdout().flush()
}

mod activity_age;
mod agent_resume;
mod agent_runs;
mod agent_state;
mod aloop;
mod api;
mod app;
mod board;
mod build_info;
mod checksum;
mod cli;
mod client;
mod config;
mod connectivity;
mod contract_false_positive;
mod day;
mod detect;
mod events;
mod files;
mod fleet;
mod ghostty;
mod goals;
mod groups;
mod handoff_runtime;
mod hyperspace;
mod input;
mod integration;
mod ipc;
mod kitty_graphics;
mod layout;
mod logging;
mod loop_runs;
mod metadata_tokens;
mod noninteractive_process;
mod notepad;
mod pane;
mod pane_graphics_files;
mod persist;
mod platform;
mod plugin_command;
mod plugin_paths;
mod pomodoro;
mod popup_size;
mod product_announcements;
mod protocol;
mod provider_usage;
mod pty;
mod raw_input;
mod release_notes;
mod remote;
mod render_prof;
mod render_signal;
mod scratchpad;
mod selection;
mod server;
mod session;
mod sound;
mod status_log;
mod symphony;
mod terminal;
mod terminal_effects;
mod terminal_modes;
mod terminal_notify;
mod terminal_theme;
mod ui;
mod update;
mod watchdog;
mod work_context;
mod work_index;
mod work_projection;
mod work_title;
mod workspace;
mod worktree;

fn init_logging() {
    crate::logging::init_file_logging("herdr.log");
}

const DEFAULT_CONFIG: &str = r##"# herdr configuration
# Place this file at ~/.config/herdr/config.toml

# Show first-run notification setup on startup.
# Missing also shows onboarding; set false after you've chosen.
# onboarding = true

[theme]
# Built-in themes: catppuccin, terminal, tokyo-night, dracula, nord,
#                  gruvbox, one-dark, solarized, kanagawa, rose-pine,
#                  vesper
# name = "catppuccin"

# Follow host terminal light/dark appearance and switch Herdr UI themes.
# Existing manual behavior is unchanged unless this is true.
# auto_switch = false
# Override missing or incorrect host reports behind mosh/multiplexers.
# host_appearance = "auto" # auto | light | dark
# Re-query OSC 11 for appearance changes; default 60 seconds (5..3600).
# auto_switch_poll_interval_seconds = 60
# dark_name = "catppuccin"
# light_name = "catppuccin-latte"

# Override individual color tokens on top of the base theme.
# Accepts: hex (#rrggbb), named colors, rgb(r,g,b), or panel_bg = "reset"
# [theme.custom]
# Sidebar background. Unset follows panel_bg, so the sidebar keeps the
# theme's own background; panel_bg = "reset" hands both to the terminal.
# sidebar_bg = "#181825"
# panel_bg = "reset"
# accent = "#f5c2e7"
# red = "#ff6188"
# green = "#a6e3a1"

# Layer appearance-specific overrides on top when auto_switch is enabled.
# [theme.custom.light]
# panel_bg = "#eff1f5"
# text = "#4c4f69"
#
# [theme.custom.dark]
# panel_bg = "#1e1e2e"
# text = "#cdd6f4"

[terminal]
# Executable used for new interactive panes.
# Empty means $SHELL, then /bin/sh.
# default_shell = ""

# Startup mode for new interactive pane shells: "auto", "login", or "non_login".
# "auto" uses login shells on macOS and keeps the current behavior elsewhere.
# shell_mode = "auto"

# CWD policy for new panes, tabs, and workspaces when no explicit --cwd is provided.
# Use "follow" to inherit the source pane/workspace, "home" for $HOME,
# "current" for Herdr's process directory, or a fixed path such as "~/Projects".
# new_cwd = "follow"

[update]
# Update channel used by background version checks and `herdr update`.
# Stable builds default to "stable". Windows preview builds default to "preview"
# so existing preview installs stay there until explicitly switched.
# channel = "stable"

# Check herdr.dev for new Herdr versions in the background.
# version_check = true

# Check herdr.dev for remote agent-detection manifest updates in the background.
# manifest_check = true

[work_index]
# Enable repo-wide GitHub, Linear, agent-pane, and day-link work indexing.
# Day items complete themselves from linked merged PRs and done tickets only
# while this is on.
# enabled = false
# Refresh interval in seconds.
# refresh_interval_seconds = 300
# Linear team key, for example "SCA".
# linear_team = "SCA"
# GitHub repositories to include, for example ["owner/repo"].
# repos = []

[panel]
# Surfaces opened when a new right panel is expanded. Empty shows the chooser.
# Values: terminal, files, diff, pull_request, linear, missive, agents, home,
# editor, shortcuts, context, scratchpad.
# default_surfaces = []

[source_control]
# Merge strategy for the primary PR action: "merge", "squash", or "rebase".
# merge_method = "merge"

[notepad]
# Markdown notes edited at the bottom of the sidebar. Point `dir` at a folder a
# sync tool already carries, or set git_sync to let herdr pull and push it.
# enabled = false
# Notes directory. Empty uses <config dir>/notes. "~" is expanded.
# dir = ""
# Note names offered first, in this order. Others follow alphabetically.
# files = ["todo", "ideas"]
# Sidebar rows the panel occupies, header included (3-24).
# height = 8
# Pull and push the notes directory as a git checkout.
# git_sync = false
# git_sync_interval_seconds = 120

[goals_panel]
# Show goals and work streams from the focused session's .streams.json file.
# enabled = true

[pomodoro]
# Break reminder. The overlay it raises has to be answered in writing.
# enabled = false
# work_minutes = 25
# short_break_minutes = 5
# long_break_minutes = 20
# Work intervals between long breaks (4 x 25min is roughly two hours).
# long_break_every = 4
# Characters required to dismiss a due reminder.
# min_confirm_chars = 3
# Note in the notepad directory confirmations are appended to. Empty disables it.
# log_file = "pomodoro-log.md"

[keys]
# Prefix key to enter prefix mode (default: "ctrl+b")
# Examples: "ctrl+b", "f12", "esc", "-"
# Action bindings use explicit syntax: "prefix+n" requires the prefix;
# "ctrl+alt+n" is a direct terminal-mode shortcut.
# Accepted key syntax: plain keys, ctrl/shift/alt/cmd/super modifiers, and special keys like enter/tab/esc/left/right/up/down.
# Named punctuation such as minus, comma, ampersand, plus, and backtick is also accepted.
# Most reliable direct bindings are ctrl+letter, function keys, and explicit modified chords.
# alt+..., cmd/super, and punctuation-with-modifiers may depend on your terminal/tmux setup.
# prefix = "ctrl+b"

# Prefix-mode actions
# help = "prefix+?"
# settings = "prefix+s"
# command_palette = ["ctrl+alt+p", "super+p"]
# home = "ctrl+alt+h"
# detach = "prefix+q"
# reload_config = "prefix+shift+r"
# open_notification_target = "prefix+o"
# open_work_link = "prefix+u"
# copy_work_link = "prefix+shift+u"
# workspace_picker = "prefix+w"
# goto = "prefix+g"
# new_workspace = "prefix+shift+n"
# new_worktree = "prefix+shift+g"
# open_worktree = ""    # optional, unset by default
# remove_worktree = ""  # optional, unset by default; opens confirmation
# rename_workspace = "prefix+shift+w"
# close_workspace = "prefix+shift+d"
# previous_workspace = "" # optional, unset by default
# next_workspace = ""     # optional, unset by default
# previous_agent = ""     # optional, unset by default
# next_agent = ""         # optional, unset by default
# focus_agent = ""        # optional indexed binding, e.g. "prefix+alt+1..9"
# remote_image_paste = "ctrl+v" # only active in herdr --remote; empty disables raw-key image paste
# new_tab = "prefix+c"
# rename_tab = "prefix+shift+t"
# toggle_tab_prio = "prefix+shift+f"
# toggle_prio_panel = "" # optional, unset by default
# previous_tab = "prefix+ctrl+p" # previous tab in the active workspace
# next_tab = "prefix+ctrl+n"     # next tab in the active workspace
# previous_window = "prefix+p"   # previous tab across all workspaces
# next_window = "prefix+n"       # next tab across all workspaces
# next_blocked_window = "prefix+b" # next blocked tab across all workspaces
# switch_tab = "prefix+1..9"
# switch_workspace = ""   # optional indexed binding, e.g. "prefix+shift+1..9"
# close_tab = "prefix+shift+x"
# rename_pane = "prefix+shift+p"
# edit_scrollback = "prefix+e"
# focus_pane_left = "prefix+h"
# focus_pane_down = "prefix+j"
# focus_pane_up = "prefix+k"
# focus_pane_right = "prefix+l"
# cycle_pane_next = "prefix+tab"
# cycle_pane_previous = "prefix+shift+tab"
# last_pane = ""          # optional, unset by default; bind e.g. "prefix+tab" for global back-and-forth
# split_vertical = "prefix+v"
# split_horizontal = "prefix+minus"
# close_pane = "prefix+x"
# zoom = "prefix+z"       # legacy alias: fullscreen
# resize_mode = "prefix+r"
# toggle_sidebar = "prefix+shift+b"
# sidebar_cycle_group_mode = ""
# sidebar_refresh = ""
# toggle_blocked_filter = "prefix+f"
# symphony = "prefix+shift+s"
# runs = "prefix+alt+r"
# work = "prefix+ctrl+w"         # work projection view: PRs / tickets / agents / review
# usage = "prefix+ctrl+y"        # historical Claude Code and Codex usage
# tickets = "prefix+ctrl+t"      # Linear tickets view

# Navigate-mode movement. These local shortcuts win while navigate mode is open.
# They are independent from focus_pane_*. Do not include prefix+, esc, enter, tab, or 1..9 here.
# navigate_workspace_up = "up"
# navigate_workspace_down = "down"
# navigate_pane_left = "h"      # left arrow always focuses the pane to the left
# navigate_pane_down = "j"
# navigate_pane_up = "k"
# navigate_pane_right = "l"     # right arrow always focuses the pane to the right

# Custom commands use the same binding syntax.
# type = "shell" runs detached in the background.
# type = "pane" opens a temporary pane and closes it when the command exits.
# type = "popup" opens a session-modal terminal without changing the tab layout.
# Popup width and height accept terminal cells or percentages such as "80%".
# On Windows, command strings run through cmd.exe /d /c.
# [[keys.command]]
# key = "prefix+alt+g"
# type = "popup"
# command = "lazygit"
# width = "80%"
# height = "80%"

# Legacy indexed shortcut config is still parsed for compatibility.
# Prefer switch_tab, switch_workspace, and focus_agent for new configs.
# [keys.indexed]
# tabs = ""       # e.g. "ctrl" makes ctrl+1..9 switch tabs directly
# workspaces = "" # e.g. "ctrl+shift" makes ctrl+shift+1..9 switch workspaces directly
# agents = ""     # e.g. "alt" makes alt+1..9 focus agent rows directly

# Size of the virtual terminal used when no client is attached.
# Attached clients always use their own terminal size.
[server]
# headless_cols = 120
# headless_rows = 40

# [worktrees]
# directory = "~/.herdr/worktrees"

[ui]
# Sidebar width (auto-scaled based on workspace names, this sets the default)
# sidebar_width = 26

# Minimum sidebar width when expanded (columns)
# sidebar_min_width = 18

# Maximum sidebar width when expanded (columns)
# sidebar_max_width = 36

# Start with the sidebar collapsed. Changes take effect on the next launch.
# sidebar_start_collapsed = false

# Idle star-field animation at the bottom of the sidebar. It also has a pause
# button; this switch removes the panel and gives its rows back to the list.
# sidebar_animation = true

# Scope for next/previous and next-blocked cycles. Choices: "this-machine" or
# "this-machine-and-fleet". The sidebar footer cycle control changes this live.
# window_cycle_mode = "this-machine"

# Exclude collapsed spaces and sidebar sections from both cycles.
# skip_collapsed_cycle = false

# Collapsed sidebar presentation: "compact" keeps the narrow status rail, "hidden" uses zero width.
# sidebar_collapsed_mode = "compact"

# Terminal width at or below which Herdr uses the mobile single-column layout.
# Increase this for foldables, tablets, or wide phone terminals.
# mobile_width_threshold = 64

# Capture mouse input for Herdr's mouse UI.
# Set false to let the terminal handle normal clicks, such as Cmd-clicking URLs.
# Pane apps like lazygit and btop can still receive mouse when they request it.
# mouse_capture = true

# Automatically copy text selected with the mouse.
# Set false to retain drag or double-click word selection until Ctrl+C,
# or Cmd+C when the host forwards it, copies and clears it.
# copy_on_select = true

# Host cursor policy: "auto", "native", or "drawn".
# "auto" draws Herdr's own cursor on native Windows builds and WSL to avoid ConPTY cursor flicker, and uses the native terminal cursor elsewhere.
# "native" always uses the outer terminal cursor. "drawn" always draws Herdr's cursor as terminal cell content.
# host_cursor = "auto"

# Optional modifier that forwards right-click hold/drag gestures to pane apps instead of opening Herdr's pane menu.
# Empty/off disables this. Shift is intentionally unsupported because terminals commonly reserve Shift+mouse.
# right_click_passthrough_modifier = ""

# Force a full redraw when the outer terminal regains focus.
# Set false to reduce visible flashing when switching back to Herdr.
# Trade-off: rare host terminal surface corruption may persist until the next full redraw.
# redraw_on_focus_gained = true

# Pane scrollback lines to scroll per mouse wheel notch.
# mouse_scroll_lines = 3

# Ask for confirmation before closing a workspace, and before deleting a settled
# thread from the sidebar menu or the settings archive.
# confirm_close = true

# Group workspaces that check out the same repository under one project header
# even when their checkout roots differ.
# combine_repos_across_hosts = false

# Hide whitespace-only changes in the diff surface.
# hide_whitespace_in_diff = false

# Workspace preselected in the Home composer: "current_checkout" or "new_worktree".
# new_thread_workspace = "current_checkout"

# Directory the add-project picker starts in. Empty keeps the last used one.
# add_project_start_dir = ""

# Ask for a tab name before creating a new tab.
# Set false to create tabs immediately with generated names.
# prompt_new_tab_name = true

# Ask for a workspace name before interactive creation.
# prompt_new_workspace_name = false

# Draw borders around split panes.
# pane_borders = true

# Draw interactive scrollbars beside terminal panes.
# Set false to reclaim the scrollbar column and keep it out of terminal-native selections.
# pane_scrollbars = true

# Keep split panes visually separated instead of sharing divider borders.
# pane_gaps = true

# Show detected/reported agent labels in split pane borders when no manual pane name is set.
# show_agent_labels_on_pane_borders = false

# Hide the tab row when a workspace has exactly one tab.
# New tabs can still be created with the configured keybinding.
# hide_tab_bar_when_single_tab = false

# Show the pull button in the top-right action row.
# Off by default. It is the only entry point to the git menu, so turn it on if
# you want that menu.
# show_pull_button = false

# Open the dock automatically when the focused pane carries a pull request,
# ticket, or conversation.
# Off by default: the status row names each link after the pane title, and a
# click on that name opens it in the dock.
# open_dock_on_work_link = false

# Show the split-below and split-right buttons in the top-right action row.
# Off by default: both splits have keybindings, and the buttons crowd the tab titles.
# show_pane_toggle_buttons = false

# Open the home view on launch. Esc dismisses it. Off by default: the launch
# screen hides the panes you asked for.
# show_home_on_start = false

# Start the status row expanded: usage percentages and reset times beside every
# column, instead of the columns alone. Toggle at runtime with the status-bar
# button or keys.toggle_status_detail.
# status_bar_expanded = false

# Show `fleet:` in the status row with a dot for each other device that has
# blocked agents or stopped reporting. Click `fleet:` to hide or show the dots.
# fleet_status = true

# Desktop tab row placement: "hidden", "top" or "bottom". Hidden is the default
# because the sidebar already lists every tab in every space.
# tab_bar_position = "hidden"

# Agent panel ordering: "spaces" (grouped by space) or "priority" (attention queue).
# "workspaces" is accepted as an alias for "spaces".
# agent_panel_sort = "spaces"

# Agent status indicators: "dots" preserves the compact color marks; "symbols" uses
# distinct static glyphs for blocked, working, done, idle, and unknown states.
# status_indicators = "dots"

# Sidebar presentation. "current" keeps the configured legacy grouping;
# "sections" shows Pinned, Active, Snoozed, and Settled thread shelves.
# [ui.sidebar]
# layout = "current"
# Header decoration for the sections layout. "plain" keeps the controls and
# their positions but removes the theme-aware half-block night sky.
# header = "sky"

# Optional sections-layout areas. All are shown by default; the current layout
# ignores these settings.
# [ui.sidebar.areas]
# sky_header = true
# view_bar = true
# unassigned = true
# runs = true
# aloops = true
# symphony = true
# notes = true
# pomodoro = true
# hosts = true

# Expanded agent rows. Built-ins are state_icon, state_text, workspace, tab, pane, agent,
# terminal_title, and terminal_title_stripped.
# Custom values reported through pane metadata use a $name token.
# A token occurrence may be styled with { token = "workspace", fg = "#89b4fa", bold = true, dim = false }.
# Omitted style fields preserve the contextual default.
# [ui.sidebar.agents]
# Blank rows between agent entries. Set to 1 to restore the previous spacing.
# row_gap = 0
# rows = [["state_icon", "workspace", "tab"], ["agent"]]
# Optional canonical agent IDs replace the default rows for matching agents.
# [ui.sidebar.agents.rows_by_agent]
# claude = [["state_icon", "workspace", "tab"], ["terminal_title_stripped"], ["agent"]]

# Space rows use a fixed one-line disclosure, title, and direct window count.
# [ui.sidebar.spaces]
# Blank rows between complete space groups. Direct tab/window rows stay packed.
# row_gap = 1
# Legacy row declarations remain accepted for configuration compatibility but are not rendered.
# rows = [["state_icon", "workspace"], ["branch", "git_status"]]

# Accent color for highlights, borders, and navigation UI.
# Accepts: hex (#89b4fa), named colors (cyan, blue, magenta), or rgb(r,g,b)
# accent = "cyan"

# Agent, Space, and usage labels plus file rows use Nerd Font glyphs by default.
# Disable this for terminals without a Nerd Font.
# nerd_font = true

# Override built-in Nerd Font icons by repo name, owner/repo binding, or label.
# [ui.icons.spaces]
# scalablev2 = "◆"

# Legacy per-surface override. "badges" also forces portable file kinds.
[files]
# icons = "nerd"

# Full-width top status row on desktop layouts.
[ui.status_bar]
# enabled = true

# Background notification popup delivery
[ui.toast]
# off = disable pop-up notifications
# herdr = show in-app toasts
# terminal = ask the outer terminal to show a desktop notification
# system = ask the OS notification service directly
# delivery = "off"
# Outer-terminal protocol: auto, osc9, or osc99. auto detects the attaching terminal.
# terminal_backend = "auto"
# delay_seconds = 1

[ui.toast.herdr]
# position = "bottom-right"

[ui.toast.clipboard]
# enabled = true
# position = "bottom-center"

# Play sounds when agents change state in background workspaces
[ui.sound]
# enabled = false
# Optional custom mp3 sound files. Relative paths are resolved from this config file's directory.
# path = "sounds/notification.mp3"   # one mp3 file for all sound notifications
# done_path = "sounds/done.mp3"      # overrides only finished notifications
# request_path = "sounds/request.mp3" # overrides only needs-attention notifications

# Per-agent overrides: default | on | off
# By default, droid is muted.
# [ui.sound.agents]
# droid = "off"

[session]
# Resume supported AI-agent panes into their native conversation sessions after
# a Herdr server restart. Requires official integrations that report session refs.
# resume_agents_on_restore = true
# Move Done panes into the collapsed Settled section after this many minutes.
# hide_done_after_minutes = 30
# Close eligible Done panes after this many minutes. Default: 240 (4 hours).
# reap_done_after_minutes = 240
# Set false to disable automatic pane reaping entirely.
# reap_done_panes = true
# Move inactive or completed work into the Settled sidebar section after this many days.
# settle_after_days = 3
# Settle a pane when the work it is linked to finishes.
# auto_settle_finished = true
# Hold the finished trigger this many minutes: the work has to keep reading as
# finished and the pane has to stay quiet for that long before it settles.
# settle_finished_after_minutes = 10
# Settle a pane that has only gone quiet for settle_after_days.
# auto_settle_inactive = true
# Settle an agent pane after it stays idle without gates, usage limits, active
# sub-agents, or a held shell. Seen panes and stale panes whose screen resolves
# idle use the same quiet clock.
# auto_settle_done = true
# How long an eligible agent pane stays quiet before auto_settle_done settles it.
# settle_done_after_minutes = 30
# Stop resumable agent processes when their pane settles.
# settle_stops_agent = true
# After a resumed agent comes back up idle, submit one prompt so it continues
# the work it was doing. Skipped when the agent resumes blocked or already
# working, and when the pane holds a draft you typed.
# nudge_resumed_agents = true
# The prompt sent by nudge_resumed_agents.
# resume_nudge_message = "continue"
# Nudge an agent when both its status declaration and pane activity have gone quiet.
# auto_nudge_stalled_agents = false
# Mark a quiet, unattended finished report with unreported child work stale
# after this many minutes. Working reports keep the 20-minute budget.
# agent_stale_after_minutes = 5
# Parents waiting on declared sub-agents use a longer quiet budget.
# agent_subagent_stale_after_minutes = 30
# Initial quiet period and maximum sends in one stale-status episode.
# nudge_after_minutes = 5
# max_nudges = 3
# Maximum stalled-agent nudges per pane before human input. Set to 0 for no cap.
# max_nudges_without_human = 5
# The plain-text prompt sent to a stalled agent.
# stall_nudge_message = "Re-verify what you are working on now; do not answer from memory. If you have subagents, poll them and restart any that are stalled. If everything is still progressing, reply with one line: Progressing, plus the count and names of running subagents if any (e.g. Progressing, 2 subagents: build, review). If it is done or something changed, say so and continue."
# Harnesses with the herdr-status skill installed can use this sharper alternative:
# stall_nudge_message = "/status"

[remote]
# Whether herdr manages the ssh config used for `herdr --remote`.
# When true (default), herdr runs remote ssh through a generated config that
# includes your ~/.ssh/config first and adds ServerAliveInterval/
# ServerAliveCountMax as fallbacks (so any keepalive values you set yourself
# still win) to survive idle network/NAT timeouts. Herdr also uses a private
# per-attach OpenSSH control socket to reuse the first authenticated connection.
# Set false to run plain ssh against your ssh config unchanged — this does not
# force keepalive or multiplexing off, it only stops herdr from adding its own.
# manage_ssh_config = true

# Read-only host inventory for `herdr fleet status`.
# [remote.fleet]
# Name used for this server in cross-host agent references. Defaults to the local fleet host name, then the system hostname.
# self_name = "laptop"
# refresh_interval_ms = 5000
# timeout_ms = 5000
# heartbeat_stale_ms = 1800000
# symphony_host = "server" # optional fleet host running Temporal; default is local
# [[remote.fleet.hosts]]
# name = "local"
# local = true
# socket = "/path/to/herdr.sock" # optional
# [[remote.fleet.hosts]]
# name = "workbox"
# target = "workbox"             # OpenSSH config alias or user@host
# socket = "/path/to/herdr.sock" # optional
# session = "agents"              # optional named Herdr session

[day_board]
# Mark a working item stale after this many quiet seconds. Default: 600 (10 minutes).
# stale_after = 600

[experimental]
# Allow launching herdr from inside a herdr-managed pane.
# allow_nested = false
# Experimental local Kitty graphics rendering for attached clients.
# Requires a Kitty graphics-compatible outer terminal.
# kitty_graphics = false
# Save recent pane screen history across full server restarts.
pane_history = false
# While prefix mode is active, temporarily switch the host input source to
# an ASCII-capable mode so prefix commands register even when an IME is
# active, then restore the previous input source when prefix mode exits. On
# macOS this selects the ASCII-capable keyboard layout; on Windows it toggles
# a Korean IME between Hangul and English (other IME languages are left
# unchanged). macOS and Windows only; best-effort. Default: false.
# switch_ascii_input_source_in_prefix = false
# Expose the focused pane's cursor to the outer terminal so macOS input
# methods keep tracking the candidate window when TUIs paint their own
# cursor (Claude Code, pi, codex). Trade-off: extra cursor visible for
# apps that hide it without painting a replacement (vim normal mode, etc.).
# reveal_hidden_cursor_for_cjk_ime = false
# Optional allow-list: only reveal for focused panes whose detected agent
# matches one of these names. Empty means apply to any focused pane.
# If the list contains no valid names, the reveal does not apply.
# Accepted: pi, claude, codex, gemini, cursor, devin, cline, opencode,
# copilot, kimi, kiro, droid, amp, grok, hermes, kilo, qodercli, qoder, qwen,
# qwen-code, maki.
# cjk_ime_agents = []
# Cursor shape rendered when reveal_hidden_cursor_for_cjk_ime is true.
# Values: block, steady_block (default), underline, steady_underline, bar, steady_bar.
# cjk_ime_cursor_shape = "steady_block"

[agent_detection]
# Seconds a full-lifecycle hook report remains authoritative without a fresh
# accepted lifecycle event. Valid range: 30..=3600. Default: 600.
# full_lifecycle_hook_authority_timeout_seconds = 600

[advanced]
# Maximum scrollback buffer size in bytes retained per pane terminal.
# Matches Ghostty's default scrollback-limit behavior.
# scrollback_limit_bytes = 10000000
"##;

// Bundled at build time so the printed skill always matches this binary's release.
const SKILL: &str = include_str!("../skills/herdr/SKILL.md");

fn should_block_nested(config: &config::Config) -> bool {
    should_block_nested_for_env(config, std::env::var(HERDR_ENV_VAR).ok().as_deref())
}

fn should_block_nested_for_env(config: &config::Config, herdr_env: Option<&str>) -> bool {
    !config.experimental.allow_nested && herdr_env == Some(HERDR_ENV_VALUE)
}

fn random_nested_message() -> &'static str {
    use std::time::{SystemTime, UNIX_EPOCH};

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.subsec_nanos() as usize)
        .unwrap_or(0);
    let index = (nanos ^ (std::process::id() as usize)) % NESTED_HERDR_MESSAGES.len();
    NESTED_HERDR_MESSAGES[index]
}

fn exit_if_nested_disabled(config: &config::Config) {
    if should_block_nested(config) {
        eprintln!("\x1b[1merror:\x1b[0m nested herdr is disabled by default.");
        eprintln!("see configuration if you want to enable it.");
        eprintln!();
        eprintln!("\x1b[2m\"{}\"\x1b[0m", random_nested_message());
        std::process::exit(1);
    }
}

fn args_as_utf8<I>(args: I) -> Result<Vec<String>, String>
where
    I: IntoIterator<Item = std::ffi::OsString>,
{
    args.into_iter()
        .enumerate()
        .map(|(index, arg)| {
            arg.into_string()
                .map_err(|_| format!("argument {index} is not valid UTF-8"))
        })
        .collect()
}

fn finish_cli(outcome: io::Result<cli::CommandOutcome>) -> io::Result<()> {
    match outcome {
        Ok(cli::CommandOutcome::Handled(code)) => std::process::exit(code),
        Ok(cli::CommandOutcome::NotCli) => Ok(()),
        Err(err) if cli::protocol_mismatch_was_reported(&err) => std::process::exit(1),
        Err(err) if cli::server_not_running_was_reported(&err) => {
            if let Some(response) = cli::server_not_running_reported_response(&err) {
                if let Ok(json) = serde_json::to_string(response) {
                    eprintln!("{json}");
                }
            }
            std::process::exit(1);
        }
        Err(err) => Err(err),
    }
}

fn main() -> io::Result<()> {
    let raw_args: Vec<String> = match args_as_utf8(std::env::args_os()) {
        Ok(args) => args,
        Err(err) => {
            eprintln!("error: {err}");
            eprintln!("run 'herdr --help' for usage");
            std::process::exit(2);
        }
    };
    if let Some(outcome) = cli::maybe_run_machine(&raw_args) {
        return finish_cli(outcome);
    }
    let args = match session::configure_from_args(&raw_args) {
        Ok(args) => args,
        Err(err) => {
            eprintln!("error: {err}");
            eprintln!("run 'herdr --help' for usage");
            std::process::exit(2);
        }
    };
    let (args, remote_launch) = match remote::extract_remote_args(&args) {
        Ok(parsed) => parsed,
        Err(err) => {
            eprintln!("error: {err}");
            eprintln!("run 'herdr --help' for usage");
            std::process::exit(2);
        }
    };

    if remote_launch.is_some()
        && args.get(1).is_some()
        && !args.iter().any(|a| {
            matches!(
                a.as_str(),
                "--help" | "-h" | "--version" | "-V" | "--default-config" | "--skill"
            )
        })
    {
        eprintln!("error: --remote can only be used with the default launch command");
        eprintln!("run 'herdr --help' for usage");
        std::process::exit(2);
    }

    match cli::maybe_run(&args) {
        Ok(cli::CommandOutcome::Handled(code)) => std::process::exit(code),
        Ok(cli::CommandOutcome::NotCli) => {}
        Err(err) if cli::protocol_mismatch_was_reported(&err) => std::process::exit(1),
        Err(err) if cli::server_not_running_was_reported(&err) => {
            if let Some(response) = cli::server_not_running_reported_response(&err) {
                if let Ok(json) = serde_json::to_string(response) {
                    eprintln!("{json}");
                }
            }
            std::process::exit(1);
        }
        Err(err) => return Err(err),
    }

    if args.get(1).map(String::as_str) == Some("remote-api-bridge") {
        return remote::run_remote_api_bridge(&args[2..]);
    }

    // Subcommands and flags (no TUI, no logging needed)
    if args.get(1).map(|s| s.as_str()) == Some("remote-client-bridge") {
        return remote::run_remote_client_bridge();
    }

    if args.get(1).map(|s| s.as_str()) == Some("remote-control-bridge") {
        return remote::run_remote_control_bridge();
    }

    if args.get(1).map(|s| s.as_str()) == Some("server") {
        return server::headless::run_server();
    }

    // Hidden client mode: connect to an existing server's client socket.
    if args.get(1).map(|s| s.as_str()) == Some("client") {
        let loaded_config = config::Config::load();
        exit_if_nested_disabled(&loaded_config.config);
        return client::run_client();
    }

    if args.get(1).map(|s| s.as_str()) == Some("update") {
        let options = match update::parse_self_update_args(&args[2..]) {
            Ok(options) => options,
            Err(err) if err.starts_with("usage:") => {
                eprintln!("{err}");
                std::process::exit(0);
            }
            Err(err) => {
                eprintln!("{err}");
                eprintln!("usage: herdr update [--handoff]");
                std::process::exit(2);
            }
        };
        match update::self_update(options) {
            Ok(_) => return Ok(()),
            Err(e) => {
                if e.starts_with("self-update is disabled") {
                    eprintln!("{e}");
                } else {
                    eprintln!("update failed: {e}");
                }
                std::process::exit(1);
            }
        }
    }

    if args.iter().any(|a| a == "--help" || a == "-h") {
        platform::begin_cli_output();
        println!("herdr — terminal workspace manager for AI coding agents");
        println!();
        println!("Usage: herdr [options]");
        println!("       herdr --session <name> [options]");
        println!("       herdr --remote <ssh-target> [--session <name>]");
        println!("       herdr session attach <name>");
        println!("       herdr completion zsh");
        println!("       herdr update [--handoff]");
        println!("       herdr channel set <stable|preview>");
        println!("       herdr server stop");
        println!("       herdr server reload-config");
        println!("       herdr api <subcommand> ...");
        println!("       herdr completion <shell>");
        println!("       herdr config <subcommand> ...");
        println!("       herdr fleet <subcommand> ...");
        println!("       herdr channel <subcommand> ...");
        println!("       herdr workspace <subcommand> ...");
        println!("       herdr worktree <subcommand> ...");
        println!("       herdr tab <subcommand> ...");
        println!("       herdr notification <subcommand> ...");
        println!("       herdr agent <subcommand> ...");
        println!("       herdr pane <subcommand> ...");
        println!("       herdr session <subcommand> ...");
        println!("       herdr integration <subcommand> ...");
        println!();
        println!("Common commands:");
        for (command, description) in [
            ("herdr", "Launch or attach to the persistent session"),
            (
                "herdr status [server|client]",
                "Show local client and running server status",
            ),
            ("herdr update", "Download and install the latest version"),
            ("herdr completion zsh", "Generate shell completions for zsh"),
            (
                "herdr server stop",
                "Stop the running server via the API socket",
            ),
            (
                "herdr channel set <stable|preview>",
                "Choose the stable or preview update channel",
            ),
            (
                "herdr server reload-config",
                "Reload config.toml in the running server",
            ),
            (
                "herdr config reset-keys",
                "Back up config.toml and remove custom keybindings",
            ),
            (
                "herdr channel <subcommand>",
                "Manage the stable or preview update channel",
            ),
            (
                "herdr api <subcommand>",
                "Inspect socket API metadata and live runtime state",
            ),
            (
                "herdr fleet status",
                "Inspect agents across configured hosts",
            ),
            (
                "herdr workspace <subcommand>",
                "Workspace helpers over the socket API",
            ),
            (
                "herdr worktree <subcommand>",
                "Git worktree helpers over the socket API",
            ),
            ("herdr tab <subcommand>", "Tab helpers over the socket API"),
            (
                "herdr notification <subcommand>",
                "Notification helpers over the socket API",
            ),
            (
                "herdr agent <subcommand>",
                "Agent/terminal helpers over the socket API",
            ),
            (
                "herdr pane <subcommand>",
                "Pane control helpers over the socket API",
            ),
            (
                "herdr session <subcommand>",
                "Manage named persistent sessions",
            ),
            (
                "herdr integration <subcommand>",
                "Manage built-in agent integrations",
            ),
        ] {
            println!("  {command:<32} {description}");
        }
        println!();
        println!("Advanced commands:");
        println!("  {:<32} Run as headless server", "herdr server");
        println!();
        println!("Options:");
        println!("  --no-session        Run monolithically (no server/client, escape hatch)");
        println!("  --session <name>    Use or create a named persistent session");
        println!("  --remote <target>   Attach through SSH to a remote Herdr server");
        println!("  --remote-keybindings <local|server>");
        println!("                      Keybindings for --remote app attach (default: local)");
        println!("  --handoff           Opt into live handoff for update or remote attach");
        println!("  --default-config    Print default configuration and exit");
        println!("  --skill             Print the agent skill file and exit");
        println!("  --version, -V       Print version and exit");
        println!("  --help, -h          Show this help");
        println!();
        println!("Config: {}", config::config_path().display());
        println!("Logs:   {}", logging::help_log_paths_summary());
        println!("Env:    HERDR_CONFIG_PATH overrides config file path");
        println!("Home:   https://herdr.dev");
        println!();
        println!("{}", cli::AGENT_HELP_FOOTER);
        return Ok(());
    }

    if args.iter().any(|a| a == "--version" || a == "-V") {
        platform::begin_cli_output();
        println!("herdr {}", crate::build_info::version());
        return Ok(());
    }

    if args.iter().any(|a| a == "--default-config") {
        platform::begin_cli_output();
        print!("{DEFAULT_CONFIG}");
        return Ok(());
    }

    if args.iter().any(|a| a == "--skill") {
        platform::begin_cli_output();
        print!("{SKILL}");
        return Ok(());
    }

    // Reject unknown flags
    let known_flags = [
        "--no-session",
        "--session",
        "--remote",
        "--remote-keybindings",
        "--version",
        "-V",
        "--default-config",
        "--skill",
        "--help",
        "-h",
    ];
    for arg in &args[1..] {
        let arg_name = arg.split_once('=').map(|(name, _)| name).unwrap_or(arg);
        if arg.starts_with('-') && !known_flags.contains(&arg_name) {
            eprintln!("unknown option: {arg}");
            eprintln!("run 'herdr --help' for usage");
            std::process::exit(2);
        }
        if !arg.starts_with('-')
            && ![
                "server",
                "client",
                "remote-client-bridge",
                "remote-control-bridge",
                "update",
                "status",
                "config",
                "channel",
                "workspace",
                "worktree",
                "pane",
                "session",
                "integration",
            ]
            .contains(&arg.as_str())
        {
            eprintln!("unknown command: {arg}");
            eprintln!("run 'herdr --help' for usage");
            std::process::exit(2);
        }
    }

    if let Some(remote_launch) = remote_launch {
        let remote_target = remote_launch.target.clone();
        if let Err(err) = remote::run_remote(remote_launch) {
            eprintln!("error: {err}");
            remote::print_remote_error_hint(&err, &remote_target);
            std::process::exit(1);
        }
        return Ok(());
    }

    let loaded_config = config::Config::load();
    exit_if_nested_disabled(&loaded_config.config);

    let no_session = args.iter().any(|a| a == "--no-session");

    // Auto-detect launch: when --no-session is NOT set, use server/client mode.
    // Check if a server is running, spawn one if needed, then attach as client.
    if !no_session {
        if let Err(err) = server::autodetect::auto_detect_launch() {
            eprintln!("herdr: {err}");
            std::process::exit(1);
        }
        return Ok(());
    }

    // --- Monolithic mode (--no-session escape hatch) ---
    // This is the pre-mission single-process behavior.

    init_logging();

    let (api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
    let event_hub = api::EventHub::default();
    let _api_server = match api::start_server_with_capabilities(api_tx, event_hub.clone(), None) {
        Ok(server) => server,
        Err(err) if err.kind() == io::ErrorKind::AddrInUse => {
            eprintln!("error: herdr is already running");
            eprintln!("socket: {}", api::socket_path().display());
            std::process::exit(1);
        }
        Err(err) => return Err(err),
    };

    let modify_other_keys_mode = crate::input::host_modify_other_keys_mode();

    let original_hook = std::panic::take_hook();
    let panic_resets_modify_other_keys = modify_other_keys_mode.is_some();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!("PANIC: {info}");
        if panic_resets_modify_other_keys {
            let _ = std::io::Write::write_all(&mut io::stdout(), b"\x1b[>4;0m");
        }
        if crate::kitty_graphics::is_enabled() {
            let _ = crate::kitty_graphics::clear_all_host_graphics();
        }
        let _ = execute!(
            io::stdout(),
            DisableFocusChange,
            DisableBracketedPaste,
            DisableMouseCapture
        );
        let _ = crate::terminal_modes::clear_host_mouse_reporting(&mut io::stdout());
        let _ = set_host_color_scheme_reports(false);
        let _ = pop_keyboard_enhancement_flags();
        ratatui::restore();
        original_hook(info);
    }));

    let config = &loaded_config.config;
    let config_diagnostic = config::config_diagnostic_summary(&loaded_config.diagnostics);
    logging::startup("app");

    // Background update check (non-blocking, best-effort)
    // Only checks for newer versions and notifies the TUI.
    // Skipped in --no-session mode (testing).

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to create tokio runtime");

    let result = rt.block_on(async {
        let mut terminal = ratatui::init();
        crate::terminal_modes::clear_host_mouse_reporting(&mut io::stdout())?;
        if config.ui.mouse_capture {
            execute!(io::stdout(), EnableMouseCapture)?;
        } else {
            execute!(io::stdout(), DisableMouseCapture)?;
        }
        execute!(io::stdout(), EnableBracketedPaste, EnableFocusChange)?;
        set_host_color_scheme_reports(true)?;
        push_keyboard_enhancement_flags()?;

        // Some hosts do not honor Kitty keyboard enhancement pushes for
        // Shift+Enter. Enable xterm modifyOtherKeys only on hosts where we
        // know it is needed and parseable, so modified Enter stays distinct.
        if let Some(mode) = modify_other_keys_mode {
            use std::io::Write;
            std::io::stdout().write_all(mode.set_sequence())?;
            std::io::stdout().flush()?;
        }

        let mut app = app::App::new(
            config,
            true, // no_session — monolithic mode never saves/restores sessions
            config_diagnostic,
            api_rx,
            event_hub,
        );
        app.state.open_home_on_launch(config);
        let result = app.run(&mut terminal).await;

        // Reset modifyOtherKeys if we enabled it.
        if modify_other_keys_mode.is_some() {
            use std::io::Write;
            std::io::stdout().write_all(b"\x1b[>4;0m")?;
            std::io::stdout().flush()?;
        }

        if crate::kitty_graphics::is_enabled() {
            crate::kitty_graphics::clear_all_host_graphics()?;
        }
        pop_keyboard_enhancement_flags()?;
        execute!(
            io::stdout(),
            DisableFocusChange,
            DisableBracketedPaste,
            DisableMouseCapture
        )?;
        crate::terminal_modes::clear_host_mouse_reporting(&mut io::stdout())?;
        set_host_color_scheme_reports(false)?;
        ratatui::restore();

        // Drop app (and all workspaces/panes) before runtime shuts down
        drop(app);

        result
    });

    // Shut down runtime immediately — kills lingering PTY reader/writer tasks
    rt.shutdown_timeout(std::time::Duration::from_millis(100));

    logging::shutdown("app");
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_herdr_blocks_when_env_is_set() {
        let config = config::Config::default();
        assert!(should_block_nested_for_env(&config, Some(HERDR_ENV_VALUE)));
    }

    #[test]
    fn nested_herdr_does_not_block_when_allowed() {
        let config: config::Config =
            toml::from_str("[experimental]\nallow_nested = true\n").unwrap();
        assert!(!should_block_nested_for_env(&config, Some(HERDR_ENV_VALUE)));
    }

    #[test]
    fn nested_herdr_does_not_block_without_env() {
        let config = config::Config::default();
        assert!(!should_block_nested_for_env(&config, None));
    }

    #[test]
    fn random_nested_message_comes_from_known_set() {
        let message = random_nested_message();
        assert!(NESTED_HERDR_MESSAGES.contains(&message));
    }

    #[test]
    fn nested_message_strings_no_longer_repeat_herdr_prefix() {
        assert!(NESTED_HERDR_MESSAGES
            .iter()
            .all(|message| !message.starts_with("herdr:")));
    }

    #[test]
    fn default_config_exposes_status_bar_enabled_setting() {
        assert!(DEFAULT_CONFIG.contains("[ui.status_bar]\n# enabled = true"));
        assert!(DEFAULT_CONFIG.contains(
            "[agent_detection]\n# Seconds a full-lifecycle hook report remains authoritative"
        ));
        assert!(DEFAULT_CONFIG.contains("# full_lifecycle_hook_authority_timeout_seconds = 600"));
        assert!(DEFAULT_CONFIG.contains("# hide_done_after_minutes = 30"));
        assert!(DEFAULT_CONFIG.contains("# reap_done_after_minutes = 240"));
        assert!(DEFAULT_CONFIG.contains("# settle_after_days = 3"));
        assert!(DEFAULT_CONFIG.contains("# settle_finished_after_minutes = 10"));
        assert!(DEFAULT_CONFIG.contains("# settle_stops_agent = true"));
        assert!(DEFAULT_CONFIG.contains("# reap_done_panes = true"));
        assert!(DEFAULT_CONFIG.contains("[source_control]\n# Merge strategy"));
        assert!(DEFAULT_CONFIG.contains("# merge_method = \"merge\""));
    }

    #[test]
    fn default_config_exposes_sidebar_section_options() {
        assert!(DEFAULT_CONFIG.contains("# layout = \"current\""));
        assert!(DEFAULT_CONFIG.contains("# header = \"sky\""));
        assert!(DEFAULT_CONFIG.contains("Pinned, Active, Snoozed, and Settled"));
    }

    #[test]
    fn default_config_documents_stalled_agent_nudge_and_status_alternative() {
        assert!(DEFAULT_CONFIG.contains("# auto_nudge_stalled_agents = false"));
        assert!(DEFAULT_CONFIG.contains("# agent_stale_after_minutes = 5"));
        assert!(DEFAULT_CONFIG.contains("# agent_subagent_stale_after_minutes = 30"));
        assert!(DEFAULT_CONFIG.contains("# nudge_after_minutes = 5"));
        assert!(DEFAULT_CONFIG.contains("# max_nudges = 3"));
        assert!(DEFAULT_CONFIG.contains("# max_nudges_without_human = 5"));
        assert!(DEFAULT_CONFIG.contains(
            "# stall_nudge_message = \"Re-verify what you are working on now; do not answer from memory. If you have subagents, poll them and restart any that are stalled. If everything is still progressing, reply with one line: Progressing, plus the count and names of running subagents if any (e.g. Progressing, 2 subagents: build, review). If it is done or something changed, say so and continue.\""
        ));
        assert!(DEFAULT_CONFIG.contains(
            "Harnesses with the herdr-status skill installed can use this sharper alternative"
        ));
        assert!(DEFAULT_CONFIG.contains("# stall_nudge_message = \"/status\""));
    }

    #[test]
    fn default_config_exposes_global_window_bindings() {
        assert!(DEFAULT_CONFIG.contains("# previous_tab = \"prefix+ctrl+p\""));
        assert!(DEFAULT_CONFIG.contains("# next_tab = \"prefix+ctrl+n\""));
        assert!(DEFAULT_CONFIG.contains("# previous_window = \"prefix+p\""));
        assert!(DEFAULT_CONFIG.contains("# next_window = \"prefix+n\""));
        assert!(DEFAULT_CONFIG.contains("# next_blocked_window = \"prefix+b\""));
        assert!(DEFAULT_CONFIG.contains("# toggle_sidebar = \"prefix+shift+b\""));
    }

    #[cfg(unix)]
    fn invalid_utf8_arg() -> std::ffi::OsString {
        use std::os::unix::ffi::OsStringExt;
        std::ffi::OsString::from_vec(vec![0xff])
    }

    #[cfg(windows)]
    fn invalid_utf8_arg() -> std::ffi::OsString {
        use std::os::windows::ffi::OsStringExt;
        std::ffi::OsString::from_wide(&[0xd800])
    }

    #[test]
    fn args_as_utf8_passes_through_valid_arguments() {
        let args = ["herdr", "pane", "get", "pane-1"].map(std::ffi::OsString::from);
        assert_eq!(
            args_as_utf8(args).unwrap(),
            ["herdr", "pane", "get", "pane-1"]
        );
    }

    #[test]
    fn args_as_utf8_reports_the_offending_argument_instead_of_panicking() {
        let args = vec![
            std::ffi::OsString::from("herdr"),
            std::ffi::OsString::from("pane"),
            invalid_utf8_arg(),
        ];
        assert_eq!(
            args_as_utf8(args).unwrap_err(),
            "argument 2 is not valid UTF-8"
        );
    }
}
