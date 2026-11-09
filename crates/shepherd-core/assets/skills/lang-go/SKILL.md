---
name: lang-go
description: Use when working in a Go repository - error handling, context propagation, concurrency, testing and linting conventions.
---

# Go practices

- **Errors are values**: return them, wrap with context `fmt.Errorf("open config: %w", err)`, compare with `errors.Is/As`. No `panic` for expected failures. Error strings are lowercase, no trailing punctuation.
- **Context**: first parameter `ctx context.Context` for anything that blocks, does I/O or can be cancelled; propagate it, never store it in a struct; honor cancellation.
- **Concurrency**: every goroutine has a clear owner and exit path; use `context`, `errgroup` or `sync.WaitGroup`; protect shared state with a mutex or a single owning goroutine; the sender closes channels. Run tests with `-race`.
- **Interfaces**: accept interfaces, return structs; define them where they are used; keep them small; do not create an interface with a single implementation "for later".
- **Naming/structure**: short receiver names, no stuttering (`config.Config` is fine, `config.ConfigLoader` is not), exported identifiers documented. Keep `internal/` for non-public packages.
- **Zero values** should be useful; avoid constructors that only set defaults.
- **Resources**: `defer f.Close()` right after a successful open; check close errors on writes.
- **Tests**: table-driven with `t.Run`, `t.TempDir()`, `t.Cleanup`, `t.Helper()`; fakes over mocks; golden files via `testdata/`.
- **Tooling before delivering**: `gofmt -l .`, `go vet ./...`, `go test ./...` (add `-race` when concurrency changed), and `golangci-lint run` if the repo has a config.
- **Dependencies**: standard library first (`slices`, `maps`, `errors`, `log/slog`, `net/http`); `go mod tidy` after changes.
