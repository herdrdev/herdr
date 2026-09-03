---
name: herdr-release
description: Prepare and publish a stable Herdr release with repository just recipes. Use when asked to release Herdr or create and push a stable release tag; keep an explicit version, otherwise increment Cargo.toml's patch version.
---

# Release Herdr

Use only inside the Herdr repository. It publishes stable releases, not previews.

## Verify authority and destination

Before working:

1. Read repository release instructions and current `justfile` recipes; they override this skill.
3. Require `master`; preserve every pre-existing change. If any authority check fails, do not release from a fork, change remotes, revert changes, or publish.

## Choose the version

- Use an explicit semantic version unchanged, stripping request prefix `v` only for `just`.
- Otherwise increment only the `Cargo.toml` package patch (`0.8.2` → `0.8.3`).
- Require `MAJOR.MINOR.PATCH`; never silently substitute. Stop if the target tag exists or repository validation rejects the target.

## Establish release readiness

1. Follow `../herdr-pre-release-audit/SKILL.md`. A release request authorizes applying required release-document finalization, but not guessing product decisions. Stop for any unresolved release blocker or user decision.
2. Never edit CI-owned preview or published-release files.
3. Finalize release notes and next-release docs; align `skills/herdr/SKILL.md` with the stable release; pass `just pre-release-check` and review its benchmarks.
4. Before `just release`, commit every required finalization change except `skills/herdr/SKILL.md`, which the recipe intentionally includes in its release commit. Show every separate diff and proposed message; get alignment before committing. Exclude unrelated changes and require `just release-prepare` to accept the state.

## Confirm and publish

Immediately before publication, show current→target version, destination repository and branch, proposed commit `release: v<TARGET>`, tag `v<TARGET>`, and that `just release <TARGET>` creates the commit, pushes `master`, creates the annotated tag, and pushes it. Require explicit confirmation.

Then run only:

```bash
just release <TARGET>
```

Never manually recreate its version edits, commit, tag, or pushes. If it fails before publication, preserve state, report the exact failure, and never blindly retry a push or tag.

After success, verify remote `master` contains the release commit and remote `refs/tags/v<TARGET>` resolves to it. Report both checks and GitHub Release workflow status. Release CI owns binaries, GitHub Release, published docs, issue closure, and `distribution/latest.json`.
