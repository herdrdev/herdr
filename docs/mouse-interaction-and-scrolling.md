# Herdr Mouse Interaction & Terminal Scrolling Architecture

This document details Herdr's end-to-end mouse event architecture, terminal scrolling subsystem, text selection and clipboard pipeline, child terminal mouse reporting coexistence, and lifecycle hook integration.

---

## 1. Overview & Core Design Principles

Herdr manages multiple concurrent terminal runtimes inside a terminal workspace multiplexer. To provide a seamless, native desktop terminal feel, Herdr implements a layered mouse event architecture that reconciles:
1. **Client Chrome Navigation**: Clicking tabs, dragging split dividers, switching workspaces, and clicking status indicators.
2. **Pane Viewport Interaction**: Click-to-focus, scrollback wheel navigation, scrollbar track jumps, and scrollbar thumb dragging.
3. **Text Selection & Clipboard Copy**: Click-anchor text selection, drag-to-expand, edge autoscrolling, double-click word selection, copy-on-select, and OSC 52 / Wayland / X11 clipboard synchronization.
4. **Child Terminal Mouse Passthrough**: Transparent bidirectional passthrough for applications that request xterm/SGR mouse reporting (e.g. Vim, Neovim, htop, btop, lazygit) and alternate screen arrow scrolling for pagers (e.g. `less`).

---

## 2. Event Flow & Routing Pipeline

```
┌────────────────────────────────────────────────────────┐
│               Host Terminal Emulator                   │
│   (Emits SGR 1006 / X10 mouse escape sequences)        │
└───────────────────────────┬────────────────────────────┘
                            │
                            ▼
┌────────────────────────────────────────────────────────┐
│             src/client/terminal_setup.rs               │
│   set_mouse_capture(mouse_capture, sgr_pixels)         │
│   (Enabled via config: [ui] mouse_capture = true)      │
└───────────────────────────┬────────────────────────────┘
                            │
                            ▼ crossterm::event::MouseEvent
┌────────────────────────────────────────────────────────┐
│             src/client/shell/mouse.rs                  │
│   handle_mouse(point, mouse, outcome)                  │
│                                                        │
│   ├── Overlays / Menus (ContextMenu, Rename, Popups)   │
│   ├── Workspace List / Tab Bar Hit Testing             │
│   ├── Pane Hit Testing:                                │
│   │   ├── scrollbar_rect ──► push_pane_scroll_offset   │
│   │   ├── inner_rect:                                  │
│   │   │   ├── mouse_reporting? ──► push_pane_mouse     │
│   │   │   └── shell / scrollback ──► push_pane_mouse   │
│   │   └── rect (borders):                              │
│   │       ├── mouse_reporting? ──► push_pane_scroll     │
│   │       └── !mouse_reporting ──► push_pane_mouse    │
│   └── Click-to-Focus ──► Method::PaneFocus             │
└───────────────────────────┬────────────────────────────┘
                            │ IPC Socket Message
                            ▼
┌────────────────────────────────────────────────────────┐
│             src/server/pane_input.rs                   │
│   apply_client_pane_input_events / apply_scroll        │
│                                                        │
│   ├── WheelRouting::MouseReport ──► child PTY (SGR)    │
│   ├── WheelRouting::AlternateScroll ──► Up/Down keys   │
│   └── WheelRouting::HostScroll ──► runtime.scroll_up/  │
│                                    runtime.scroll_down │
└────────────────────────────────────────────────────────┘
```

---

## 3. Subsystem Breakdown

### 3.1 Host Terminal Capture (`mouse_capture`)

* **Configuration**: `[ui] mouse_capture = true` in `/root/.config/herdr/config.toml`.
* **Mechanism**: When enabled, Herdr issues `EnableMouseCapture` (`\x1b[?1000h\x1b[?1002h\x1b[?1006h`) to `stdout` upon startup and during live reload.
* **Impact of Disablement**: If `mouse_capture = false`, the host terminal emulator suppresses all mouse reports for standard shell panes. The terminal emulator never sends escape sequences to Herdr, disabling mouse scrolling, click-to-focus, and text selection.

### 3.2 Viewport Wheel Scrolling & Scrollback Navigation

Wheel events (`ScrollUp`, `ScrollDown`, `ScrollLeft`, `ScrollRight`) are matched against panes using a three-tier spatial hierarchy:
1. **Scrollbar Track (`hit.scrollbar_rect`)**:
   - The user is scrolling directly over the scrollbar gutter.
   - Herdr calculates the target offset using `current_offset ± mouse_scroll_lines` clamped to `max_offset_from_bottom`.
   - Dispatches `push_pane_scroll_offset` (which sends `Method::PaneScroll` via the socket API) and repaints immediately.
2. **Terminal Content Area (`hit.inner_rect`)**:
   - Dispatched via `push_pane_mouse_event`.
   - On the server, `apply_scroll` evaluates the active terminal's `wheel_routing()`:
     - **Mouse Reporting Enabled**: SGR escape sequence encoded and written directly into the child process's PTY.
     - **Alternate Screen Active (no mouse reporting)**: Encodes `KeyUp` / `KeyDown` arrow sequences (e.g. smooth pager scrolling in `less`).
     - **Normal Shell / Scrollback**: Directly adjusts the terminal runtime buffer via `runtime.scroll_up` / `runtime.scroll_down`.
3. **Pane Chrome / Borders (`hit.rect` outside `inner_rect`)**:
   - When child mouse reporting is disabled, wheel events adjust the pane's scrollback buffer.
   - When child mouse reporting is enabled, border scrolling drives Herdr's scrollback rather than injecting invalid/clamped coordinates into the child application.
4. **Click / Scroll to Focus**:
   - Rolling the scroll wheel over an unfocused pane automatically issues `Method::PaneFocus` before processing scroll events.

### 3.3 Text Selection, Autoscroll & Clipboard

* **Click-to-Anchor**: Left click on an idle terminal pane anchors a selection (`Selection::anchor`).
* **Drag-to-Select**: Dragging updates the selection bounding box in memory.
* **Edge Autoscrolling**: Dragging outside the upper or lower boundary of the viewport initiates autoscrolling (`selection_autoscroll`). Autoscroll metrics incorporate pending targets (`pane_scroll_targets`) to prevent coordinate jitter while in-flight RPCs settle.
* **Copy-on-Select**: When `copy_on_select = true` in `config.toml`, releasing the left mouse button finishes the selection, triggers `request_selection_copy`, formats the selected text, and transmits it via:
  1. Operating system clipboard bridges (`wl-copy`, `xclip`, `pbcopy`).
  2. Terminal OSC 52 sequence (`\x1b]52;c;...`).
  3. Tmux passthrough wrapping (`\x1bPtmux;\x1b]52;c;...\x1b\\`) when running under tmux or cloud notebooks (Colab).
* **Clipboard Feedback**: When `[ui.toast.clipboard] enabled = true`, an unobtrusive toast notification indicates successful copy.

### 3.4 Coexistence with Child Mouse Reporting Applications

Child applications requesting terminal mouse reporting (e.g. Vim, htop, btop) are fully supported alongside Herdr's chrome:
* Herdr inspects `runtime.mouse_reporting_enabled()` in real time.
* When active, clicks and drags inside `inner_rect` are streamed to the child application via canonical input events.
* Clicks on the outer border, title bar, tabs, split handles, or sidebar are intercepted by Herdr, preserving window management.
* Scrolling over the scrollbar track continues to scroll Herdr's terminal scrollback, providing a reliable escape hatch to inspect earlier command output without leaving the running application.

---

## 4. Agent Lifecycle Hooks vs. Mouse Routing Pipeline

Herdr integrates with autonomous coding agents (Codex CLI, Antigravity CLI, Claude Code, etc.) using lightweight shell hooks (`src/integration/assets/*/herdr-agent-state.sh`).

* **Architectural Independence**:
  - The agent state hook is strictly an **egress notification mechanism** (`pane.report_agent_session`) executed upon agent session initialization.
  - It reports the agent name and session ID over the Unix domain socket so Herdr can annotate the pane and sidebar.
  - **The hook subsystem has zero overlap with mouse input, keyboard input, terminal rendering, or scrollback buffers.**
* **Stdout Hook Hygiene**:
  - Frameworks like Codex CLI inspect `SessionStart` hook `stdout` for optional JSON session configuration.
  - Commands executed in `SessionStart` hooks must produce **either valid JSON or empty stdout**. Emitting raw human-readable logs to `stdout` causes downstream JSON parse errors.

---

## 5. Live Configuration & Non-Disruptive Reload

Herdr supports updating mouse and UI configuration without restarting running daemons or disrupting active agent workloads:

```bash
# Verify configuration syntax
herdr config check

# Apply changes to the live running server without process termination
herdr server reload-config
```

The reload pipeline:
1. Re-reads `/root/.config/herdr/config.toml`.
2. Updates `shell_mouse_capture_preference`, `copy_on_select`, and clipboard toast settings in memory.
3. Broadcasts `ServerMessage::SetHostMouseCapture` to connected client terminals.
4. Invokes `set_mouse_capture` on the client terminal guard.
5. Preserves all active PTYs, child agent processes, scrollback buffers, and layout trees.
