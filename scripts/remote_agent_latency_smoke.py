#!/usr/bin/env python3
"""Measure unsubmitted drafts in real remote coding-agent UIs.

Uses only new Herdr named sessions and generated /tmp workspaces. No model
prompt is submitted. Raw agent terminal output is discarded. Artifacts contain
generated draft rows, cursor metadata, timing, versions, and candidate debug logs,
plus SSH targets, absolute executable/workspace paths, commands, and diagnostic
errors. Treat these as private diagnostics and review/redact them before sharing.

    python3 scripts/remote_agent_latency_smoke.py --target test-host --agent-bin-dir /opt/agents/bin --agents claude
    python3 scripts/remote_agent_latency_smoke.py --target test-host --agent-bin-dir /opt/agents/bin --prediction both

With --prediction both, exits nonzero unless each agent's prediction-on median
and post-pause latency are less than half their prediction-off values. This
intentionally exposes a predictor which helps ordinary shells but not the
actual agent input editor. Use --self-test for local prompt matching assertions.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import shlex
import statistics
import subprocess
import sys
import tempfile
import time
import uuid

from remote_latency_smoke import Client, Remote, Screen, WIDTH


def is_draft_row(line, expected, *, composed=False, agent=None):
    if not expected or expected not in line:
        return False
    # Codex's Herdr-composed row has a scrollbar at the terminal's final cell.
    # Only remove that observed piece of chrome after actual blank padding;
    # authoritative pane reads and arbitrary draft suffixes stay exact.
    if composed and len(line) == WIDTH and line.endswith(" \u2590"):
        line = line[:-1]
    if agent == "pi":
        if composed:
            if "│" not in line:
                return False
            line = line.split("│", 1)[1]
        return line.strip(" \u00a0") == expected
    markers = ("┃",) if agent == "opencode" else ("❯", "›")
    marker = max(line.rfind(value) for value in markers)
    return marker >= 0 and line[marker + 1:].strip(" \u00a0") == expected


def pi_rule(line, *, composed=False):
    if composed:
        if "│" not in line:
            return False
        line = line.split("│", 1)[1]
    line = line.strip(" \u00a0")
    return len(line) >= 40 and all(char == "─" for char in line)


def pi_editor_rows(screen, workdir):
    lines = screen.splitlines()
    # The update notice also leaves a blank gap between rules. The actual
    # editor's lower rule is immediately followed by this test's cwd footer.
    return [
        index for index in range(1, len(lines) - 2)
        if not lines[index].strip()
        and pi_rule(lines[index - 1])
        and pi_rule(lines[index + 1])
        and lines[index + 2].strip() == workdir
    ]


def draft_rows(screen, expected, *, composed=False, agent=None):
    lines = screen.splitlines()
    matches = []
    for index, line in enumerate(lines):
        if not is_draft_row(line, expected, composed=composed, agent=agent):
            continue
        # Pi's default editor has no prompt glyph. Identify its actual input
        # body by the two adjacent horizontal editor rules, not loose text.
        if agent == "pi" and not (0 < index < len(lines) - 1 and pi_rule(lines[index - 1], composed=composed) and pi_rule(lines[index + 1], composed=composed)):
            continue
        matches.append((index, line))
    return matches


def binary_digest(path):
    digest = hashlib.sha256()
    with Path(path).open("rb") as binary:
        for chunk in iter(lambda: binary.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def same_comparison_build(baseline, predicted):
    return all(baseline.get(key) and baseline[key] == predicted.get(key)
               for key in ("binary_sha256", "agent_version"))


def self_test():
    scrollbar_row = "› ZQher".ljust(WIDTH - 1) + "▐"
    assert is_draft_row(scrollbar_row, "ZQher", composed=True)
    assert not is_draft_row(scrollbar_row, "ZQher")
    assert is_draft_row("❯\u00a0ZQher   ", "ZQher")
    for text in ("ZQherEXTRA", "ZQherZQher"):
        assert not is_draft_row(("› " + text).ljust(WIDTH - 1) + "▐", "ZQher", composed=True)
    assert not is_draft_row("› ZQher▐", "ZQher", composed=True)
    assert not is_draft_row("directory /tmp/herdr-agent-smoke", "herdr")
    assert is_draft_row("                   ┃  ZQher   ", "ZQher", agent="opencode")
    assert not is_draft_row("                   ┃  ZQherEXTRA", "ZQher", agent="opencode")
    pi_editor = "\n".join(("─" * 80, "ZQher", "─" * 80))
    assert draft_rows(pi_editor, "ZQher", agent="pi")
    assert not draft_rows(pi_editor.replace("ZQher", "ZQherEXTRA"), "ZQher", agent="pi")
    assert not draft_rows("ZQher", "ZQher", agent="pi")
    composed_pi = "\n".join(" " * 25 + "│" + row for row in ("─" * 114, "ZQher".ljust(114), "─" * 114))
    assert draft_rows(composed_pi, "ZQher", composed=True, agent="pi")
    assert not draft_rows(composed_pi.replace("ZQher", "ZQherZQher"), "ZQher", composed=True, agent="pi")
    generated_cwd = "/tmp/herdr-agent-smoke.fixture"
    pi_startup = "\n".join(("─" * 80, "", "─" * 80, "", "─" * 80, generated_cwd))
    assert pi_editor_rows(pi_startup, generated_cwd) == [3], "update-notice gap is not the editor"
    assert not pi_editor_rows(pi_startup, generated_cwd + "-other")
    metadata = {"binary_sha256": "candidate-a", "agent_version": "agent-1"}
    assert same_comparison_build(metadata, dict(metadata))
    assert not same_comparison_build(metadata, {**metadata, "binary_sha256": "candidate-b"})
    assert not same_comparison_build(metadata, {**metadata, "agent_version": "agent-2"})
    assert not same_comparison_build({}, {})
    print("exact prompt, scrollbar, and comparison identity assertions passed")


class CursorScreen(Screen):
    def __init__(self, agent=None):
        self.agent = agent
        self.cursor_visible = True
        self.cursor_shape = None
        super().__init__()

    def csi(self, final):
        if self.sequence.startswith("?") and final in ("h", "l"):
            modes = self.sequence[1:].split(";")
            if "25" in modes:
                self.cursor_visible = final == "h"
        if final == "q":
            self.cursor_shape = self.sequence.strip()
        super().csi(final)

    def evidence(self, expected=""):
        rows = []
        if expected:
            for index, line in draft_rows(self.text(), expected, composed=True, agent=self.agent):
                start = line.index(expected)
                rows.append({"row": index, "start_col": start, "draft": line[start:start + len(expected)], "nearby_cells": line[max(0, start - 3):start + len(expected) + 3]})
        return {"outer_cursor": {"x": self.col, "y": self.row, "visible": self.cursor_visible, "shape": self.cursor_shape}, "generated_rows": rows}


class AgentClient(Client):
    def __init__(self, command, env, artifact_dir, agent=None):
        super().__init__(command, env, artifact_dir)
        # Replace logging before reading any child output. The original file
        # remains empty; no account info, prior sessions or startup text is saved.
        self.log.close()
        self.log = open(os.devnull, "wb")
        self.screen = CursorScreen(agent)

    def close(self):
        # Release the test PTY first so terminal-restore output cannot block a
        # shutting-down child while this harness is no longer pumping output.
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
                self.process.wait(timeout=10)


def ssh_command(args, command, timeout=30):
    result = subprocess.run(["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", args.target, shlex.join(command)], capture_output=True, text=True, timeout=timeout)
    if result.returncode:
        raise RuntimeError(f"Remote {command[0]} failed: {result.stderr.strip()}")
    return result.stdout


def wait_for_agent(client, remote, session, pane, agent, workdir, timeout=55):
    deadline = time.monotonic() + timeout
    trust_accepted = False
    last_state = "agent startup"
    while time.monotonic() < deadline:
        client.pump(0.1)
        screen = remote.cli("--session", session, "pane", "read", pane, "--source", "visible", "--format", "text")
        lower = screen.lower()
        trust_dialog = any(phrase in lower for phrase in ("do you trust", "trust this folder", "trust the files", "trust the contents", "is this a project you created"))
        if trust_dialog and not trust_accepted and workdir in screen:
            # Only the empty workspace created by this invocation is approved.
            if agent == "claude":
                if "❯ Yes, I trust this folder" in screen:
                    remote.cli("--session", session, "pane", "send-keys", pane, "enter")
                    trust_accepted = True
                    last_state = "generated temporary directory trust accepted"
                elif "❯ No, exit" in screen and "Yes, I trust this folder" in screen:
                    remote.cli("--session", session, "pane", "send-keys", pane, "down")
                    last_state = "selecting generated temporary directory trust"
                continue
            if "› 1. Yes" in screen or "❯ Yes" in screen:
                remote.cli("--session", session, "pane", "send-keys", pane, "enter")
                trust_accepted = True
                last_state = "generated temporary directory trust accepted"
            continue
        if trust_dialog:
            last_state = "waiting for generated directory trust dialog to close"
            continue
        if "sign in" in lower and any(phrase in lower for phrase in ("select login method", "sign in with chatgpt", "claude account", "login method")):
            raise RuntimeError(f"{agent} requires authentication before its input editor is available")
        if agent == "claude" and ("claude code" in lower or "opus" in lower or "sonnet" in lower) and "❯" in screen and any(marker in lower for marker in ("for shortcuts", "plan mode", "bypass permissions", "shift+tab")):
            return {"startup_state": last_state, "trusted_generated_directory": trust_accepted}
        if agent == "codex" and "openai codex" in lower and "› Ask Codex to do anything" in screen:
            return {"startup_state": last_state, "trusted_generated_directory": trust_accepted}
        if agent == "opencode" and "Ask anything..." in screen and "tab agents" in lower and "ctrl+p commands" in lower:
            return {"startup_state": last_state, "trusted_generated_directory": trust_accepted}
        if agent == "pi" and workdir in screen:
            editor_rows = pi_editor_rows(screen, workdir)
            if len(editor_rows) == 1:
                return {"startup_state": last_state, "trusted_generated_directory": trust_accepted, "editor_geometry": {"authoritative_row": editor_rows[0], "input_column": 0, "layout": "plain body between adjacent horizontal rules"}}
        if "update" in lower and "skip for now" in lower:
            last_state = "agent update prompt requires skipping"
    # Save only recognizable state labels, not arbitrary startup/account text.
    raise RuntimeError(f"{agent} editor was not ready: {last_state}")


def sample_key(client, expected, char):
    expected += char
    started = time.monotonic()
    os.write(client.master, char.encode("ascii"))
    client.wait_for(lambda screen: bool(draft_rows(screen, expected, composed=True, agent=client.screen.agent)), 8, f"generated draft character {len(expected)}")
    visible = time.monotonic()
    evidence = client.screen.evidence(expected)
    # Allow authoritative repaint to settle without issuing another SSH/API
    # command (which could expire the predictor's short confidence window).
    settle = time.monotonic() + 0.28
    while time.monotonic() < settle:
        client.pump(0.02)
    return expected, {"character": char, "visible_ms": round((visible - started) * 1000, 2), "first_visible": evidence, "settled": client.screen.evidence(expected)}


def run_case(args, remote, agent, prediction, session, artifact_dir):
    case_dir = artifact_dir / f"{agent}-{'on' if prediction else 'off'}"
    case_dir.mkdir()
    config = case_dir / "config.toml"
    config.write_text(f"onboarding = false\n[remote]\npredict_input = {str(prediction).lower()}\nmanage_ssh_config = false\n", encoding="utf-8")
    env = {key: value for key, value in os.environ.items() if not key.startswith("HERDR_")}
    env.update(TERM="xterm-256color", COLORTERM="truecolor", HERDR_CONFIG_PATH=str(config), XDG_CONFIG_HOME=str(case_dir / "config"), XDG_STATE_HOME=str(case_dir / "state"), HERDR_LOG="herdr::client::shell::prediction=debug")
    command = [str(Path(args.binary).resolve()), "--remote", args.target, "--session", session]
    binary_sha256 = binary_digest(args.binary)
    client = AgentClient(command, env, case_dir, agent)
    pane, workdir, expected = None, None, ""
    case = {"agent": agent, "prediction": prediction, "session": session, "candidate_command": command, "binary_sha256": binary_sha256, "samples": [], "submitted_prompt": False}
    try:
        deadline = time.monotonic() + 45
        while True:
            client.pump(0)
            try:
                remote.json("--session", session, "workspace", "list")
                break
            except RuntimeError:
                if client.process.poll() is not None or time.monotonic() >= deadline:
                    raise
                client.pump(0.2)
        workdir = ssh_command(args, ["mktemp", "-d", "/tmp/herdr-agent-smoke.XXXXXXXX"]).strip()
        if not workdir.startswith("/tmp/herdr-agent-smoke.") or "/" in workdir[len("/tmp/"):]:
            raise RuntimeError("Unexpected generated workspace path")
        response = remote.json("--session", session, "workspace", "create", "--cwd", workdir, "--label", f"smoke-{agent}", "--focus")
        pane = response["result"]["root_pane"]["pane_id"]
        case.update(pane=pane, generated_workdir=workdir)
        agent_command = ["env", f"PATH={args.agent_bin_dir}:/usr/local/bin:/usr/bin:/bin", str(Path(args.agent_bin_dir) / agent)]
        # Record each installed version separately: the remote user may update
        # an agent between validation runs. This is outside the timing window.
        case["agent_version"] = ssh_command(args, [*agent_command, "--version"]).strip()
        if agent == "claude":
            agent_command += ["--safe-mode", "--permission-mode", "plan"]
        elif agent == "codex":
            agent_command += ["--sandbox", "read-only", "--ask-for-approval", "on-request", "--cd", workdir]
        elif agent == "pi":
            agent_command += ["--no-session"]
        elif agent == "opencode":
            agent_command += [workdir]
        case["agent_command"] = agent_command
        remote.cli("--session", session, "pane", "run", pane, shlex.join(agent_command))
        case.update(wait_for_agent(client, remote, session, pane, agent, workdir))
        settle = time.monotonic() + 1
        while time.monotonic() < settle:
            client.pump(0.05)
        case["initial"] = client.screen.evidence()
        for index, char in enumerate("ZQherdrprobe"[:args.samples + 2]):
            expected, sample = sample_key(client, expected, char)
            sample["training"] = index < 2
            case["samples"].append(sample)
            (case_dir / "case.json").write_text(json.dumps(case, indent=2) + "\n", encoding="utf-8")
        pause_start = time.monotonic()
        while time.monotonic() - pause_start < 1.5:
            client.pump(0.05)
        pause_ms = round((time.monotonic() - pause_start) * 1000, 2)
        expected, paused_sample = sample_key(client, expected, "p")
        case["pause_probe"] = {"idle_ms": pause_ms, **paused_sample}
        last_char = expected[-1]
        expected = expected[:-1]
        deletion_start = time.monotonic()
        os.write(client.master, b"\x7f")
        client.wait_for(lambda screen: bool(draft_rows(screen, expected, composed=True, agent=client.screen.agent)), 8, "exact draft after one Backspace")
        deleted = {"visible_ms": round((time.monotonic() - deletion_start) * 1000, 2), "screen": client.screen.evidence(expected)}
        expected, retyped = sample_key(client, expected, last_char)
        case["delete_retype"] = {"deletion": deleted, "retyped": retyped}
        authoritative = remote.cli("--session", session, "pane", "read", pane, "--source", "visible", "--format", "text")
        generated = []
        for index, line in draft_rows(authoritative, expected, agent=agent):
            start = line.index(expected)
            generated.append({"row": index, "start_col": start, "draft": line[start:start + len(expected)], "nearby_cells": line[max(0, start - 3):start + len(expected) + 3]})
        if len(generated) != 1:
            raise RuntimeError("Expected exactly one authoritative prompt containing exactly the generated draft")
        case["authoritative_generated_rows"] = generated
        case["median_visible_ms"] = round(statistics.median(sample["visible_ms"] for sample in case["samples"] if not sample["training"]), 2)
        case["passed_input_integrity"] = True
    except Exception as error:
        case.update(error=str(error), passed_input_integrity=False)
    finally:
        case["final"] = client.screen.evidence(expected)
        cleanup_errors = []
        try:
            client.close()
        except Exception as error:
            cleanup_errors.append(f"local client {client.process.pid}: {error}")
        try:
            sessions = remote.json("session", "list", "--json")["sessions"]
            if any(item["name"] == session for item in sessions):
                remote.cli("session", "stop", session, "--json")
                remote.cli("session", "delete", session, "--json")
        except Exception as error:
            cleanup_errors.append(f"named session {session}: {error}")
        try:
            if workdir and workdir.startswith("/tmp/herdr-agent-smoke.") and "/" not in workdir[len("/tmp/"):]:
                ssh_command(args, ["rm", "-rf", "--", workdir])
        except Exception as error:
            cleanup_errors.append(f"generated workspace {workdir}: {error}")
        case["cleanup_errors"] = cleanup_errors
        (case_dir / "case.json").write_text(json.dumps(case, indent=2) + "\n", encoding="utf-8")
    return case


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--target")
    parser.add_argument("--binary", default="target/release/herdr")
    parser.add_argument("--remote-binary", default=".local/bin/herdr")
    parser.add_argument("--agent-bin-dir", help="explicit absolute remote directory containing the agent executables")
    parser.add_argument("--agents", nargs="+", choices=("claude", "codex", "pi", "opencode"), default=["claude", "codex"])
    parser.add_argument("--prediction", choices=("off", "on", "both"), default="both")
    parser.add_argument("--samples", type=int, default=5)
    parser.add_argument("--artifacts", type=Path)
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        self_test()
        return 0
    if not args.target:
        parser.error("--target is required for live agent validation")
    if not args.agent_bin_dir or not Path(args.agent_bin_dir).is_absolute():
        parser.error("--agent-bin-dir must be an explicit absolute remote directory")
    if not Path(args.binary).is_file() or not 1 <= args.samples <= 8:
        parser.error("binary must exist and samples must be 1-8")
    if args.target.startswith("-") or any(ch.isspace() for ch in args.target):
        parser.error("target must be one SSH host/alias")
    artifacts = args.artifacts.resolve() if args.artifacts else Path(tempfile.mkdtemp(prefix="herdr-real-agents-"))
    artifacts.mkdir(parents=True, exist_ok=True)
    remote = Remote(args.target, args.remote_binary)
    existing = {item["name"] for item in remote.json("session", "list", "--json")["sessions"]}
    token = uuid.uuid4().hex[:10]
    predictions = (False, True) if args.prediction == "both" else (args.prediction == "on",)
    report = {"target": args.target, "artifacts": str(artifacts), "cases": [], "submitted_prompts": False}
    print(f"Real agent draft smoke artifacts: {artifacts}", flush=True)
    for agent in args.agents:
        for prediction in predictions:
            session = f"agent-smoke-{token}-{agent}-{'on' if prediction else 'off'}"
            if session in existing:
                raise RuntimeError("Generated test session already exists")
            print(f"Running {agent}, prediction={prediction}, session={session}", flush=True)
            case = run_case(args, remote, agent, prediction, session, artifacts)
            report["cases"].append(case)
            (artifacts / "report.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
            print(json.dumps({key: case[key] for key in ("agent", "prediction", "median_visible_ms", "error", "passed_input_integrity") if key in case}), flush=True)
    report["passed"] = all(case.get("passed_input_integrity") and not case.get("cleanup_errors") for case in report["cases"])
    if args.prediction == "both" and report["passed"]:
        report["comparisons"] = []
        for agent in args.agents:
            baseline = next(case for case in report["cases"] if case["agent"] == agent and not case["prediction"])
            predicted = next(case for case in report["cases"] if case["agent"] == agent and case["prediction"])
            same_build = same_comparison_build(baseline, predicted)
            improvement = predicted["median_visible_ms"] < baseline["median_visible_ms"] * 0.5
            pause_improvement = predicted["pause_probe"]["visible_ms"] < baseline["pause_probe"]["visible_ms"] * 0.5
            report["comparisons"].append({"agent": agent, "same_binary_and_agent_version": same_build, "baseline_ms": baseline["median_visible_ms"], "prediction_ms": predicted["median_visible_ms"], "prediction_improved": improvement, "pause_baseline_ms": baseline["pause_probe"]["visible_ms"], "pause_prediction_ms": predicted["pause_probe"]["visible_ms"], "pause_improved": pause_improvement})
            report["passed"] = report["passed"] and same_build and improvement and pause_improvement
    (artifacts / "report.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({key: report[key] for key in ("artifacts", "passed", "comparisons") if key in report}, indent=2))
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
