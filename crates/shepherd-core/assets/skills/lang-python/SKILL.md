---
name: lang-python
description: Use when working in a Python repository - typing, errors, project layout, testing and linting conventions.
---

# Python practices

- **Version and env**: read `pyproject.toml` for the supported Python and tooling; use the project's virtualenv/uv/poetry setup, never install into the system interpreter.
- **Typing**: add type hints to public functions; keep `mypy`/`pyright` clean if configured; use `dataclass`/`TypedDict`/`Protocol` over loose dicts.
- **Errors**: raise specific exceptions, never bare `except:`; chain with `raise ... from err`; validate input at boundaries.
- **Resources**: `with` for files, locks, connections; `pathlib` over string paths; `subprocess.run([...], check=True)` with an argument list, never `shell=True` on input.
- **Style**: follow the configured formatter/linter (`ruff`, `black`); f-strings; no mutable default arguments; comprehensions only when clearer than a loop.
- **Tests**: `pytest` with plain asserts, `@pytest.mark.parametrize` for tables, `tmp_path` and fixtures; no network or sleeps in unit tests.
- **Before delivering**: `pytest` (narrow, then full), `ruff check .` / `ruff format --check .` if configured, type checker if configured.
- **Dependencies**: declare in `pyproject.toml`; do not add a package for something the standard library does.
