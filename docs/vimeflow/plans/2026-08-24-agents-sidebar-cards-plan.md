# Agents Sidebar Cards — M1 Plan

**Spec:** `../specs/2026-08-24-agents-sidebar-cards-design.md` (read first)
**Repo:** this fork, branch `main`
**Discipline:** conventional commits; §4(b) notice + FORK.md registry row
same-commit for upstream files; `cargo nextest` + clippy green before each
push; fork CI green at the end; stop and report on ambiguity — especially
where sidebar internals contradict the spec's assumptions. Standing rule:
check for a newer agent-watcher tag first and bump the pin if one exists.

## Task P0 — discovery (read-only, report before coding)

Map and record (a short section appended to this plan file is fine):

1. The Agents section content render path in `src/ui/sidebar.rs` — where rows
   are built, where `agent_panel_sort` is applied, where `spaces` grouping
   headers come from.
2. Click hit-testing for agent rows (mouse → focus) and where the indexed
   `focus_agent` bindings resolve their order — confirm both read one ordered
   source (§5 index parity), and what happens to indices when entries are
   filtered.
3. How sidebar section focus + key handling work (where j/k-style keys would
   hook when the Agents section is focused) and what the existing
   config-reload path does for ui toggles (decides `agents_view` live vs
   startup-only).
4. Which watcher-crate modules are the right pure reuse surface for card
   lines (reducer/format/layout/live) and what the state-socket client
   exposes for a non-blocking cache.

**Stop after P0** with the findings; flag anything that contradicts the spec.

## Task P1 — config toggle

`[ui.sidebar] agents_view = "cards" | "legacy"` (default cards) in config
model/io + app state; startup diagnostic when rows config is present in cards
mode; reload semantics per P0 finding. Tests: default, override, diagnostic
fires only in cards mode, reload behavior.

## Task P2 — telemetry ingestion (additive module)

Non-blocking watcher-socket subscription + per-pane cache using the crate's
client/live surface against `plugin_paths::plugin_state_dir(...)`; absent or
stale socket ⇒ `None` telemetry, no blocking, no errors surfaced to render.
Tests with a fake state-socket server: live updates land in the cache;
absent socket degrades; reconnect after daemon restart.

## Task P3 — card line builder (additive module)

Pure card building: 3-line collapsed contract, inline expanded form (model,
CONTEXT/CACHE/COST bars scaled to width, TOOLS summary, TRACES cap 5),
hide-idle with `+N idle hidden`, adaptive rules incl. the 18-col floor,
selection styling without layout shift. Reuse crate pure selectors; output =
herdr sidebar primitives / `sidebar/tokens.rs` styles. Table tests at
18/26/36 cols across states (idle/working/blocked/done, telemetry
present/absent, selected/expanded).

## Task P4 — wire the swap (the upstream edit)

1. One branch at the Agents-section content render in `src/ui/sidebar.rs`:
   cards (new modules) vs legacy (existing code path untouched).
2. `agent_panel_sort` honored in cards mode: `spaces` grouping headers reuse
   the existing header machinery; `priority` = attention order.
3. Click hit-testing over cards → focus pane; **index parity test**: card
   order == `focus_agent` resolution order, including the hidden-idle rule
   found in P0.
4. Section-focused key handling: j/k/o/↵/z per spec §6, existing keys
   untouched otherwise.
5. §4(b) notices + FORK.md registry rows for every upstream file touched.

## Task P5 — close-out

1. Integration coverage: cards+telemetry end-to-end with the embedded daemon
   in a test server where feasible, else the P2 fake-socket path plus a
   documented operator live-check.
2. Operator live-verification checklist (real fork run: cards show a live
   claude/codex agent with gauges; kill the daemon → lifecycle-only; toggle
   legacy → today's view; sort modes; indexed focus; 18-col narrow drag).
3. `docs/next/` user docs draft (per repo convention, not root README);
   FORK.md registry complete; nextest + clippy + CI green.

## Execution order

P0 (stop + report) → P1 → P2/P3 (parallelizable, both additive) → P4 → P5.
