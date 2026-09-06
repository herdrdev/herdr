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
| `AGENTS.md` | Removes the Maintainer Workflow heading and scope text, and removes the external contributor guardrail. |
| `docs/porting-from-herdr-upstream.md` | Records fork deltas, unresolved decisions, and completed rebase results. |

No product source, build logic, public user documentation, or CI workflow delta
has been identified in the comparison against this marker.

## Release intent

Fork releases contain the latest `upstream/master` changes. Without an explicit
version, use the latest upstream stable version plus the next `+fork.N` suffix,
for example `0.8.2+fork.1`, then `0.8.2+fork.2`.

Release recipes, upstream-only CI gates, and updater version handling have not
yet been migrated. Do not publish a fork release until those blockers are
resolved.

## Unresolved governance decision

Upstream policy recognizes maintainers only when the account is listed in
`.github/MAINTAINERS`, the remote is canonical `herdrdev/herdr`, and write access
is verified. Adding `trungnt13` to the file does not grant upstream authority,
while this fork removes the external-contributor guardrail.

Do not resolve this implicitly during a rebase. The owner must choose whether
the fork retains upstream governance, defines fork-specific authority, or
restores the guardrail.

## Completed rebase reviews

None recorded.
