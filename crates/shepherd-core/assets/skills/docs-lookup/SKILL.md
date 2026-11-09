---
name: docs-lookup
description: Use before assuming how a library, API, CLI flag or framework behaves - find the official documentation, cite the source and version, and say when you could not verify.
---

# Look it up, do not guess

1. **Identify the exact version** in use (lockfile, `go.mod`, `--version`, installed package). Docs for another version mislead.
2. **Go to the primary source**: official docs, the project's repository (README, source, changelog), or the tool's own `--help`. Prefer these over blog posts and memory.
3. **Verify locally when cheap**: run `<cli> --help`, a one-line script, or a tiny test. Observed behavior beats recalled behavior.
4. **Answer with evidence**:
   - the fact (flag, signature, behavior),
   - the source (URL or file path) and the version it applies to,
   - confidence: *verified* (ran it), *documented* (read in docs) or *unverified* (inferred).
5. If docs and behavior disagree, report both.
6. Never invent flags, function names, config keys or file paths. "I could not verify this" is a valid answer; a confident wrong one is not.
7. When asked via `ask_docs`, reply concisely: answer first, then source/version, then caveats. Save durable findings with `update_task_context`.
