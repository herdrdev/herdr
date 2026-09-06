---
name: rebase-upstream-master
description: Safely rebase the current branch onto fetched upstream/master, preserve documented fork deltas, and record the review. Never merge or push.
---

# Rebase Upstream Master

Rebase the current branch only. Never merge, push, stash, reset, delete, or
discard work.

## Check

1. Show `git status --short --branch`, the branch, and upstream URL.
2. Stop unless the repository has an `upstream` remote, HEAD is attached, no Git
   operation is active, and the index and worktree are clean, including
   untracked files.
3. Read `docs/porting-from-herdr-upstream.md`. Treat its marker, fork deltas,
   release intent, and unresolved decisions as review evidence, not procedure.
   Stop if the file or marker is missing or contradicts Git history.
4. Run `git fetch --prune upstream` and verify `upstream/master` exists.
5. Show both commit ranges:

   ```bash
   git log --oneline --decorate upstream/master..HEAD
   git log --oneline --decorate HEAD..upstream/master
   ```

6. If `upstream/master` is already an ancestor of HEAD, record a no-op review
   without advancing the marker or claiming new validation, then stop.
7. If `git rev-list --merges upstream/master..HEAD` is nonempty, ask whether to
   preserve merges with `--rebase-merges` or flatten them. Do not choose.

## Review and rebase

Review upstream changes against every documented fork delta and unresolved
decision. Report proposed keep, adapt, drop, or pending decisions with reasons.
Ask the owner about ambiguous behavior or policy; a clean textual rebase does
not prove preservation.

Report the branch, HEAD, local commit count, possible commit-ID changes, proposed
backup name, and that this skill will not push. Obtain confirmation before
rewriting history. Keep notes outside the checkout until the rebase ends.

After confirmation, save the original merge base. Create
`backup/rebase-upstream-master-<UTC timestamp>` at HEAD and stop if that fails.
Run `git rebase upstream/master` without autostash, adding `--rebase-merges` only
when chosen.

On conflict, show status and conflicted files. Resolve only unambiguous
conflicts; otherwise ask. Never skip, abort, or discard without explicit
approval.

## Verify and record

Before editing the guide, require `upstream/master` to be an ancestor of HEAD
and `git status --porcelain` to be empty. Show HEAD and local commits. If commits
were replayed, run:

```bash
git range-diff <original-merge-base>..<backup-branch> upstream/master..HEAD
```

Review meaningful range-diff changes and the final fork diff. Run targeted
checks required by the affected surfaces and `AGENTS.md`, then run `just check`.
Record exact commands and results, including skipped, blocked, or failed checks.

Append a compact entry under the guide's completed rebase reviews with:

- UTC date, branch, original HEAD and merge base, fetched upstream HEAD,
  resulting HEAD, and backup branch;
- substantive keep, adapt, drop, or pending decisions and reasons;
- checks and exact results; and
- outcome and unresolved owner decisions.

Refresh the fork-delta inventory from evidence. Advance the historical marker
only after a complete rebase and all required validation succeed. Otherwise,
leave it unchanged and record the blocker. Never edit the guide while a rebase
is unresolved.

Run `git diff --check`, inspect final status, and show the guide diff. Leave the
guide update uncommitted and propose a commit message for approval. Report the
review outcome, validation, backup branch, and that no push occurred. Do not
claim a failed or blocked review is complete.
