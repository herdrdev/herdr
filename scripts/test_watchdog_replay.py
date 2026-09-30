from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("watchdog_replay.py")
SPEC = importlib.util.spec_from_file_location("watchdog_replay", SCRIPT)
assert SPEC and SPEC.loader
replay = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(replay)


class WatchdogReplayTests(unittest.TestCase):
    def test_background_bash_task_notification_completed_clears_live_state(self) -> None:
        state: dict = {}
        replay.apply_event(state, {
            "ts": 100,
            "call": [{"id": "tool-1", "name": "Bash", "input": {"run_in_background": True}}],
        })
        replay.apply_event(state, {
            "ts": 101,
            "result": [{"tool_use_id": "tool-1", "content": "Task started (task-id: 7)"}],
        })
        self.assertEqual(replay.background_status(state, 102), "live")

        replay.apply_event(state, {
            "ts": 120,
            "text": (
                "<task-notification><task-id>7</task-id><status>completed</status>"
                "</task-notification>"
            ),
        })
        self.assertEqual(replay.background_status(state, 120), "none")

    def test_stale_pending_call_is_unknown_not_live(self) -> None:
        state = {"pending": {"tool-1": {"call": {}, "ts": 100}}, "background_shells": {}}
        self.assertEqual(replay.background_status(state, 100 + replay.OP_DEADLINE_SECS), "unknown")

    def test_background_with_no_later_output_ages_to_unknown(self) -> None:
        state = {"pending": {}, "background_shells": {"tool-1": {"ids": {"tool-1"}, "ts": 100}}}
        self.assertEqual(replay.background_status(state, 100 + replay.OP_DEADLINE_SECS - 1), "live")
        self.assertEqual(replay.background_status(state, 100 + replay.OP_DEADLINE_SECS), "unknown")

    def test_codex_exec_poll_exit_clears_background_session(self) -> None:
        state: dict = {}
        replay.apply_event(state, {
            "ts": 100,
            "call": [{"id": "call-start", "name": "exec", "input": {"cmd": "long command"}}],
        })
        replay.apply_event(state, {
            "ts": 101,
            "result": [{"tool_use_id": "call-start", "content": "Command running with session ID: 4242"}],
        })
        self.assertEqual(replay.background_status(state, 102), "live")

        replay.apply_event(state, {
            "ts": 110,
            "call": [{"id": "call-poll", "name": "exec", "input": {"session_id": 4242}}],
        })
        replay.apply_event(state, {
            "ts": 111,
            "result": [{"tool_use_id": "call-poll", "content": "Process exited with code 78"}],
        })
        self.assertEqual(replay.background_status(state, 111), "none")


if __name__ == "__main__":
    unittest.main()
