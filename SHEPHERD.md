# Shepherd as a fork of herdr

Decision of the owner (2026-11-03): **Shepherd is a fork of herdr**, "a herdr with more features", not a separate program that calls herdr. This replaces the earlier idea of an external `shepherd-core` + adapter (kept as library code to be ported in).

Base: `herdrdev/herdr` 0.9.5 (Apache-2.0), the terminal UI. The graphical `herdr-app` is not in that repository, so it is not part of this fork. Credit to herdr stays in the README and NOTICE; no upstream issues, PRs or pushes (the upstream `AGENTS.md` forbids them for forks). The `upstream` remote has its push URL disabled on purpose.

## Product

herdr's runtime (PTYs, sessions, workspaces, panes, agent detection, API server, mouse-first TUI) stays as is, **including watching and talking to an agent directly in its native terminal pane**, which the owner prefers over any view of our own. Shepherd adds, in-process:

- roles, vendors and routing (soft `must_differ_from`, single-vendor mode), limits, the agent tree;
- task context, plans, the plan > code > review > PR workflow with human gates (push and PR always wait for a person);
- an MCP server so agents talk to each other (13 tools);
- skills (global, role, per-session) injected per session, never written into the user's repo;
- a git tree/worktree/PR view and safe branch operations;
- panels for all of that inside the herdr TUI, and the same functions as `shepherd` CLI subcommands over herdr's API socket (headless use).

## Phases

| Phase | What | Needs |
|---|---|---|
| F0 | Baseline: build and test upstream unchanged in this clone | Rust 1.96.1 (installed: 1.93.1) and Zig (for `ghostty-vt`): **install needs the owner's OK** |
| F1 | Add `crates/shepherd-core` (the R1 library: tasks, plans, registry, bus, skills, MCP, workflow) as a workspace member; replace its `Terminals` adapters by an in-process one over herdr's `AppState`/`PaneRuntime` | F0 |
| F2 | Orchestration services in the app runtime: spawn into worktrees, reconcile, first-prompt delivery with verification, resume after restart, "needs you" notices, clean-up, MCP server | F1 |
| F3 | UI: agents tree, tasks, runs, skills and git panels in the TUI, following herdr's UI patterns (mouse-first, no god objects) | F2 |
| F4 | API methods and `shepherd` CLI subcommands; JSON output | F2 |
| F5 | Branding (the "herdr" name is not licensed by Apache-2.0), README credit, NOTICE, docs, an upstream rebase routine | owner decision on name |

## Rules inherited from upstream (they apply to forks)

State is separate from runtime (`AppState` is pure data); render is pure; no god objects; platform code isolated under `src/platform/`; render/layout loops are performance-critical (profile with `just bench-render-scale` when touching them). Read the upstream `AGENTS.md` before any change.

## Carried over from the Go and R1 work (requirements, not suggestions)

- A prompt accepted by a terminal can be lost while a CLI starts: verify the agent reacted, resend up to 3 times.
- A CLI sitting on a trust/login dialog never accepts its first prompt: mark it "needs the human" and **never answer the dialog for them**.
- Agent ids and the spawn counter never go backwards; names are unique among open panes (reclaim only shepherd's own leftovers).
- `done` = finished its turn, pane alive; only `exited`, `killed`, `failed` are final.
- Editing a skill's text reaches a running agent; adding or removing one needs a restart.
- Push and PR always wait for an explicit human approval unless `--auto-push` for that run.
