# M1 agents-sidebar-cards review

**Reviewed:** 2026-08-24

**Spec:** `docs/vimeflow/specs/2026-08-24-agents-sidebar-cards-design.md`

**Plan:** `docs/vimeflow/plans/2026-08-24-agents-sidebar-cards-plan.md`

**Fork baseline:** `edde368a2ca7b7b560196892889b71cfd102e863`

**Watcher baseline:** tag `v0.2.2`
(`f0de59e89a8c9e3332634cdb096893ba702eeeee`)

**Verdict:** **NO-GO as written.** The render-only swap point exists, and the
current visible ordering is shared by rendering, clicks, and indexed focus.
The plan nevertheless assumes a public watcher socket client, an Agents-section
focus model, workspace group headers, and scroll behavior that the code does
not have. Resolve the HIGH findings before implementation.

This was the requested skeptical review only. No spec, plan, source, config,
dependency, or watcher-crate file was changed.

## Claims that check out

- A render-only branch can be placed at `src/ui/sidebar.rs:956-981`, where the
  expanded sidebar delegates all Agents content to `render_agent_detail`.
  That validates the narrow claim that a card/legacy render branch exists; it
  does not remove the separate geometry, hit-testing, and input work described
  in HIGH-3/HIGH-4.
- The current legacy renderer, mouse hit-test, direct indexed focus, and
  previous/next focus all derive their order from `agent_panel_entries`
  (`src/ui/sidebar.rs:112-134,1448-1467`,
  `src/app/input/sidebar.rs:485-522`, and
  `src/app/input/navigate.rs:735-762`). `apply_agent_view` performs filtering
  before sorting (`src/app/agent_view.rs:45-75`), so filtered entries do **not**
  reserve indexes today. A card-only hide-idle filter must preserve that rule
  by feeding the same filtered source to rendering and focus resolution
  (MEDIUM-1).
- The configured expanded-sidebar bounds really are 18/26/36 columns
  (`src/config/model.rs:820-828,1044-1048`), and `[ui]` values are live-reloaded
  through `App::apply_live_config` (`src/app/mod.rs:1404-1455`). Therefore
  `agents_view` should be live rather than startup-only. The usable Agents body
  is narrower than the configured outer width (MEDIUM-2).
- `sidebar::reducer::{State, apply_line}`, `sidebar::layout`, and the high-level
  `sidebar::view` types/functions are public in watcher v0.2.2. Those pieces
  are reusable once ingestion and lifecycle authority are specified correctly.

## HIGH findings

### HIGH-1 — The claimed watcher `client/live` reuse surface does not exist

**Spec/plan:** design §§3-4 and §8, lines 38-46, 62-64, 114-115, 122; plan P0.4
and P2-P3, lines 26-28 and 39-55.

**What is wrong:** Watcher v0.2.2 has no sidebar client module.
`agent-watcher/src/sidebar/live.rs` is mutable settings state (`Live`), not a
socket subscription. The actual Unix-socket connect, subscribe request,
reader thread, and reconnect loop are private functions inside
`agent-watcher/src/sidebar/tui.rs:42-76,1826-1998`. The public reducer can fold
wire lines, but it cannot obtain them. In addition,
`agent-watcher/src/sidebar/mod.rs:10-14` keeps `format`, `metrics`, `select`, and
`style` crate-private. The public `view::render` is not a drop-in alternative:
it clamps width to the watcher's 34-column minimum and selects only panes
present in watcher telemetry (`sidebar/view.rs:875-924`), while this spec makes
Herdr lifecycle authoritative and requires lifecycle-only cards at 18 columns.
Using it directly would omit core-only agents and let telemetry choose card
membership/state.

The fork could write its own UnixStream/reconnect thread, but that contradicts
both the named supported surface and the no-watcher-change invariant. It would
also duplicate exactly the connection lifecycle the plan says to reuse.

**Concrete fix:** Publish a minimal watcher state-stream API in a new watcher
release: explicit socket path, subscribe/reconnect on a worker, folded snapshot
delivery (or line delivery to the public reducer), and clean shutdown/join.
Also publish one narrow formatting/card-lines API that accepts authoritative
lifecycle input and the actual width without the 34-column clamp, or revise the
spec to make the fork own that formatting. Pin the fork to that release before
P2. Do not start P2 under the current “no watcher changes” rule.

### HIGH-2 — “Section focused” has no state or input path

**Spec/plan:** design §6, lines 77-88; plan P0.3 and P4.4, lines 22-25 and
66-67.

**What is wrong:** `AppState` has a selected workspace and an
`agent_panel_scroll`, but no selected sidebar section or selected agent card
(`src/app/state.rs:1400-1452,1476-1485`). Navigate mode gives its first
up/down bindings to workspace selection, then maps the pane-direction keys
(default `j`/`k`) to pane focus (`src/app/input/navigate.rs:127-180,1280-1295`).
Clicking an agent immediately focuses its pane
(`src/app/input/mouse.rs:623-627`); it does not focus the Agents section or
establish a card cursor. Indexed and previous/next agent actions likewise
focus panes immediately (`src/app/input/navigate.rs:252-287,735-762`). There is
therefore nowhere for `j/k/o/Enter/z` “when the section is focused” to hook,
and no defined action that enters or leaves such a focus.

**Concrete fix:** Specify the missing interaction model before P4: how focus
enters/leaves Agents, whether moving the card cursor also focuses a pane, how
the cursor relates to the active pane, and precedence over existing navigate
workspace/pane bindings. Then budget an explicit section/cursor state and
input routing with tests for entry, exit, and key conflicts. If no new focus
model is wanted, keep existing global indexed/previous/next focus and make
expand/hide mouse or explicit prefix actions instead.

### HIGH-3 — Inline expansion is incompatible with the current scroll model

**Spec/plan:** design §4 and §6, lines 57-60 and 83-85; plan P3-P4, lines
47-67.

**What is wrong:** The Agents panel scroll is an **entry index**, not a line
offset. Its geometry counts only whole entries that fit and caps every entry's
height to the body height (`src/ui/sidebar.rs:534-645`). Rendering stops before
an entry that does not wholly fit and renders at most that capped height
(`src/ui/sidebar.rs:1464-1515`); mouse hit-testing repeats the same whole-entry
calculation (`src/app/input/sidebar.rs:485-522`). The Agents section may be only
three rows high, all consumed by its header
(`src/ui/sidebar.rs:42-57,534-542`). A watcher-style expanded card can contain
model, multiple two-line gauges, tools, and five traces—more lines than a
typical half-sidebar viewport. Capping it makes the tail unreachable because
the existing scroll can move only to another card, never within this card.

**Concrete fix:** Give cards their own line-offset scroll model with card line
spans (the watcher's public `sidebar::layout::{LineSpan, ensure_visible}` is
suited to that), and use the same spans for render and hit-testing. Keep the
legacy entry-index scroll untouched behind the toggle. Alternatively constrain
the expanded form to a guaranteed viewport-sized line budget and state exactly
which details are dropped; the current unbounded inline contract cannot use
the legacy scroll path.

### HIGH-4 — Agents has no workspace group headers to reuse

**Spec/plan:** design §5 and §8, lines 68-75 and 108-125; plan P0.1 and P4.1-2,
lines 15-17 and 57-62.

**What is wrong:** `agent_panel_entries` collects panes in workspace order and
`AgentPanelSort::Spaces` leaves that natural order alone
(`src/ui/sidebar.rs:127-183`; `src/app/agent_view.rs:45-75`). It does not create
header entries. The workspace grouping/header machinery belongs to the
separate Spaces list (`WorkspaceListEntry`, workspace card areas, and
`render_workspace_list`); `render_agent_detail` draws a flat sequence of
agent rows (`src/ui/sidebar.rs:1404-1521`). Reusing Spaces headers inside
Agents would pull workspace expansion, scroll, drag/drop, and workspace
selection semantics into a different list. It is not an existing Agents
branch or a few-line reuse.

**Concrete fix:** Choose one contract explicitly. The smallest is “spaces =
stable workspace-order cards, with workspace shown inside each card” and no
group headers. If headers are required, define a card-list display-item enum
(header/card/hidden-count), make cards own its geometry and hit-testing, and
state that headers do not consume `focus_agent` indexes. Revise the “existing
header machinery” and “few-line merge surface” claims accordingly.

## MEDIUM findings

### MEDIUM-1 — The hide-idle index rule is left open even though current behavior answers it

**Spec/plan:** design §5, lines 71-75; plan P4.3, lines 63-65.

**What is wrong:** The spec permits either reserved or collapsed binding
indexes, leaving an implementer to guess. Existing external agent-view filters
already establish the rule: filtering occurs inside `agent_panel_entries`, and
render, click, `focus_agent`, and previous/next all consume that filtered vec.
The test `next_agent_starts_at_first_visible_entry_when_focused_agent_is_filtered_out`
in `src/app/input/navigate.rs:1934-1964` codifies it. Hidden entries do not
occupy indexes. Applying hide-idle only in the card renderer would break the
current invariant even though the unfiltered functions share a source today.

**Concrete fix:** State “hidden idle entries do not occupy `focus_agent`
indexes.” Build one cards-visible ordered source after agent-view filtering,
panel sort, and hide-idle; consume it in card render, card hit-testing,
indexed focus, and previous/next focus. Keep the hidden-count indicator outside
the indexed item sequence.

### MEDIUM-2 — The 18-column contract is measuring the wrong rectangle and ignores compact mode

**Spec/plan:** design §4 and §9, lines 53-56 and 123; plan P3/P5, lines 49-55
and 75-77.

**What is wrong:** An outer sidebar width of 18 yields a 17-column expanded
content area because `expanded_sidebar_sections` reserves the right separator
column (`src/ui/sidebar.rs:59-68`). When the Agents scrollbar is visible,
`agent_panel_body_rect` removes another column (`src/ui/sidebar.rs:534-542`),
so the actual card budget is 16. Tests that hand 18 directly to a card builder
will pass a layout users never see. Separately, collapsed `compact` mode has a
distinct rail renderer that shows each agent in one row
(`src/ui/sidebar.rs:727-857`) and is not in the claimed 18-36 range. The spec
does not say whether cards replace that rail too.

**Concrete fix:** Define adaptive rules against the actual `body.width` and
table-test outer widths 18/26/36 with and without a scrollbar (expected card
budgets 16/17, 24/25, and 34/35). State that card mode affects only the
expanded Agents section and the compact rail remains byte-identical, unless a
separate compact-card design is added.

### MEDIUM-3 — “Rows config is present” is not represented by the config model

**Spec/plan:** design §7, lines 90-104; plan P1, lines 32-37.

**What is wrong:** `AgentsSidebarConfig` is defaulted into concrete `rows`,
`rows_by_agent`, and `row_gap` values (`src/config/sidebar.rs:372-405`), so
after deserialization the model cannot distinguish an omitted rows section
from an explicitly supplied value equal to the default. `Config::load` calls
`collect_diagnostics` only after that information is lost
(`src/config/io.rs:106-145`). Live reload parses a raw TOML table first and
then applies the entire `[ui]` section (`src/config/io.rs:241-329`), while
`App::apply_live_config` already makes sidebar UI values live
(`src/app/mod.rs:1404-1455`). The requested “present rows config” diagnostic
and its behavior on live cards/legacy changes therefore need an explicit
presence source.

**Concrete fix:** Detect `[ui.sidebar.agents].rows`, `rows_by_agent`, or
`row_gap` presence from the raw TOML value in both startup and live loaders,
then attach the cards-only diagnostic there; do not infer presence by comparing
materialized values to defaults. Define the toggle as live and reset/clamp the
mode-specific scroll/cursor state on reload. Test absent, explicitly-default,
custom, cards-to-legacy, and legacy-to-cards cases.

## LOW findings

### LOW-1 — `sidebar/tokens.rs` is not a general card-style API

**Spec/plan:** design §4, lines 62-64; plan P3, lines 52-54.

**What is wrong:** `src/ui/sidebar/tokens.rs` resolves user-configurable legacy
row tokens. Its resolved types are exposed only to the parent sidebar module,
and it does not define gauge, trace, card, or semantic-role styling. The
watcher's public card lines use their own role/semantic style model instead.
“Draw through tokens.rs styling” does not identify an actual reusable adapter.

**Concrete fix:** Have the additive card renderer map the watcher's public
`Role`/`Semantic` values directly to the existing `AppState.palette`. Leave
`tokens.rs` and the legacy path unchanged; add no new abstraction unless a
second card surface needs it.
