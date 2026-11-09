---
name: write-tests
description: Use when writing or fixing tests - table-driven cases, edge cases, and avoiding mocks that do not earn their keep.
---

# Writing tests

- **Test behavior, not implementation.** Assert on outputs and observable effects, so refactors do not break tests.
- **Table-driven** when the same logic has several inputs: one slice of named cases, one loop, `t.Run(name, ...)` (or the language's equivalent). A failing case must say which one.
- **Edge cases to always consider**: empty/nil, zero, one, many; boundaries (off by one); invalid input; duplicates; unicode; very large; concurrent access; error paths from every dependency.
- **Do not mock what you do not need to.** Prefer real implementations, temp dirs/files, in-memory fakes, and local fake servers. Mock only slow, nondeterministic or external systems (network, clock, AI APIs).
- **Deterministic**: no sleeps for synchronization, no reliance on test order, no real time or randomness without a seed; clean up with the framework's cleanup hooks.
- **Bug fix = regression test first**: write the test, watch it fail for the right reason, then fix.
- **Names** say the scenario and expectation: `TestParse_RejectsEmptyInput`.
- **Run** the new tests alone, then the package, then the project's checks. Report the exact commands and outcomes.
- A test that cannot fail is worthless: if you cannot make it fail by breaking the code, rewrite it.
