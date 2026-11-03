---
name: debug
description: Use when something fails or behaves unexpectedly - reproduce, isolate, find the root cause, fix it, and add a regression test.
---

# Debugging

1. **Reproduce** with the smallest command or test that fails. If you cannot reproduce it, you cannot claim to fix it; gather logs and state/version differences instead.
2. **Read the error fully** - message, stack trace, exit code. The answer is often in the first failing line, not the last.
3. **Isolate**: bisect (inputs, commits with `git bisect`, code paths), add targeted logging or a debugger, shrink the repro until removing anything makes it pass.
4. **Form one hypothesis at a time** and test it with an experiment that can prove it wrong. Do not change several things at once.
5. **Find the root cause**, not the symptom: grep every caller of the broken function; fix it where all paths converge instead of patching one caller.
6. **Fix minimally** in the cause location.
7. **Regression test**: write a test that fails before the fix and passes after (see `write-tests`). Confirm it fails when you revert the fix.
8. **Verify**: run the original repro, the new test, then the wider suite. Report commands and outcomes.
9. If three fix attempts failed, stop: re-read your assumptions, and report what you learned rather than stacking more patches.

Remove temporary debugging code before delivering.
