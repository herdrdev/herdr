# Porting From Herdr Upstream

This file records fork-specific facts used when rebasing onto `herdrdev/herdr`.
The rebase procedure lives in `.agents/skills/rebase-upstream-master/SKILL.md`.

Repository identity: `origin` is `trungnt13/herdr`, `upstream` is
`herdrdev/herdr`, and the canonical branch is `master`.

## Last integrated upstream marker

| Field | Value |
| --- | --- |
| Commit | `cc88b3b8e5bb9f7d9f23ed6ae85a52fd7b5b9ed6` |
| Date | 2026-09-02 |
| Subject | `feat: add stable client endpoint compatibility (#3509)` |

This is a historical marker, not the current `upstream/master`. Advance it only
after a complete upstream rebase and all required validation succeed.

## Fork deltas relative to the marker

| Path | Fork delta and reason |
| --- | --- |
| `.gitignore` | Ignores `.vscode` and exposes this guide inside the otherwise ignored `/docs/*` tree. |
| `.agents/skills/herdr-release/SKILL.md` | Adds the fork release workflow. |
| `.agents/skills/rebase-upstream-master/SKILL.md` | Adds the safe upstream rebase workflow. |
| `.github/MAINTAINERS` | Adds `trungnt13`. |
| `AGENTS.md` | Recognizes verified fork maintainer authority and limits releases to manually installed GitHub prereleases; preserves universal runtime rules. |
| `justfile`, `scripts/fork_release.py`, `scripts/test_fork_release.py` | Strict fork versions, verified publication destination, reservation checks and focused maintenance tests. |
| `.github/workflows/release.yml` | Four Linux/macOS builds, patched macOS Zig, checksums and staged prereleases; removes upstream promotion jobs. |
| `.github/workflows/build-artifacts-manual.yml` | Uses patched Homebrew Zig 0.15.2 on macOS; preserves Windows builds. |
| `.github/workflows/website-deploy.yml`, `.github/workflows/label-next-release-issues.yml` | Restricts upstream automation to `herdrdev/herdr`. |
| `docs/porting-from-herdr-upstream.md` | Records fork deltas, unresolved decisions, and completed rebase results. |

No product runtime, Cargo version, update destination or published docs changes are part of this tooling migration.

## Release intent

Fork releases contain the latest `upstream/master` changes. Without an explicit
version, use the latest upstream stable version plus the next `+fork.N` suffix,
for example `0.8.2+fork.1`, then `0.8.2+fork.2`.

## Resolved tooling and governance

The release recipes accept strict `BASE+fork.N` and verify fork identity,
write access and reserved versions. Fork maintainer authority is scoped to
`trungnt13/herdr`; it grants no upstream permission. Release CI stages four
Linux/macOS binaries and deterministic checksums, then publishes a prerelease
without moving latest or promoting upstream services.

## Remaining release prerequisites

Fresh upstream integration and explicit approval are still required before
release commits or publication. At migration review, upstream stable is `v0.9.0`
while fork Cargo remains `0.8.2`; no version is selected or bumped here. Requery
upstream stable and master immediately before release, integrate them, and
reconcile the Cargo base before selecting the next fork revision.

The existing release-docs check fails because the distribution catalog lacks
the bundled `muse` agent. Resolve release readiness separately; do not weaken
checks. Cross-platform builds and actual upload/publication require CI validation.

Updater independence is deferred: runtime updates still target upstream and
SemVer metadata does not order fork revisions. Releases must warn users not to
run `herdr update` for this fork and to install GitHub prerelease assets manually.
No upstream website, issue, channel, Homebrew or Nix promotion is authorized.

## Completed rebase reviews

None recorded.
