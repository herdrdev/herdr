---
name: conventional-commits
description: Use when writing commit messages - Conventional Commits format with a short imperative subject and a body that explains why.
---

# Commit messages

Format: `<type>(<scope>): <subject>`

- **type**: `feat`, `fix`, `refactor`, `perf`, `test`, `docs`, `build`, `ci`, `chore`, `revert`.
- **scope**: optional, the area touched (`config`, `shepherd`, `api`).
- **subject**: imperative, lowercase, no trailing period, max ~72 chars. "add retry to client", not "added retries".
- **body** (when the why is not obvious): blank line, then wrap at ~72 cols. Explain motivation and trade-offs, not a line-by-line recap.
- **footer**: `Closes #123` for issues; `BREAKING CHANGE: <what and migration>` for breaking changes (or `!` after the type).

Rules:
- One logical change per commit; the tree builds and tests pass at every commit.
- Do not mix refactors with behavior changes.
- Follow the repository's own convention if it differs and is documented.
- Follow any trailer or attribution rule given in the workspace instructions.

Example:
```
fix(client): reconnect after socket close

The subscription goroutine exited on EOF and never restarted, so status
events stopped after a shepherd restart. Retry with backoff instead.

Closes #57
```
