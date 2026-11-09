---
name: plan-tasks
description: Use when you must turn a task or issue into a plan - split it into independent, parallelizable subtasks, each with acceptance criteria and a verification command.
---

# Planning a task

1. **Read first**: the task context (issue, spec, acceptance criteria), the workspace instructions and the code that the change touches. Do not plan from the title alone.
2. **List unknowns** and resolve them by reading code or asking the docs agent. Write the ones you cannot resolve as explicit assumptions.
3. **Split by independence**, not by layer. A good subtask:
   - changes a disjoint set of files (so coders do not conflict),
   - can be verified on its own (a test, a command),
   - fits in one focused session (roughly one reviewable PR-sized diff or smaller).
4. **Order**: mark dependencies; run independent subtasks in parallel, one coder per subtask, each in its own worktree.
5. **Write the plan** to `plan.md` in the task space using this shape:

```
## Goal
<one paragraph>
## Assumptions
- ...
## Subtasks
### 1. <title>
- Files: <paths>
- Depends on: <none | n>
- Done when: <acceptance criteria, checkable>
- Verify: <exact command>
```

6. Map every acceptance criterion of the task to at least one subtask. If a criterion is not covered, the plan is incomplete.
7. Keep the plan short. If it needs more than ~8 subtasks, the task is probably two tasks: say so.
