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

Recorded on macOS arm64 with Rust/Cargo 1.96.1:

| Command | Exit | Result |
| --- | ---: | --- |
| `cargo build` | 101 | Failed before compiling Herdr: the vendored `libghostty-vt` Zig build received HTTP `400 Bad Request` for `ghostty-themes-release-20260629-161812-8c97c3c.tgz`. |
| `cargo test` | 101 | Same build-script failure; 0 tests ran (0 passed, 0 failed). |

This failure reproduces from the unmodified `v0.8.0` base and is the fork's
pre-existing bootstrap baseline. The vendored dependency was not changed.

Upstream already shipped a macOS/Linux CI matrix, but its push trigger omitted
the fork's `main` branch. `.github/workflows/fork-ci.yml` supplies the requested
`cargo build --locked` and `cargo test --locked` matrix for pushes and pull
requests targeting `main` without modifying the upstream workflow.

## Upstream-edit registry

Apache-2.0 section 4(b) changes to upstream source files carry this notice at
the top of the file:

```rust
// Modified from herdr by the vimeflow project — see FORK.md
```

| Upstream path | Reason | Fork commit |
| --- | --- | --- |
| `src/update.rs` | Prevent hosted Herdr manifest fetches, self-update installs, and background update checks in the fork. | `faf956e9b815045ca114d89b7faf9534386e0e8b` |
| `src/cli.rs` | Reject update-channel changes before config writes or self-update dispatch. | `faf956e9b815045ca114d89b7faf9534386e0e8b` |
| `src/product_announcements.rs` | Ignore announcements delivered through stock Herdr update manifests while retaining local preview support. | `faf956e9b815045ca114d89b7faf9534386e0e8b` |

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
