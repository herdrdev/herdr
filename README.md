# Shepherd

A terminal-based runtime and orchestrator for coding agents.

Shepherd combines a high-performance terminal multiplexer with an in-process orchestrator for multi-agent workflows. It lets you monitor, coordinate, and steer coding agents across tasks and worktrees without taking over their native terminal sessions.

## Upstream Credit

Shepherd is a fork of [herdr](https://github.com/herdrdev/herdr) (base version 0.9.5), created by the herdr contributors and licensed under the [Apache License 2.0](LICENSE).
We build upon herdr's mouse-first terminal multiplexing, PTY management, session persistence, and agent screen detection.

## What Shepherd Adds

- **TUI Overlay (`prefix + a` or mouse menu)**:
  - **Tasks**: Create tasks, preview Markdown specs and acceptance criteria, edit specs in `$EDITOR` in a split pane, and archive completed tasks.
  - **Runs**: Track multi-phase task execution pipelines and approve human gates.
  - **Skills**: Skill catalog with global, role and per-session levels, shown in the overlay; default skills are protected from deletion.
  - **Git**: Read-only inspection of repository worktrees, branch statuses, dirty state, and linked agent identifiers.
- **Enhanced Agent Sidebar**:
  - Live agent status badges (`starting`, `working`, `idle`, `blocked`, `done`).
  - Multiline, non-truncated `[NEEDS YOU]` alerts when an agent requires human intervention (e.g., trust dialogs or logins).
- **Inter-Agent Communication (MCP & Bus)**:
  - In-process Model Context Protocol (MCP) server over loopback HTTP with per-agent tokens.
  - Message delivery for non-MCP agents: bus messages are formatted and typed into the terminal pane (`[shep] from <role>#<n>: ...`) once the pane is idle and quiet.
- **Unified CLI & Automation**:
  - Direct subcommands on `shepherd` (e.g., `shepherd agents`, `shepherd task new`, `shepherd runs list`, `shepherd git`).
  - Consistent `--json` output across all data-returning commands for headless workflows.
- **Headless Daemon**:
  - Orchestration service runs inside the background server and handles reconciliation, agent recovery on restart, and state persistence.

## Differences from herdr

- **Focus**: herdr is a terminal multiplexer for managing agent panes; Shepherd adds automated multi-agent coordination, tasks, and workflow gates.
- **Upstream Auto-Updates Disabled**: Auto-update checks against herdr release channels are disabled to prevent replacing the fork binary.
- **Workflow Gates**: Changes and git publishing require explicit human confirmation unless explicitly overridden.
- **Separate State & Config**: Stores configuration in `~/.config/shepherd` and per-session orchestration state under that session directory, with a read-only fallback to herdr's `config.toml`.

## Project Status

**Experimental**. Shepherd is functional for local development and multi-agent coordination, but is currently in active development. Binary distribution packages (Homebrew, installers) and public release channels are not yet published.

## Installation from Source

Shepherd must be built from source using Cargo:

```bash
git clone <repo-url>
cd shepherd
cargo build --release
```

The resulting binary will be at `target/release/shepherd`.

Requirements: Rust 1.96.1+ (or use `./cargo-fork.sh`) and Zig 0.16.0 (for ghostty-vt compilation).

## Configuration

Shepherd stores configuration at `~/.config/shepherd/config.toml` (or `~/.config/shepherd-dev/config.toml` in debug builds). If no configuration file exists at that path, Shepherd falls back to reading `~/.config/herdr/config.toml` in read-only mode.

To enable Shepherd's orchestration features, add the following to your `config.toml`:

```toml
[shepherd]
enabled = true
```

When disabled, Shepherd behaves as a standard terminal multiplexer.

## License

Shepherd is licensed under the [Apache License 2.0](LICENSE).
See [NOTICE](NOTICE) for copyright details and upstream attribution.
