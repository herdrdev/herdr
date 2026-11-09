# Contributing to Shepherd

Thanks for wanting to help. Shepherd is a young fork of [herdr](https://github.com/herdrdev/herdr) and every kind of help is welcome: bug reports, ideas, documentation, tests and code.

This file is the contribution guide for Shepherd. `AGENTS.md` and a few other files were inherited from herdr and describe its own maintainers' workflow; where they differ from this file, follow this one.

## Ways to help

- **Report a bug.** Open an issue with what happened, what you expected, the shortest steps to reproduce it, and your Shepherd version, operating system and terminal. Search the open issues first.
- **Pick up a `good first issue`.** They are small, self-contained and written to be approachable. Comment on the issue to say you are taking it.
- **Suggest an idea.** Use [Discussions](https://github.com/Rivaldito/shepherd/discussions) for features, patterns and roles you would like to see.
- **Share a pattern or a role.** A useful team (a `pattern` yaml) or role (`config.yaml`) with a short description is a great contribution; it can go under the docs or in a discussion.
- **Improve the docs.** The guides live in [`docs/shepherd/`](docs/shepherd/). Fixes and clarifications are very welcome.

## Before you change code

1. Build and test (see below) so you know the starting point is green.
2. For anything bigger than a small fix (new behavior, a new view, changes to how agents or patterns work), open a discussion or an issue first so we agree on the direction.
3. Keep the change focused: one pull request solves one problem, with no unrelated cleanup.

## Building and testing

You need Rust 1.96.1+ and Zig 0.16.0 (to build the vendored terminal library). If zig is not on your `PATH`, set `ZIG=/path/to/zig`.

```bash
./cargo-fork.sh build                   # debug build
./cargo-fork.sh test -p shepherd-core   # orchestrator core tests
./cargo-fork.sh test --bin shepherd -- shepherd:: client::shell protocol:: api:: config::
```

The native app is a separate workspace:

```bash
cd native
cargo fmt -p shepherd-app-core -p shepherd-native --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Before you open a pull request, also run `scripts/rename-from-herdr.sh --check` (no leftover upstream names in Shepherd's own files).

To try your change without touching your own setup, use an isolated session: `scripts/shepherd-iso.sh start`, or for the native app `native/dev/demo.sh`, which uses a throwaway server and a stand-in agent.

Some tests from herdr's code base may fail on your machine for reasons unrelated to your change (for example the checksum and plugin CLI tests). Mention it in the pull request if you see it; do not hide it.

## Commits and pull requests

- Use lowercase conventional commits with a descriptive subject: `fix: handle an empty topic list`, `docs: explain consumer groups`.
- Put `refs #<issue>` in the commit body when it relates to an issue. Do not use closing keywords such as `fixes`: issues are closed when a release ships.
- Explain in the pull request what changed and why, and how you tested it. You must be able to explain every line you submit; using an AI assistant to write it is fine.
- Do not edit generated or vendored files by hand.

## Code style

- Rust: `rustfmt` and `clippy` clean. No `unwrap()` in production code; use `tracing` for logging. Use `#[allow]` only with a comment saying why.
- Shepherd's code lives in its own modules (`crates/shepherd-core`, `src/shepherd`, `native/`). Changes to herdr's files should stay small and additive, so merging upstream stays easy; see [docs/UPSTREAM-SYNC.md](docs/UPSTREAM-SYNC.md).
- User-facing text is in English.

## Fixes that belong in herdr

If a bug or improvement is in herdr's own code (the terminal runtime, agent detection, the plugin system), it is best reported to [herdr](https://github.com/herdrdev/herdr) so everyone benefits; Shepherd picks it up when it merges upstream.

## Conduct

Be kind and assume good faith. Harassment, personal attacks and spam are not welcome and will be removed.

## License

By contributing you agree that your work is licensed under the Apache License 2.0, like the rest of the project (see [LICENSE](LICENSE) and [NOTICE](NOTICE)).
