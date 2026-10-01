#!/usr/bin/env python3
import importlib.util
from pathlib import Path
import unittest


SCRIPT = Path(__file__).with_name("verify_agent_watchdog_live.py")
SPEC = importlib.util.spec_from_file_location("verify_agent_watchdog_live", SCRIPT)
LIVE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(LIVE)


class ClosingSetupSignalTests(unittest.TestCase):
    def test_accepts_parsed_block_with_blocking_item(self):
        accepted, fields = LIVE._closing_setup_signal({"tokens": {
            "closing_parse": "ok", "closing_blocking": "1",
            "closing_wait": "Continue the acceptance probe?",
        }})

        self.assertTrue(accepted)
        self.assertEqual(fields["closing_blocking"], "1")

    def test_rejects_missing_parse_or_blocking_item(self):
        for tokens in (
            {"closing_parse": "missing", "closing_blocking": "1"},
            {"closing_parse": "ok", "closing_blocking": "0"},
            {"closing_parse": "ok", "closing_blocking": "not-a-count"},
        ):
            with self.subTest(tokens=tokens):
                self.assertFalse(LIVE._closing_setup_signal({"tokens": tokens})[0])

    def test_rejects_absent_or_malformed_tokens(self):
        self.assertFalse(LIVE._closing_setup_signal({})[0])
        self.assertFalse(LIVE._closing_setup_signal({"tokens": None})[0])


if __name__ == "__main__":
    unittest.main()
