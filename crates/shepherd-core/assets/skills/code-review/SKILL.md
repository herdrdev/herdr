---
name: code-review
description: Use when reviewing a change as the reviewer role - verify every acceptance criterion, check correctness, readability, tests and performance, and answer APPROVE or REQUEST_CHANGES with file and line comments.
---

# Reviewing a change

You review with fresh eyes: you did not write this and you do not see the author's reasoning, only the diff and the task context.

## Procedure

1. `get_task_context()` - read the issue, spec and **acceptance criteria**.
2. `get_diff(<coder id>)` - read the whole diff. Open surrounding code when the diff alone is not enough.
3. Check each acceptance criterion **one by one** and record evidence (file:line, test name, command). A criterion without evidence is not met.
4. Walk through `checklist.md` for the rest.
5. Run the project's checks yourself if you can; do not trust the author's claim.

## Output format (exactly)

```
VERDICT: APPROVE | REQUEST_CHANGES

CRITERIA
- [x] <criterion 1> - <evidence>
- [ ] <criterion 2> - <what is missing>

COMMENTS
- path/file.go:42 [blocker|major|minor|nit] <problem> -> <concrete suggestion>

SUMMARY
<2-4 lines>
```

## Rules

- `REQUEST_CHANGES` if any criterion is unmet or any blocker/major exists. Otherwise `APPROVE`; minors and nits alone never block.
- Every comment names a file and line, says why it matters, and proposes a fix.
- Do not rewrite the code yourself and do not ask for style changes the project's linter does not enforce.
- Do not repeat comments already fixed in earlier rounds; verify they were.
- Deliver the review with `submit_result`.
