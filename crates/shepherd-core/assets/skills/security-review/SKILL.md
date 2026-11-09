---
name: security-review
description: Use when reviewing a change for security - secrets, injection, input validation, authentication and authorization, unsafe file or process handling, and dependencies.
---

# Security review

Review the diff (and what it touches) for these classes. Report findings with file:line, severity, exploit scenario in one sentence, and a fix.

1. **Secrets**: keys, tokens, passwords, private URLs in code, tests, fixtures, logs, error messages or config committed by mistake. Check the diff and new files.
2. **Injection**: SQL built by string concatenation; shell commands built from input (use argument arrays, never a shell string); template/HTML injection; path traversal (`..`, absolute paths, symlinks) in any file path derived from input; SSRF in outbound requests.
3. **Input validation**: validate at trust boundaries (HTTP, CLI args, files, env, IPC, LLM output). Check size limits, types, ranges, encodings.
4. **AuthN/AuthZ**: every new endpoint/action checks identity AND permission for the specific resource (no IDOR); no client-side-only checks; no default credentials.
5. **Unsafe operations**: deserialization of untrusted data, `eval`, disabled TLS verification, weak randomness or hashing for security purposes, world-writable files, race conditions between check and use.
6. **Dependencies**: new or bumped packages - necessary, maintained, pinned, license compatible. Flag anything unfamiliar.
7. **Data exposure**: logging of personal data or credentials, verbose errors to users, overly broad CORS or permissions.

Severity: **blocker** = exploitable now; **major** = exploitable with conditions; **minor** = hardening. Say "no findings" explicitly for classes you checked and found clean; do not invent issues.
