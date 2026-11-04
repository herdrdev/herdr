# Upstream Synchronization Routine (herdr -> Shepherd)

This document describes the workflow and automation routine for bringing new versions, tags, and commits from the upstream repository (`herdrdev/herdr`) into Shepherd.

## Overview

Shepherd is a downstream fork of herdr that adds multi-agent orchestration (`crates/shepherd-core`, `src/shepherd/`, tasks, runs, skills, MCP, and a structured CLI).

Because Shepherd renames components from herdr to shepherd and adds orchestration hooks, pulling upstream changes requires a repeatable, automated procedure to re-apply renames and detect conflicts.

The routine is implemented by `scripts/upstream-sync.sh` and supported by `scripts/rename-from-herdr.sh`.

---

## Sync Workflow Steps

The routine follows these sequential steps:

1. **Verify State**:
   - Ensures the working tree is completely clean (no unstaged or untracked modifications).
   - Verifies that the current branch is `shepherd`.
2. **Create Temporary Sync Branch**:
   - Creates a dedicated branch named `sync/<tag>` (e.g. `sync/v0.9.6`).
3. **Fetch & Merge Upstream**:
   - Executes `git fetch upstream`.
   - Attempts a `git merge <tag-or-commit>` into `sync/<tag>`.
   - If merge conflicts occur, the process stops, lists the conflicting files, and provides instructions for manual resolution.
4. **Re-apply Automated Renames**:
   - Runs `./scripts/rename-from-herdr.sh` to adapt newly merged upstream code to Shepherd naming, environment variables (`SHEPHERD_*`), paths, and configurations.
   - Runs `./scripts/rename-from-herdr.sh --check` to ensure no unexcluded occurrences of `herdr` remain.
   - Commits any automated rename adaptations onto `sync/<tag>`.
5. **Build & Test Battery**:
   - Builds the workspace with `./cargo-fork.sh build`.
   - Runs the test battery by functional area:
     - `shepherd-core`
     - `client::shell`
     - `shepherd::`
     - `protocol::`
     - `api::`
     - `server::`
6. **Leave Branch Ready (No Push / No Merge)**:
   - Leaves `sync/<tag>` ready for final human review.
   - **Never pushes** to any remote.
   - **Never merges** automatically into `shepherd`.

---

## Running the Sync Script

### Previewing Steps (Dry Run)

To inspect what actions will be taken without touching git or modifying any files:

```bash
./scripts/upstream-sync.sh --dry-run v0.9.6
```

### Performing the Sync

Make sure your working tree is clean and you are on `shepherd`:

```bash
git checkout shepherd
./scripts/upstream-sync.sh v0.9.6
```

You may also specify a commit hash:

```bash
./scripts/upstream-sync.sh 1a2b3c4d
```

### Script Options

- `--dry-run`: Prints the planned commands without executing them.
- `--skip-build`: Skips `./cargo-fork.sh build`.
- `--skip-tests`: Skips the cargo test suite run.
- `--allow-branch`: Allows running the sync when on a branch other than `shepherd` (useful for testing).
- `--allow-dirty`: Bypasses the clean working tree check (use with caution).
- `--upstream <name>`: Custom remote name if different from `upstream` (default: `upstream`).

---

## Typical Expected Conflicts & Resolution

When merging new upstream changes, certain files and areas are known collision points:

### 1. Renamed & Relocated Files
- **Symptom**: Git reports modify/delete or rename conflicts on files that Shepherd moved or renamed (e.g., hooks in `src/integration/assets/shepherd-agent-*`, modules under `src/shepherd/`).
- **Resolution**:
  - Accept upstream's substantive changes into Shepherd's renamed file.
  - Delete any newly re-introduced obsolete files that were renamed.
  - Re-run `./scripts/rename-from-herdr.sh` to update identifiers.

### 2. `Cargo.lock`
- **Symptom**: Conflict markers inside `Cargo.lock` due to upstream dependency updates versus Shepherd's `shepherd-core` workspace crate.
- **Resolution**:
  - Accept upstream dependency version changes while keeping `crates/shepherd-core` declared in the workspace.
  - Re-generate `Cargo.lock` cleanly using `./cargo-fork.sh check` or `cargo update`.

### 3. Frozen Protocol Fixtures (`tests/fixtures/`)
- **Symptom**: Upstream changes tests or fixtures under `tests/fixtures/` (e.g. `endpoint-method-shapes-v1.json`, `endpoint-snapshot-v1.json`, `session/legacy-pre-tabs-v2.json`).
- **Resolution**:
  - Shepherd maintains Generation 1 client/server protocol compatibility contracts.
  - Do NOT modify frozen generation-1 fixtures to blindly match wire alterations.
  - Review `AGENTS.md` rules regarding stable client endpoints. If upstream introduced a new protocol method or field, ensure it is additive and negotiated properly.

### 4. Wire Contract & `PaneRightClickTarget`
- **Symptom**: Upstream modifies `src/api/schema/panes.rs` around right-click options (`pane.split` or `pane.input.set`).
- **Resolution**:
  - Shepherd preserves wire compatibility using:
    ```rust
    #[serde(rename = "herdr", alias = "shepherd")]
    ```
    This ensures older v1 clients and frozen wire fixtures remain valid while allowing `--right-click shepherd`.
  - Retain this serde attribute during resolution.

### 5. Disabled Auto-Updater (`src/update.rs`)
- **Symptom**: Upstream modifies the updater or telemetries in `src/update.rs`.
- **Resolution**:
  - Shepherd intentionally disables upstream update downloads from `herdr.dev` to protect users from having the fork binary overwritten by upstream releases.
  - Preserve Shepherd's update stubs in `src/update.rs`.

### 6. Configuration Fallback (`src/config/io.rs`)
- **Symptom**: Upstream alters configuration loading paths.
- **Resolution**:
  - Ensure Shepherd continues to look in `~/.config/shepherd` first and gracefully falls back to `~/.config/herdr` in read-only mode if the former does not exist.

---

## Verification & Testing Post-Merge

Once merge conflicts are resolved and renames are applied:

1. Run rename check:
   ```bash
   ./scripts/rename-from-herdr.sh --check
   ```
2. Build with the fork toolchain:
   ```bash
   ./cargo-fork.sh build
   ```
3. Run tests by area:
   ```bash
   ./cargo-fork.sh test -p shepherd-core
   ./cargo-fork.sh test client::shell
   ./cargo-fork.sh test shepherd::
   ./cargo-fork.sh test protocol::
   ./cargo-fork.sh test api::
   ./cargo-fork.sh test server::
   ```
   > **Note on Test Suite**: Running the entire workspace suite at once (`cargo test`) may terminate abruptly with `SIGPIPE` in certain terminal environments due to upstream PTY test harness behavior. Additionally, plugin and configuration diagnostic tests (`app::api::plugins::tests::manifest_action_invoke_injects_plugin_paths` and `config_diagnostic_summary_uses_compact_actionable_banner`) are known to be flaky when executed in parallel; they pass when run in isolation.

---

## Final Review & Integration

1. Inspect the diff between `shepherd` and `sync/<tag>`:
   ```bash
   git log -p shepherd..sync/<tag>
   ```
2. Run manual smoke tests using the isolated session runner:
   ```bash
   ./scripts/shepherd-iso.sh start
   # Verify overlay, sidebar, and CLI
   ./scripts/shepherd-iso.sh stop
   ```
3. When completely satisfied, merge into `shepherd` locally:
   ```bash
   git checkout shepherd
   git merge --no-ff sync/<tag>
   ```
4. Never push to `upstream`. Upstream remote push URL is permanently disabled.
