# Review checklist

**Correctness**
- Does it do what the spec says in the normal case AND the edge cases (empty, nil, zero, limits)?
- Error paths: are errors handled, wrapped with context, never swallowed?
- Concurrency: shared state, locks, goroutine/task leaks, cancellation.
- Resource handling: files, connections, processes closed on every path.
- Backward compatibility: public API, config, data formats, migrations.

**Readability**
- Names say what things are; functions do one thing; no dead code or leftover debugging.
- Follows the existing patterns of this codebase.
- Comments explain why, not what.

**Tests**
- New behavior and each fixed bug has a test that would fail without the change.
- Tests are deterministic and assert behavior, not implementation.

**Performance**
- No accidental O(n^2), N+1 queries, unbounded memory, or work in hot loops.
- Only flag performance if it matters for the realistic input size.

**Scope**
- Diff contains only what the task needs; unrelated changes are split out.
- No new dependency without justification.
