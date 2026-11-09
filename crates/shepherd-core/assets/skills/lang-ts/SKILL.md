---
name: lang-ts
description: Use when working in a TypeScript or JavaScript repository - types, async handling, module style, testing and linting conventions.
---

# TypeScript practices

- **Types**: keep `strict` on; no `any` (use `unknown` and narrow); model states with discriminated unions; validate external data at the boundary (schema/guard), then trust the types inside.
- **Async**: `async/await`; always handle rejections; never leave a floating promise (`await` or `void` it deliberately); use `Promise.all` for independent work; support `AbortSignal` for cancellable operations.
- **Errors**: throw `Error` subclasses with context and `cause`; do not swallow in empty `catch`.
- **Modules**: follow the repo's existing style (ESM vs CJS, path aliases); avoid circular imports; named exports over default unless the repo uses defaults.
- **Immutability**: prefer `const`, `readonly`, non-mutating array methods unless profiling says otherwise.
- **Tests**: use the repo's runner (vitest/jest/node:test); test behavior, mock only I/O boundaries; no `setTimeout` waits - use fake timers or awaited events.
- **Before delivering**: run the package scripts the repo defines (`npm test`, `npm run lint`, `npm run build` / `tsc --noEmit`). Check `package.json` first; do not guess script names.
- **Dependencies**: use what is already installed; check bundle/security impact before adding one; commit the lockfile with `package.json`.
