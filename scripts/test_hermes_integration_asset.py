import importlib.util
import unittest
from pathlib import Path
from unittest import mock


ASSET = Path(__file__).parents[1] / "src/integration/assets/hermes/__init__.py"


def load_asset():
    spec = importlib.util.spec_from_file_location("herdr_hermes_integration", ASSET)
    if spec is None or spec.loader is None:
        raise RuntimeError("could not load Hermes integration asset")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class FakeContext:
    def __init__(self):
        self.hooks = {}

    def register_hook(self, name, callback):
        self.hooks[name] = callback


class HermesIntegrationAssetTests(unittest.TestCase):
    def test_reports_only_root_session_identity(self):
        module = load_asset()
        calls = []
        module._send_session = lambda session_id, start_source: calls.append(
            (session_id, start_source)
        )
        context = FakeContext()
        module.register(context)

        self.assertEqual(
            set(context.hooks),
            {"on_session_start", "on_session_reset", "pre_llm_call"},
        )

        context.hooks["on_session_start"](session_id="root-1", platform="tui")
        context.hooks["pre_llm_call"](session_id="root-1", platform="tui")
        context.hooks["pre_llm_call"](session_id="child", platform="subagent")
        context.hooks["on_session_reset"](session_id="root-2", platform="tui")
        context.hooks["pre_llm_call"](
            session_id="background", platform="tui"
        )

        self.assertEqual(calls, [("root-1", "startup"), ("root-2", "new")])

    def test_send_session_uses_cli_with_the_active_pane(self):
        module = load_asset()
        environment = {
            "HERDR_ENV": "1",
            "HERDR_PANE_ID": "w1:p2",
            "HERDR_BIN_PATH": "C:/bin/herdr.exe",
        }
        with mock.patch.dict(module.os.environ, environment, clear=True):
            with mock.patch.object(module.subprocess, "run") as run:
                module._send_session("session-1", "resume")

        command = run.call_args.args[0]
        self.assertEqual(
            command[:4],
            ["C:/bin/herdr.exe", "pane", "report-agent-session", "w1:p2"],
        )
        self.assertIn("session-1", command)
        self.assertIn("resume", command)
        self.assertFalse(run.call_args.kwargs["check"])
        if module.os.name == "nt":
            self.assertEqual(
                run.call_args.kwargs["creationflags"],
                module.subprocess.CREATE_NO_WINDOW,
            )

    def test_first_turn_recovers_resumed_session_identity(self):
        module = load_asset()
        calls = []
        module._send_session = lambda session_id, start_source: calls.append(
            (session_id, start_source)
        )
        context = FakeContext()
        module.register(context)

        context.hooks["pre_llm_call"](session_id="resumed", platform="cli")

        self.assertEqual(calls, [("resumed", "resume")])

if __name__ == "__main__":
    unittest.main()
