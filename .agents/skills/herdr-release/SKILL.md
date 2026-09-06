---
name: herdr-release
description: Prepare and publish releases of trungnt13/herdr containing the latest upstream master changes. Without an explicit version, use the latest upstream stable release version plus an incrementing +fork.N suffix.
---

# Release this Herdr fork

Publish only to `trungnt13/herdr`. Use `herdrdev/herdr` as the read-only upstream source, never as the publication destination. Updating this skill does not authorize publishing a release or changing GitHub's fork relationship.

## Verify destination and release tooling

- Read current repository instructions, `justfile`, and release workflows. Verify `origin` targets `trungnt13/herdr`, `upstream` targets `herdrdev/herdr`, and the authenticated account has write access to the fork. Stop on a mismatch; do not change remotes automatically.
- Preserve unrelated changes. Require a clean release checkout on `master` before preparing a release; never reset, stash, or force-push user work without approval.
- Check that repository policy permits fork publication. If inherited upstream-only rules still prohibit it, report the conflict and obtain approval to update those rules rather than bypassing them.
- Verify recipes and CI support `MAJOR.MINOR.PATCH+fork.N` end to end: Cargo and lockfile, changelog parsing, tag validation, binaries, release assets, and version display. Ensure CI runs for this fork and publication, issue automation, manifests, and update destinations cannot affect upstream services.
- SemVer build metadata does not affect version precedence. Do not claim automatic updates between fork revisions work unless fork-aware comparison and update sources have been implemented and validated.
- Known migration blockers: the inherited `release-prepare` and `release-publish` recipes accept only three-part versions, and release CI contains `herdrdev/herdr` repository gates. Recheck these each time. If unresolved, stop before version edits, commits, tags, or pushes; request a separate tooling migration. Do not silently substitute a version or manually bypass the recipes.

## Include the latest upstream changes

1. Fetch `upstream/master` and `origin/master`. Query upstream's latest published stable GitHub Release, excluding drafts and prereleases; resolve its tag to a commit. Do not infer the stable version from Cargo.toml, local tags, preview releases, or the most recently created tag.
2. Integrate the fetched `upstream/master` while preserving fork changes. Follow the repository's rebase skill when applicable; stop for conflicts or any required history rewrite that lacks approval.
3. Require both the upstream stable tag commit and fetched `upstream/master` to be ancestors of the release candidate. The stable tag supplies the version prefix, not the code cutoff: include unreleased upstream master changes too.
4. Record the upstream tag and master commit in the release notes. Immediately before publication, fetch master and query the latest stable release again. If either changed, update the candidate/version and repeat affected validation and confirmation. “Latest” means the upstream state verified at that check, not future commits.

## Choose the version

- Let `BASE` be the latest upstream stable tag without its leading `v`; require `MAJOR.MINOR.PATCH`.
- Without an explicit version, inspect the fork's remote tags and GitHub Releases, including draft reservations, for exact `vBASE+fork.N` versions. Choose one greater than the highest positive integer `N`; start at `1` when none exist. Compare revisions numerically, not lexicographically. If enumeration is incomplete or fails, stop rather than assume no releases exist.
- Examples: upstream `v0.8.2` → `0.8.2+fork.1` → `0.8.2+fork.2`; a new upstream `v0.8.3` starts `0.8.3+fork.1`.
- If explicitly supplied, strip only the leading `v`; require the current `BASE+fork.N` form and a revision greater than existing revisions. Reject an incompatible request with an explanation; never silently rewrite it.
- Use the same version in Cargo.toml, Cargo.lock, and release metadata, with `v` prepended for the Git tag. Check local and remote tag collisions; stop rather than overwrite or reuse a tag. An orphan local tag is a blocker, not evidence of a published release.

## Validate and publish

1. Follow `../herdr-pre-release-audit/SKILL.md` where applicable to this fork. Finalize release notes and docs for the complete candidate, including unreleased upstream changes. Do not modify CI-owned snapshots manually.
2. Run `just check` and `just pre-release-check`; inspect benchmark results. Align `skills/herdr/SKILL.md` with the release as required by repository policy. Resolve failures without weakening checks.
3. Propose messages and obtain alignment before any commits. Use the supported repository recipes for finalization and release; keep unrelated changes out.
4. Immediately before publication, show the destination `trungnt13/herdr`, branch, upstream stable tag and master commit, current→target version, proposed `release: v<TARGET>` commit, and `v<TARGET>` tag. Explain that `just release <TARGET>` prepares a commit, pushes master, creates an annotated tag, and pushes it. Require explicit confirmation.
5. Run `just release <TARGET>` only after all tooling blockers are resolved. On failure, preserve state and report the exact completed and failed steps; do not blindly retry publication.
6. Verify the remote master contains the release commit and the remote tag resolves to it. Monitor release CI to completion and verify the GitHub Release and expected assets belong to `trungnt13/herdr`. Report failures or pending external dependencies; never describe a pushed tag alone as a completed release.
