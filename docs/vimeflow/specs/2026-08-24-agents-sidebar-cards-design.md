# Agents Sidebar Cards — M1 Design

**Date:** 2026-08-24
**Status:** Approved direction (operator decisions 2026-08-20); pending one
codex review pass
**Parent:** vimeflow pivot spec (M1) — **this spec formally supersedes the
pivot spec's 3-section sidebar shape**: the operator chose to REPLACE the
built-in Agents section's content with the agent-watcher card view instead of
adding a third Watcher section. Rationale: materially smaller upstream diff
(no new section machinery, no `sidebar_section_split` persistence migration)
and one surface instead of two overlapping ones.

## 1. Summary

The fork's sidebar **Agents section** renders the agent-watcher **card view**
instead of herdr's token-row list. Cards show lifecycle + telemetry
(context/cache/cost/model/tools/traces) per agent, with inline expand,
Vim-style keys, and graceful degradation when telemetry is absent. The
legacy row view remains available behind a config toggle.

## 2. Non-goals

- The Spaces section: untouched.
- A third Watcher sidebar section: never.
- The full ratatui watcher pane (`herdr watcher sidebar`): unchanged; it stays
  the full-detail view.
- Embedding ratatui widgets inside herdr's chrome renderer: no — reuse the
  watcher crate's **pure** logic only, draw through herdr's own sidebar
  primitives.
- The Dynamic Island (M2), branding, Windows.
- Persisting card selection/expand state across restarts (ephemeral in v1).

## 3. Data plane: merge, degrade

- **Lifecycle (authoritative, always present):** herdr core App state —
  agent kind/name, state (idle/working/blocked/done/unknown), focus,
  workspace/tab location. In-process, zero latency.
- **Telemetry (enrichment):** the watcher state socket at
  `plugin_paths::plugin_state_dir("herdr-agent-watcher")` — context %, cache,
  cost, model, tool calls, traces — read via the watcher crate's existing
  client/live modules with a **non-blocking** subscription + cache keyed by
  pane. Works against whichever daemon holds the singleton (embedded or
  plugin), per the Scope 2 contract.
- **Degradation:** socket absent/stale ⇒ cards render lifecycle-only (no
  gauges, no error chrome, no placeholder spinners). Rendering never blocks
  on socket I/O.

## 4. Card view

- **Collapsed card = 3 lines** (M0a-4 contract): ① state glyph + agent name +
  state label; ② `cwd › task` (task = pane label/terminal title); ③ context
  gauge + `N calls`.
- **Adaptive width** (sidebar is 18–36 cols): below ~34 cols the state label
  drops to the glyph alone (M0a-4 rule); at the **18-col floor**: line ① =
  glyph + name only, line ② = truncated cwd, line ③ = gauge only. Nothing
  wraps; selection changes styling, never layout.
- **Inline expand** (decision 2026-08-20): `o`/`↵` expands the card in place
  within the sidebar scroll — model line, CONTEXT/CACHE/COST bars scaled to
  width, TOOLS summary, TRACES capped at 5. Collapse with the same key.
- **Hide idle:** `z` toggles hiding idle agents with a `+N idle hidden`
  indicator (M0a-4 behavior).
- **Rendering path:** an additive card module builds styled lines by reusing
  the watcher crate's pure reducer/format/layout selectors, then draws via
  herdr's sidebar text/draw primitives and `sidebar/tokens.rs` styling.

## 5. Sort, grouping, and index parity

- Honor **`agent_panel_sort`** (decision 2026-08-20): `spaces` = grouped by
  workspace under the existing headers; `priority` = attention queue
  (blocked first). The watcher's activity ordering maps onto `priority`.
- **Index parity invariant:** the visible card order IS the order the
  indexed `focus_agent` bindings resolve — cards and bindings must read the
  same ordered source. Hidden-idle cards still occupy their binding index or
  the bindings re-resolve consistently — executor verifies which rule herdr
  uses today and keeps it.

## 6. Interaction

| Input | Effect |
| --- | --- |
| Click card | focus that agent's pane (existing Agents-section behavior) |
| `focus_agent` indexed bindings | unchanged, resolve per §5 |
| Section focused: `j`/`k` | move card selection |
| Section focused: `o`/`↵` | expand/collapse selected card |
| Section focused: `z` | toggle hide-idle |
| Everything else | herdr's existing sidebar/navigate keys, untouched |

No new global keybindings.

## 7. Config

```toml
[ui.sidebar]
# agents_view = "cards"   # default; "legacy" restores the token-row view
```

- **`legacy`** renders today's row view byte-identically; `[ui.sidebar.agents]`
  rows/rows_by_agent config applies only there.
- In **`cards`** mode a present rows config produces one startup diagnostic
  ("superseded by agents_view = cards"), not an error.
- `agent_panel_sort` is honored in both modes.
- Reload semantics: follow the sidebar's existing config-reload behavior for
  ui toggles — do not invent new machinery; if the existing path makes
  `agents_view` live, it is live, otherwise startup-only and documented.

## 8. Compatibility invariants

- Upstream edits concentrate on the **section-content branch point** in
  `src/ui/sidebar.rs` (+ config model/io, app state for the toggle, input
  hit-testing/keys). Card building lives in additive module(s). Every
  upstream edit: §4(b) notice + FORK.md registry row.
- The legacy path is untouched upstream code — the toggle branches around
  it, never rewrites it.
- No changes to the watcher crate required; if one becomes necessary, stop
  and report (new tag ceremony per the standing latest-tag rule).

## 9. Risks

| Risk | Mitigation |
| --- | --- |
| `src/ui/sidebar.rs` is a 2.8k-line upstream hotspot | one branch at the section render + additive card modules keep the merge surface a few lines |
| Socket coupling to crate internals | crate is pinned same-repo family; client/live modules are the supported surface |
| 18-col floor is new design territory | floor contract fixed in §4; table-tested at 18/26/36 cols |
| Legacy toggle doubles render paths | legacy = untouched upstream code, zero maintenance |
| Index parity drift (focus_agent vs cards) | §5 invariant + a dedicated test |

## 10. Acceptance

1. Cards render with live telemetry while a daemon runs; lifecycle-only when
   the socket is absent; no render blocking either way.
2. Click + indexed focus work; both `agent_panel_sort` modes honored.
3. j/k/o/↵/z behave per §6; narrow renders per §4 at 18/26/36 cols.
4. `agents_view = "legacy"` restores today's view byte-identically; rows
   config diagnostic fires only in cards mode.
5. No behavior change outside the Agents section; nextest + clippy + CI
   green; FORK.md registry complete.
