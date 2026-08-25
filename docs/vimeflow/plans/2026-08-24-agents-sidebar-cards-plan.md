# Agents Sidebar Cards — M1 Plan

**Spec:** `../specs/2026-08-24-agents-sidebar-cards-design.md` (v2 — read first)
**Review applied:** `../reviews/2026-08-24-m1-sidebar-cards-review.md` (all
HIGH/MEDIUM/LOW folded into spec v2)
**Repos:** this fork (`main`) and `~/projects/agent-watcher` (Task M-W only)
**Discipline:** conventional commits; §4(b) notice + FORK.md registry row
same-commit for upstream files; `cargo nextest` + clippy green before each
push; fork CI green at the end; stop and report on ambiguity. Standing rule:
pin the latest agent-watcher tag at the start of any fork work order.

## Task M-W — watcher state-stream client API (agent-watcher repo, FIRST)

Implements spec §3. Do not touch `src/agent/**` (PORT-SURFACE.md).

1. Public client: constructor takes the explicit socket path; a worker owns
   connect + subscribe + reconnect (extract from the private `sidebar::tui`
   functions — behavior identical for the standalone TUI, which becomes a
   consumer of the new API); delivery = folded snapshots or raw lines into
   the public `sidebar::reducer`; `shutdown()`/`join()` handle idiom.
2. Tests: fake state-socket server — subscribe, receive, reconnect after
   server restart, clean shutdown (no leaked threads/sockets); standalone
   TUI behavior unchanged.
3. cargo test + clippy green; bump BOTH Cargo.toml and herdr-plugin.toml to
   **0.2.3**; tag `v0.2.3`; push main + tag; mirror the release process.

## Task P0 — verify the review's map (fork, read-only, small)

The review already located everything; verify its coordinates still hold on
current main and additionally locate: (a) where pane-focus changes propagate
so the sidebar re-renders (the expand-follows-focus hook), (b) the
`render_agent_detail` delegation call site, (c) the mouse hit-test and
indexed/prev-next consumers of `agent_panel_entries`. Do not stop unless
something contradicts spec v2; fold findings into the P4 commit messages.

## Task P1 — config (fork)

`[ui.sidebar] agents_view = "cards"|"legacy"` (default cards) +
`agents_hide_idle = false`, both **live** via the existing
`App::apply_live_config` path; cards↔legacy flip resets Agents
scroll/expansion state; rows-config presence detected from the **raw TOML**
in startup AND live loaders, diagnostic only in cards mode. Tests: absent /
explicitly-default / custom rows config, live flip both directions,
hide-idle live change, diagnostic scoping.

## Task P2 — telemetry ingestion (fork, additive)

Bump the watcher pin to v0.2.3 (Cargo.lock + nix outputHashes; registry).
Additive module: the v0.2.3 client against
`plugin_paths::plugin_state_dir("herdr-agent-watcher")`, folding into a
non-blocking per-pane cache read by render. Absent/stale socket ⇒ `None`,
no blocking, no surfaced errors. Tests with a fake socket: updates land,
absence degrades, reconnect after daemon restart.

## Task P3 — card builder (fork, additive)

Pure card building per spec §4: collapsed 3-line contract (workspace named
in line ②), bounded expanded form with the priority-drop order
(model → gauges → tools → traces≤5) against a given body height, adaptive
rules against the ACTUAL body width, watcher public `Role`/`Semantic` →
`AppState.palette` mapping, `+N idle hidden` line outside the indexed items,
lifecycle-only variants. Table tests: card budgets 16/17, 24/25, 34/35 ×
states (idle/working/blocked/done) × telemetry present/absent ×
collapsed/expanded × several body heights (drop order verified).

## Task P4 — wire the swap (fork, the upstream edit)

1. Branch at the Agents-content delegation point: cards vs legacy (legacy
   path untouched).
2. **One cards-visible ordered source** (agent-view filter → panel sort →
   hide-idle) consumed by render, hit-testing, indexed `focus_agent`, and
   prev/next. Index parity test modeled on
   `next_agent_starts_at_first_visible_entry_when_focused_agent_is_filtered_out`,
   covering hide-idle.
3. Expand-follows-focus: focused agent's card expanded, others collapsed;
   `▸` zone click toggles the focused card; click card body focuses the
   pane. Entry-index scroll keeps working because every entry fits the body
   (bounded expansion).
4. `agent_panel_sort` both modes: spaces = stable workspace-order flat list,
   priority = attention order.
5. §4(b) notices + FORK.md registry rows for every upstream file touched
   (sidebar.rs, input/sidebar.rs, input/mouse.rs, config files, app state).

## Task P5 — close-out (fork)

1. Integration coverage where feasible (fake-socket end-to-end), plus the
   operator live-verification checklist in the final report: real fork run —
   focused claude/codex card expands with gauges; kill daemon →
   lifecycle-only; `agents_view = legacy` live-flip restores today's view;
   both sorts; indexed focus with hide-idle on; 18-col narrow drag; compact
   rail unchanged.
2. `docs/next/` user docs update; FORK.md registry complete; nextest +
   clippy + CI green.
3. Record the M1.1 follow-up (full card-cursor model) as a one-paragraph
   stub at the bottom of this plan.

## Execution order

M-W first (tag gates P2). Then P0 → P1 → P2/P3 (parallelizable) → P4 → P5.
Stop and report after M-W, then run P0–P5 straight through unless a
contradiction appears.

## Follow-up stub — M1.1 card cursor (not in this plan)

Section focus state + card cursor + new prefix bindings for j/k/o/z +
precedence over navigate mode; manual expansion of non-focused cards;
`z` key replacing the hide-idle config round-trip. Requires its own
spec pass.
