---
description: Audit next-release docs and changelog before release
---
Audit release readiness for this repo.

Optional starting ref override: `$1`
Extra user intent/context: `${@:2}`

Process:

1. Determine the base ref.
   - If `$1` is non-empty and looks like a ref/tag, use it.
   - Otherwise use the latest release tag, preferring the repo's semver tag style:
     ```bash
     git describe --tags --abbrev=0
     ```

2. Inspect the range from base ref to `HEAD`.
   - Use first-parent history for release context:
     ```bash
     git log --first-parent --reverse --format='%H%x09%s' <base>..HEAD
     ```
   - Also inspect full commits and commit bodies when needed:
     ```bash
     git log --reverse --format='%H%x09%s%n%b' <base>..HEAD
     ```

3. Detect merged PRs if any.
   - Look for first-parent subjects that indicate PR merges, including squash merges like `title (#123)`.
   - If GitHub CLI is available and the PR number is known, use it to fetch PR title/body for context.
   - Treat a merged PR as the primary release unit.
   - Do **not** also list individual commits that belong to that PR.

4. Handle direct commits separately.
   - Any commit in the range not represented by a merged PR should be considered on its own.

5. Infer what matters.
   - For each PR or direct commit, inspect changed files and diff stats.
   - Read the most relevant files in full when needed to understand user-facing impact.
   - Ignore pure housekeeping unless it has release value:
     - version bumps
     - release/tag commits
     - changelog-only commits
     - formatting-only changes
     - comment-only/doc-only changes unless they materially affect users

6. Audit `docs/next/CHANGELOG.md` and issue references.
   - Treat root `CHANGELOG.md` as the latest released changelog.
   - Treat `docs/next/CHANGELOG.md` as the next-release changelog.
   - Compare meaningful user-facing changes in the commit range against `docs/next/CHANGELOG.md`.
   - Flag missing entries for new features, bug fixes, removals, breaking changes, defaults, compatibility changes, user-visible command/config/API behavior, and security-relevant changes.
   - Do not require changelog entries solely for internal client/server protocol version bumps. Mention protocol only when the release intentionally changes user-facing compatibility guidance beyond the normal restart requirement.
   - Inspect commit bodies for issue reference lines in the form `refs #<issue-number>`.
   - Flag normal commits that use GitHub closing keywords like `fixes #<issue-number>`, `closes #<issue-number>`, or `resolves #<issue-number>`, because they close issues before release when they land on `master`.
   - For each shipped issue reference, check whether the changelog has a matching user-facing entry that mentions `#<issue-number>` when appropriate.
   - For each merged external human PR, check whether the changelog entry mentions the PR number and thanks the contributor in the existing style, e.g. `(#129, thanks @username)`. If the PR primarily ships an issue fix, include both the issue and PR numbers when useful, e.g. `(#128, #129, thanks @username)`. Do not add thanks text for maintainer-owned bots or automation accounts such as `kangal-bot` or `dependabot`.
   - Do not require or add GitHub closing keywords like `fixes #<issue-number>`, `closes #<issue-number>`, or `resolves #<issue-number>` to changelog entries or release notes.
   - List shipped issue references under `Issue references to close after release:` so the release operator can verify what release CI will close after the GitHub Release is published.
   - Flag stale entries that do not appear to correspond to shipped changes in the range.
   - Flag entries that are too implementation-focused or unclear for end users.
   - Preserve the existing changelog style and sections: `Added`, `Changed`, `Fixed`, `Removed`, and `Breaking Changes` when applicable.

7. Audit next-release public docs.
   - Treat root `README.md` and the version selected by `docs/versions/manifest.json` under `docs/versions/<current>/website/src/content/docs/` as the latest released public docs. Published version docs may contain factual corrections made after the release tag.
   - Treat `docs/next/README.md` as the next-release root README and `docs/next/website/src/content/docs/` as the complete unpublished website-doc draft.
   - Treat `docs/preview/website/` as bot-owned output for the active preview release. Never edit it during release review and never use it as the stable release source.
   - Compare meaningful user-facing changes in the range against next-release docs first.
   - Flag missing release docs for new or changed features, commands, config keys, protocol behavior, integrations, defaults, and compatibility notes.
   - Compare English next-release website docs against `docs/next/website/src/content/docs/ja/` and `docs/next/website/src/content/docs/zh-cn/`. Flag missing localized files, stale localized files, and heading-outline drift where translated docs do not have the same section structure as English.
   - Compare `docs/next/README.md` and the next website draft against current stable docs. Flag each difference as intended to ship, stale, or needing user decision. Do not require the draft and stable trees to match before release.
   - Also audit example config snippets for release readiness.
   - Audit `skills/herdr/SKILL.md` against shipped changes to the CLI, public IDs, pane and agent workflows, lifecycle semantics, and safety guidance. Flag stale commands, options, examples, or behavioral claims. The binary bundles this exact file, so review semantic freshness rather than file synchronization.

8. Verify finalization state.
   - Before `just release`, approved README changes must be finalized in `docs/next/README.md`; release CI promotes that tagged file after publication. Do not copy draft website docs into `website/src/content/docs/` or `docs/preview/`.
   - `nix/package.nix` imports `Cargo.lock` through `cargoLock.lockFile`; normal version and lockfile updates do not require a separate cargo hash refresh. If git dependencies are introduced, verify the required `cargoLock.outputHashes` entries.
   - Run or recommend:
     ```bash
     just pre-release-check
     ```
   - The docs check validates the staged draft, localized heading parity, published preview and stable snapshot provenance, and both production and draft website builds.
   - The render benchmark has no automatic timing threshold, but reviewing it is a required release checkpoint. Record the 1, 15, and 50-count median and p95 results for background-workspace resize/layout and active panes, compare their scaling ratios, and treat a material regression as a release blocker until investigated rather than relying on absolute timing across machines.
   - Do not run `just release` unless the working tree is clean, the docs check passes, and the render-scale result has been reviewed.

9. Apply changes only when asked.
   - Do not edit files during the audit unless the user explicitly asks you to apply fixes.
   - When asked to apply audit fixes, update `docs/next/CHANGELOG.md`, `docs/next/README.md`, and any required staged website docs under `docs/next/website/src/content/docs/`.
   - When asked to finalize release docs, finalize only the staged files under `docs/next/`, then run `just release-docs-check`. Preview and stable publication remain CI-owned.

Output format:

```md
Release readiness: READY | NOT READY

Base: <base ref>
Range: <base ref>..HEAD
Meaningful shipped changes: yes | no

Changelog: OK | MISSING ENTRIES | NEEDS ATTENTION
Missing:
- <only user-facing shipped changes missing from docs/next/CHANGELOG.md>

Docs: OK | MISSING | INACCURATE | NEEDS DECISION
Missing:
- <only required next-release public docs gaps>

Wrong or questionable:
- <docs that disagree with implementation, if any>

Issue refs: OK | NEEDS ATTENTION
Will close after release:
- #<issue>

Accepted/no action:
- <items the user explicitly accepted, such as known closing-keyword commits>

Root docs finalized: YES | NO
<result of just release-docs-check or why it was not run>

Agent skill: UP TO DATE | NEEDS UPDATE | NOT CHECKED
<whether skills/herdr/SKILL.md matches the shipped CLI and agent-control behavior>

Nix Cargo lock integration: OK | NEEDS ATTENTION | NOT CHECKED
<result of nix flake check or any required cargoLock.outputHashes status>

Render scaling: OK | NEEDS ATTENTION | NOT CHECKED
<1, 15, and 50-count median/p95 results and ratios for background-workspace resize/layout and active panes>

Required before release:
1. <short action>
```

Keep the main output glanceable. Put commit inventories, excluded housekeeping, and commands run in an appendix only when they materially help the operator.

If the range has no meaningful user-facing changes, say that plainly instead of forcing entries.
