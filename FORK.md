# Vimeflow terminal fork

This repository is an Apache-2.0 tracking fork of
[`herdrdev/herdr`](https://github.com/herdrdev/herdr). It preserves Herdr's
license and notices while carrying the native Vimeflow terminal work on a
separate product branch.

## Fork base

- Upstream repository: `https://github.com/herdrdev/herdr`
- Fork repository: `https://github.com/winoooops/vimeflow-terminal`
- Base tag: `v0.8.0`
- Base commit: `346411fa21afd297f5ed3b3fa56f9e3fbf7654b7`
- Bootstrap date: 2026-08-18

## Branch model

- `master` is a pristine, fast-forward-only mirror of `upstream/master`. Never
  commit fork changes there.
- `main` is the default product branch. It began at `v0.8.0` and carries all
  Vimeflow changes.
- Upstream releases are merged into `main` through review branches and pull
  requests, once per release rather than once per upstream commit.

`origin` points to the fork and `upstream` points to `herdrdev/herdr`.

## Baseline

Rechecked on macOS arm64 with Rust/Cargo 1.96.1 on 2026-08-18:

| Command | Exit | Result |
| --- | ---: | --- |
| `cargo build --locked` with a cold Zig dependency cache | 101 | Zig 0.15.2 still received HTTP `400 Bad Request` for the themes tarball even though `curl` fetched the same URL with HTTP 200. This is local Zig-fetch behavior, not a dead artifact. |
| `cargo build --locked` after pre-seeding that tarball into Zig's package cache | 0 | Herdr built successfully. The fork did not modify the vendored dependency or build script. |
| Focused stock-announcement regression test | 0 | 1 passed, 0 failed. Stock manifest announcements remain disabled; the existing local-preview test remains intact. |
| Focused graphics cancellation test, 20 isolated runs | 0 | 20 passed, 0 failed. See the upstream baseline flake below. |
| `cargo test --locked` | 101 | 2936 passed, 2 failed: the untouched upstream graphics race and the raw-harness workspace-ID counter test described below. The announcement regression and all collateral `PoisonError` failures are gone. |

The cold-cache HTTP failure reproduces from the unmodified `v0.8.0` base. The
vendored dependency was not changed. CI fetches the artifact successfully on
both operating systems; macOS CI uses upstream's patched Homebrew Zig setup.

For a cold local Zig cache, run `scripts/preseed_zig_cache.sh` before
`cargo build --locked`. The script downloads the themes tarball and other
archives that Zig 0.15.2 fails to fetch locally, imports them into Zig's global
package cache, and rejects content hashes that no longer match the vendored
`build.zig.zon` manifests.

Upstream already shipped a macOS/Linux CI matrix, but its push trigger omitted
the fork's `main` branch. `.github/workflows/fork-ci.yml` supplies the requested
`cargo build --locked` plus the upstream `cargo nextest` test harness for pushes
and pull requests targeting `main` without modifying the upstream workflow.

Local Rust tests follow upstream CI: use `cargo nextest run --locked` on Linux
and `cargo nextest run --locked -E 'not binary(live_handoff)'` on macOS, where
upstream CI excludes that platform-sensitive integration binary. Raw
`cargo test` is not the project baseline because its shared-process harness
triggers the two known upstream failures below.

### Upstream baseline test behavior

- `api::server::pane_graphics_stream::tests::inactive_owner_cancels_idle_stream_and_dispatches_close`
  is order/timing dependent on slow raw-`cargo test` runners. At v0.8.0 the
  server writes the open acknowledgement before registering the stream, so the
  test thread can request cancellation before registration and then time out.
  Fork CI runs it under upstream's isolated nextest harness and retries exactly
  this test up to two times via `.config/nextest.toml`; fork code does not patch
  the graphics implementation or test. Two consecutive CI runs demonstrated
  that process isolation reduced but did not eliminate the race.
- `workspace::tests::generated_workspace_ids_are_short_base32_handles` assumes
  a fresh global workspace-ID counter. Raw `cargo test` shares that counter
  across the 2938-test unit-test process and can exceed the asserted two-digit
  range before this test runs. Upstream CI uses nextest's per-test processes,
  which preserve the test's intended isolation; fork CI now does the same.

## Upstream-edit registry

Apache-2.0 section 4(b) changes to upstream source files carry this notice at
the top of the file:

```rust
// Modified from herdr by the vimeflow project — see FORK.md
```

| Upstream path | Reason | Fork commit |
| --- | --- | --- |
| `src/update.rs` | Prevent hosted Herdr manifest fetches, self-update installs, and background update checks in the fork. | `faf956e9b815045ca114d89b7faf9534386e0e8b` |
| `src/cli.rs` | Reject fork-disabled update channels, dispatch the native watcher CLI, and share its coexistence diagnostics with server startup. | `dded4c73`, `b3aff323` |
| `src/product_announcements.rs` | Ignore announcements delivered through stock Herdr update manifests while retaining local preview support. | `faf956e9b815045ca114d89b7faf9534386e0e8b` |
| `src/app/mod.rs` | Align the stock-manifest startup test, register native title sync, apply live Agents-card settings, and initialize the watcher telemetry cache. | `e312ccf8`, `e006a1ea`, P1 (this commit), P2 (this commit) |
| `src/app/actions.rs` | Trigger coalesced title recomputation and reset focused-card collapse state on pane focus changes. | `e006a1ea`, P4 (this commit) |
| `src/app/input/navigate.rs` | Keep indexed and relative Agent focus aligned with the cards-visible order. | P4 (this commit) |
| `src/app/input/mouse.rs` | Focus Agent cards and toggle the focused card from its chevron zone. | P4 (this commit) |
| `src/app/input/sidebar.rs` | Use card-aware body geometry for Agent hit testing. | P4 (this commit) |
| `src/app/api/panes.rs` | Route pane-label API mutations through the shared title ownership and event helper. | `e006a1ea` |
| `src/app/state.rs` | Track title-sync generations, the live Agents-card presentation state, and non-blocking watcher telemetry snapshots. | `e006a1ea`, P1 (this commit), P2 (this commit) |
| `.gitignore` | Whitelist `docs/vimeflow/` (fork specs/plans) alongside upstream's docs whitelist entries. | `f8229b2e` |
| `docs/next/website/src/content/docs/agents.mdx` | Document the built-in watcher, automatic agent titles, startup-only configuration, and standalone-plugin migration. | `18a6a734` |
| `docs/next/website/src/data/config-reference.json` | Add the native agent watcher and title-sync configuration keys to the generated user reference snapshot. | `18a6a734` |
| `Cargo.toml` | Add the Unix-only `herdr-agent-watcher` runtime dependency, now pinned to `v0.2.3`, plus direct SQLite access for OpenCode titles. | `ebff8667`, `4df9fb1a`, P2 (this commit) |
| `Cargo.lock` | Lock the watcher tag and direct SQLite reader dependency. | `ebff8667`, `4df9fb1a`, P2 (this commit) |
| `nix/package.nix` | Supply the fixed-output hash for the watcher Git dependency. | `ebff8667`, P2 (this commit) |
| `src/api/schema/tests.rs` | Keep the generated schema canonically ordered when the watcher enables `serde_json/preserve_order`. | `502f2f6b993e62e99ad98b97a71e813a0e258bc3` |
| `src/config/model.rs` | Add startup-only native watcher and title-sync configuration sections. | `c3c70979` |
| `src/config/io.rs` | Recognize native feature sections and diagnose legacy Agents-row settings from raw startup/live TOML. | `c3c70979`, P1 (this commit) |
| `src/config.rs` | Export the fork's Agents-card view configuration type. | P1 (this commit) |
| `src/config/sidebar.rs` | Add live Agents-card view and idle-filter settings beside legacy row configuration. | P1 (this commit) |
| `src/server/headless.rs` | Own the embedded watcher, telemetry ingestion, and title-sync lifecycles across normal and handoff server paths and warn about enabled standalone twins. | `dd08df50`, `e006a1ea`, `b3aff323`, P2 (this commit) |
| `src/cli/spec.rs` | Describe the native watcher command group and its supported subcommands. | `dded4c73` |
| `src/main.rs` | Register the Unix-only native title-sync and Agent-card modules. | `f4af78b5`, P2 (this commit) |
| `src/events.rs` | Return blocking title-reader results to the server thread for identity-checked application. | `e006a1ea` |
| `src/ui.rs` | Export shared Agent-card geometry to sidebar input handling. | P4 (this commit) |
| `src/ui/sidebar.rs` | Delegate Agents content to adaptive cards while preserving the legacy and compact-rail renderers. | P4 (this commit), P5 (this commit) |

Non-commentable modified files must also be listed in `MODIFICATIONS` beside
`LICENSE`.

## Upstream merge procedure

For each upstream release:

1. Fetch `upstream` and tags, then fast-forward local `master` to
   `upstream/master` and push that mirror to `origin/master`.
2. Create `sync/vX.Y.Z` from `main` and merge the signed or verified upstream
   release tag into it with a merge commit.
3. Compare changed upstream paths with the registry above. Resolve conflicts
   explicitly, retain the in-file notice on every fork-modified upstream
   source file, and update the registry and `MODIFICATIONS` in the same PR.
4. Run the macOS/Linux build-and-test matrix and review the complete diff.
5. Merge the sync PR into `main`; never auto-resolve conflicts or commit fork
   work directly to `master`.

## Deferred branding rename surface

The M0b bootstrap intentionally keeps the `herdr` binary/CLI name, socket and
state/config paths, environment variables, and command grammar so existing
Tier-1 plugins and operator workflows remain compatible.

A later, separately specified branding pass must inventory and migrate:

- Cargo package, binary, release asset, installer, and package-manager names;
- user-facing Herdr strings, help text, documentation, icons, and logos;
- socket/session identifiers, config/state/cache paths, and `HERDR_*`
  environment variables;
- plugin command contracts and a compatibility alias/migration period.

## M0b boundary note

`src/remote/unix.rs` also references Herdr-hosted manifests to provision a
matching binary on a remote host. It does not update the local fork and was
left untouched because M0b explicitly scopes neutralization to `src/update.rs`,
channel machinery, and product announcements. Reassess that remote workflow
before enabling remote execution in the fork.
