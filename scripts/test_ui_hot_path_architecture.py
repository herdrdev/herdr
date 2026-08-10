from __future__ import annotations

import re
import unittest
from pathlib import Path


PROJECT_ROOT = Path(__file__).resolve().parent.parent
HOT_PATH_SOURCES = (
    PROJECT_ROOT / "src" / "ui.rs",
    *sorted((PROJECT_ROOT / "src" / "ui").rglob("*.rs")),
    PROJECT_ROOT / "src" / "server" / "render_stream.rs",
)
TEST_MODULE = re.compile(r"(?m)^#\[cfg\(test\)\]\s*\nmod\s+\w+\s*\{")
FORBIDDEN_CALLS = (
    (
        re.compile(r"(?:\.|::)input_state\b"),
        "aggregate terminal input state; add a narrow accessor",
    ),
    (
        re.compile(r"(?:\.|::)(?:keyboard_state_ansi|kitty_keyboard_state_ansi)\b"),
        "formatted keyboard state",
    ),
    (
        re.compile(r"(?:\.|::)screen_text_snapshot\b"),
        "formatted terminal screen snapshot",
    ),
    (
        re.compile(r"\bforeground_job\s*\("),
        "process-tree inspection",
    ),
)


def blank_non_newlines(chars: list[str], start: int, end: int) -> None:
    for index in range(start, end):
        if chars[index] != "\n":
            chars[index] = " "


def mask_comments_and_literals(source: str) -> str:
    chars = list(source)
    index = 0
    while index < len(source):
        if source.startswith("//", index):
            end = source.find("\n", index + 2)
            end = len(source) if end == -1 else end
            blank_non_newlines(chars, index, end)
            index = end
            continue

        if source.startswith("/*", index):
            depth = 1
            end = index + 2
            while end < len(source) and depth > 0:
                if source.startswith("/*", end):
                    depth += 1
                    end += 2
                elif source.startswith("*/", end):
                    depth -= 1
                    end += 2
                else:
                    end += 1
            blank_non_newlines(chars, index, end)
            index = end
            continue

        if source[index] == "r":
            quote = index + 1
            while quote < len(source) and source[quote] == "#":
                quote += 1
            if quote < len(source) and source[quote] == '"':
                suffix = '"' + "#" * (quote - index - 1)
                end = source.find(suffix, quote + 1)
                end = len(source) if end == -1 else end + len(suffix)
                blank_non_newlines(chars, index, end)
                index = end
                continue

        if source[index] == '"':
            end = index + 1
            while end < len(source):
                if source[end] == "\\":
                    end += 2
                elif source[end] == '"':
                    end += 1
                    break
                else:
                    end += 1
            blank_non_newlines(chars, index, min(end, len(source)))
            index = end
            continue

        if source[index] == "'":
            end = index + 2
            if index + 1 < len(source) and source[index + 1] == "\\":
                end += 1
            if end < len(source) and source[end] == "'":
                end += 1
                blank_non_newlines(chars, index, end)
                index = end
                continue

        index += 1

    return "".join(chars)


def production_code(source: str) -> str:
    code = mask_comments_and_literals(source)
    chars = list(code)
    search_from = 0

    while test_module := TEST_MODULE.search(code, search_from):
        depth = 0
        end = test_module.end() - 1
        while end < len(code):
            if code[end] == "{":
                depth += 1
            elif code[end] == "}":
                depth -= 1
                if depth == 0:
                    end += 1
                    break
            end += 1
        blank_non_newlines(chars, test_module.start(), end)
        code = "".join(chars)
        search_from = end

    return code


class UiHotPathArchitectureTests(unittest.TestCase):
    def test_render_hot_paths_avoid_known_expensive_runtime_queries(self) -> None:
        violations: list[str] = []

        for path in HOT_PATH_SOURCES:
            source = path.read_text(encoding="utf-8")
            code = production_code(source)
            for pattern, description in FORBIDDEN_CALLS:
                for match in pattern.finditer(code):
                    line = code.count("\n", 0, match.start()) + 1
                    relative_path = path.relative_to(PROJECT_ROOT)
                    violations.append(f"{relative_path}:{line}: {description}")

        self.assertEqual(
            violations,
            [],
            "Render/layout code must not perform pane-scaled expensive reads:\n"
            + "\n".join(violations),
        )

    def test_scanner_ignores_non_production_references(self) -> None:
        source = '''
// runtime.input_state()
const EXAMPLE: &str = "runtime.input_state()";
#[cfg(test)]
mod tests {
    fn aggregate_state_test() { runtime.input_state(); }
}
fn production_after_tests() {}
'''
        code = production_code(source)
        self.assertNotRegex(code, FORBIDDEN_CALLS[0][0])
        self.assertIn("fn production_after_tests()", code)
        self.assertEqual(code.count("\n"), source.count("\n"))

    def test_scanner_checks_production_after_test_modules(self) -> None:
        source = '''
#[cfg(test)]
mod tests {
    const BRACES: &str = "}}";
}
fn render() { TerminalRuntime::input_state; }
'''
        self.assertRegex(production_code(source), FORBIDDEN_CALLS[0][0])

    def test_scanner_catches_imported_process_query(self) -> None:
        source = "fn render() { foreground_job(pid); }"
        self.assertRegex(production_code(source), FORBIDDEN_CALLS[3][0])


if __name__ == "__main__":
    unittest.main()
