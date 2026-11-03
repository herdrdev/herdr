---
name: pr-workflow
description: Use when taking work from branch to pull request - branch naming, commits, running checks, and writing the PR description that links the issue and summarizes the spec and review.
---

# Branch to PR

1. **Branch** from the configured base branch (never assume `main`): `shep/<id>-<slug>`. Work in the worktree you were given.
2. **Commit small and often**, each commit building and passing tests (see `conventional-commits`).
3. **Before asking for review**: run the project's checks (listed in the task context or workspace instructions). Fix failures; do not skip or weaken tests to get green.
4. **Rebase/merge the base** if it moved; resolve conflicts yourself and re-run checks.
5. **Never push, merge or force-push without the approval flow of the session.** Pushing and `gh pr create` are done by the workflow engine or after explicit approval.
6. **PR description** must contain:
   - `Closes #<issue>` (or the tracker link) on its own line,
   - What changed and why, in 3-6 lines (from the spec, not a diff recital),
   - How it was verified: exact commands and results,
   - Review summary: reviewer verdict, criteria checked, open items,
   - Which agent/vendor did each part.
7. Keep the diff within scope. Unrelated cleanups go in a separate PR.
