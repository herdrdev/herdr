#!/usr/bin/env python3
"""Run isolated live watchdog fixtures against a disposable Herdr server.

The harness uses only Python's standard library.  Every server, workspace,
worker process, run file and watchdog state file is created below a short-lived
directory and is removed on exit.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import shlex
import signal
import socket
import subprocess
import sys
import tempfile
import time
from typing import Any, Callable

ROOT = Path(__file__).resolve().parents[1]


def _local_server_pids(sock: Path, binary: Path, pgid: int | None = None) -> list[str]:
    """Find only this harness's server process, including on systems without /proc."""
    socket_marker = f"HERDR_SOCKET_PATH={sock}"
    binary_path = str(binary)
    if sys.platform.startswith("linux"):
        leaked = []
        for entry in Path("/proc").iterdir():
            if not entry.name.isdigit():
                continue
            try:
                env = (entry / "environ").read_bytes().split(b"\0")
                argv = (entry / "cmdline").read_bytes().split(b"\0")
            except OSError:
                continue
            if (os.fsencode(socket_marker) in env
                    and os.fsencode(binary_path) in argv and b"server" in argv):
                leaked.append(entry.name)
        return leaked

    if sys.platform == "darwin":
        result = subprocess.run(["ps", "-E", "-ww", "-o", "pid=,command="],
                                text=True, stdout=subprocess.PIPE,
                                stderr=subprocess.PIPE, check=False)
        if result.returncode:
            raise RuntimeError(f"could not inspect local processes with ps: {result.stderr[-500:]}")
        leaked = []
        for line in result.stdout.splitlines():
            fields = line.strip().split(None, 1)
            if len(fields) != 2:
                continue
            pid, details = fields
            if (socket_marker in details and binary_path in details
                    and re.search(r"(?:^|\s)server(?:\s|$)", details)):
                leaked.append(pid)
        return leaked

    # The server runs in its own session, making its process group harness-specific.
    if pgid is None:
        return []
    try:
        os.killpg(pgid, 0)
    except ProcessLookupError:
        return []
    except PermissionError:
        pass
    return [f"process group {pgid}"]


class Harness:
    def __init__(self, args: argparse.Namespace, real_agent: bool = False):
        self.args = args
        self.root = Path(tempfile.mkdtemp(prefix="wdl.", dir="/tmp"))
        self.cfg = self.root / "cfg"
        self.state = self.root / "state"
        self.sock = self.root / "s.sock"
        self.real_agent = real_agent
        if real_agent:
            login_path = subprocess.run([os.environ.get("SHELL", "/bin/bash"), "-lc",
                                         "printf %s \"$PATH\""], text=True,
                                        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                        check=False).stdout
            self.env = {key: os.environ[key] for key in (
                            "HOME", "USER", "LANG", "CODEX_HOME", "CLAUDE_CONFIG_DIR")
                        if key in os.environ}
            self.env["PATH"] = login_path or os.environ.get("PATH", "")
            self.env["HERDR_ENV"] = "1"
        else:
            self.env = {k: v for k, v in os.environ.items() if not k.startswith("HERDR_")}
        self.env.update(XDG_CONFIG_HOME=str(self.cfg), XDG_STATE_HOME=str(self.state))
        self.server: subprocess.Popen[str] | None = None
        self.started: list[subprocess.Popen[Any]] = []
        self.panes: dict[str, str] = {}
        self.timeline = self.args.out / "timeline.jsonl"
        self.peer_root: str | None = None
        self.peer_session: str | None = None
        self.probe_wrapper: Path | None = None
        self.notify_markers: dict[str, Path] = {}
        self.socket_probe_passed: set[str] = set()
        self.real_agent_cwds: dict[str, Path] = {}
        self.real_agent_hook_args: dict[str, list[str]] = {}

    def start(self) -> None:
        app = "herdr-dev" if "debug" in str(self.args.binary) else "herdr"
        config = self.cfg / app / "config.toml"
        config.parent.mkdir(parents=True, exist_ok=True)
        config.write_text("allow_nested = true\n", encoding="utf-8")
        self.state.mkdir(parents=True, exist_ok=True)
        self.env["HERDR_SOCKET_PATH"] = str(self.sock)
        self.server = subprocess.Popen([str(self.args.binary), "server"], cwd=self.root,
                                       env=self.env, stdout=subprocess.DEVNULL,
                                       stderr=subprocess.PIPE, text=True, start_new_session=True)
        deadline = time.monotonic() + 30
        last = ""
        while time.monotonic() < deadline:
            if self.server.poll() is not None:
                last = self.server.stderr.read() if self.server.stderr else ""
                raise RuntimeError(f"disposable server exited {self.server.returncode}: {last}")
            if self.sock.exists():
                try:
                    self.call("ping", {})
                    return
                except (OSError, RuntimeError):
                    pass
            time.sleep(.1)
        raise RuntimeError(f"server socket did not become ready: {last}")

    def call(self, method: str, params: dict[str, Any]) -> dict[str, Any]:
        request = {"id": "watchdog-harness", "method": method, "params": params}
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
            client.settimeout(10)
            client.connect(str(self.sock))
            client.sendall((json.dumps(request) + "\n").encode())
            data = b""
            while b"\n" not in data:
                chunk = client.recv(65536)
                if not chunk:
                    break
                data += chunk
        try:
            result = json.loads(data.splitlines()[0])
        except (IndexError, json.JSONDecodeError) as exc:
            raise RuntimeError(f"invalid response for {method}: {data[:500]!r}") from exc
        if result.get("error"):
            raise RuntimeError(f"{method}: {result['error']}")
        return result.get("result", {})

    def setup_peer_parent(self) -> None:
        if self.peer_session:
            return
        if not self.args.peer or not self.args.peer_binary:
            raise RuntimeError("p-cross-host requires --peer and --peer-binary")
        self.peer_root = f"/tmp/{self.root.name}"
        peer = self.args.peer
        binary = str(self.args.peer_binary)
        xdg_cfg = f"{self.peer_root}/cfg"
        xdg_state = f"{self.peer_root}/state"
        sock = f"{self.peer_root}/s.sock"
        app = "herdr-dev" if "debug" in binary else "herdr"
        root_q, cfg_q, state_q, sock_q = map(shlex.quote,
            (self.peer_root, xdg_cfg, xdg_state, sock))
        binary_q = shlex.quote(binary)
        pidfile_q = shlex.quote(f"{self.peer_root}/server.pid")
        script = (f"mkdir -p {cfg_q}/{app} {state_q}; "
                  f"printf '%s\\n' 'allow_nested = true' > {cfg_q}/{app}/config.toml; "
                  f"setsid nohup env -i PATH=/usr/bin:/bin XDG_CONFIG_HOME={cfg_q} "
                  f"XDG_STATE_HOME={state_q} HERDR_SOCKET_PATH={sock_q} {binary_q} server "
                  f"</dev/null >/dev/null 2>&1 & echo $! > {pidfile_q}; "
                  f"i=0; while [ $i -lt 100 ] && [ ! -S {sock_q} ]; do sleep .1; i=$((i+1)); done; "
                  f"[ -S {sock_q} ] || exit 3; "
                  f"env -i PATH=/usr/bin:/bin XDG_CONFIG_HOME={cfg_q} XDG_STATE_HOME={state_q} "
                  f"HERDR_SOCKET_PATH={sock_q} {binary_q} workspace create --cwd /tmp "
                  f"--label watchdog-peer-parent")
        preflight = subprocess.run(["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=5",
                                    peer, "sh", "-lc", f"hostname -s; whoami; test -x {binary_q}"], text=True,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=12)
        if preflight.returncode:
            raise RuntimeError(f"peer SSH preflight failed ({preflight.returncode}): {preflight.stderr[-500:]}")
        result = subprocess.run(["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=5",
                                 peer, "sh", "-lc", script], text=True,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=40)
        if result.returncode:
            raise RuntimeError(f"peer fixture startup failed ({result.returncode}): {result.stderr[-500:]}")
        payload = _last_json(result.stdout)
        pane_id = _find_value(payload, "pane_id")
        if not pane_id:
            raise RuntimeError(f"peer workspace response omitted pane_id: {result.stdout[-500:]}")
        self.peer_session = f"watchdog-peer-{os.getpid()}"
        report = [binary, "pane", "report-agent-session", str(pane_id), "--source",
                  "herdr:codex", "--agent", "codex", "--agent-session-id", self.peer_session]
        command = (f"env -i PATH=/usr/bin:/bin XDG_CONFIG_HOME={cfg_q} XDG_STATE_HOME={state_q} "
                   f"HERDR_SOCKET_PATH={sock_q} " + " ".join(map(shlex.quote, report)))
        reported = subprocess.run(["ssh", "-o", "BatchMode=yes", peer, command], text=True,
                                  stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=20)
        if reported.returncode:
            raise RuntimeError(f"peer session report failed: {reported.stderr[-500:]}")
        self.probe_wrapper = self.root / "parent-probe"
        self.probe_wrapper.write_text("#!/usr/bin/env python3\nimport os, subprocess, sys\n"
            f"cfg={cfg_q!r}; state={state_q!r}; sock={sock_q!r}; binary={binary!r}\n"
            "host=sys.argv[1]\n"
            "p=subprocess.run(['ssh','-o','BatchMode=yes','-o','ConnectTimeout=5',host,'env','-i',"
            "'PATH=/usr/bin:/bin',"
            "'XDG_CONFIG_HOME='+cfg,'XDG_STATE_HOME='+state,'HERDR_SOCKET_PATH='+sock,"
            "binary,'pane','list'],text=True)\n"
            "sys.exit(p.returncode)\n", encoding="utf-8")
        self.probe_wrapper.chmod(0o755)
        probe = subprocess.run([str(self.probe_wrapper), peer, "herdr", "pane", "list"],
            text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=20)
        if probe.returncode or self.peer_session not in probe.stdout:
            raise RuntimeError("isolated peer parent session was not visible through the parent probe: "
                               f"exit={probe.returncode} stdout={probe.stdout[-500:]} "
                               f"stderr={probe.stderr[-500:]}")

    def cleanup_peer(self) -> None:
        if not self.peer_root or not self.args.peer or not self.args.peer_binary:
            return
        sock = f"{self.peer_root}/s.sock"
        inspect_and_stop = "\n".join((
            "import os, signal, sys, time",
            "sock = os.fsencode('HERDR_SOCKET_PATH=' + sys.argv[1])",
            "binary = os.fsencode(sys.argv[2])",
            "def match(pid):",
            "    try:",
            "        env = open(f'/proc/{pid}/environ', 'rb').read().split(b'\\0')",
            "        argv = open(f'/proc/{pid}/cmdline', 'rb').read().split(b'\\0')",
            "        return sock in env and binary in argv and b'server' in argv",
            "    except OSError:",
            "        return False",
            "pids = [int(p) for p in os.listdir('/proc') if p.isdigit() and match(p)]",
            "print('peer cleanup matched exact server pids:', pids)",
            "for pid in pids:",
            "    os.kill(pid, signal.SIGTERM)",
            "time.sleep(0.5)",
            "left = [int(p) for p in os.listdir('/proc') if p.isdigit() and match(p)]",
            "for pid in left:",
            "    os.kill(pid, signal.SIGKILL)",
            "time.sleep(0.1)",
            "left = [int(p) for p in os.listdir('/proc') if p.isdigit() and match(p)]",
            "print('remaining exact peer servers:', left)",
            "raise SystemExit(bool(left))",
        ))
        cmd = (f"python3 -c {shlex.quote(inspect_and_stop)} {shlex.quote(sock)} "
               f"{shlex.quote(str(self.args.peer_binary))} && "
               f"rm -rf {shlex.quote(self.peer_root)}")
        result = subprocess.run(["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=5",
                                 self.args.peer, cmd], stdout=subprocess.PIPE,
                                stderr=subprocess.PIPE, text=True, timeout=20, check=False)
        if result.stdout.strip():
            print(result.stdout.strip())
        if result.returncode:
            raise RuntimeError(f"peer server cleanup failed ({result.returncode}): {result.stderr[-500:]}")

    def cli(self, *args: str, check: bool = True,
            timeout: int = 45) -> subprocess.CompletedProcess[str]:
        result = subprocess.run([str(self.args.binary), *args], cwd=self.root, env=self.env,
                                text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                timeout=timeout, check=False)
        if check and result.returncode:
            raise RuntimeError(f"herdr {' '.join(args)} exited {result.returncode}: "
                               f"{result.stdout[-800:]} {result.stderr[-800:]}")
        return result

    def workspace(self, label: str, command: str, ready_lines: tuple[str, ...],
                  report_fixture_agent: bool = True, cwd_override: Path | None = None) -> str:
        cwd = cwd_override or (ROOT if self.real_agent else self.root)
        result = self.cli("workspace", "create", "--cwd", str(cwd), "--label", label)
        payload = _last_json(result.stdout)
        pane = _find_value(payload, "pane_id")
        if not pane:
            raise RuntimeError(f"workspace create omitted pane_id: {result.stdout[-500:]}")
        self.panes[label] = str(pane)
        if command:
            self.cli("pane", "run", str(pane), command)
            self.wait_for_pane_stable(str(pane), ready_lines)
        if report_fixture_agent:
            self.call("pane.report_agent", {"pane_id": str(pane), "source": "watchdog-harness",
                                            "agent": "codex", "state": "working"})
        return str(pane)

    def real_agent_screen(self, pane_id: str) -> str:
        response = self.call("pane.read", {"pane_id": pane_id, "source": "detection",
            "lines": 60, "format": "text"})
        return (response.get("read") or {}).get("text", "")

    def real_agent_dialog(self, screen: str) -> str | None:
        """Name known blocking startup dialogs before any prompt is submitted."""
        lowered = screen.lower()
        dialogs = (
            ("codex_update_available", "update available"),
            ("codex_hooks_need_review", "hooks need review"),
            ("codex_folder_trust", "do you trust the contents of this directory"),
            ("claude_folder_trust", "trust this folder?"),
            ("claude_folder_safety_check", "quick safety check:"),
            ("agent_login_required", "login required"),
            ("agent_sign_in_required", "sign in to"),
        )
        return next((name for name, marker in dialogs if marker in lowered), None)

    def prepare_real_agent_prompt(self, pane_id: str) -> None:
        dialog = self.real_agent_dialog(self.real_agent_screen(pane_id))
        if dialog:
            raise RuntimeError(f"blocking_dialog:{dialog}")

    def trust_codex_hooks_for_probe(self, pane_id: str) -> None:
        """Choose Trust all and wait for Codex's isolated pane to clear dialogs."""
        deadline = time.monotonic() + 15
        sent_trust = False
        while time.monotonic() < deadline:
            screen = self.real_agent_screen(pane_id)
            dialog = self.real_agent_dialog(screen)
            if dialog == "codex_hooks_need_review":
                if not re.search(r"(?m)^\s*2[.)]?\s+Trust all\b", screen, re.IGNORECASE):
                    raise RuntimeError("codex_hooks_dialog_missing_trust_all_option")
                self.cli("pane", "send-keys", pane_id, "2", "enter")
                sent_trust = True
            elif dialog:
                raise RuntimeError(f"blocking_dialog:{dialog}")
            else:
                return
            time.sleep(.2)
        if sent_trust:
            raise RuntimeError("blocking_dialog:codex_hooks_dialog_did_not_clear")

    def configure_real_agent_hooks(self, agent: str, ident: str,
                                   cwd: Path) -> list[str]:
        """Configure probe-scoped session/status reporting without editing home configs."""
        hook_source = ROOT / "src/integration/assets" / agent / "herdr-agent-state.sh"
        hook_dir = self.root / f"{agent}-hooks-{ident}"
        hook_dir.mkdir()
        hook = hook_dir / "herdr-agent-state.sh"
        shutil.copy2(hook_source, hook)
        hook.chmod(0o700)
        command = shlex.quote(str(hook)) + " session"
        if agent == "claude":
            closing_block_dir = hook_dir / "closing-block"
            closing_block_dir.mkdir()
            for name in ("closing_block.py", "herdr_status.py", "herdr-closing-block.py"):
                shutil.copy2(ROOT / "src/integration/assets/closing-block" / name,
                             closing_block_dir / name)
            closing_hook = shlex.quote(str(closing_block_dir / "herdr-closing-block.py"))
            config_dir = cwd / ".claude"
            config_dir.mkdir()
            settings = {"hooks": {
                "SessionStart": [{
                    "matcher": "^(startup|resume|clear|compact|fork)$",
                    "hooks": [{"type": "command", "command": command}],
                }],
                "Stop": [{"hooks": [{"type": "command",
                                      "command": "python3 " + closing_hook}]}],
            }}
            (config_dir / "settings.json").write_text(
                json.dumps(settings, indent=2) + "\n", encoding="utf-8")
            return ["--setting-sources", "project"]

        closing_block_dir = hook_dir / "closing-block"
        closing_block_dir.mkdir()
        for name in ("closing_block.py", "herdr_status.py", "herdr-codex-notify.py"):
            shutil.copy2(ROOT / "src/integration/assets/closing-block" / name,
                         closing_block_dir / name)
        notify_marker = hook_dir / "notify-called"
        notify_wrapper = hook_dir / "codex-notify-probe.py"
        notify_wrapper.write_text(
            "import os, sys\n"
            f"open({str(notify_marker)!r}, 'a', encoding='utf-8').write('called\\n')\n"
            f"os.execv(sys.executable, [sys.executable, {str(closing_block_dir / 'herdr-codex-notify.py')!r}, *sys.argv[1:]])\n",
            encoding="utf-8")
        self.notify_markers[ident] = notify_marker
        notify = ["python3", str(notify_wrapper)]
        return ["-c", "notify=" + json.dumps(notify)]

    def real_agent_setup_evidence(self, ident: str, error: Exception) -> str:
        """Capture the isolated pane screen when real-agent setup fails."""
        label = "real-" + ident
        pane = self.panes.get(label)
        if not pane:
            return str(error)
        agents = self.cli("agent", "list", check=False)
        agent_payload = _last_json(agents.stdout)
        agent_rows = ((agent_payload or {}).get("result") or {}).get("agents", [])
        pane_agent = next((item for item in agent_rows
                           if isinstance(item, dict) and item.get("pane_id") == pane), {})
        agent_session = pane_agent.get("agent_session") or {}
        hook_observation = {
            "pane_status": pane_agent.get("agent_status"),
            "session_source": agent_session.get("source"),
            "session_reported": bool(agent_session.get("value")),
            "socket_route_verified": ident in self.socket_probe_passed,
            "codex_notify_called": self.notify_markers.get(ident, Path()).is_file()
                if agent_session.get("source") == "herdr:codex" else None,
        }
        explain = self.cli("agent", "explain", pane, "--json", check=False)
        explain_payload = _last_json(explain.stdout) or {}
        matched_rule = explain_payload.get("matched_rule") or {}
        hook_observation.update({
            "screen_state": explain_payload.get("screen_state"),
            "arbitration": explain_payload.get("arbitration"),
            "matched_rule": matched_rule.get("id"),
            "visible_idle": explain_payload.get("visible_idle"),
            "visible_working": explain_payload.get("visible_working"),
        })
        screen = self.cli("pane", "read", pane, "--source", "detection", "--lines", "60",
                          "--format", "text", check=False)
        process = self.cli("pane", "process-info", "--pane", pane, check=False)
        body = (f"setup_error: {error}\npane: {pane}\nhook_observation: "
                f"{json.dumps(hook_observation, sort_keys=True)}\n\n"
                f"process_info:\n{process.stdout}{process.stderr}\n"
                f"pane_read:\n{screen.stdout}{screen.stderr}")
        evidence_dir = self.args.out / "real-agent-setup-errors"
        evidence_dir.mkdir(parents=True, exist_ok=True)
        evidence_path = evidence_dir / f"{ident}.txt"
        evidence_path.write_text(body, encoding="utf-8")
        return f"{body}\nevidence_file: {evidence_path}"

    def accept_real_agent_folder_trust(self, pane_id: str) -> bool:
        """Accept the CLI trust gate only inside this probe's temporary cwd."""
        screen = self.cli("pane", "read", pane_id, "--source", "detection", "--lines", "50",
                          "--format", "text", check=False)
        text = screen.stdout
        if "Trust this folder?" in text and "1. Trust and continue" in text:
            keys = ["enter"]
        elif ("Quick safety check: Is this a project you created or one you trust?" in text
              and "Yes, I trust this folder" in text):
            keys = ["down", "enter"]
        else:
            return False
        self.cli("pane", "send-keys", pane_id, *keys)
        return True

    def real_agent_process_running(self, pane_id: str, agent: str) -> bool:
        result = self.cli("pane", "process-info", "--pane", pane_id, check=False)
        payload = _last_json(result.stdout)
        if not isinstance(payload, dict):
            return False
        info = _find_value(payload, "process_info")
        processes = info.get("foreground_processes", []) if isinstance(info, dict) else []
        return any(isinstance(item, dict) and item.get("name", "").lower() == agent
                   for item in processes)

    def wait_for_pane_stable(self, pane_id: str, ready_lines: tuple[str, ...]) -> None:
        deadline = time.monotonic() + 20
        previous: str | None = None
        last_screen = ""
        stable_samples = 0
        while time.monotonic() < deadline:
            response = self.call("pane.read", {"pane_id": pane_id, "source": "detection",
                "lines": 40, "format": "text"})
            current = (response.get("read") or {}).get("text", "")
            last_screen = current
            if not any(line.strip() in ready_lines for line in current.splitlines()):
                previous = None
                stable_samples = 0
                time.sleep(.1)
                continue
            if current == previous:
                stable_samples += 1
                if stable_samples >= 2:
                    return
            else:
                previous = current
                stable_samples = 0
            time.sleep(.1)
        screen_tail = "\n".join(last_screen.splitlines()[-20:])
        raise RuntimeError(f"pane {pane_id} did not reach a stable fixture screen; "
                           f"last 20 screen lines:\n{screen_tail}")

    def run_watchdog(self, family: str, options: list[str], dry: bool = True) -> dict[str, Any]:
        command = [str(self.args.binary), "watchdog"]
        if family == "B":
            command.append("workers")
        command += ["--once", "--json", "--confirm-secs", str(self.args.confirm_secs), *options]
        if dry:
            command.append("--dry-run")
        start = time.monotonic()
        result = subprocess.run(command, cwd=self.root, env=self.env, text=True,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                timeout=max(90, self.args.confirm_secs + 60), check=False)
        elapsed = round((time.monotonic() - start) * 1000)
        payload = _last_json(result.stdout) if result.stdout.strip() else {}
        if payload is None:
            payload = {"exit_code": result.returncode, "stdout": result.stdout[-1500:],
                       "stderr": result.stderr[-1000:]}
        payload["_exit_code"] = result.returncode
        payload["_stdout_tail"] = result.stdout[-1500:]
        payload["_stderr_tail"] = result.stderr[-1000:]
        payload["_wall_time_ms"] = elapsed
        payload["_model_latency_ms"] = (payload.get("summary") or {}).get("model_latency_ms")
        with self.timeline.open("a", encoding="utf-8") as output:
            output.write(json.dumps({"family": family, "command": command[1:],
                                     "wall_time_ms": elapsed,
                                     "model_latency_ms": payload["_model_latency_ms"],
                                     "result": payload}, sort_keys=True) + "\n")
        return payload

    def run_real_agent_case(self, agent: str, ident: str, footer: str) -> dict[str, Any]:
        """Exercise watchdog behavior in a real interactive agent pane."""
        executable = shutil.which(agent)
        if not executable:
            return {"id": ident, "watchdog": "real-agents", "expected": "pass",
                    "actual": "skipped", "match": None,
                    "evidence": f"skipped: {agent} executable is unavailable"}
        # Keep both agents in empty temporary directories. Codex project trust
        # and repo-local instructions must not affect the acceptance probe.
        cwd_override = self.real_agent_cwds[ident]
        hook_args = self.real_agent_hook_args[ident]
        pane = self.workspace("real-" + ident, "", (), report_fixture_agent=False,
                              cwd_override=cwd_override)
        socket_probe = (
            "import os; print('watchdog socket probe=' + "
            "('ok' if os.environ.get('HERDR_SOCKET_PATH') == " + repr(str(self.sock))
            + " and not os.environ.get('HERDR_CLIENT_SOCKET_PATH') else 'bad'), flush=True)"
        )
        self.cli("pane", "run", pane, "python3 -c " + shlex.quote(socket_probe))
        self.wait_for_pane_stable(pane, ("watchdog socket probe=ok",))
        if "watchdog socket probe=ok" not in self.real_agent_screen(pane):
            raise RuntimeError("real-agent shell did not retain the isolated API socket override")
        self.socket_probe_passed.add(ident)
        agent_name = "wd-" + hashlib.sha256(ident.encode()).hexdigest()[:12]
        start_args = ["agent", "start", agent_name, "--kind", agent,
                      "--timeout", "30000",
                      "--pane", pane]
        if agent == "claude":
            start_args += ["--", "--model", "sonnet", "--tools", "",
                           *hook_args]
        else:
            start_args += ["--", "--model", "gpt-6-luna", "-c",
                           "model_reasoning_effort=low", "-c", "check_for_update_on_startup=false",
                           "--disable", "multi_agent", "--disable", "multi_agent_v2",
                           "--sandbox", "read-only", *hook_args]
        interactive_env = self.env.copy()
        interactive_env["HERDR_INTERACTIVE"] = "1"
        startup_deadline = time.monotonic() + 15
        started: subprocess.CompletedProcess[str]
        while True:
            started = subprocess.run([str(self.args.binary), *start_args], cwd=self.root,
                                     env=interactive_env, text=True, stdout=subprocess.PIPE,
                                     stderr=subprocess.PIPE, timeout=40, check=False)
            if started.returncode and self.accept_real_agent_folder_trust(pane):
                if time.monotonic() < startup_deadline:
                    time.sleep(.5)
                    continue
            if started.returncode and self.real_agent_process_running(pane, agent):
                # A retry after a startup-state transition can see the already
                # launched process and return agent_name_taken. Reuse the pane
                # process we just started instead of treating it as setup loss.
                started.returncode = 0
                break
            if started.returncode == 0 or "agent_pane_busy" not in (started.stdout + started.stderr):
                break
            if time.monotonic() >= startup_deadline:
                break
            time.sleep(.5)
        if started.returncode:
            detail = (started.stdout + started.stderr)[-1200:]
            process = self.cli("pane", "process-info", "--pane", pane, check=False)
            detail += "\nprocess-info=" + (process.stdout + process.stderr)[-1000:]
            pane_state = self.call("pane.read", {"pane_id": pane, "source": "detection",
                "lines": 40, "format": "text"})
            screen = (pane_state.get("read") or {}).get("text", "")
            detail += "\npane-screen=" + screen[-1400:]
            if "prompt-bar.sh" in detail:
                return {"id": ident, "watchdog": "real-agents", "expected": "pass",
                        "actual": "skipped", "match": None,
                        "evidence": "skipped: host shell startup hook kept the isolated pane "
                                   "from reaching an available prompt"}
            if "readonly database" in detail.lower() or "read-only database" in detail.lower():
                return {"id": ident, "watchdog": "real-agents", "expected": "pass",
                        "actual": "skipped", "match": None,
                        "evidence": "skipped: Codex subscription profile state database is "
                                   "read-only in this execution environment"}
            if any(token in detail.lower() for token in ("not logged", "not authenticated",
                                                         "login required", "log in to",
                                                         "authentication failed", "sign in")):
                return {"id": ident, "watchdog": "real-agents", "expected": "pass",
                        "actual": "skipped", "match": None,
                        "evidence": f"skipped: {agent} subscription auth unavailable: {detail}"}
            dialog = self.real_agent_dialog(screen)
            if dialog:
                raise RuntimeError(f"blocking_dialog:{dialog}: agent start failed: {detail}")
            raise RuntimeError(f"agent start failed: {detail}")
        if agent == "codex":
            self.trust_codex_hooks_for_probe(pane)
        interim_footer = "Now: idle"
        interim_block = "**Needs you: nothing.**\n**Now:** idle\nDone here."
        prompt = ("This is a watchdog acceptance probe. Reply with the exact word READY. "
                  "Then end your reply with exactly this closing block on its own final lines:\n\n"
                  + interim_block + "\nDo not add text after it.")
        self.prepare_real_agent_prompt(pane)
        response = self.cli("agent", "prompt", pane, prompt, "--wait",
                            "--timeout", "30000", timeout=40)
        if response.returncode:
            raise RuntimeError("initial real-agent prompt failed: " + response.stderr[-900:])
        time.sleep(1)
        if ident.endswith("waiting_on_you"):
            closing_block = (
                "**Needs you (1)**\n"
                "1. **Decide** — Continue the acceptance probe?\n"
                "   1a) Continue.\n"
                "   1b) Stop.\n"
                "Reply 1a / 1b. Silence holds.\n"
                "**Now:** " + footer.removeprefix("Now: ")
            )
            expected_status = "blocked"
            final_marker = "Now: waiting on you"
        elif ident.endswith("done_here"):
            closing_block = "**Needs you: nothing.**\n**Now:** idle\nDone here."
            expected_status = "idle"
            final_marker = "Done here."
        else:
            closing_block = "**Needs you: nothing.**\n" + footer
            expected_status = ("working" if ident.endswith("vendor_callback")
                               else ("idle" if agent == "claude" else "unknown"))
            final_marker = footer
        final_prompt = ("Reply with the exact word READY, then end this turn with exactly "
                        "this closing block on its own final lines:\n\n" + closing_block
                        + "\nDo not add text after it.")
        self.prepare_real_agent_prompt(pane)
        if ident.endswith("waiting_on_you"):
            # First let the model finish, then inspect its actual pane output.
            # Waiting for the blocked hook here would hide a malformed reply
            # behind a timeout and leave no chance to repair the setup.
            response = self.cli("agent", "prompt", pane, final_prompt, "--wait",
                                "--timeout", "30000", timeout=40, check=False)
            def observe_closing_setup() -> tuple[bool, dict[str, Any], str]:
                agents_result = self.cli("agent", "list", check=False)
                agents_payload = _last_json(agents_result.stdout)
                rows = ((agents_payload or {}).get("result") or {}).get("agents", [])
                row = next((item for item in rows if isinstance(item, dict)
                            and item.get("pane_id") == pane), {})
                parsed, fields = _closing_setup_signal(row)
                screen_result = self.call("pane.read", {"pane_id": pane,
                    "source": "detection", "lines": 40, "format": "text"})
                tail = (screen_result.get("read") or {}).get("text", "")[-1200:]
                return parsed, fields, tail

            parsed, structured_fields, pane_tail = observe_closing_setup()
            if not parsed:
                retry_prompt = (
                    "The closing-block parser has not reported a parsed blocking item yet. "
                    "Reply READY and finish with this closing block:\n\n" + closing_block)
                self.prepare_real_agent_prompt(pane)
                response = self.cli("agent", "prompt", pane, retry_prompt, "--wait", "--until",
                                    expected_status, "--timeout", "30000", timeout=40,
                                    check=False)
                parsed, structured_fields, pane_tail = observe_closing_setup()
            if not parsed:
                raise RuntimeError("real agent closing-block parser did not report a parsed "
                    "blocking item after one retry; structured fields: "
                    + json.dumps(structured_fields, sort_keys=True)
                    + "; pane tail: " + pane_tail)
        else:
            response = self.cli("agent", "prompt", pane, final_prompt, "--wait", "--until",
                                expected_status, "--timeout", "30000", timeout=40)
            if response.returncode:
                raise RuntimeError("closing-block real-agent prompt failed: " + response.stderr[-900:])
        settled = self.cli("agent", "wait", pane, "--until", expected_status,
                           "--timeout", "30000", timeout=40, check=False)
        if settled.returncode:
            raise RuntimeError(f"real agent hook did not report {expected_status} before stall clock: "
                               + settled.stderr[-900:])
        agent_list = self.cli("agent", "list", check=False)
        agent_payload = _last_json(agent_list.stdout)
        agent_rows = ((agent_payload or {}).get("result") or {}).get("agents", [])
        pane_agent = next((item for item in agent_rows
                           if isinstance(item, dict) and item.get("pane_id") == pane), {})
        agent_session = pane_agent.get("agent_session") or {}
        session_source = agent_session.get("source")
        if session_source != "herdr:" + agent or not agent_session.get("value"):
            raise RuntimeError(f"real {agent} SessionStart hook did not report through the "
                               f"isolated server: source={session_source!r}")
        hook_status = pane_agent.get("agent_status")
        if hook_status != expected_status:
            raise RuntimeError(f"real {agent} status hook reported {hook_status!r}; "
                               f"expected {expected_status!r}")
        screen_result = self.call("pane.read", {"pane_id": pane, "source": "detection",
            "lines": 40, "format": "text"})
        screen_text = (screen_result.get("read") or {}).get("text", "")
        if not ident.endswith("waiting_on_you") and (
                final_marker not in screen_text or interim_footer not in screen_text):
            raise RuntimeError(f"real agent did not change from {interim_footer!r} to "
                               f"{final_marker!r}: {screen_text[-1200:]}")
        # Remove any agent-created composer draft before taking the watchdog baseline.
        self.cli("pane", "send-keys", pane, "ctrl+u")
        pane_before = self.real_agent_screen(pane)
        state = self.root / f"{ident}.json"
        log = self.root / f"{ident}.jsonl"
        options = ["--stall-secs", str(self.args.real_stall_secs),
            "--quiet-secs", str(self.args.real_quiet_secs), "--state-file", str(state),
            "--status-log", str(log), "--confirm-secs", "0", "--no-model"]
        self.run_watchdog("A", options, dry=False)
        time.sleep(self.args.real_stall_secs + self.args.real_quiet_secs + 1)
        payload = self.run_watchdog("A", options, dry=False)
        pane_after = self.real_agent_screen(pane)
        logged_rows = ([json.loads(line) for line in log.read_text().splitlines() if line.strip()]
                       if log.exists() else [])
        decision = next((item for item in payload.get("decisions", [])
                         if item.get("pane_id") == pane), {})
        actual = decision.get("class", "missing")
        status = decision.get("status")
        evidence = decision.get("evidence", "")
        # This was the fixed prompt used by the removed watchdog send path.
        watchdog_text = "resume: continue your open work to its done criterion"
        no_input_fields = all(key not in decision for key in
                              ("action", "action_text", "delivered", "reason", "attempt"))
        no_attempt_rows = all(not any(key in row for key in
                              ("action", "action_text", "delivered", "reason", "attempt"))
                              for row in logged_rows)
        pane_untouched_by_watchdog = watchdog_text not in pane_before + pane_after
        if ident.endswith("promised_idle"):
            classification_ok = (actual in ("stalled", "finished_idle")
                                 and "promised work stopped" in str(evidence)
                                 and status in ("corrected", "consistent"))
        elif ident.endswith("waiting_on_you"):
            classification_ok = actual == "waiting_human" and status in ("corrected", "consistent")
        elif ident.endswith("done_here"):
            classification_ok = (actual == "finished_idle"
                                 and decision.get("new_state", "done") in ("done", None)
                                 and status in ("corrected", "consistent"))
        else:
            # Vendor-specific callback screens are intentionally recorded as observed.
            classification_ok = bool(actual != "missing" and status is not None)
        matched = (classification_ok and no_input_fields and no_attempt_rows
                   and pane_untouched_by_watchdog)
        observation = {"class": actual, "status": status, "new_state": decision.get("new_state"),
                      "evidence": evidence, "hook_session_source": session_source,
                      "hook_status": hook_status,
                      "socket_route_verified": ident in self.socket_probe_passed,
                      "codex_notify_called": self.notify_markers.get(ident, Path()).is_file()
                          if agent == "codex" else None,
                      "no_nudge_fields": no_input_fields,
                      "no_attempt_rows": no_attempt_rows,
                      "pane_has_no_watchdog_text": pane_untouched_by_watchdog,
                      "pane_tail": pane_after[-1800:]}
        expected = ("stalled/finished_idle with promised work stopped" if ident.endswith("promised_idle")
                    else "waiting_human" if ident.endswith("waiting_on_you")
                    else "idle/done" if ident.endswith("done_here")
                    else "record main classification as observed")
        return {"id": ident, "watchdog": "real-agents", "expected": expected,
                "actual": actual, "match": matched,
                "evidence": json.dumps(observation, sort_keys=True)}

    def age_memory(self, path: Path, key: str, seconds: int, *, session: str | None = None) -> None:
        try:
            memory = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            memory = {}
        entry = memory.setdefault(key, {})
        aged = int(time.time()) - seconds
        for field in ("since", "retry_since", "op_since", "quiet_since"):
            if field in entry:
                entry[field] = aged
        if "since" not in entry:
            entry["since"] = aged
        if "draft_since" in entry:
            entry["draft_since"] = aged
        if session is not None:
            entry["agent_session"] = session
        path.write_text(json.dumps(memory) + "\n", encoding="utf-8")

    def age_worker_memory(self, path: Path, worker_id: str, seconds: int,
                          *, previous_turn: bool = False) -> None:
        try:
            memory = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            memory = {}
        workers = memory.setdefault("semantic", {}).setdefault("workers", {})
        key = next((k for k in workers if k == "codex:" + worker_id
                    or k.endswith(":" + worker_id)), "codex:" + worker_id)
        entry = workers.setdefault(key, {})
        aged = int(time.time()) - seconds
        for field in ("since", "retry_since", "op_since"):
            if field in entry:
                entry[field] = aged
        entry.setdefault("since", aged)
        if previous_turn:
            entry["turn_id"] = "previous-turn"
            if entry.get("op_signature"):
                entry["op_since"] = aged
        path.write_text(json.dumps(memory) + "\n", encoding="utf-8")

    def cleanup(self) -> None:
        cleanup_handlers = {sig: signal.signal(sig, signal.SIG_IGN)
                            for sig in (signal.SIGINT, signal.SIGTERM)}
        peer_error: Exception | None = None
        try:
            self.cleanup_peer()
        except (OSError, subprocess.SubprocessError, RuntimeError) as exc:
            peer_error = exc
        try:
            for proc in reversed(self.started):
                if proc.poll() is None:
                    if os.name == "posix" and proc.pid != os.getpid():
                        try:
                            os.killpg(proc.pid, signal.SIGTERM)
                        except ProcessLookupError:
                            pass
                    else:
                        proc.terminate()
                    try:
                        proc.wait(timeout=3)
                    except subprocess.TimeoutExpired:
                        proc.kill()
                        proc.wait()
            if self.server and self.server.poll() is None:
                # Ask the isolated server to shut down its own pane runtimes first.
                try:
                    self.cli("server", "stop", check=False)
                except (OSError, subprocess.SubprocessError):
                    pass
                try:
                    self.server.wait(timeout=8)
                except subprocess.TimeoutExpired:
                    pass
            if self.server:
                # This process group belongs to the harness; the parent can exit
                # before a detached child does.
                try:
                    os.killpg(self.server.pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
                time.sleep(.2)
                try:
                    os.killpg(self.server.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                self.server.wait()
            leaked = _local_server_pids(self.sock, self.args.binary,
                                        self.server.pid if self.server else None)
            if leaked:
                raise RuntimeError(f"local harness server still running for {self.sock}: {leaked}")
        finally:
            shutil.rmtree(self.root, ignore_errors=True)
            for sig, handler in cleanup_handlers.items():
                signal.signal(sig, handler)
        if peer_error:
            raise RuntimeError(f"harness teardown failed: {peer_error}") from peer_error

    def incident_rearm_check(self) -> dict[str, Any]:
        parent_pane = self.workspace("incident-parent", _script("Parent remains available"),
                                     ("Parent remains available",))
        parent_session = "incident-parent-session"
        self.call("pane.report_agent_session", {"pane_id": parent_pane,
            "source": "herdr:codex", "agent": "codex", "agent_session_id": parent_session})
        runs = self.root / "incident-runs" / "incident-worker"
        turn = runs / "turns" / "turn-1"
        turn.mkdir(parents=True)
        worker = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(3600)"],
            cwd=self.root, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            start_new_session=True)
        self.started.append(worker)
        now = int(time.time())
        old = now - 3600
        trace = turn / "trace.log"
        trace.write_text("codex\nwaiting for external operation\n")
        os.utime(trace, (old, old))
        (turn / "start.json").write_text(json.dumps({"started_at": now - 3600}))
        (runs / "state.json").write_text(json.dumps({"schema": 1, "run_id": "incident-worker",
            "host": self.args.host_label, "agent": "codex", "pid": worker.pid,
            "started_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), "state": "active",
            "parent": {"host": self.args.host_label, "pane": parent_pane,
                       "session": parent_session}}))
        state = self.root / "incident-state.json"
        log = self.root / "incident-log.jsonl"
        options = ["--stall-minutes", "1", "--runs-dir", str(runs.parent),
                   "--history-days", "3650", "--all",
                   "--claude-projects-dir", str(self.root / "incident-claude-projects"),
                   "--state-file", str(state), "--log-file", str(log),
                   "--local-host", self.args.host_label]
        first = self.run_watchdog("B", options, dry=False)
        second = self.run_watchdog("B", options, dry=False)
        records = log.read_text(encoding="utf-8").splitlines() if log.exists() else []
        records_after_second = len(records)
        with trace.open("a", encoding="utf-8") as output:
            output.write("new semantic progress\n")
        recovered = self.run_watchdog("B", options, dry=False)
        self.age_worker_memory(state, "incident-worker", 3600)
        os.utime(trace, (old, old))
        rearmed = self.run_watchdog("B", options, dry=False)
        records_after_rearm = len(log.read_text().splitlines()) if log.exists() else 0
        def incident_action(payload: dict[str, Any]) -> str | None:
            decisions = payload.get("decisions", [])
            item = next((d for d in decisions if d.get("worker_id") == "incident-worker"), {})
            return (item.get("incident") or {}).get("action")
        return {"first_action": incident_action(first), "second_action": incident_action(second),
                "records_after_second": records_after_second,
                "records": [json.loads(record) for record in records],
                "recovery_class": next((d.get("class") for d in recovered.get("decisions", [])
                    if d.get("worker_id") == "incident-worker"), None),
                "rearmed_action": incident_action(rearmed), "records_after_rearm": records_after_rearm}


def _last_json(text: str) -> Any:
    for line in reversed(text.splitlines()):
        try:
            return json.loads(line)
        except json.JSONDecodeError:
            continue
    return None


def _closing_setup_signal(agent_row: dict[str, Any]) -> tuple[bool, dict[str, Any]]:
    """Check the parser's structured report for a blocking closing item."""
    tokens = agent_row.get("tokens")
    tokens = tokens if isinstance(tokens, dict) else {}
    fields = {key: tokens.get(key) for key in
              ("closing_parse", "closing_blocking", "closing_wait")}
    try:
        blocking_count = int(tokens.get("closing_blocking", 0))
    except (TypeError, ValueError):
        blocking_count = 0
    return tokens.get("closing_parse") == "ok" and blocking_count >= 1, fields


def _find_value(value: Any, key: str) -> Any:
    if isinstance(value, dict):
        if key in value and value[key]:
            return value[key]
        for child in value.values():
            found = _find_value(child, key)
            if found:
                return found
    elif isinstance(value, list):
        for child in value:
            found = _find_value(child, key)
            if found:
                return found
    return None


def _fixture_command(code: str) -> str:
    if sys.platform == "darwin":
        launcher = "import os,sys; os.execv(sys.executable, ['codex','-u','-c', " + repr(code) + "])"
        return "python3 -u -c " + shlex.quote(launcher)
    linux_code = "import ctypes; ctypes.CDLL(None).prctl(15,b'codex',0,0,0)\n" + code
    return "python3 -u -c " + shlex.quote(linux_code)


def _script(text: str, repeat: bool = False) -> str:
    literal = json.dumps(text)
    if repeat:
        code = ("import time\n" + f"print({literal}, flush=True)\n" +
                "i=0\nwhile True:\n time.sleep(2)\n i+=1\n " +
                f"print({literal} + ' ' + str(i), flush=True)\n")
    else:
        code = f"print({literal}, flush=True)\nimport time; time.sleep(3600)\n"
    return _fixture_command(code)


def _promised_draft_agent(root: Path, account: str, active_tool: bool = False) -> str:
    script = root / "fake_agent.sh"
    script.write_text('''#!/bin/sh
draft=cont
draw() {
  printf '\\033[2J\\033[H'
  printf '\\033]0;Codex\\007'
  printf '%s\\n' \\
    "Now: Codex reviewers — reviewing PR 1656; the config-folder worker starts after it merges" \\
    "⎿ Stop says: /review completed — invoke /retro to capture lessons." \\
    "__ACCOUNT_LINE__"
  if [ -n "${turn1:-}" ]; then printf '%s\\n' "$turn1" "$turn2"; fi
  printf '%s\\n' '────────────────────────' "❯ $draft" \\
    '────────────────────────' \\
    '░░░░░░ 92% 78k tokens │ 2h 24m ago │ -- INSERT -- · 1 feedback draft'
}
draw
while IFS= read -r submitted; do
  turn1="❯ $draft"
  turn2="cont$submitted"
  draft=
  draw
done
'''.replace("__ACCOUNT_LINE__", account), encoding="utf-8")
    if active_tool:
        script.write_text(script.read_text(encoding="utf-8").replace("draw\nwhile", "sleep 3600 &\ndraw\nwhile"),
                          encoding="utf-8")
    script.chmod(0o755)
    return "clear; exec /bin/sh " + shlex.quote(str(script))


def setup_quiet(h: Harness, ident: str) -> str:
    code = "import subprocess,time; print('Running cargo test', flush=True); " \
           "subprocess.Popen(['sleep','3600']); time.sleep(3600)"
    return h.workspace(ident, _fixture_command(code), ("Running cargo test",))


def setup_stall(h: Harness, ident: str) -> str:
    return h.workspace(ident, _script("static build output"), ("static build output",))


def setup_prompt(h: Harness, ident: str) -> str:
    code = "import subprocess,time; print('Overwrite generated snapshot? [y/n]', flush=True); " \
           "subprocess.Popen(['sleep','3600']); time.sleep(3600)"
    return h.workspace(ident, _fixture_command(code),
                       ("Overwrite generated snapshot? [y/n]",))


def setup_progress(h: Harness, ident: str) -> str:
    return h.workspace(ident, _script("Compiling crate", repeat=True), ("Compiling crate",))


def setup_summary(h: Harness, ident: str) -> str:
    return h.workspace(ident, _script("Build completed successfully"),
                       ("Build completed successfully",))


def setup_promised_draft(h: Harness, ident: str) -> str:
    account = ("You hit your usage limit" if "usage_limit" in ident else
               "Please sign in" if "logged_out" in ident else
               "" if ident in ("promised_stale_working_stalled",
                             "promised_stale_working_active_tool") else
               "● Background shell command didn't finish before the previous session ended")
    screen = ("Now: Codex reviewers — reviewing PR 1656; the config-folder worker starts after it merges\n"
              "⎿ Stop says: /review completed — invoke /retro to capture lessons.\n"
              f"{account}\n"
              "────────────────────────\n❯ cont\n────────────────────────\n"
              "░░░░░░ 92% 78k tokens │ 2h 24m ago │ -- INSERT -- · 1 feedback draft")
    ready = tuple(screen.splitlines())
    pane_id = h.workspace(ident, _promised_draft_agent(
        h.root, account, ident == "promised_stale_working_active_tool"), ready)
    return pane_id


def setup_gate_inline_reply(h: Harness, ident: str) -> str:
    screen = ("**Needs you (1)**\n"
              "1. **Approve** — review the change\n"
              "a) Approve. b) Hold. Reply 1a / 1b. Silence holds.\n"
              "Now: waiting at the /hcode review gate.\n\n"
              "› Stall check: verify live state, then reply only `Progressing`\n"
              "• No subagents are active or stalled. PR #1674 remains at the required human review gate.\n"
              "↳ Recap: the required human diff review still blocks rollout.\n"
              "Next: Have you reviewed the diff and approved the merge and rollout?\n"
              "────────────────────────\n› Ask Codex to do anything\n"
              "────────────────────────\n0 shells")
    return h.workspace(ident, _script(screen),
                       ("0 shells",))


def setup_gate_now_waiting_review(h: Harness, ident: str) -> str:
    screen = ("Now: waiting at the /hcode review gate.\n"
              "────────────────────────\n› Ask Codex to do anything\n"
              "────────────────────────\n0 shells")
    return h.workspace(ident, _script(screen), ("0 shells",))


def setup_promised_background(h: Harness, ident: str) -> str:
    kind = "shell" if "shell" in ident else "agent"
    command = _promised_draft_agent(h.root, "")
    script = h.root / "fake_agent.sh"
    contents = script.read_text(encoding="utf-8")
    contents = contents.replace("draft=cont", "draft=")
    contents = contents.replace(
        "Now: Codex reviewers — reviewing PR 1656; the config-folder worker starts after it merges",
        "Now: running the verification job until its result is ready")
    if kind == "shell":
        contents = contents.replace("1 feedback draft", "1 shell")
    else:
        contents = contents.replace("1 feedback draft", "0 shells")
        contents = contents.replace(
            "'░░░░░░ 92% 78k tokens │ 2h 24m ago │ -- INSERT -- · 0 shells'",
            "'░░░░░░ 92% 78k tokens │ 2h 24m ago │ -- INSERT -- · 0 shells' "
            "'● main' '◯ subagent  Running tests'")
    script.write_text(contents, encoding="utf-8")
    return h.workspace(ident, command,
                       ("Now: running the verification job until its result is ready",))


def setup_done_here(h: Harness, ident: str) -> str:
    screen = ("Needs you: nothing.\nDone here.\n────────────────────────\n❯ \n"
              "────────────────────────\n0 shells")
    return h.workspace(ident, _script(screen), ("Needs you: nothing.", "Done here."))


def setup_done_here_suffix(h: Harness, ident: str) -> str:
    screen = ("It will make a round 2 prototype, open it on your Mac, and update the checklist and the data format before asking for your go-ahead.\n"
              "Needs you: nothing.\n"
              "Now: Done here — round 2 arrives in the Translation Text Editing tab.\n"
              "────────────────────────\n❯ \n────────────────────────\n"
              "░░░░░░ 87% 127k ⚓28.8k │ ~/Repos/scalablev2 │ Opus·med @scalable.so │ 29m ago")
    return h.workspace(ident, _script(screen), ("Needs you: nothing.", "Done here —"))


def setup_retry(h: Harness, ident: str) -> str:
    return h.workspace(ident, _script("API 429; retry in 120 seconds"),
                       ("API 429; retry in 120 seconds",))


def setup_account(h: Harness, ident: str) -> str:
    return h.workspace(ident, _script("You hit your usage limit; change account"),
                       ("You hit your usage limit; change account",))


def setup_spinner(h: Harness, ident: str) -> str:
    code = "import itertools,time; glyphs=itertools.cycle('◐◓◑◒'); " \
           "exec(\"while True:\\n print(next(glyphs), flush=True)\\n time.sleep(2)\")"
    return h.workspace(ident, "python3 -u -c " + shlex.quote(code), ("◐", "◓", "◑", "◒"))


def setup_quote(h: Harness, ident: str) -> str:
    return h.workspace(ident, _script('> "Should I continue?"\nNew turn started\nWorking on files'),
                       ("Working on files",))


def setup_dialog(h: Harness, ident: str) -> str:
    return h.workspace(ident, _script("Do you want to allow this command?\n❯ 1. Yes\n  2. No\nEsc to cancel"),
                       ("Esc to cancel",))


def setup_prose_question(h: Harness, ident: str) -> str:
    return h.workspace(ident, _script("Which deployment option should I choose?"),
                       ("Which deployment option should I choose?",))


def setup_worker(h: Harness, ident: str) -> dict[str, str]:
    # Each run directory has its own live fixture process and aged trace.
    if ident == "b-claude-subagent":
        session = "claude-parent-" + ident
        parent_pane = h.workspace("parent-" + ident,
                                  _script(f"Parent Claude fixture {session}"),
                                  (f"Parent Claude fixture {session}",))
        h.call("pane.report_agent_session", {"pane_id": parent_pane,
            "source": "watchdog-harness", "agent": "claude", "agent_session_id": session})
        parent_transcript = h.root / "claude-projects" / "project" / f"{session}.jsonl"
        parent_transcript.parent.mkdir(parents=True, exist_ok=True)
        parent_transcript.write_text(json.dumps({"type": "assistant", "message": {
            "content": [{"type": "text", "text": "Parent session active"}]}}) + "\n")
        transcript = h.root / "claude-projects" / "project" / session / "subagents" / f"{ident}.jsonl"
        transcript.parent.mkdir(parents=True, exist_ok=True)
        transcript.write_text(json.dumps({"type": "assistant", "message": {
            "content": [{"type": "tool_use", "id": "toolu-live", "name": "Bash",
                         "input": {"command": "sleep 3600"}}]}}) + "\n")
        return {"runs": str(h.root / "runs")}
    runs = h.root / "runs" / ident
    runs.mkdir(parents=True, exist_ok=True)
    current_turn = "turn-2" if ident in ("b-resumed", "b-resumed-same-op") else "turn-1"
    if current_turn == "turn-2":
        prior = runs / "turns" / "turn-1"
        prior.mkdir(parents=True, exist_ok=True)
        (prior / "start.json").write_text(json.dumps({"started_at": int(time.time()) - 60}))
        (prior / "trace.log").write_text("older turn output\n")
    turn = runs / "turns" / current_turn
    turn.mkdir(parents=True, exist_ok=True)
    old = int(time.time()) - 3600
    terminal = ident == "b-finished"
    retry = "retry in 120 seconds\n" if "retry" in ident else ""
    prompt = "Overwrite? [y/n]\n" if "subprocess-yn" in ident else ""
    approval = ident == "b-approval"
    trace = turn / "trace.log"
    quote = '> "Should I continue?"\nprogress made\n' if ident == "b-quoted-question" else ""
    operation = "codex\nexec\necho build\n" if ident == "b-resumed-same-op" else "codex\n"
    trace.write_text(quote + operation + retry + prompt)
    if ident == "b-spinner-progress":
        code = ("import time; p=" + repr(str(trace)) + "; i=0\nwhile True:\n "
                "i+=1; open(p,'a').write(f'progress {i}\\n'); time.sleep(1)\n")
        process_code = code
    elif ident == "b-spinner-only":
        # Keep the worker alive, but don't refresh trace mtime after setup ages it.
        code = ("import time; p=" + repr(str(trace)) + "; open(p,'a').write('◐ (12s)\\n'); "
                "time.sleep(3600)\n")
        process_code = code
    elif ident in ("b-quiet-build", "b-subprocess-yn"):
        process_code = "import subprocess,time; subprocess.Popen(['sleep','3600']); time.sleep(3600)"
    else:
        process_code = "import time; time.sleep(3600)"
    worker = subprocess.Popen([sys.executable, "-u", "-c", process_code], cwd=h.root,
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
    h.started.append(worker)
    if ident not in ("b-quiet-build", "b-silent-stall", "b-retry-renewed",
                     "b-spinner-only", "b-resumed-same-op"):
        os.utime(trace, (time.time(), time.time()))
    parent: dict[str, Any] = {"host": h.args.host_label, "pane": "nonexistent",
                              "workspace": "missing", "session": "missing"}
    if ident == "p-unreachable":
        parent["host"] = "wd-unreachable.invalid"
    elif ident == "p-present-local":
        parent_pane = h.workspace("parent-" + ident, _script("Parent agent fixture"),
                                  ("Parent agent fixture",))
        parent_session = "parent-session-" + ident
        h.call("pane.report_agent_session", {"pane_id": parent_pane,
            "source": "watchdog-harness", "agent": "codex", "agent_session_id": parent_session})
        parent.update(pane=parent_pane, session=parent_session)
    elif ident == "p-cross-host":
        h.setup_peer_parent()
        parent.update(host=h.args.peer, session=h.peer_session)
    worker_pid = 99999999 if ident == "b-dead-pid" else worker.pid
    (runs / "state.json").write_text(json.dumps({"schema": 1, "run_id": ident,
        "host": h.args.host_label, "agent": "codex", "pid": worker_pid,
        "started_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "state": "complete" if terminal else ("blocked" if approval else "active"),
        "blocked_reason": "approval required" if approval else None,
        "progress_at": None, "parent": parent}))
    os.utime(runs / "state.json", (old, old))
    (turn / "start.json").write_text(json.dumps({"started_at": int(time.time())}))
    if ident in ("b-quiet-build", "b-silent-stall", "b-retry-renewed",
                 "b-spinner-only", "b-resumed-same-op"):
        os.utime(trace, (old, old))
    if terminal:
        (turn / "receipt.json").write_text(json.dumps({"status": "ended-unverified", "exit_code": 0}))
    return {"runs": str(h.root / "runs")}


# Each row is deliberately self-contained: id, watchdog family, fixture setup,
# expected class. B and P records are created by their setup functions above.
CASES: list[tuple[str, str, Callable[[Harness, str], Any], str]] = [
    ("a-quiet-build", "A", setup_quiet, "working"),
    ("a-silent-stall", "A", setup_stall, "stalled"),
    ("a-quoted-question", "A", setup_quote, "working"),
    ("a-subprocess-yn", "A", setup_prompt, "waiting_tool_input"),
    ("a-finished-idle", "A", setup_summary, "finished_idle"),
    ("stale_draft_promised_work_stalled", "A", setup_promised_draft, "stalled"),
    ("promised_quiet_idle_stalled", "A", setup_promised_draft, "stalled"),
    ("promised_stale_working_stalled", "A", setup_promised_draft, "stalled"),
    ("promised_stale_working_active_tool", "A", setup_promised_draft, "working"),
    ("promised_stale_working_dead_marker_stalled", "A", setup_promised_draft, "stalled"),
    ("gate_inline_reply", "A", setup_gate_inline_reply, "waiting_human"),
    ("gate_now_waiting_review", "A", setup_gate_now_waiting_review, "waiting_human"),
    ("promised_background_shell_past_deadline", "A", setup_promised_background, "stalled"),
    ("promised_background_agent_past_deadline", "A", setup_promised_background, "stalled"),
    ("fresh_draft_typing", "A", setup_promised_draft, "working"),
    ("done_here_negative_control", "A", setup_done_here, "finished_idle"),
    ("done_here_with_suffix", "A", setup_done_here_suffix, "finished_idle"),
    ("a-retry-backoff", "A", setup_retry, "waiting_retry"),
    ("a-retry-renewed", "A", setup_retry, "stalled"),
    ("a-account-limit", "A", setup_account, "waiting_human"),
    ("a-spinner-only", "A", setup_spinner, "stalled"),
    ("a-spinner-progress", "A", setup_progress, "working"),
    ("a-resumed", "A", setup_quote, "working"),
    ("a-approval-dialog", "A", setup_dialog, "waiting_human"),
    ("a-approval-hook", "A", setup_dialog, "waiting_human"),
    ("a-missing-parent", "A", setup_progress, "working"),
    ("a-prose-question", "A", setup_prose_question, "unknown"),
    ("b-quiet-build", "B", setup_worker, "tool_wait"),
    ("b-silent-stall", "B", setup_worker, "suspected_stall"),
    ("b-quoted-question", "B", setup_worker, "working"),
    ("b-subprocess-yn", "B", setup_worker, "waiting_tool_input"),
    ("b-finished", "B", setup_worker, "finished"),
    ("b-retry-backoff", "B", setup_worker, "waiting_retry"),
    ("b-retry-renewed", "B", setup_worker, "suspected_stall"),
    ("b-spinner-only", "B", setup_worker, "suspected_stall"),
    ("b-spinner-progress", "B", setup_worker, "working"),
    ("b-resumed", "B", setup_worker, "working"),
    ("b-resumed-same-op", "B", setup_worker, "suspected_stall"),
    ("b-approval", "B", setup_worker, "waiting_approval"),
    ("b-dead-pid", "B", setup_worker, "dead"),
    ("b-claude-subagent", "B", setup_worker, "working"),
    ("p-present-local", "P", setup_worker, "present"),
    ("p-missing-local", "P", setup_worker, "orphaned"),
    ("p-cross-host", "P", setup_worker, "present"),
    ("p-unreachable", "P", setup_worker, "parent_unknown"),
]


REAL_AGENT_CASES = [
    (f"real_{agent}_{suffix}", agent, footer)
    for agent in ("claude", "codex")
    for suffix, footer in (
        ("promised_idle", "Now: preparing the acceptance response"),
        ("waiting_on_you", "Now: waiting on you"),
        ("done_here", "Done here."),
        ("vendor_callback", "Now: wait — the vendor's callback"),
    )
]


def self_test(args: argparse.Namespace) -> int:
    h = Harness(args)
    try:
        h.start()
        pane_id = h.workspace("self-test", _script("harness fixture ready"),
                              ("harness fixture ready",))
        deadline = time.monotonic() + 20
        pane_text = ""
        while time.monotonic() < deadline:
            response = h.call("pane.read", {"pane_id": pane_id, "source": "detection",
                "lines": 20, "format": "text"})
            pane_text = (response.get("read") or {}).get("text", "")
            if "harness fixture ready" in pane_text:
                break
            time.sleep(.1)
        if "harness fixture ready" not in pane_text:
            raise RuntimeError("fixture command did not produce the expected pane output")
        proc = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(3600)"],
                                cwd=h.root, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        h.started.append(proc)
        run_dir = h.root / "self-test-runs" / "self-test-run"
        (run_dir / "turns" / "turn-1").mkdir(parents=True)
        (run_dir / "state.json").write_text(json.dumps({"schema": 1, "run_id": "self-test-run",
            "host": args.host_label, "agent": "codex", "pid": proc.pid,
            "started_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), "state": "active"}))
        (run_dir / "turns" / "turn-1" / "start.json").write_text(
            json.dumps({"started_at": time.time()}))
        (run_dir / "turns" / "turn-1" / "trace.log").write_text("codex\nself-test fixture\n")
        (h.root / "timeline.jsonl").write_text(json.dumps({"self_test": True}) + "\n")
        print("SELF_TEST server_started=true workspace_created=true pane_created=true "
              "pane_output_verified=true "
              f"worker_pid={proc.pid} timeline_written=true")
        return 0
    except Exception as exc:
        print(f"SELF_TEST_DENIED {type(exc).__name__}: {exc}", file=sys.stderr)
        return 1
    finally:
        h.cleanup()
        print("SELF_TEST teardown=complete")


def main() -> int:
    def interrupt(signum: int, _frame: Any) -> None:
        raise KeyboardInterrupt(f"received signal {signum}; cleaning up harness")

    previous_handlers = {sig: signal.signal(sig, interrupt)
                         for sig in (signal.SIGINT, signal.SIGTERM)}
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, help="herdr executable to test")
    parser.add_argument("--host-label", default=platform.node().split(".")[0])
    parser.add_argument("--out", type=Path, default=Path("watchdog-live-results"))
    parser.add_argument("--peer")
    parser.add_argument("--peer-binary", type=Path)
    parser.add_argument("--confirm-secs", type=int, default=5)
    parser.add_argument("--real-agents", action="store_true",
                        help="run the real Claude/Codex classification group")
    parser.add_argument("--real-stall-secs", type=int, default=2)
    parser.add_argument("--real-quiet-secs", type=int, default=2)
    parser.add_argument("--only", action="append", help="case id to run (repeatable; comma-separated also accepted)")
    parser.add_argument("--gemini-bin", help="pass through to the pane watchdog")
    parser.add_argument("--self-test", action="store_true", help="exercise fixture setup/teardown only")
    args = parser.parse_args()
    if args.binary is None:
        parser.error("--binary is required")
    args.binary = args.binary.resolve()
    if not args.binary.is_file():
        parser.error(f"binary not found: {args.binary}")
    if args.confirm_secs < 0:
        parser.error("--confirm-secs must be nonnegative")
    if args.real_stall_secs < 1 or args.real_quiet_secs < 1:
        parser.error("--real-stall-secs and --real-quiet-secs must be positive")
    args.out.mkdir(parents=True, exist_ok=True)
    if args.self_test:
        try:
            return self_test(args)
        finally:
            for sig, handler in previous_handlers.items():
                signal.signal(sig, handler)
    # Filter is validated before touching the server. CASES is the audited
    # source of ids and expected classes for every matrix entry.
    selected = ({part.strip() for value in args.only for part in value.split(",")}
                if args.only else None)
    known = {case[0] for case in CASES}
    known.update(case[0] for case in REAL_AGENT_CASES)
    if selected and selected - known:
        parser.error("unknown --only case(s): " + ", ".join(sorted(selected - known)))
    real_selected = selected.intersection(case[0] for case in REAL_AGENT_CASES) if selected else set()
    if real_selected and not args.real_agents:
        parser.error("real-agent --only cases require --real-agents")
    if args.gemini_bin:
        located = shutil.which(args.gemini_bin)
        gemini_path = Path(located or args.gemini_bin).resolve()
        if not gemini_path.is_file():
            parser.error(f"Gemini executable not found: {args.gemini_bin}")
        args.gemini_bin = str(gemini_path)
    harness = Harness(args)
    rows: list[dict[str, Any]] = []
    try:
        run_fixtures = selected is None or bool(selected.intersection(case[0] for case in CASES))
        if run_fixtures:
            harness.start()
        if args.real_agents:
            agent_skips: dict[str, str] = {}
            for ident, agent, footer in REAL_AGENT_CASES:
                if selected is not None and ident not in selected:
                    continue
                if agent in agent_skips:
                    rows.append({"id": ident, "watchdog": "real-agents", "expected": "pass",
                                 "actual": "skipped", "match": None,
                                 "evidence": "skipped: " + agent_skips[agent]})
                    continue
                real_harness = Harness(args, real_agent=True)
                try:
                    if shutil.which(agent):
                        probe_cwd = real_harness.root / f"{agent}-cwd-{ident}"
                        probe_cwd.mkdir()
                        hook_args = real_harness.configure_real_agent_hooks(agent, ident, probe_cwd)
                        real_harness.real_agent_cwds[ident] = probe_cwd
                        real_harness.real_agent_hook_args[ident] = hook_args
                    real_harness.start()
                    row = real_harness.run_real_agent_case(agent, ident, footer)
                    rows.append(row)
                    if row["actual"] == "skipped":
                        agent_skips[agent] = str(row.get("evidence", "agent unavailable")).removeprefix(
                            "skipped: ")
                except Exception as exc:
                    evidence = real_harness.real_agent_setup_evidence(ident, exc)
                    if agent == "claude" and any(marker in evidence.lower() for marker in
                            ("erofs", "read-only file system", "read-only filesystem")):
                        reason = ("Claude SessionStart hook requires writing under the read-only "
                                  "home directory")
                        agent_skips[agent] = reason
                        rows.append({"id": ident, "watchdog": "real-agents", "expected": "pass",
                                     "actual": "skipped", "match": None,
                                     "evidence": "skipped: " + reason})
                    else:
                        rows.append({"id": ident, "watchdog": "real-agents",
                                     "expected": "real-agent classification", "actual": "setup_error",
                                     "setup_error": str(exc), "model_calls": 0, "match": False,
                                     "evidence": evidence, "wall_time_ms": None,
                                     "model_latency_ms": None})
                finally:
                    real_harness.cleanup()
        for ident, family, setup, expected in CASES:
            if not run_fixtures:
                break
            if selected is not None and ident not in selected:
                continue
            try:
                result = setup(harness, ident)
            except Exception as exc:
                rows.append({"id": ident, "watchdog": family, "expected": expected,
                             "actual": "setup_error", "setup_error": str(exc),
                             "model_calls": 0, "match": False, "evidence": str(exc),
                             "wall_time_ms": None, "model_latency_ms": None})
                continue
            if family == "A":
                pane_id = harness.panes[ident]
                session_id = "session-" + ident
                promise_cases = {
                    "stale_draft_promised_work_stalled",
                    "promised_quiet_idle_stalled",
                    "promised_stale_working_stalled",
                    "promised_stale_working_active_tool",
                    "promised_stale_working_dead_marker_stalled",
                }
                session_source = "herdr:codex" if ident in promise_cases else "watchdog-harness"
                status = "idle" if ident in {
                    "a-finished-idle", "stale_draft_promised_work_stalled",
                    "promised_quiet_idle_stalled", "done_here_with_suffix",
                } else ("blocked" if ident == "a-approval-hook" else "working")
                harness.call("pane.report_agent", {"pane_id": pane_id,
                    "source": "watchdog-harness", "agent": "codex", "state": status})
                harness.call("pane.report_agent_session", {"pane_id": pane_id,
                    "source": session_source, "agent": "codex", "agent_session_id": session_id})
                state = harness.root / f"pane-{ident}.json"
                log = harness.root / f"pane-{ident}.jsonl"
                cmd_options = ["--stall-secs", "600", "--confirm-secs",
                               str(args.confirm_secs), "--state-file", str(state),
                               "--status-log", str(log)]
                if ident == "stale_draft_promised_work_stalled":
                    cmd_options += ["--stale-draft-secs", "2", "--quiet-secs", "2"]
                elif ident in promise_cases:
                    cmd_options += ["--quiet-secs", "2"]
                if args.gemini_bin:
                    cmd_options += ["--gemini-bin", args.gemini_bin]
                else:
                    cmd_options.append("--no-model")
                harness.run_watchdog("A", cmd_options, dry=True)
                age = (1800 if ident in promise_cases else
                       1200 if ident == "a-retry-renewed" else
                       90 if ident == "a-prose-question" else
                       900 if ident in ("a-quiet-build", "a-silent-stall", "a-spinner-only",
                                       "a-spinner-progress", "a-resumed") else 0)
                if age:
                    harness.age_memory(state, pane_id, age,
                                       session="old-session" if ident == "a-resumed" else None)
                payload = harness.run_watchdog("A", cmd_options,
                    dry=ident not in {"stale_draft_promised_work_stalled", *promise_cases})
                decision = next((d for d in payload.get("decisions", [])
                                 if d.get("pane_id") == pane_id), {})
                actual = decision.get("class", "missing")
                evidence = decision.get("evidence", payload.get("_stderr_tail"))
                if ident == "stale_draft_promised_work_stalled":
                    case_match = (actual == expected
                                  and decision.get("status") == "corrected"
                                  and decision.get("new_state") == "blocked"
                                  and decision.get("action") is None
                                  and decision.get("delivered") is None)
                elif ident in ("fresh_draft_typing", "done_here_negative_control",
                               "done_here_with_suffix"):
                    case_match = (actual == expected and decision.get("action") is None
                                  and decision.get("delivered") is None)
                else:
                    case_match = actual == expected
            elif family in ("B", "P"):
                opts = ["--stall-minutes", "1", "--runs-dir",
                        str(result["runs"]), "--history-days", "3650", "--all",
                        "--state-file", str(harness.root / f"worker-{ident}.json"),
                        "--claude-projects-dir", str(harness.root / "claude-projects"),
                        "--local-host", args.host_label,
                        "--log-file", str(harness.root / f"worker-{ident}.jsonl")]
                (harness.root / "claude-projects").mkdir(exist_ok=True)
                if ident == "p-cross-host" and harness.probe_wrapper:
                    opts += ["--parent-probe", str(harness.probe_wrapper)]
                memory_path = harness.root / f"worker-{ident}.json"
                dry_run = ident != "p-cross-host"
                harness.run_watchdog("B", opts, dry=dry_run)
                memory_age = (0 if family == "P" else 7200 if ident == "b-resumed-same-op" else
                    0 if ident in ("b-retry-backoff", "b-quoted-question", "b-claude-subagent") else 3600)
                harness.age_worker_memory(memory_path, ident, memory_age,
                    previous_turn=ident in ("b-resumed", "b-resumed-same-op"))
                if ident == "b-spinner-progress":
                    time.sleep(2.1)
                payload = harness.run_watchdog("B", opts, dry=dry_run)
                decision = next((d for d in payload.get("decisions", []) if d.get("worker_id") == ident), {})
                actual = decision.get("parent", "missing") if family == "P" else decision.get("class", "missing")
                evidence = decision.get("evidence", payload.get("_stderr_tail"))
            else:
                actual, evidence = "not_checked", "parent check included in worker result"
                payload = {"_wall_time_ms": 0, "_model_latency_ms": None}
            calls = (payload.get("summary") or {}).get("model_calls", 0)
            calls_expected = 1 if ident == "a-prose-question" and args.gemini_bin else 0
            case_match = actual == expected and calls == calls_expected
            rows.append({"id": ident, "watchdog": family, "expected": expected,
                         "actual": actual, "model_calls": calls,
                         "match": case_match,
                         "evidence": evidence, "wall_time_ms": payload.get("_wall_time_ms"),
                         "model_latency_ms": payload.get("_model_latency_ms")})
        incident = (harness.incident_rearm_check() if selected is None and run_fixtures else
                    {"skipped": True, "match": True})
        # The raw incident result has no match field; evaluate its observations.
        incident["match"] = incident.get("match", True) and (incident.get("skipped", False) or (
            incident["first_action"] == "logged"
            and incident["second_action"] == "already_logged"
            and incident["records_after_second"] == 1
            and incident["recovery_class"] == "working"
            and incident["rearmed_action"] == "logged"
            and incident["records_after_rearm"] == 2))
        digest = hashlib.sha256(args.binary.read_bytes()).hexdigest()
        source = subprocess.run(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True,
                                stdout=subprocess.PIPE, stderr=subprocess.DEVNULL).stdout.strip()
        doc = {"host": args.host_label, "binary_sha256": digest, "source_sha": source,
               "source": source,
               "cases": rows, "incident_rearm": incident}
        (args.out / "matrix.json").write_text(json.dumps(doc, indent=2, sort_keys=True) + "\n")
        lines = ["# Watchdog live matrix", "", f"Host: `{args.host_label}`  ",
                 f"Source: `{source}`  ", f"Binary SHA256: `{digest}`", "",
                 "| Case | Watchdog | Expected | Actual | Match |", "|---|---|---|---|---|"]
        lines.extend(f"| {r['id']} | {r['watchdog']} | {r['expected']} | {r['actual']} | {'yes' if r['match'] else 'no'} |" for r in rows)
        (args.out / "matrix.md").write_text("\n".join(lines) + "\n")
        print(f"MATRIX {args.out / 'matrix.md'} matched={sum(r['match'] is True for r in rows)}/{len(rows)} "
              f"incident_rearm={'pass' if incident['match'] else 'fail'}")
        valid = all(r["match"] is True for r in rows if r["watchdog"] != "real-agents")
        valid = valid and all(r["match"] is True or r["actual"] == "skipped"
                              for r in rows if r["watchdog"] == "real-agents")
        return 0 if rows and valid and incident["match"] else 1
    finally:
        harness.cleanup()
        for sig, handler in previous_handlers.items():
            signal.signal(sig, handler)


if __name__ == "__main__":
    raise SystemExit(main())
