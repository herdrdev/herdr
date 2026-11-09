# Agent brief: Shepherd in Rust, stage R1

Read `docs/rust-architecture.md` and `docs/rust-migration.md` first. You work in the current worktree (a branch derived from `rust-core`). You implement **only stage R1**, in `crates/shepherd-core`, with TDD.

## Rules (mandatory)

- You only edit `crates/`, `Cargo.toml`, `Cargo.lock`, `docs/` and `Makefile`. **Do not touch the Go code.** Go is the behavior reference: read it, do not change it.
- Never run `git push`, never create PRs, never change branches outside your worktree. Small commits with explicit paths (`git add <paths>`; never `git add -A`), each one with `cargo test --workspace` green.
- Never run against the real system: `hc kill|rm|keep|server|update|machine`, `shepherd-app do`, or anything that creates, kills or changes terminals, agents or the user's app. **Read-only** commands (`hc --help`, `hc <cmd> --help`, `hc ls --json`) may be used to find out formats; do not copy third-party project names or paths seen in their output into the repo. The `hc` adapter tests use a **fake `hc`** (a script in a temporary directory placed first on `PATH`).
- Do not invent the shepherd/hc API: verify it (`hc --help`, `hc <cmd> --help`, `~/.config/shepherd-app/shepherd.d.ts`, `internal/shepherd/schema` and `docs/vendors.md`). Mark anything you cannot verify in `docs/rust-architecture.md` as **unverified**.
- Dependencies: the minimum; avoid adding crates you do not need. If you add one, justify it in the commit message.
- Never answer trust or login dialogs on behalf of the human; never expose secrets or tokens in logs or in `--json` (an agent's token is not printed).
- Do not mark anything as done without having run `cargo test --workspace` and read its output.

## R1 scope (in this order; one commit per item, tests included)

1. **`trait Terminals`** (module `terminals`): open an agent in a new terminal inside a worktree (with env), prompt, send text and keys, read the screen, wait, agent status, label, close, attach. Refactor `shepherd.rs` + `spawn.rs` + `workflow.rs` to depend on the trait, not on the concrete client.
   - `ShepherdSocket`: the existing code, behind the trait, with no change in behavior.
   - `HcCli`: new; runs `hc` (`run --bg`, `send`, `keys`, `read`, `wait`, `ls --json`, `label`, `kill`, `attach`). Discover the real formats with `--help` and document the trait→command mapping in `docs/rust-architecture.md`.
   - An in-memory fake for the core tests, and a fake `hc` (script) for the adapter tests.
   - Adapter selection by configuration (`shepherd.backend: socket|hc`, default `socket`) and by the `SHEPHERD_TERMINALS` variable.
2. **`--json`** on `agents`, `task list`, `task show`, `roles list`. Stable schema documented in `docs/rust-json-schema.md`; it never includes tokens. Tests that pin the schema.
3. **Missing MCP tools** (today there are `submit_result`, `get_task_context`, `read_messages`, `send_message`, `get_diff`, `update_task_context`): `spawn_agent`, `wait_for`, `get_result`, `list_agents`, `read_output`, `ask_docs`, `get_workspace_instructions`. Behavior and permissions as in `internal/daemon/mcp.go` (for example, only brain and docs update the task context; depth and concurrent-agent limits).
4. **Skills** (module `skills`), porting `internal/skills` (`catalog.go`, `effective.go`, `install.go`, `manage.go`) and their tests: catalog with priority repo > user > embedded, global/role/session levels (`SkillAdjust`), `effective_for`, edited-copy detection, `SKILL.md` validation, and per-session installation for the `claude-plugin`, `codex-developer`, `worktree-file` and prompt styles. The embedded skills live in `skills/` of the Go repo; include a way to read them without duplicating them by hand (for example `include_dir`-like with a generated `include_str!`, or copy them to `crates/shepherd-core/skills` with a test that checks they do not diverge).

## Definition of done

- `cargo test --workspace` green, with new tests for every item.
- `cargo clippy --workspace` with no new errors if clippy is installed.
- Final list in `docs/rust-architecture.md` (section "R1 status") with what was done, what is unverified and what remains. If you get blocked on an item, leave the previous ones complete, document the blocker and stop.
