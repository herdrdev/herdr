from __future__ import annotations

import json
import os
import socket
import subprocess
import tempfile
import threading
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("smoke_live_handoff_sessions.sh").resolve()


@unittest.skipUnless(os.name == "posix", "the smoke requires Bash and Unix process tools")
class LiveHandoffSmokeTests(unittest.TestCase):
    request = {"id": "smoke:test", "method": "server.live_handoff", "params": {}}

    def helper_environment(self, root: Path) -> dict[str, str]:
        tools = root / "tools"
        tools.mkdir()
        for name in ("cargo", "herdr"):
            path = tools / name
            path.write_text("#!/bin/sh\nexit 97\n")
            path.chmod(0o755)
        return {
            **os.environ,
            "PATH": f"{tools}{os.pathsep}{os.environ['PATH']}",
            "BASE": str(root / "must-not-be-created"),
            "HERDR_BIN": str(tools / "herdr"),
        }

    def exchange(
        self, response: bytes | None, *, timeout: float = 1
    ) -> subprocess.CompletedProcess[str]:
        with tempfile.TemporaryDirectory(prefix="h-smoke-", dir="/tmp") as directory:
            root = Path(directory)
            environment = self.helper_environment(root)
            address = str(root / "api.sock")
            received = []
            errors = []
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as listener:
                listener.bind(address)
                listener.listen(1)
                listener.settimeout(2)

                def serve() -> None:
                    try:
                        with listener.accept()[0] as connection:
                            connection.settimeout(2)
                            body = bytearray()
                            while b"\n" not in body:
                                chunk = connection.recv(65536)
                                if not chunk:
                                    raise AssertionError("client closed before sending its request")
                                body.extend(chunk)
                            received.append(json.loads(body))
                            if response is None:
                                self.assertEqual(connection.recv(1), b"")
                            else:
                                connection.sendall(response)
                    except BrokenPipeError:
                        if response is None or len(response) <= 1024 * 1024:
                            errors.append("unexpected broken pipe")
                    except Exception as error:
                        errors.append(repr(error))

                worker = threading.Thread(target=serve)
                worker.start()
                try:
                    result = subprocess.run(
                        [
                            "bash",
                            "-c",
                            'source "$1"; json_request "$2" "$3" "$4" "$5"',
                            "smoke-test",
                            str(SCRIPT),
                            address,
                            json.dumps(self.request),
                            "ok",
                            str(timeout),
                        ],
                        env=environment,
                        text=True,
                        capture_output=True,
                        timeout=5,
                    )
                finally:
                    worker.join(timeout=3)
                self.assertFalse(worker.is_alive())
            self.assertEqual(errors, [])
            self.assertEqual(received, [self.request])
            self.assertFalse(Path(environment["BASE"]).exists())
            return result

    def test_source_has_no_setup_or_shell_option_effects(self) -> None:
        with tempfile.TemporaryDirectory(prefix="h-smoke-", dir="/tmp") as directory:
            environment = self.helper_environment(Path(directory))
            result = subprocess.run(
                [
                    "bash",
                    "-c",
                    'before_opts="$-"; trap ":" EXIT; before_trap="$(trap -p EXIT)"; '
                    'source "$1"; test "$-" = "$before_opts" && '
                    'test "$(trap -p EXIT)" = "$before_trap" && test ! -e "$BASE"',
                    "smoke-test",
                    str(SCRIPT),
                ],
                env=environment,
                text=True,
                capture_output=True,
                timeout=5,
            )
            self.assertEqual(result.returncode, 0, result.stderr)

    def test_accepts_matching_success(self) -> None:
        response = {"id": "smoke:test", "result": {"type": "ok"}}
        result = self.exchange(json.dumps(response).encode() + b"\n")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout), response)

    def test_reassembles_a_response_larger_than_one_read(self) -> None:
        response = {
            "id": "smoke:test",
            "result": {"type": "ok"},
            "padding": "x" * (128 * 1024),
        }
        result = self.exchange(json.dumps(response).encode() + b"\n")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout), response)

    def test_rejects_invalid_responses(self) -> None:
        responses = [
            b'{"id":"smoke:test","error":{"code":"handoff_failed","message":"rejected"}}\n',
            b'{"id":"wrong","result":{"type":"ok"}}\n',
            b'{"id":"smoke:test","result":{"type":"pong"}}\n',
            b'{"id":"smoke:test","result":[]}\n',
            b'{"id":"smoke:test","result":{}}\n',
            b'{"id":"smoke:test"}\n',
            b'{"id":"smoke:test","result":{"type":"ok"}}',
            b'{"id":"smoke:test","result":{"type":"ok"}}\nnot json\n',
            b"not json\n",
            b"[]\n",
            b"\xff\n",
            b"",
        ]
        for response in responses:
            with self.subTest(response=response):
                result = self.exchange(response)
                self.assertNotEqual(result.returncode, 0, result.stdout)

    def test_request_timeout_fails(self) -> None:
        result = self.exchange(None, timeout=0.05)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("timed out", result.stderr)

    def test_rejects_oversized_response(self) -> None:
        result = self.exchange(b"x" * (1024 * 1024 + 1) + b"\n")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("response exceeded", result.stderr)

    def run_shell(self, body: str) -> subprocess.CompletedProcess[str]:
        with tempfile.TemporaryDirectory(prefix="h-smoke-", dir="/tmp") as directory:
            root = Path(directory)
            return subprocess.run(
                [
                    "bash",
                    "-c",
                    'source "$1"; ' + body,
                    "smoke-test",
                    str(SCRIPT),
                    str(root / "missing.sock"),
                ],
                env=self.helper_environment(root),
                text=True,
                capture_output=True,
                timeout=15,
            )

    def test_wait_for_server_exit_accepts_successful_child(self) -> None:
        result = self.run_shell('bash -c "exit 0" & child="$!"; wait_for_server_exit "$child"')
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_wait_for_server_exit_preserves_failed_child_status(self) -> None:
        result = self.run_shell('bash -c "exit 7" & child="$!"; wait_for_server_exit "$child"')
        self.assertEqual(result.returncode, 7, result.stderr)

    def test_wait_for_server_exit_rejects_running_child(self) -> None:
        result = self.run_shell(
            'command sleep 30 & child="$!"; '
            'trap \'kill "$child" 2>/dev/null || :; wait "$child" 2>/dev/null || :\' EXIT; '
            'sleep() { :; }; wait_for_server_exit "$child"'
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("did not exit", result.stderr)

    def test_wait_for_api_rejects_missing_replacement_socket(self) -> None:
        result = self.run_shell('sleep() { :; }; wait_for_api "$2"')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("replacement API did not become ready", result.stderr)

    def select_replacement(
        self, rows: list[tuple[int, str]], *, symlink_binary: bool = False
    ) -> subprocess.CompletedProcess[str]:
        with tempfile.TemporaryDirectory(prefix="h-smoke-", dir="/tmp") as directory:
            root = Path(directory)
            environment = self.helper_environment(root)
            requested_binary = root / "binary with spaces" / "herdr"
            requested_binary.parent.mkdir()
            requested_binary.write_text("#!/bin/sh\nexit 97\n")
            requested_binary.chmod(0o755)
            if symlink_binary:
                alias = root / "binary alias with spaces"
                alias.symlink_to(requested_binary.parent, target_is_directory=True)
                requested_binary = alias / "herdr"
            binary = requested_binary.resolve()
            handoff = root / "session.with spaces" / "herdr-handoff-111.sock"
            fixture = root / "processes.txt"
            fixture.write_text(
                "".join(
                    f"{pid} {command.format(binary=binary, requested_binary=requested_binary, handoff=handoff)}\n"
                    for pid, command in rows
                )
            )
            ps = root / "tools" / "ps"
            ps.write_text('#!/bin/sh\ncat "$SMOKE_PS_FIXTURE"\n')
            ps.chmod(0o755)
            environment.update(
                HERDR_BIN=str(requested_binary),
                SMOKE_PS_FIXTURE=str(fixture),
            )
            return subprocess.run(
                [
                    "bash",
                    "-c",
                    'source "$1"; replacement_pid_for_handoff "$2" "$3"',
                    "smoke-test",
                    str(SCRIPT),
                    "111",
                    str(handoff),
                ],
                env=environment,
                text=True,
                capture_output=True,
                timeout=5,
            )

    def test_replacement_requires_exact_binary_and_handoff_path(self) -> None:
        candidates = [
            (111, "{binary} server --handoff-import {handoff} token"),
            (222, "{binary} server --handoff-import {handoff}.other token"),
            (333, "{binary}.other server --handoff-import {handoff} token"),
            (444, "bash -c {binary} server --handoff-import {handoff} token"),
            (555, "{binary} server --handoff-import /wrong/session.sock token"),
        ]
        result = self.select_replacement(candidates)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        result = self.select_replacement(
            candidates + [(666, "{binary} server --handoff-import {handoff} token")]
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "666")

    def test_replacement_accepts_requested_and_resolved_binary_paths(self) -> None:
        for command in (
            "{requested_binary} server --handoff-import {handoff} token",
            "{binary} server --handoff-import {handoff} token",
        ):
            with self.subTest(command=command):
                result = self.select_replacement([(222, command)], symlink_binary=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout.strip(), "222")

    def test_replacement_rejects_ambiguous_processes(self) -> None:
        result = self.select_replacement(
            [
                (222, "{binary} server --handoff-import {handoff} token"),
                (333, "{binary} server --handoff-import {handoff} token"),
            ]
        )
        self.assertNotEqual(result.returncode, 0, result.stdout)
