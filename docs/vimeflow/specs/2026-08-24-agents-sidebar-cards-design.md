# Agents Sidebar Cards — M1 Design

**Date:** 2026-08-24 (v2 same day — revised for all findings in
`../reviews/2026-08-24-m1-sidebar-cards-review.md`)
**Status:** Approved (operator decisions 2026-08-20 + 2026-08-24)
**Supersedes:** the pivot spec's 3-section sidebar shape — the Agents
section's content is REPLACED by the agent-watcher card view; no third
Watcher section. Rationale: materially smaller upstream diff, one surface.

## 1. Summary

The fork's sidebar **Agents section** renders agent **cards** (lifecycle +
telemetry: context/cache/cost/model/tools/traces) instead of herdr's
token-row list. The focused agent's card auto-expands. The legacy row view
stays behind a live config toggle. Full detail remains in
`herdr watcher sidebar`.

## 2. Non-goals

- Spaces section, compact rail (`sidebar_collapsed_mode = "compact"`):
  **byte-identical** — cards exist only in the expanded Agents section.
- A third Watcher section; ratatui embedding inside herdr chrome.
- A new sidebar focus model / card cursor / new keybindings — **deferred to
  M1.1** (operator decision 2026-08-24: expand-follows-focus first, full
  cursor model as follow-up).
- A new scroll model — the legacy entry-index scroll is kept; expansion is
  bounded to fit it (§4).
- Persisting selection/expand state across restarts.

## 3. Prerequisite: watcher state-stream API (v0.2.3)

Review HIGH-1: watcher v0.2.2 has **no public socket client** — connect /
subscribe / reconnect live as private functions in its `sidebar::tui`, and
`view::render` clamps to 34 columns and lets telemetry choose membership,
both incompatible with this spec. Before fork work, the **agent-watcher
repo** publishes a minimal state-stream client API, released as **v0.2.3**:

- constructor takes the **explicit socket path**;
- a worker owns subscribe + reconnect;
- delivery = folded snapshots (or raw lines into the already-public
  `sidebar::reducer::{State, apply_line}`);
- clean `shutdown()`/`join()` (same handle idiom as the daemon API).

**Card formatting is fork-owned** (resolves the 34-col clamp and
telemetry-membership problems, and LOW-1): the fork maps the watcher's
public `Role`/`Semantic` style values directly onto `AppState.palette`.
`src/ui/sidebar/tokens.rs` and the legacy path are untouched. The standing
latest-tag rule applies: the fork pins v0.2.3.

## 4. Data plane and card view

- **Membership + lifecycle are herdr-authoritative:** the card list comes
  from the same source as today's rows (`agent_panel_entries` after
  agent-view filtering); telemetry from the state socket only **enriches**
  cards. Socket absent/stale ⇒ lifecycle-only cards, no error chrome, never
  blocking render.
- **Collapsed card = 3 lines:** ① state glyph + agent name (+ state label
  when width allows); ② `workspace · cwd › task`; ③ context gauge +
  `N calls`. Telemetry-absent cards drop line ③'s gauge to `— no telemetry`
  dimmed text or omit gracefully — never a spinner.
- **Expanded card (exactly one, §6):** adds, in priority order, as much as
  fits the Agents **body height** (the existing per-entry height cap):
  model line → CONTEXT/CACHE/COST bars → TOOLS summary → TRACES (≤5).
  Content that does not fit is **dropped by reverse priority**, never
  clipped into an unreachable tail (review HIGH-3). Full detail =
  `herdr watcher sidebar`.
- **Adaptive width against the ACTUAL body rect** (review MEDIUM-2): the
  outer sidebar 18–36 cols yields card budgets of **16/17, 24/25, 34/35**
  (separator −1, scrollbar −1 when shown). Below ~32 budget the state label
  drops to the glyph; at the 16-col floor: line ① glyph+name, line ②
  truncated, line ③ gauge only. Nothing wraps; no layout shift on focus.

## 5. Ordering, sort, index parity

- Honor **`agent_panel_sort`**: `spaces` = the existing stable
  workspace-order **flat** list with the workspace named inside each card —
  **no group headers** (review HIGH-4: Agents has none; header machinery
  belongs to Spaces and is not reused). `priority` = attention order.
- **One cards-visible ordered source** built after agent-view filtering +
  panel sort + hide-idle, consumed by card render, mouse hit-testing,
  indexed `focus_agent`, and previous/next (review MEDIUM-1). **Hidden idle
  entries do not occupy `focus_agent` indexes** — matching the existing
  filtered-entries invariant codified in
  `next_agent_starts_at_first_visible_entry_when_focused_agent_is_filtered_out`.
  The `+N idle hidden` indicator renders outside the indexed sequence.

## 6. Interaction (v1: expand-follows-focus)

| Input | Effect |
| --- | --- |
| Click a card | focus that agent's pane (existing behavior) — its card auto-expands, all others collapse |
| `focus_agent` indexed / previous / next bindings | unchanged; the newly focused agent's card auto-expands |
| Click the `▸` zone on the **focused** card | toggle that card's expansion off/on |
| Everything else | existing sidebar / navigate keys, untouched |

- Exactly **one** card is ever expanded: the focused agent's (unless
  toggled collapsed). No section focus, no card cursor, no new keybindings
  in M1 — **M1.1 follow-up**: full cursor model (section focus, j/k/o/z on
  new prefix bindings, precedence over navigate mode).
- **Hide idle** is a config toggle in v1 (no `z` key until M1.1):
  `[ui.sidebar] agents_hide_idle = false` (default).

## 7. Config

```toml
[ui.sidebar]
# agents_view = "cards"      # default; "legacy" = the untouched row view
# agents_hide_idle = false   # cards mode only
```

- **Live** (review MEDIUM-3: `[ui]` already live-reloads via
  `App::apply_live_config`): both keys apply on reload; a cards↔legacy flip
  resets Agents scroll/expansion state.
- `legacy` renders today's rows byte-identically; `[ui.sidebar.agents]`
  rows config applies only there.
- In cards mode, a **present** rows config produces one diagnostic —
  presence detected from the **raw TOML** in both the startup and live
  loaders (defaults erase presence after deserialization; never compare
  materialized values to defaults).
- `agent_panel_sort` honored in both modes.

## 8. Compatibility invariants

- The swap is a render branch at the Agents-content delegation point
  (`render_agent_detail` call site, `src/ui/sidebar.rs` ~956–981) plus the
  input/hit-testing counterparts; card building, ingestion, and the ordered
  source live in additive modules. Legacy path = untouched upstream code.
- Every upstream edit: §4(b) notice + FORK.md registry row.
- Watcher changes limited to the §3 prerequisite release.

## 9. Risks

| Risk | Mitigation |
| --- | --- |
| `src/ui/sidebar.rs` upstream hotspot | branch + additive modules; merge surface stays small |
| Bounded expansion hides detail on short sidebars | deterministic priority-drop order; full pane one command away |
| Index parity drift | single ordered source + dedicated test incl. hide-idle |
| Auto-expand feels noisy | it tracks pane focus only; `▸` toggle opts out per card; M1.1 adds manual control |
| v0.2.3 API scope creep | client API only; formatting stays fork-side |

## 10. Acceptance

1. Cards render with live telemetry when a daemon runs; lifecycle-only when
   the socket is absent; render never blocks.
2. Focus (click / indexed / prev-next) auto-expands exactly the focused
   card; `▸` toggles it; both `agent_panel_sort` modes honored; hidden idle
   entries excluded from indexes with the indicator outside the sequence.
3. Card budgets verified at outer 18/26/36 with and without scrollbar
   (16/17, 24/25, 34/35); compact rail byte-identical; expanded content
   drops by priority, no unreachable tail.
4. `agents_view` and `agents_hide_idle` are live; cards↔legacy flip resets
   state; rows-config diagnostic fires only in cards mode and only on raw
   presence.
5. No behavior change outside the Agents section; nextest + clippy + CI
   green; FORK.md registry complete.
