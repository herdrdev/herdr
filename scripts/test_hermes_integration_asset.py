import importlib.util
import unittest
from pathlib import Path


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
        module._send = lambda method, params: calls.append((method, params))
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

        self.assertEqual(
            calls,
            [
                (
                    "pane.report_agent_session",
                    {
                        "agent_session_id": "root-1",
                        "session_start_source": "startup",
                    },
                ),
                (
                    "pane.report_agent_session",
                    {
                        "agent_session_id": "root-2",
                        "session_start_source": "new",
                    },
                ),
            ],
        )

    def test_first_turn_recovers_resumed_session_identity(self):
        module = load_asset()
        calls = []
        module._send = lambda method, params: calls.append((method, params))
        context = FakeContext()
        module.register(context)

        context.hooks["pre_llm_call"](session_id="resumed", platform="cli")

        self.assertEqual(
            calls,
            [
                (
                    "pane.report_agent_session",
                    {
                        "agent_session_id": "resumed",
                        "session_start_source": "resume",
                    },
                )
            ],
        )

if __name__ == "__main__":
    unittest.main()
