#!/usr/bin/env python3
"""Measure actual Herdr SSH input rendering with an isolated delayed-echo pane.

    python3 scripts/remote_latency_smoke.py --binary target/debug/herdr --target test-host
    python3 scripts/remote_latency_smoke.py --binary target/debug/herdr --target test-host --reconnect

The target needs an existing compatible Herdr installation and Python 3. Only
new uniquely named sessions are created and removed. No installation is changed.
Each character is echoed by the remote fixture after --delay-ms; use 0 to
measure the actual network without added delay. A separate on-screen
acknowledgement measures when that authoritative update arrives. Nonzero delay
is controlled application delay, not an emulated network RTT benchmark.
Artifacts are private diagnostics: they include terminal output, SSH target,
local and remote paths, and error text. Review/redact them before sharing.
"""

import argparse
import codecs
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import shlex
import signal
import statistics
import struct
import subprocess
import sys
import tempfile
import termios
import time
import unicodedata
import uuid


PROMPT = "HSMOKE> "
ACK = "HSMOKE_ACK:"
WIDTH, HEIGHT = 140, 42


class Screen:
    """Small VT screen reader for the cursor-addressed Herdr renderer.

    Unlike searching raw terminal bytes, this checks the rendered character
    sequence, even when a redraw changes just one cell or splits UTF-8/CSI.
    It intentionally does not emulate graphics, colors, or terminal replies.
    """

    def __init__(self, width=WIDTH, height=HEIGHT):
        self.width, self.height = width, height
        self.row = self.col = 0
        self.saved = (0, 0)
        self.state, self.sequence = "text", ""
        self.replies = []
        self.decoder = codecs.getincrementaldecoder("utf-8")("replace")
        self.clear()

    def clear(self):
        self.cells = [[" "] * self.width for _ in range(self.height)]

    def text(self):
        return "\n".join("".join(row) for row in self.cells)

    def feed(self, data):
        for ch in self.decoder.decode(data):
            if self.state == "string":
                if ch == "\x07":
                    self.state = "text"
                elif ch == "\x1b":
                    self.state = "string_escape"
                continue
            if self.state == "string_escape":
                self.state = "text" if ch == "\\" else "string"
                continue
            if self.state == "charset":
                self.state = "text"
                continue
            if self.state == "escape":
                self.state = "text"
                if ch == "[":
                    self.state, self.sequence = "csi", ""
                elif ch in "]P^_":
                    self.state = "string"
                elif ch in "()*+":
                    self.state = "charset"
                elif ch == "7":
                    self.saved = (self.row, self.col)
                elif ch == "8":
                    self.row, self.col = self.saved
                elif ch == "c":
                    self.clear()
                    self.row = self.col = 0
                continue
            if self.state == "csi":
                if "@" <= ch <= "~":
                    self.csi(ch)
                    self.state = "text"
                else:
                    self.sequence += ch
                continue
            if ch == "\x1b":
                self.state = "escape"
            elif ch == "\r":
                self.col = 0
            elif ch == "\n":
                self.row += 1
                if self.row >= self.height:
                    self.cells.pop(0)
                    self.cells.append([" "] * self.width)
                    self.row = self.height - 1
            elif ch == "\b":
                self.col = max(0, self.col - 1)
            elif ch == "\t":
                self.col = min(self.width - 1, (self.col // 8 + 1) * 8)
            elif ch >= " " and ch != "\x7f":
                if unicodedata.combining(ch):
                    if self.col:
                        self.cells[self.row][self.col - 1] += ch
                    continue
                width = 2 if unicodedata.east_asian_width(ch) in "WF" else 1
                self.cells[self.row][self.col] = ch
                if width == 2 and self.col + 1 < self.width:
                    self.cells[self.row][self.col + 1] = ""
                self.col = min(self.width - 1, self.col + width)

    def csi(self, final):
        private = self.sequence.startswith("?")
        raw = self.sequence.lstrip("?<>=!")
        values = [int(v) if v.isdigit() else 0 for v in raw.split(";")]
        n = values[0] or 1
        if final in "Hf":
            self.row = min(self.height - 1, n - 1)
            self.col = min(self.width - 1, (values[1] or 1) - 1) if len(values) > 1 else 0
        elif final in "ABCD":
            self.row = min(self.height - 1, max(0, self.row + (n if final == "B" else -n if final == "A" else 0)))
            self.col = min(self.width - 1, max(0, self.col + (n if final == "C" else -n if final == "D" else 0)))
        elif final == "G":
            self.col = min(self.width - 1, n - 1)
        elif final == "d":
            self.row = min(self.height - 1, n - 1)
        elif final == "J":
            mode = values[0]
            for row in range(self.height):
                for col in range(self.width):
                    if mode in (2, 3) or (mode == 0 and (row, col) >= (self.row, self.col)) or (mode == 1 and (row, col) <= (self.row, self.col)):
                        self.cells[row][col] = " "
        elif final == "K":
            start = self.col if values[0] == 0 else 0
            end = self.col + 1 if values[0] == 1 else self.width
            self.cells[self.row][start:end] = [" "] * (end - start)
        elif final == "s":
            self.saved = (self.row, self.col)
        elif final == "u" and self.sequence == "?":
            self.replies.append(b"\x1b[?0u")
        elif final == "u" and not private:
            self.row, self.col = self.saved
        elif final == "n" and values[0] == 6:
            self.replies.append(f"\x1b[{self.row + 1};{self.col + 1}R".encode())
        elif final == "c" and not self.sequence.startswith(">"):
            self.replies.append(b"\x1b[?1;2c")
        elif private and final == "h" and 1049 in values:
            self.clear()
            self.row = self.col = 0


class Client:
    def __init__(self, command, env, artifact_dir):
        self.master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", HEIGHT, WIDTH, 0, 0))
        self.log = (artifact_dir / "terminal.ansi").open("wb")
        self.screen = Screen()
        def own_terminal():
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)
        self.process = subprocess.Popen(command, stdin=slave, stdout=slave, stderr=slave, env=env, preexec_fn=own_terminal)
        os.close(slave)

    def pump(self, timeout=0.05):
        readable, _, _ = select.select([self.master], [], [], timeout)
        if readable:
            try:
                data = os.read(self.master, 262144)
            except OSError:
                data = b""
            if not data:
                raise RuntimeError(f"Herdr client closed (exit {self.process.poll()})")
            self.log.write(data)
            self.log.flush()
            self.screen.feed(data)
            for reply in self.screen.replies:
                os.write(self.master, reply)
            self.screen.replies.clear()

    def wait_for(self, predicate, timeout, description):
        deadline = time.monotonic() + timeout
        while not predicate(self.screen.text()):
            if time.monotonic() >= deadline:
                raise RuntimeError(f"Timed out waiting for {description}")
            self.pump()

    def close(self):
        # Unblock terminal restoration before waiting for a child whose PTY we
        # no longer pump. Remote cleanup must still run if process exit fails.
        try:
            os.close(self.master)
        finally:
            self.log.close()
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=5)


class Remote:
    def __init__(self, target, binary):
        self.target, self.binary = target, binary

    def cli(self, *args, timeout=25):
        command = shlex.join(["env", "-u", "HERDR_SOCKET_PATH", "-u", "HERDR_CLIENT_SOCKET_PATH", "-u", "HERDR_SESSION", self.binary, *args])
        result = subprocess.run(["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", self.target, command], capture_output=True, text=True, timeout=timeout)
        if result.returncode:
            raise RuntimeError(f"Remote {' '.join(args[:2])} failed: {result.stderr.strip()}")
        return result.stdout

    def json(self, *args):
        response = json.loads(self.cli(*args))
        if "error" in response:
            raise RuntimeError(f"Remote CLI error: {response['error']}")
        return response


def fixture_command(delay_ms):
    source = f"""import os,sys,termios,time
fd=sys.stdin.fileno()
old=termios.tcgetattr(fd)
raw=termios.tcgetattr(fd)
raw[3] &= ~(termios.ECHO | termios.ICANON)
raw[6][termios.VMIN]=1
raw[6][termios.VTIME]=0
termios.tcsetattr(fd,termios.TCSANOW,raw)
try:
    os.write(1,b'\\x1b[?1049l\\x1b[?25h\\x1b[2J\\x1b[1;1H{ACK}000000\\x1b[3;1H{PROMPT}')
    count=0
    while True:
        data=os.read(fd,1)
        if not data or data == b'\\x04': break
        if not 32 <= data[0] < 127: continue
        time.sleep({delay_ms / 1000!r})
        count+=1
        os.write(1,data+b'\\x1b7\\x1b[1;1H{ACK}'+str(count).zfill(6).encode()+b'\\x1b8')
finally:
    termios.tcsetattr(fd,termios.TCSANOW,old)
"""
    return shlex.join(["python3", "-u", "-c", source])


def bridge_children(client_pid):
    output = subprocess.check_output(["ps", "-axo", "pid=,ppid=,args="], text=True)
    processes = {}
    for line in output.splitlines():
        parts = line.strip().split(None, 2)
        if len(parts) == 3:
            processes[int(parts[0])] = (int(parts[1]), parts[2])
    descendants = {client_pid}
    while True:
        added = {pid for pid, (parent, _) in processes.items() if parent in descendants}
        if added <= descendants:
            break
        descendants |= added
    matches = {}
    for pid in descendants - {client_pid}:
        command = processes[pid][1]
        try:
            argv = shlex.split(command)
        except ValueError:
            continue
        if argv and Path(argv[0]).name == "ssh" and "remote-client-bridge" in command:
            matches[pid] = command
    return matches


def measure(client, expected, char):
    expected += char
    ack = ACK + str(len(expected)).zfill(6)
    started = time.monotonic()
    os.write(client.master, char.encode("ascii"))
    visible_at = ack_at = None
    while visible_at is None or ack_at is None:
        client.pump()
        now = time.monotonic()
        rendered = client.screen.text()
        if visible_at is None and PROMPT + expected in rendered:
            visible_at = now
        if ack_at is None and ack in rendered:
            ack_at = now
        if now - started > 10:
            raise RuntimeError(f"Input/acknowledgement timed out at character {len(expected)}")
    return expected, {"character": char, "visible_ms": round((visible_at - started) * 1000, 2), "authoritative_ack_ms": round((ack_at - started) * 1000, 2)}


def cleanup_fixture(client, remote, session, case_dir, pane, created):
    """Attempt each cleanup independently; session names were reserved before launch."""
    errors = []

    def attempt(label, action):
        try:
            action()
        except Exception as error:
            errors.append(f"{label}: {error}")

    attempt("client screen artifact", lambda: (case_dir / "client-screen.txt").write_text(client.screen.text(), encoding="utf-8"))
    if pane:
        def save_authoritative():
            path = case_dir / "authoritative-screen.txt"
            if not path.exists():
                screen = remote.cli("--session", session, "pane", "read", pane, "--source", "visible", "--format", "text")
                path.write_text(screen, encoding="utf-8")
        attempt("authoritative screen artifact", save_authoritative)
    attempt("local client", client.close)
    try:
        sessions = remote.json("session", "list", "--json")["sessions"]
        created = created or any(item["name"] == session for item in sessions)
    except Exception as error:
        errors.append(f"named session lookup: {error}")
        # Startup may have created the reserved session before its API failed.
        # The exact name is still ours, so attempt stop/delete despite lookup failure.
        created = True
    if created:
        attempt("named session stop", lambda: remote.cli("session", "stop", session, "--json"))
        attempt("named session delete", lambda: remote.cli("session", "delete", session, "--json"))
    return errors


def run_ssh_baseline(args, artifacts):
    case_dir = artifacts / "direct-ssh"
    case_dir.mkdir()
    env = {key: value for key, value in os.environ.items() if not key.startswith("HERDR_")}
    env.update(TERM="xterm-256color", COLORTERM="truecolor")
    command = ["ssh", "-tt", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", args.target, fixture_command(args.delay_ms)]
    client = Client(command, env, case_dir)
    try:
        client.wait_for(lambda text: PROMPT in text and ACK + "000000" in text, 25, "direct SSH fixture prompt")
        expected, samples = "", []
        for char in "ab":
            expected, _ = measure(client, expected, char)
        for char in "cdefghjklmnpqrstuvwxyz"[:args.samples]:
            expected, sample = measure(client, expected, char)
            samples.append(sample)
        (case_dir / "samples.json").write_text(json.dumps(samples, indent=2) + "\n", encoding="utf-8")
        (case_dir / "client-screen.txt").write_text(client.screen.text(), encoding="utf-8")
        return {"samples": samples, "median_visible_ms": round(statistics.median(sample["visible_ms"] for sample in samples), 2), "median_authoritative_ack_ms": round(statistics.median(sample["authoritative_ack_ms"] for sample in samples), 2)}
    finally:
        client.close()


def run_case(args, remote, session, prediction, artifacts):
    case_dir = artifacts / ("prediction-on" if prediction else "prediction-off")
    case_dir.mkdir()
    config = case_dir / "config.toml"
    config.write_text(f"onboarding = false\n[remote]\npredict_input = {str(prediction).lower()}\nmanage_ssh_config = false\n", encoding="utf-8")
    env = {key: value for key, value in os.environ.items() if not key.startswith("HERDR_")}
    env.update(TERM="xterm-256color", COLORTERM="truecolor", HERDR_CONFIG_PATH=str(config), XDG_CONFIG_HOME=str(case_dir / "config"), XDG_STATE_HOME=str(case_dir / "state"))
    command = [str(Path(args.binary).resolve()), "--remote", args.target, "--session", session]
    client = Client(command, env, case_dir)
    created = False
    pane = None
    cleanup_errors = []
    try:
        # Starting the real remote client creates its named session. API polls
        # only address that name and never start or change the default session.
        deadline = time.monotonic() + 45
        while True:
            client.pump(0)
            try:
                remote.json("--session", session, "workspace", "list")
                created = True
                break
            except RuntimeError:
                if client.process.poll() is not None or time.monotonic() >= deadline:
                    raise
                client.pump(0.2)
        response = remote.json("--session", session, "workspace", "create", "--cwd", "/tmp", "--label", "latency-smoke", "--focus")
        pane = response["result"]["root_pane"]["pane_id"]
        remote.cli("--session", session, "pane", "run", pane, fixture_command(args.delay_ms))
        try:
            client.wait_for(lambda text: PROMPT in text and ACK + "000000" in text, 6, "fixture prompt")
        except RuntimeError:
            # A first-run product announcement may cover the otherwise-ready
            # pane. Enter only dismisses it or is ignored by the echo fixture.
            os.write(client.master, b"\r")
            client.wait_for(lambda text: PROMPT in text and ACK + "000000" in text, 10, "fixture prompt after startup announcement")
        expected = ""
        for char in "ab":
            expected, _ = measure(client, expected, char)
        samples = []
        for char in "cdefghjklmnpqrstuvwxyz"[:args.samples]:
            expected, sample = measure(client, expected, char)
            samples.append(sample)
        (case_dir / "samples.json").write_text(json.dumps(samples, indent=2) + "\n", encoding="utf-8")
        reconnect = None
        if args.reconnect and prediction:
            candidates = bridge_children(client.process.pid)
            if len(candidates) != 1:
                raise RuntimeError(f"Expected one SSH bridge owned by test client; found {len(candidates)}")
            bridge_pid, bridge_command = next(iter(candidates.items()))
            if bridge_children(client.process.pid).get(bridge_pid) != bridge_command:
                raise RuntimeError("SSH bridge ownership changed before disconnect test")
            disconnected_at = time.monotonic()
            os.kill(bridge_pid, signal.SIGTERM)
            new_pid = None
            while time.monotonic() - disconnected_at < 30:
                client.pump(0.1)
                replacements = bridge_children(client.process.pid)
                other = [pid for pid in replacements if pid != bridge_pid]
                if len(other) == 1:
                    new_pid = other[0]
                    break
            if new_pid is None:
                raise RuntimeError("Client did not replace its interrupted SSH bridge")
            batch_mode = any(option in ("BatchMode=yes", "-oBatchMode=yes") for option in shlex.split(replacements[new_pid]))
            if args.require_noninteractive_reconnect and not batch_mode:
                raise RuntimeError("Replacement SSH bridge is missing BatchMode=yes")
            # Process creation precedes SSH authentication and the protocol
            # handshake. Clear only our observer's cached screen so old cells
            # cannot be mistaken for a fresh authoritative surface.
            client.screen.clear()
            client.wait_for(lambda text: PROMPT + expected in text and ACK + str(len(expected)).zfill(6) in text, 25, "fresh fixture frame after SSH reconnect")
            reconnect = {"interrupted_bridge_pid": bridge_pid, "replacement_bridge_pid": new_pid, "replacement_batch_mode": batch_mode, "fresh_frame_after_ms": round((time.monotonic() - disconnected_at) * 1000, 2)}
            (case_dir / "reconnect.json").write_text(json.dumps(reconnect, indent=2) + "\n", encoding="utf-8")
            # Visible pixels precede the final presentation-effects-ready
            # acknowledgement. Input is intentionally gated until that reply;
            # this probe checks new input after recovery, not outage replay.
            settle = time.monotonic() + 1
            while time.monotonic() < settle:
                client.pump(0.05)
            expected, sample = measure(client, expected, "Z")
            reconnect.update(verified_after_ms=round((time.monotonic() - disconnected_at) * 1000, 2), post_reconnect_input=sample)
        authoritative = remote.cli("--session", session, "pane", "read", pane, "--source", "visible", "--format", "text")
        (case_dir / "authoritative-screen.txt").write_text(authoritative, encoding="utf-8")
        lines = [line.strip() for line in authoritative.splitlines() if line.startswith(PROMPT)]
        if lines != [PROMPT + expected]:
            raise RuntimeError("Authoritative pane content differs from exact submitted input")
        (case_dir / "client-screen.txt").write_text(client.screen.text(), encoding="utf-8")
        return {"session": session, "prediction": prediction, "command": command, "pane": pane, "samples": samples, "median_visible_ms": round(statistics.median(sample["visible_ms"] for sample in samples), 2), "median_authoritative_ack_ms": round(statistics.median(sample["authoritative_ack_ms"] for sample in samples), 2), "exact_authoritative_input": expected, "reconnect": reconnect, "cleanup_errors": cleanup_errors}
    finally:
        cleanup_errors.extend(cleanup_fixture(client, remote, session, case_dir, pane, created))
        if cleanup_errors:
            print(json.dumps({"session": session, "cleanup_errors": cleanup_errors}), file=sys.stderr)


def self_test():
    screen = Screen(30, 6)
    for chunk in (b"\x1b[2J\x1b[3;1HHS", b"MOKE> ab\x1b[", b"3;11Hc\x1b[1;1HACK:000003"):
        screen.feed(chunk)
    assert "HSMOKE> abc" in screen.text()
    screen.feed(b"\x1b[3;9H\x1b[K")
    assert screen.cells[2][:8] == list(PROMPT)
    assert "abc" not in screen.text()
    screen.feed(b"\x1b]2;fake HSMOKE> secret\x1b\\")
    assert "secret" not in screen.text()
    for byte in "\x1b[4;1H界x".encode():
        screen.feed(bytes([byte]))
    assert screen.cells[3][:3] == ["界", "", "x"]
    cleanup_calls = []

    class FailingArtifact:
        def __truediv__(self, name):
            return self
        def write_text(self, *args, **kwargs):
            raise OSError("simulated full disk")

    class FailingClient:
        screen = Screen()
        def close(self):
            cleanup_calls.append("close")
            raise RuntimeError("simulated child shutdown failure")

    class FailingRemote:
        def json(self, *args):
            cleanup_calls.append("lookup")
            raise RuntimeError("simulated session lookup failure")
        def cli(self, *args):
            cleanup_calls.append(args[1])
            assert args[2] == "owned-test-session"
            if args[1] == "stop":
                raise RuntimeError("simulated stop failure")

    errors = cleanup_fixture(FailingClient(), FailingRemote(), "owned-test-session", FailingArtifact(), None, False)
    assert cleanup_calls == ["close", "lookup", "stop", "delete"]
    assert len(errors) == 4
    print("terminal parser and independent cleanup self-tests passed")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--binary", default="target/debug/herdr")
    parser.add_argument("--target", help="explicit SSH host/alias used for disposable live sessions")
    parser.add_argument("--remote-binary", default=".local/bin/herdr")
    parser.add_argument("--session-prefix", default="latency-smoke")
    parser.add_argument("--delay-ms", type=int, default=180)
    parser.add_argument("--samples", type=int, default=5)
    parser.add_argument("--artifacts", type=Path)
    parser.add_argument("--reconnect", action="store_true")
    parser.add_argument("--require-noninteractive-reconnect", action="store_true", help="fail unless replacement bridge uses BatchMode=yes (requires --reconnect)")
    parser.add_argument("--skip-ssh-baseline", action="store_true", help="skip the same delayed-echo fixture over direct ssh -tt")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        self_test()
        return 0
    if not args.target:
        parser.error("--target is required for live SSH validation")
    if args.require_noninteractive_reconnect and not args.reconnect:
        parser.error("--require-noninteractive-reconnect requires --reconnect")
    if not Path(args.binary).is_file():
        parser.error(f"binary does not exist: {args.binary}")
    if not re.fullmatch(r"[a-zA-Z0-9][a-zA-Z0-9_-]{0,30}", args.session_prefix):
        parser.error("session prefix must be 1-31 letters, digits, underscores or hyphens")
    if not args.target or args.target.startswith("-") or any(ch.isspace() for ch in args.target):
        parser.error("target must be one SSH alias/host, not options")
    if not 1 <= args.samples <= 20 or not 0 <= args.delay_ms <= 400:
        parser.error("samples must be 1-20 and delay-ms 0-400")
    artifacts = args.artifacts.resolve() if args.artifacts else Path(tempfile.mkdtemp(prefix="herdr-remote-smoke-"))
    artifacts.mkdir(parents=True, exist_ok=True)
    remote = Remote(args.target, args.remote_binary)
    token = uuid.uuid4().hex[:10]
    names = [f"{args.session_prefix}-{token}-{mode}" for mode in ("off", "on")]
    existing = {session["name"] for session in remote.json("session", "list", "--json")["sessions"]}
    if existing.intersection(names):
        raise RuntimeError("Generated test session already exists; refusing to reuse it")
    report = {"target": args.target, "artifacts": str(artifacts), "fixture_delay_ms": args.delay_ms, "measurement": "local PTY input to rendered cell, plus separate authoritative on-screen acknowledgement", "cases": []}
    print(f"Live SSH smoke artifacts: {artifacts}", flush=True)
    try:
        if not args.skip_ssh_baseline:
            print("Running direct SSH delayed-echo baseline", flush=True)
            report["direct_ssh"] = run_ssh_baseline(args, artifacts)
        for session, prediction in zip(names, (False, True)):
            print(f"Running {session}, prediction={prediction}", flush=True)
            report["cases"].append(run_case(args, remote, session, prediction, artifacts))
            case = report["cases"][-1]
            print(f"Median visible {case['median_visible_ms']} ms; authoritative ACK {case['median_authoritative_ack_ms']} ms", flush=True)
            (artifacts / "report.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
        baseline, predicted = report["cases"]
        report["prediction_improvement_ms"] = round(baseline["median_visible_ms"] - predicted["median_visible_ms"], 2)
        report["passed"] = not any(case["cleanup_errors"] for case in report["cases"]) and predicted["median_visible_ms"] < baseline["median_visible_ms"] * 0.5 and predicted["median_visible_ms"] < max(50, args.delay_ms * 0.7)
        if not report["passed"]:
            raise RuntimeError("Prediction did not beat the controlled delayed-echo baseline or cleanup failed")
    except Exception as error:
        report.update(passed=False, error=str(error))
    (artifacts / "report.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(report, indent=2))
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
