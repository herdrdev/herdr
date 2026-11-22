import contextlib
import errno
import importlib.util
import io
import unittest
from pathlib import Path
from unittest import mock


ASSET = Path(__file__).parents[1] / "src/integration/assets/hermes/__init__.py"

HOOK_NAMES = {
    "on_session_start",
    "on_session_reset",
    "pre_llm_call",
    "post_llm_call",
    "on_human_input_request",
    "on_human_input_resolved",
    "pre_approval_request",
    "post_approval_response",
    "on_session_end",
    "agent_loop_stopped",
}

# Old herdr (terminal app) exports HERDR_*; herdr-core (hc) exports HC_*.
HERDR_ENV = {"HERDR_ENV": "1", "HERDR_PANE_ID": "w1:p2"}
HC_ENV = {"HC_TERMINAL_ID": "t10"}


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


class FakeTty:
    """Stands in for os.open/os.write/os.close on terminal devices."""

    def __init__(self, fail=(), writer=None):
        self.fail = set(fail)
        self.opened = []
        self.writes = []
        self.closed = []
        self.fd_paths = {}
        self._next_fd = 40
        self._writer = writer

    def open(self, path, flags):
        self.opened.append((path, flags))
        if path in self.fail:
            raise OSError(errno.ENXIO, "No such device or address", path)
        fd = self._next_fd
        self._next_fd += 1
        self.fd_paths[fd] = path
        return fd

    def write(self, fd, data):
        if self._writer is not None:
            return self._writer(fd, data, self)
        self.writes.append((self.fd_paths[fd], bytes(data)))
        return len(data)

    def close(self, fd):
        self.closed.append(fd)

    def payloads(self):
        return [data for _, data in self.writes]


def osc_payload(state):
    return f"\x1b]7501;state={state}:app=hermes\x1b\\".encode()


@contextlib.contextmanager
def reporting_env(module, env, *, fail=(), candidates=("/dev/tty", "/dev/pts/3"), writer=None):
    """Patch environ plus the tty surface the OSC path writes through."""
    tty = FakeTty(fail=fail, writer=writer)
    module._tty_candidates = lambda: list(candidates)
    with mock.patch.dict(module.os.environ, env, clear=True), \
            mock.patch.object(module.os, "open", tty.open), \
            mock.patch.object(module.os, "write", tty.write), \
            mock.patch.object(module.os, "close", tty.close):
        yield tty


class FakeProc:
    """Serves /proc/<pid>/stat and /proc/<pid>/fd/1 from in-memory maps."""

    def __init__(self, stats, links):
        self.stats = stats
        self.links = links

    def open(self, path, *args, **kwargs):
        parts = path.strip("/").split("/")
        if len(parts) == 3 and parts[0] == "proc" and parts[2] == "stat":
            content = self.stats.get(int(parts[1]))
            if content is not None:
                return io.StringIO(content)
        raise FileNotFoundError(errno.ENOENT, "No such file or directory", path)

    def readlink(self, path):
        target = self.links.get(path)
        if target is None:
            raise OSError(errno.ENOENT, "No such file or directory", path)
        return target


def stat_line(pid, ppid, comm="python3"):
    return f"{pid} ({comm}) S {ppid} {pid} {pid} 34816 2752 0 0 0 0 0 0 0 20 0 1 0 5 0 0"


class HermesIntegrationAssetTests(unittest.TestCase):
    def test_reports_only_root_session_identity(self):
        module = load_asset()
        calls = []
        module._send_session = lambda session_id, start_source: calls.append(
            (session_id, start_source)
        )
        context = FakeContext()
        with mock.patch.dict(module.os.environ, {}, clear=True):
            module.register(context)

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
        with mock.patch.dict(module.os.environ, {}, clear=True):
            module.register(context)

            context.hooks["pre_llm_call"](session_id="resumed", platform="cli")

        self.assertEqual(calls, [("resumed", "resume")])


class EnvironmentGatingTests(unittest.TestCase):
    """T1: each environment enables exactly one reporting path; neither is inert."""

    def fire_all_hooks(self, module, context):
        context.hooks["on_session_start"](session_id="s1", platform="tui")
        context.hooks["on_session_reset"](session_id="s2", platform="tui")
        context.hooks["pre_llm_call"](session_id="s1", platform="cli")
        context.hooks["post_llm_call"](session_id="s1", platform="tui")
        context.hooks["on_human_input_request"](platform="tui", kind="question")
        context.hooks["on_human_input_resolved"](platform="tui", outcome="answered")
        context.hooks["pre_approval_request"](surface="cli", command="rm x")
        context.hooks["post_approval_response"](surface="cli", choice="allow")
        context.hooks["on_session_end"](session_id="s1", completed=True, platform="tui")
        context.hooks["agent_loop_stopped"](platform="tui", reason="user_stop")

    def test_herdr_env_path_reports_via_cli_without_osc(self):
        module = load_asset()
        context = FakeContext()
        with reporting_env(module, HERDR_ENV, fail={"/dev/tty", "/dev/pts/3"}) as tty:
            with mock.patch.object(module.subprocess, "run") as run:
                self.assertEqual(module._pane_id(), "w1:p2")
                self.assertFalse(module._osc_enabled())
                module.register(context)
                context.hooks["on_session_start"](session_id="s1", platform="tui")

        self.assertEqual(run.call_count, 1, "session identity must go through the CLI")
        self.assertEqual(tty.writes, [], "HERDR_ENV alone must not emit OSC 7501")

    def test_hc_terminal_id_path_emits_osc_without_cli(self):
        module = load_asset()
        context = FakeContext()
        with reporting_env(module, HC_ENV, fail={"/dev/tty"}) as tty:
            with mock.patch.object(module.subprocess, "run") as run:
                self.assertIsNone(module._pane_id())
                self.assertTrue(module._osc_enabled())
                module.register(context)
                context.hooks["pre_llm_call"](session_id="s1", platform="tui")

        self.assertEqual(run.call_count, 0, "HC_TERMINAL_ID alone must not shell out")
        self.assertEqual(
            tty.payloads(),
            [osc_payload("idle"), osc_payload("working")],
        )

    def test_neither_environment_is_inert(self):
        module = load_asset()
        context = FakeContext()
        with reporting_env(module, {}) as tty:
            with mock.patch.object(module.subprocess, "run") as run:
                module.register(context)
                self.fire_all_hooks(module, context)

        self.assertEqual(run.call_count, 0)
        self.assertEqual(tty.opened, [])
        self.assertEqual(tty.writes, [])
        self.assertIsNone(module._last)

    def test_blank_environment_values_disable_both_paths(self):
        module = load_asset()
        environment = {"HERDR_ENV": "1", "HERDR_PANE_ID": "   ", "HC_TERMINAL_ID": " "}
        with mock.patch.dict(module.os.environ, environment, clear=True):
            self.assertIsNone(module._pane_id())
            self.assertFalse(module._osc_enabled())


class TtyCandidateTests(unittest.TestCase):
    """T2: /dev/tty first, then each ancestor's stdout pty via /proc."""

    def candidates_with(self, module, proc, pid):
        module.open = proc.open
        with mock.patch.object(module.os, "getpid", lambda: pid), \
                mock.patch.object(module.os, "readlink", proc.readlink):
            return module._tty_candidates()

    def test_dev_tty_first_then_ancestor_ptys(self):
        module = load_asset()
        proc = FakeProc(
            stats={
                100: stat_line(100, 90),
                90: stat_line(90, 80, comm="hermes (gateway)"),
                80: stat_line(80, 1, comm="bash"),
            },
            links={
                # The gateway child's parent logs to a file, the grandparent
                # still owns the pane's pty on stdout.
                "/proc/90/fd/1": "/dev/null",
                "/proc/80/fd/1": "/dev/pts/7",
            },
        )
        paths = self.candidates_with(module, proc, 100)
        self.assertEqual(paths, ["/dev/tty", "/dev/pts/7"])

    def test_repeated_pty_appears_once(self):
        module = load_asset()
        proc = FakeProc(
            stats={
                100: stat_line(100, 90),
                90: stat_line(90, 80),
                80: stat_line(80, 70),
                70: stat_line(70, 1),
            },
            links={
                "/proc/90/fd/1": "/dev/pts/7",
                "/proc/80/fd/1": "/dev/pts/7",
                "/proc/70/fd/1": "/dev/pts/7",
            },
        )
        paths = self.candidates_with(module, proc, 100)
        self.assertEqual(paths, ["/dev/tty", "/dev/pts/7"])

    def test_walk_stops_at_init(self):
        module = load_asset()
        proc = FakeProc(
            stats={100: stat_line(100, 1)},
            links={"/proc/1/fd/1": "/dev/pts/0"},
        )
        self.assertEqual(self.candidates_with(module, proc, 100), ["/dev/tty"])

    def test_walk_stops_on_missing_proc(self):
        module = load_asset()
        proc = FakeProc(stats={}, links={})
        self.assertEqual(self.candidates_with(module, proc, 100), ["/dev/tty"])

    def test_walk_stops_on_malformed_stat(self):
        module = load_asset()
        proc = FakeProc(stats={100: "not a stat line"}, links={})
        self.assertEqual(self.candidates_with(module, proc, 100), ["/dev/tty"])

    def test_walk_is_bounded(self):
        module = load_asset()
        stats = {}
        links = {}
        pid = 1000
        for depth in range(20):
            ppid = pid - 1 if depth < 19 else 1
            stats[pid] = stat_line(pid, ppid)
            links[f"/proc/{ppid}/fd/1"] = f"/dev/pts/{100 + depth}"
            pid = ppid
        proc = FakeProc(stats=stats, links=links)
        paths = self.candidates_with(module, proc, 1000)
        self.assertEqual(paths[0], "/dev/tty")
        self.assertEqual(len(paths), 1 + module._MAX_ANCESTOR_HOPS)


class OscEmitTests(unittest.TestCase):
    """T3: exact bytes, candidate fall-through, _last updated only on success."""

    def test_writes_exact_bytes_to_first_writable_candidate(self):
        module = load_asset()
        with reporting_env(module, HC_ENV, fail={"/dev/tty"}) as tty:
            module._emit("working")

        self.assertEqual(tty.payloads(), [osc_payload("working")])
        self.assertEqual(
            tty.payloads()[0], b"\x1b]7501;state=working:app=hermes\x1b\\"
        )
        self.assertEqual([path for path, _ in tty.opened], ["/dev/tty", "/dev/pts/3"])
        self.assertEqual(module._last, "working")

    def test_opens_write_only_without_controlling_tty(self):
        module = load_asset()
        with reporting_env(module, HC_ENV, fail={"/dev/tty"}) as tty:
            module._emit("idle")

        _, flags = tty.opened[1]
        expected = module.os.O_WRONLY | module.os.O_NOCTTY
        self.assertEqual(flags & expected, expected)

    def test_write_failure_falls_through_to_next_candidate(self):
        module = load_asset()

        def writer(fd, data, tty):
            if tty.fd_paths[fd] == "/dev/tty":
                raise OSError(errno.EIO, "I/O error")
            tty.writes.append((tty.fd_paths[fd], bytes(data)))
            return len(data)

        with reporting_env(module, HC_ENV, writer=writer) as tty:
            module._emit("blocked")

        self.assertEqual(tty.payloads(), [osc_payload("blocked")])
        self.assertEqual(tty.writes[0][0], "/dev/pts/3")
        self.assertEqual(sorted(tty.closed), sorted(tty.fd_paths), "every fd must close")
        self.assertEqual(module._last, "blocked")

    def test_partial_writes_are_completed(self):
        module = load_asset()

        def writer(fd, data, tty):
            step = max(1, len(data) // 3)
            tty.writes.append((tty.fd_paths[fd], bytes(data[:step])))
            return step

        with reporting_env(module, HC_ENV, candidates=("/dev/tty",), writer=writer) as tty:
            module._emit("working")

        rejoined = b"".join(data for _, data in tty.writes)
        self.assertEqual(rejoined, osc_payload("working"))
        self.assertEqual(module._last, "working")

    def test_last_state_updates_only_after_a_successful_write(self):
        module = load_asset()
        with reporting_env(module, HC_ENV, fail={"/dev/tty", "/dev/pts/3"}) as tty:
            module._emit("working")
            self.assertIsNone(module._last)
            self.assertEqual(tty.writes, [])

        # A later hook retries the state that failed before.
        with reporting_env(module, HC_ENV, fail={"/dev/tty"}) as tty:
            module._emit("working")
            self.assertEqual(tty.payloads(), [osc_payload("working")])
            self.assertEqual(module._last, "working")


class StateDedupeTests(unittest.TestCase):
    """T4: consecutive identical states are reported once."""

    def test_consecutive_duplicates_are_skipped(self):
        module = load_asset()
        with reporting_env(module, HC_ENV, candidates=("/dev/tty",)) as tty:
            for state in ("working", "working", "blocked", "blocked", "working"):
                module._emit(state)

        self.assertEqual(
            tty.payloads(),
            [osc_payload("working"), osc_payload("blocked"), osc_payload("working")],
        )
        self.assertEqual(module._last, "working")

    def test_dedupe_survives_hook_reentry(self):
        module = load_asset()
        context = FakeContext()
        with reporting_env(module, HC_ENV, candidates=("/dev/tty",)) as tty:
            module.register(context)
            context.hooks["pre_llm_call"](platform="tui")
            context.hooks["pre_llm_call"](platform="tui")
            context.hooks["post_llm_call"](platform="tui")
            context.hooks["post_llm_call"](platform="tui")

        self.assertEqual(
            tty.payloads(),
            [osc_payload("idle"), osc_payload("working"), osc_payload("done")],
        )


class HookMappingTests(unittest.TestCase):
    """T5: every hook maps to the state herdr-core expects."""

    def test_registered_hook_set(self):
        module = load_asset()
        context = FakeContext()
        with mock.patch.dict(module.os.environ, {}, clear=True):
            module.register(context)
        self.assertEqual(set(context.hooks), HOOK_NAMES)

    def test_each_hook_emits_its_state(self):
        module = load_asset()
        emissions = []
        module._emit = lambda state: emissions.append(state)
        context = FakeContext()
        with mock.patch.dict(module.os.environ, HC_ENV, clear=True):
            module.register(context)
            self.assertEqual(emissions, ["idle"], "register reports a resting agent")

            hooks = context.hooks
            hooks["pre_llm_call"](session_id="s", platform="tui")
            hooks["pre_llm_call"](session_id="child", platform="subagent")
            hooks["on_human_input_request"](platform="tui", kind="question")
            hooks["on_human_input_resolved"](platform="tui", outcome="answered")
            hooks["pre_approval_request"](surface="gateway", command="rm -r x")
            hooks["post_approval_response"](surface="gateway", choice="allow")
            hooks["post_llm_call"](session_id="s", platform="tui")
            hooks["post_llm_call"](session_id="child", platform="subagent")
            hooks["on_session_end"](session_id="s", completed=True, platform="tui")
            hooks["on_session_start"](session_id="s2", platform="tui")
            hooks["on_session_reset"](session_id="s3", platform="tui")
            hooks["agent_loop_stopped"](platform="tui", reason="user_stop")
            hooks["on_session_end"](
                session_id="s3", completed=False, interrupted=True, platform="tui"
            )
            hooks["on_session_end"](
                session_id="child", completed=False, interrupted=True, platform="subagent"
            )

        self.assertEqual(
            emissions,
            [
                "idle",     # register
                "working",  # pre_llm_call (root turn begins)
                "blocked",  # on_human_input_request
                "working",  # on_human_input_resolved
                "blocked",  # pre_approval_request
                "working",  # post_approval_response
                "done",     # post_llm_call (turn completed)
                "idle",     # on_session_start
                "idle",     # on_session_reset
                "idle",     # agent_loop_stopped
                "idle",     # on_session_end (interrupted turn)
            ],
        )

    def test_issue_358_verified_sequence_end_to_end(self):
        # idle -> working -> blocked -> working -> done, as verified in
        # herdrdev/herdr-app#358 with real bytes on the wire.
        module = load_asset()
        context = FakeContext()
        with reporting_env(module, HC_ENV, candidates=("/dev/tty",)) as tty:
            module.register(context)
            hooks = context.hooks
            hooks["pre_llm_call"](session_id="s", platform="tui")
            hooks["on_human_input_request"](platform="tui", kind="question")
            hooks["on_human_input_resolved"](platform="tui", outcome="answered")
            hooks["post_llm_call"](session_id="s", platform="tui")

        self.assertEqual(
            tty.payloads(),
            [
                osc_payload("idle"),
                osc_payload("working"),
                osc_payload("blocked"),
                osc_payload("working"),
                osc_payload("done"),
            ],
        )

    def test_both_environments_report_on_both_paths(self):
        module = load_asset()
        context = FakeContext()
        environment = {**HERDR_ENV, **HC_ENV}
        with reporting_env(module, environment, candidates=("/dev/tty",)) as tty:
            with mock.patch.object(module.subprocess, "run") as run:
                module.register(context)
                context.hooks["on_session_start"](session_id="s1", platform="tui")

        self.assertEqual(run.call_count, 1)
        self.assertEqual(tty.payloads(), [osc_payload("idle")])


class FailSilentTests(unittest.TestCase):
    """T6: broken environments, ttys, and subprocesses never raise into Hermes."""

    def test_unexpected_open_errors_are_swallowed(self):
        module = load_asset()

        def explode(path, flags):
            raise RuntimeError("os.open exploded")

        with reporting_env(module, HC_ENV, candidates=("/dev/tty",)):
            with mock.patch.object(module.os, "open", explode):
                module._emit("working")
        self.assertIsNone(module._last)

    def test_unexpected_walk_errors_are_swallowed(self):
        module = load_asset()

        def explode():
            raise RuntimeError("walk exploded")

        with reporting_env(module, HC_ENV, candidates=("/dev/tty",)):
            # reporting_env installs a quiet candidate list; put the explosion
            # back after the patch is in place.
            module._tty_candidates = explode
            module._emit("working")
        self.assertIsNone(module._last)

    def test_subprocess_errors_are_swallowed(self):
        module = load_asset()
        with mock.patch.dict(module.os.environ, HERDR_ENV, clear=True):
            with mock.patch.object(
                module.subprocess, "run", side_effect=OSError("exec failed")
            ):
                module._send_session("s1", "startup")

    def test_every_hook_survives_a_broken_terminal(self):
        module = load_asset()
        context = FakeContext()

        def refuse(path, flags):
            raise OSError(errno.ENXIO, "No such device or address", path)

        with mock.patch.dict(module.os.environ, HC_ENV, clear=True), \
                mock.patch.object(module.os, "open", refuse):
            module.register(context)
            for callback in context.hooks.values():
                callback()
                callback(session_id=None, platform="tui")
                callback(session_id="s", platform="cli", completed=False)
        self.assertIsNone(module._last)


if __name__ == "__main__":
    unittest.main()
