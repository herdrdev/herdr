#!/usr/bin/env python3
"""Capture repeated Shepherd pane reads for agent screen detection fixtures."""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import time
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


DEFAULT_OUT_DIR = Path(".local/agent-screen-captures")
DEFAULT_PANE = "harness-test"
STATE_CHOICES = {
    "i": "idle",
    "idle": "idle",
    "w": "working",
    "working": "working",
    "b": "blocked",
    "blocked": "blocked",
    "d": "done",
    "done": "done",
    "u": "unknown",
    "unknown": "unknown",
    "c": "custom",
    "custom": "custom",
}


@dataclass
class CommandResult:
    code: int
    stdout: bytes
    stderr: bytes


@dataclass
class PaneMatch:
    pane_id: str
    label: str | None
    name: str | None
    agent: str | None
    title: str | None
    display_agent: str | None
    raw: dict[str, Any] | None
    agent_raw: dict[str, Any] | None = None


def main() -> int:
    args = parse_args()
    run_dir = args.out / datetime.now().strftime("%Y%m%d-%H%M%S")
    run_dir.mkdir(parents=True, exist_ok=True)

    print(f"writing captures under {run_dir}")
    print("state shortcuts: i=idle, w=working, b=blocked, d=done, u=unknown, c=custom, q=quit")

    capture_index = 1
    while True:
        pane = resolve_target(args.shepherd, args.pane, args.agent)
        if pane is None:
            if args.agent:
                print(f"agent '{args.agent}' was not found by `shepherd agent get`")
            else:
                print(f"pane '{args.pane}' was not found by `shepherd pane list`")
                print("pass a pane id with --pane, or rename the target pane to harness-test")
            return 1

        print(f"\npane found: {pane.pane_id}, agent: {agent_display(pane)}{format_pane_context(pane)}")
        state = prompt_state()
        if state is None:
            break

        name = prompt_name(pane, state)
        if name is None:
            break

        capture_dir = run_dir / f"{capture_index:03d}-{slugify(name)}"
        capture_dir.mkdir(parents=True, exist_ok=False)
        print(f"capturing {args.samples} sample(s) into {capture_dir}")

        started_at = iso_now()
        failures = capture_case(args, pane, state, name, capture_dir)
        write_metadata(
            capture_dir,
            args=args,
            pane=pane,
            state=state,
            name=name,
            started_at=started_at,
            finished_at=iso_now(),
            failures=failures,
        )

        if failures:
            print(f"saved with {len(failures)} command failure(s); see metadata.toml")
        else:
            print("saved")

        capture_index += 1
        if args.once:
            break

    print("done")
    return 0


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Interactively capture Shepherd pane screen reads for agent detection fixture work."
    )
    parser.add_argument(
        "--pane",
        default=DEFAULT_PANE,
        help=f"pane label/title/id to capture when --agent is not set (default: {DEFAULT_PANE})",
    )
    parser.add_argument(
        "--agent",
        help="agent target to capture; resolved with `shepherd agent get`",
    )
    parser.add_argument(
        "--out",
        type=Path,
        default=DEFAULT_OUT_DIR,
        help=f"output directory (default: {DEFAULT_OUT_DIR})",
    )
    parser.add_argument(
        "--samples",
        type=positive_int,
        default=5,
        help="samples to capture per state (default: 5)",
    )
    parser.add_argument(
        "--interval",
        type=non_negative_float,
        default=1.0,
        help="seconds between samples (default: 1.0)",
    )
    parser.add_argument(
        "--lines",
        type=positive_int,
        default=120,
        help="recent-buffer lines to save per sample (default: 120)",
    )
    parser.add_argument(
        "--shepherd",
        default="shepherd",
        help="Shepherd CLI binary to call (default: shepherd)",
    )
    parser.add_argument(
        "--once",
        action="store_true",
        help="capture one state and exit",
    )
    return parser.parse_args()


def positive_int(value: str) -> int:
    parsed = int(value)
    if parsed <= 0:
        raise argparse.ArgumentTypeError("must be greater than zero")
    return parsed


def non_negative_float(value: str) -> float:
    parsed = float(value)
    if parsed < 0:
        raise argparse.ArgumentTypeError("must be zero or greater")
    return parsed


def resolve_pane(shepherd: str, pane_ref: str) -> PaneMatch | None:
    result = run_command([shepherd, "pane", "list"])
    if result.code != 0:
        pane = fallback_pane_id(pane_ref)
        return enrich_pane_with_agent(shepherd, pane) if pane else None

    try:
        response = json.loads(result.stdout.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError):
        pane = fallback_pane_id(pane_ref)
        return enrich_pane_with_agent(shepherd, pane) if pane else None

    panes = response.get("result", {}).get("panes", [])
    if not isinstance(panes, list):
        pane = fallback_pane_id(pane_ref)
        return enrich_pane_with_agent(shepherd, pane) if pane else None

    exact_matches = []
    loose_matches = []
    for pane in panes:
        if not isinstance(pane, dict):
            continue
        pane_id = string_value(pane.get("pane_id"))
        if pane_id is None:
            continue
        fields = pane_match_fields(pane)
        if any(field == pane_ref for field in fields):
            exact_matches.append(pane)
        elif any(field and pane_ref in field for field in fields):
            loose_matches.append(pane)

    matches = exact_matches or loose_matches
    if len(matches) == 1:
        return enrich_pane_with_agent(shepherd, pane_from_dict(matches[0]))
    if len(matches) > 1:
        print(f"pane ref '{pane_ref}' matched multiple panes:")
        for pane in matches:
            print(f"  {pane.get('pane_id')}{format_pane_context(pane_from_dict(pane))}")
        return None

    pane = fallback_pane_id(pane_ref)
    return enrich_pane_with_agent(shepherd, pane) if pane else None


def resolve_target(shepherd: str, pane_ref: str, agent_ref: str | None) -> PaneMatch | None:
    if agent_ref:
        agent = get_agent(shepherd, agent_ref)
        if agent is None:
            return None
        pane = pane_from_agent_dict(agent)
        return enrich_pane_with_pane_list(shepherd, pane)
    return resolve_pane(shepherd, pane_ref)


def get_agent(shepherd: str, agent_ref: str) -> dict[str, Any] | None:
    result = run_command([shepherd, "agent", "get", agent_ref])
    if result.code != 0:
        return None
    try:
        response = json.loads(result.stdout.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError):
        return None
    agent = response.get("result", {}).get("agent")
    return agent if isinstance(agent, dict) else None


def enrich_pane_with_agent(shepherd: str, pane: PaneMatch) -> PaneMatch:
    result = run_command([shepherd, "agent", "list"])
    if result.code != 0:
        return pane
    try:
        response = json.loads(result.stdout.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError):
        return pane
    agents = response.get("result", {}).get("agents", [])
    if not isinstance(agents, list):
        return pane
    for agent in agents:
        if isinstance(agent, dict) and agent.get("pane_id") == pane.pane_id:
            return merge_agent_into_pane(pane, agent)
    return pane


def enrich_pane_with_pane_list(shepherd: str, pane: PaneMatch) -> PaneMatch:
    result = run_command([shepherd, "pane", "list"])
    if result.code != 0:
        return pane
    try:
        response = json.loads(result.stdout.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError):
        return pane
    panes = response.get("result", {}).get("panes", [])
    if not isinstance(panes, list):
        return pane
    for candidate in panes:
        if isinstance(candidate, dict) and candidate.get("pane_id") == pane.pane_id:
            pane_info = pane_from_dict(candidate)
            pane_info.name = pane.name or pane_info.name
            pane_info.agent = pane.agent or pane_info.agent
            pane_info.display_agent = pane.display_agent or pane_info.display_agent
            pane_info.agent_raw = pane.agent_raw
            return pane_info
    return pane


def merge_agent_into_pane(pane: PaneMatch, agent: dict[str, Any]) -> PaneMatch:
    pane.name = string_value(agent.get("name")) or pane.name
    pane.agent = string_value(agent.get("agent")) or pane.agent
    pane.display_agent = string_value(agent.get("display_agent")) or pane.display_agent
    pane.title = string_value(agent.get("title")) or pane.title
    pane.agent_raw = agent
    return pane


def fallback_pane_id(pane_ref: str) -> PaneMatch | None:
    if re.fullmatch(r"p_(?:[A-Za-z0-9]+_)?\d+", pane_ref) or re.fullmatch(
        r"[A-Za-z0-9_]+-\d+", pane_ref
    ):
        return PaneMatch(
            pane_id=normalize_pane_id(pane_ref),
            label=None,
            name=None,
            agent=None,
            title=None,
            display_agent=None,
            raw=None,
        )
    return None


def normalize_pane_id(pane_ref: str) -> str:
    return pane_ref


def pane_match_fields(pane: dict[str, Any]) -> list[str]:
    fields = [
        string_value(pane.get("pane_id")),
        string_value(pane.get("terminal_id")),
        string_value(pane.get("label")),
        string_value(pane.get("title")),
        string_value(pane.get("agent")),
        string_value(pane.get("display_agent")),
    ]
    return [field for field in fields if field]


def pane_from_dict(pane: dict[str, Any]) -> PaneMatch:
    return PaneMatch(
        pane_id=string_value(pane.get("pane_id")) or "",
        label=string_value(pane.get("label")),
        name=None,
        agent=string_value(pane.get("agent")),
        title=string_value(pane.get("title")),
        display_agent=string_value(pane.get("display_agent")),
        raw=pane,
    )


def pane_from_agent_dict(agent: dict[str, Any]) -> PaneMatch:
    return PaneMatch(
        pane_id=string_value(agent.get("pane_id")) or "",
        label=None,
        name=string_value(agent.get("name")),
        agent=string_value(agent.get("agent")),
        title=string_value(agent.get("title")),
        display_agent=string_value(agent.get("display_agent")),
        raw=None,
        agent_raw=agent,
    )


def agent_display(pane: PaneMatch) -> str:
    return pane.agent or pane.display_agent or "unknown"


def string_value(value: Any) -> str | None:
    return value if isinstance(value, str) and value else None


def format_pane_context(pane: PaneMatch) -> str:
    details = []
    if pane.name:
        details.append(f"name={pane.name}")
    if pane.label:
        details.append(f"label={pane.label}")
    if pane.title:
        details.append(f"title={pane.title}")
    if not details:
        return ""
    return " (" + ", ".join(details) + ")"


def prompt_state() -> str | None:
    while True:
        raw = input("state [idle/working/blocked/done/unknown/custom/q]: ").strip()
        if raw.lower() in {"q", "quit", "exit"}:
            return None
        if not raw:
            continue
        state = STATE_CHOICES.get(raw.lower())
        if state == "custom":
            custom = input("custom state label: ").strip()
            if custom:
                return custom
            continue
        if state:
            return state
        return raw


def prompt_name(pane: PaneMatch, state: str) -> str | None:
    default_parts = [
        pane.agent or pane.display_agent or pane.name or pane.label or "agent",
        state,
        datetime.now().strftime("%H%M%S"),
    ]
    default_name = "-".join(slugify(part) for part in default_parts if part)
    raw = input(f"capture name [{default_name}]: ").strip()
    if raw.lower() in {"q", "quit", "exit"}:
        return None
    return raw or default_name


def capture_case(
    args: argparse.Namespace,
    pane: PaneMatch,
    state: str,
    name: str,
    capture_dir: Path,
) -> list[str]:
    failures: list[str] = []
    for index in range(1, args.samples + 1):
        prefix = f"sample-{index:03d}"
        print(f"  sample {index}/{args.samples}")

        commands = [
            (
                f"{prefix}.detection.txt",
                [args.shepherd, "pane", "read", pane.pane_id, "--source", "detection", "--format", "text"],
            ),
            (
                f"{prefix}.detection.ansi",
                [args.shepherd, "pane", "read", pane.pane_id, "--source", "detection", "--format", "ansi"],
            ),
            (
                f"{prefix}.recent.txt",
                [
                    args.shepherd,
                    "pane",
                    "read",
                    pane.pane_id,
                    "--source",
                    "recent",
                    "--lines",
                    str(args.lines),
                    "--format",
                    "text",
                ],
            ),
            (
                f"{prefix}.recent.ansi",
                [
                    args.shepherd,
                    "pane",
                    "read",
                    pane.pane_id,
                    "--source",
                    "recent",
                    "--lines",
                    str(args.lines),
                    "--format",
                    "ansi",
                ],
            ),
            (
                f"{prefix}.explain.json",
                [args.shepherd, "agent", "explain", pane.pane_id, "--json"],
            ),
        ]

        for filename, command in commands:
            result = run_command(command)
            output_path = capture_dir / filename
            if result.code == 0:
                output_path.write_bytes(result.stdout)
            else:
                failures.append(f"{filename}: exit {result.code}: {' '.join(command)}")
                output_path.with_suffix(output_path.suffix + ".stderr").write_bytes(result.stderr)
                output_path.write_bytes(result.stdout)

        sample_meta = {
            "captured_at": iso_now(),
            "sample": index,
            "state": state,
            "name": name,
        }
        (capture_dir / f"{prefix}.json").write_text(
            json.dumps(sample_meta, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )

        if index < args.samples:
            time.sleep(args.interval)
    return failures


def run_command(command: list[str]) -> CommandResult:
    try:
        completed = subprocess.run(command, capture_output=True, check=False)
    except FileNotFoundError as err:
        return CommandResult(code=127, stdout=b"", stderr=str(err).encode("utf-8"))
    return CommandResult(
        code=completed.returncode,
        stdout=completed.stdout,
        stderr=completed.stderr,
    )


def write_metadata(
    capture_dir: Path,
    *,
    args: argparse.Namespace,
    pane: PaneMatch,
    state: str,
    name: str,
    started_at: str,
    finished_at: str,
    failures: list[str],
) -> None:
    lines = [
        f"name = {toml_string(name)}",
        f"state = {toml_string(state)}",
        f"started_at = {toml_string(started_at)}",
        f"finished_at = {toml_string(finished_at)}",
        f"pane_ref = {toml_string(args.pane)}",
        f"agent_ref = {toml_optional_string(args.agent)}",
        f"pane_id = {toml_string(pane.pane_id)}",
        f"samples = {args.samples}",
        f"interval_seconds = {args.interval}",
        f"recent_lines = {args.lines}",
        f"shepherd = {toml_string(args.shepherd)}",
        f"pane_label = {toml_optional_string(pane.label)}",
        f"agent_name = {toml_optional_string(pane.name)}",
        f"pane_agent = {toml_optional_string(pane.agent)}",
        f"display_agent = {toml_optional_string(pane.display_agent)}",
        f"pane_title = {toml_optional_string(pane.title)}",
        "",
        "[commands]",
        'detection_text = "shepherd pane read <pane> --source detection --format text"',
        'detection_ansi = "shepherd pane read <pane> --source detection --format ansi"',
        'recent_text = "shepherd pane read <pane> --source recent --lines <n> --format text"',
        'recent_ansi = "shepherd pane read <pane> --source recent --lines <n> --format ansi"',
        'explain = "shepherd agent explain <pane> --json"',
    ]
    if failures:
        lines.append("")
        lines.append("failures = [")
        for failure in failures:
            lines.append(f"  {toml_string(failure)},")
        lines.append("]")

    (capture_dir / "metadata.toml").write_text("\n".join(lines) + "\n", encoding="utf-8")
    if pane.raw is not None:
        (capture_dir / "pane.json").write_text(
            json.dumps(pane.raw, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )
    if pane.agent_raw is not None:
        (capture_dir / "agent.json").write_text(
            json.dumps(pane.agent_raw, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )


def toml_optional_string(value: str | None) -> str:
    if value is None:
        return '""'
    return toml_string(value)


def toml_string(value: str) -> str:
    return json.dumps(value)


def slugify(value: str) -> str:
    slug = re.sub(r"[^A-Za-z0-9._-]+", "-", value.strip().lower())
    slug = re.sub(r"-+", "-", slug).strip("-")
    return slug or "capture"


def iso_now() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="seconds")


if __name__ == "__main__":
    sys.exit(main())
