---
name: implement-task
description: Use when implementing a coding task in your worktree - small focused changes, stay in scope, verify before delivering.
---

# Implementing a task

1. **Understand before editing.** Read the task context and acceptance criteria, then the code you will touch and its callers. Find the existing pattern and follow it.
2. **Scope.** Change only what the task requires. Do not reformat, rename or refactor unrelated code. If you find a real problem outside scope, note it in your result; do not fix it.
3. **Small steps.** One logical change at a time; build and run the narrowest relevant test after each.
4. **Tests.** Add or update tests for new behavior and bug fixes (see `write-tests`). Write the failing test first for bugs.
5. **Reuse.** Search for an existing helper before writing one. Prefer the standard library and dependencies already in the project; do not add a dependency without a strong reason.
6. **Errors and edges.** Handle errors where they can happen; validate input at trust boundaries; no silent catches.
7. **Verify before delivering**, in this order: build, narrow tests, project checks. Read the output. Never claim something passes without running it.
8. **Deliver** with `submit_result`: summary, files changed, exact commands run with results, anything not done or uncertain. Commit in the worktree using `conventional-commits`; do not push.

If acceptance criteria are ambiguous or contradict the code, ask the brain with `send_message` instead of choosing silently.
