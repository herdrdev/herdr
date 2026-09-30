#!/usr/bin/env python3
"""Rebuild conservative pane observations and ask Herdr's Rust watchdog to replay them."""

from __future__ import annotations

import argparse
import datetime as dt
import glob
import hashlib
import json
import os
import re
import subprocess
import sys
from collections import defaultdict
from pathlib import Path

UTC = dt.timezone.utc
STATE = Path.home() / ".local/state/herdr"
OP_DEADLINE_SECS = 30 * 60
TERMINAL_MARKERS = (
    "completed successfully", "task completed", "process exited", "command finished",
    "no shell", "exited with", "exit code", "was killed", "was stopped",
    "process was terminated", "command was terminated", "stopped by user",
    "task was stopped", "process was stopped", "process stopped", "stopped process",
    "has been stopped", "terminated with",
)


def epoch(value: str | None) -> int | None:
    if not value:
        return None
    try:
        return int(dt.datetime.fromisoformat(value.replace("Z", "+00:00")).timestamp())
    except (TypeError, ValueError):
        return None


def text_content(value: object) -> str:
    if isinstance(value, str):
        return value
    if not isinstance(value, list):
        return ""
    parts = []
    for block in value:
        if not isinstance(block, dict):
            continue
        if block.get("type") in ("text", "output_text", "input_text"):
            content = block.get("text")
            if isinstance(content, str):
                parts.append(content)
    return "\n".join(parts).strip()


def transcript_paths() -> tuple[dict[str, Path], dict[str, Path]]:
    claude = {}
    for path in glob.glob(str(Path.home() / ".claude/projects/*/*.jsonl")):
        claude[Path(path).stem] = Path(path)
    codex = {}
    for path in glob.glob(str(Path.home() / ".codex*/sessions/**/rollout-*"), recursive=True):
        name = Path(path).name
        match = re.search(r"([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})\.jsonl$", name)
        if match:
            session = match.group(1)
            codex[session] = Path(path)
    return claude, codex


def transcript_events(agent: str, session_id: str, files: tuple[dict[str, Path], dict[str, Path]]):
    claude, codex = files
    path = claude.get(session_id) if agent == "claude" else codex.get(session_id)
    if path is None or not path.is_file():
        return None
    events = []
    try:
        with path.open() as source:
            for line in source:
                try:
                    row = json.loads(line)
                except (json.JSONDecodeError, UnicodeDecodeError):
                    continue
                when = epoch(row.get("timestamp"))
                if when is None:
                    continue
                event = {"ts": when, "role": "", "text": "", "call": None, "result": None}
                if agent == "claude":
                    message = row.get("message", {})
                    if row.get("type") == "assistant":
                        event["role"] = "assistant"
                        content = message.get("content", [])
                        event["text"] = text_content(content)
                        calls = [b for b in content if isinstance(b, dict) and b.get("type") == "tool_use"]
                        event["call"] = calls
                    elif row.get("type") == "user":
                        content = message.get("content", [])
                        event["text"] = text_content(content) if isinstance(content, list) else str(content or "")
                        results = [b for b in content if isinstance(b, dict) and b.get("type") == "tool_result"]
                        if results:
                            event["result"] = results
                        else:
                            event["role"] = "user"
                    if not event["text"]:
                        content = message.get("content", []) if isinstance(message, dict) else []
                        event["text"] = text_content(content) if isinstance(content, list) else str(content or "")
                        if not event["text"]:
                            event["text"] = str(row.get("text") or "")
                elif row.get("type") == "response_item":
                    payload = row.get("payload", {})
                    kind = payload.get("type")
                    if kind == "message":
                        event["role"] = str(payload.get("role") or "")
                        content = payload.get("content", [])
                        event["text"] = text_content(content) if isinstance(content, list) else str(content or "")
                    elif kind in ("function_call", "custom_tool_call"):
                        event["call"] = [{"id": payload.get("call_id"), "name": payload.get("name"), "input": payload.get("arguments", "{}") }]
                        if kind == "custom_tool_call":
                            event["call"][0]["input"] = payload.get("input", {})
                    elif kind in ("function_call_output", "custom_tool_call_output"):
                        event["result"] = [{"tool_use_id": payload.get("call_id"), "content": payload.get("output", "") }]
                event["notifications"] = task_notifications(event["text"])
                if event["role"] or event["call"] or event["result"] or event["notifications"]:
                    events.append(event)
    except OSError:
        return None
    events.sort(key=lambda item: item["ts"])
    return events


def call_input(call: dict) -> dict:
    value = call.get("input", {})
    if isinstance(value, dict):
        return value
    try:
        parsed = json.loads(value)
        return parsed if isinstance(parsed, dict) else {}
    except (TypeError, json.JSONDecodeError):
        return {}


def tool_name(call: dict) -> str:
    return str(call.get("name", "")).lower()


def tool_result_text(result: dict) -> str:
    value = result.get("content", "")
    if isinstance(value, list):
        return " ".join(str(part.get("text", "")) if isinstance(part, dict) else str(part) for part in value)
    return str(value)


def task_notifications(text: str) -> list[dict[str, str]]:
    notifications = []
    for block in re.findall(r"<task-notification\b[^>]*>.*?</task-notification\s*>", text, re.I | re.S):
        fields = {}
        for name in ("tool-use-id", "task-id", "status"):
            match = re.search(rf"<{name}\b[^>]*>\s*([^<]+?)\s*</{name}\s*>", block, re.I | re.S)
            if match:
                fields[name] = match.group(1).strip()
        if fields.get("status", "").lower() in {"completed", "failed", "killed"}:
            notifications.append(fields)
    return notifications


def is_terminal_result(text: str) -> bool:
    lower = text.lower()
    return any(marker in lower for marker in TERMINAL_MARKERS)


def apply_event(state: dict, event: dict) -> None:
    """Update pending and background calls from one transcript event."""
    pending = state.setdefault("pending", {})
    background = state.setdefault("background_shells", {})
    for call in event.get("call") or []:
        key = str(call.get("id") or "")
        if key:
            pending[key] = {"call": call, "ts": event["ts"]}
    for result in event.get("result") or []:
        key = str(result.get("tool_use_id") or "")
        entry = pending.pop(key, None)
        call = entry["call"] if entry else None
        name = tool_name(call) if call else ""
        result_text = tool_result_text(result)
        call_input_value = call_input(call) if call else {}
        if is_terminal_result(result_text):
            target_ids = {key}
            target_ids.update(str(call_input_value.get(field) or "") for field in ("task_id", "bash_id", "session_id"))
            target_ids.discard("")
            matches = [bg_key for bg_key, bg in background.items() if bg["ids"].intersection(target_ids)]
            if not matches and len(background) == 1 and name in {"bashoutput", "monitor", "exec", "exec_command"}:
                matches = list(background)
            for bg_key in matches:
                background.pop(bg_key, None)
        if call and name in {"bash", "exec_command", "agent", "task"} and call_input_value.get("run_in_background"):
            if is_terminal_result(result_text):
                background.pop(key, None)
            else:
                background_ids = {key}
                for match in re.finditer(r"\b(?:task|bash|shell|session)[-_ ]?id\s*[:=]?\s*([\w.-]+)", result_text, re.I):
                    background_ids.add(match.group(1))
                if "background" in result_text.lower():
                    background_ids.update(re.findall(r"\bID\s*[:=]\s*([\w.-]+)", result_text, re.I))
                background[key] = {"ids": background_ids, "ts": event["ts"]}
        elif call and name in {"bashoutput", "monitor"}:
            pass
        elif call and name in {"exec", "exec_command"}:
            if not is_terminal_result(result_text):
                running_id = re.search(r"\bsession id\s*[:=]?\s*(\d+)\b", result_text, re.I)
                if running_id:
                    background[key] = {"ids": {key, running_id.group(1)}, "ts": event["ts"]}
    for notification in event.get("notifications") or task_notifications(event.get("text", "")):
        done_ids = {notification.get("tool-use-id", ""), notification.get("task-id", "")} - {""}
        for key, bg in list(background.items()):
            if key in done_ids or bg["ids"].intersection(done_ids):
                background.pop(key, None)


def background_status(state: dict, timestamp: int) -> str:
    pending = state.setdefault("pending", {})
    background = state.setdefault("background_shells", {})
    if any(timestamp - call["ts"] < OP_DEADLINE_SECS for call in pending.values()) or any(
        timestamp - task["ts"] < OP_DEADLINE_SECS for task in background.values()
    ):
        return "live"
    if pending or background:
        return "unknown"
    return "none"


def external_wait(text: str) -> bool:
    """Treat a visible wait on an outside event as unknown background work."""
    for line in text.splitlines():
        line = line.strip()
        match = re.match(r"(?i)^\*{0,2}now:\*{0,2}\s*(.*)$", line)
        if not match:
            continue
        value = match.group(1).strip().strip("*").lower()
        if value.startswith(("waiting on you", "waiting for your", "waiting for human", "awaiting your")):
            return True
        if "worker" in value and re.search(r"\b(?:building|running|working|starts|finish|finishes)\b", value):
            return True
        if value.startswith(("wait", "waiting", "awaiting")) and re.search(
            r"\b(?:ci|qa|github|review|approval|decision|worker|ub1|ub2|free up|notification|result)\b",
            value,
        ):
            return True
    return False


def read_jsonl(path: Path) -> list[dict]:
    rows = []
    try:
        with path.open() as source:
            for line in source:
                try:
                    row = json.loads(line)
                    if isinstance(row, dict):
                        rows.append(row)
                except json.JSONDecodeError:
                    continue
    except FileNotFoundError:
        pass
    return rows


def status_value(row: dict) -> str:
    value = str(row.get("to_state") or "unknown").lower()
    if value in {"idle", "working", "blocked", "done", "stale", "unknown"}:
        return value
    if row.get("completion") == "complete":
        return "done"
    if row.get("idle") is True:
        return "idle"
    return "unknown"


def old_nudge_count(cutoff: int, end: int) -> int:
    count = 0
    for path in (STATE / "watchdog-status.log", STATE / "watchdog-status-log.jsonl"):
        for row in read_jsonl(path):
            try:
                when = int(row.get("timestamp", 0))
            except (TypeError, ValueError):
                continue
            if cutoff <= when <= end and row.get("action") == "nudge":
                count += 1
    return count


def load_data(cutoff: int, end: int, files):
    by_pair = defaultdict(list)
    for row in read_jsonl(STATE / "status-changes.jsonl"):
        when = epoch(row.get("ts"))
        session = row.get("session_id")
        pane = row.get("pane_id")
        if when is None or not pane or when > end:
            continue
        by_pair[(pane, session or "")].append((when, row))

    active = {}
    for key, records in by_pair.items():
        records.sort(key=lambda item: item[0])
        recent = [record for record in records if record[0] >= cutoff]
        if not recent:
            continue
        latest = max(recent, key=lambda item: item[0])[1]
        agent = str(latest.get("agent") or "unknown").lower()
        active[key] = {
            "records": records,
            "agent": agent,
            "events": transcript_events(agent, key[1], files) if key[1] else None,
        }
    return active


def iter_observations(active, cutoff: int, end: int, host: str):
    step = ((cutoff + 59) // 60) * 60
    quiet_periods = 0
    current = {}
    for timestamp in range(step, end + 1, 60):
        for (pane, session), item in active.items():
            status_records = item["records"]
            status_index = next((i for i in range(len(status_records) - 1, -1, -1) if status_records[i][0] <= timestamp), None)
            if status_index is None:
                continue
            status_time, status_row = status_records[status_index]
            events = item["events"]
            status = status_value(status_row)
            tail = ""
            background = "unknown" if events is None else "none"
            last_entry = None
            assistant_texts = []
            event_index = current.get((pane, session), {}).get("event_index", 0)
            state = current.get((pane, session), {})
            previous_entry = state.get("last_entry")
            assistant_texts = state.get("assistant_texts", [])
            last_entry = state.get("last_entry")
            if events is None:
                last_entry = state.get("last_entry", status_time)
            if events is not None:
                while event_index < len(events) and events[event_index]["ts"] <= timestamp:
                    event = events[event_index]
                    last_entry = event["ts"]
                    apply_event(state, event)
                    if event["role"] == "assistant" and event["text"]:
                        assistant_texts.append(event["text"])
                        assistant_texts = assistant_texts[-6:]
                    event_index += 1
                background = background_status(state, timestamp)
                if assistant_texts:
                    tail = "\n".join(assistant_texts)[-7000:] + "\n❯ \n"
                    if background == "none" and external_wait(tail):
                        background = "unknown"
                if last_entry != previous_entry:
                    state["in_quiet"] = False
                state.update({
                    "event_index": event_index,
                    "assistant_texts": assistant_texts,
                    "last_entry": last_entry,
                })
            else:
                state.setdefault("last_entry", last_entry)
            quiet_age = timestamp - last_entry if last_entry is not None else 0
            if quiet_age < 60:
                state["in_quiet"] = False
                current[(pane, session)] = state
                continue
            if not state.get("in_quiet", False):
                quiet_periods += 1
                state["in_quiet"] = True
            current[(pane, session)] = state
            hook_age = max(0, timestamp - status_time)
            yield {
                "timestamp": timestamp,
                "host": host,
                "pane_id": pane,
                "session_id": session,
                "agent": item["agent"],
                "status": status,
                "reported_at": str(status_time),
                "hook_age_secs": hook_age,
                "tail": tail,
                "background": background,
            }, quiet_periods


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, help="PR build of herdr")
    parser.add_argument("--host", default=os.uname().nodename.split(".")[0])
    parser.add_argument("--hours", type=int, default=24)
    parser.add_argument("--out", default=f"/tmp/wd-replay-{os.uname().nodename.split('.')[0]}")
    parser.add_argument(
        "--expect-stall", action="append", default=[], metavar="HOST:PANE:FROM_UTC:TO_UTC",
        help="recall check: require a would-nudge attempt for this pane inside the UTC window (repeatable)",
    )
    args = parser.parse_args()
    expected_stalls = []
    for spec in args.expect_stall:
        match = re.fullmatch(r"([^:]+):(.+):(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ):(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ)", spec)
        if not match:
            parser.error(f"invalid --expect-stall {spec!r}; expected HOST:PANE:YYYY-MM-DDTHH:MM:SSZ:YYYY-MM-DDTHH:MM:SSZ")
        from_ts, to_ts = epoch(match.group(3)), epoch(match.group(4))
        if from_ts is None or to_ts is None or from_ts > to_ts:
            parser.error(f"invalid --expect-stall time range: {spec!r}")
        expected_stalls.append({"host": match.group(1), "pane_id": match.group(2), "from": from_ts, "to": to_ts, "spec": spec})
    end = int(dt.datetime.now(UTC).timestamp())
    cutoff = end - args.hours * 3600
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    rows_path = out / "input.jsonl"
    decisions_path = out / "decisions.jsonl"
    attempts_path = Path(f"/tmp/wd-replay-{args.host}.jsonl")
    summary_path = out / "summary.json"
    report_path = out / "result.md"
    files = transcript_paths()
    active = load_data(cutoff, end, files)
    quiet_periods = 0
    observation_count = 0
    with rows_path.open("w") as target:
        for row, quiet_periods in iter_observations(active, cutoff, end, args.host):
            target.write(json.dumps(row, ensure_ascii=False) + "\n")
            observation_count += 1
    cmd = [args.binary, "watchdog", "--replay", str(rows_path), "--json"]
    with decisions_path.open("w") as output:
        result = subprocess.run(cmd, stdout=output, stderr=subprocess.PIPE, text=True, check=False)
    if result.returncode:
        print(result.stderr, file=sys.stderr)
        return result.returncode

    attempts = []
    decisions = []
    with decisions_path.open() as source:
        for line in source:
            try:
                row = json.loads(line)
            except json.JSONDecodeError:
                continue
            decisions.append(row)
            if row.get("would_nudge"):
                attempts.append(row)
    with attempts_path.open("w") as target:
        for row in attempts:
            target.write(json.dumps({"host": args.host, **row}, ensure_ascii=False) + "\n")
    episode_counts = defaultdict(int)
    for row in attempts:
        episode_counts[(row["pane_id"], row["session_id"], row["tail_hash"])] += 1
    stalled_background = defaultdict(int)
    for row in decisions:
        if row.get("class") == "stalled":
            stalled_background[row.get("background", "unknown")] += 1
    recall = []
    for expected in expected_stalls:
        window_attempts = [row for row in attempts if expected["host"] == args.host and row["pane_id"] == expected["pane_id"] and expected["from"] <= row["timestamp"] <= expected["to"]]
        midpoint = (expected["from"] + expected["to"]) // 2
        nearby = [row for row in decisions if expected["host"] == args.host and row["pane_id"] == expected["pane_id"]]
        nearest = min(nearby, key=lambda row: abs(row["timestamp"] - midpoint)) if nearby else None
        recall.append({
            **expected,
            "detected": bool(window_attempts),
            "attempts": window_attempts,
            "midpoint": midpoint,
            "midpoint_decision": nearest,
            "reason": None if window_attempts else ("no replay decision for pane" if nearest is None else "no would-nudge attempt inside the expected window"),
            "eligibility_block": (
                None if window_attempts or nearest is None else
                (f"background is {nearest.get('background')}; nudging requires background=none" + (" because no terminal result was observed" if nearest.get("background") == "live" else "") if nearest.get("background") != "none" else
                 ("classifier did not produce a stalled class" if nearest.get("class") != "stalled" else "other watchdog quiet/deadline rule did not make an attempt due"))
            ),
        })
    summary = {
        "host": args.host,
        "window_start_utc": dt.datetime.fromtimestamp(cutoff, UTC).isoformat(),
        "window_end_utc": dt.datetime.fromtimestamp(end, UTC).isoformat(),
        "panes_scanned": len({pane for pane, _ in active}),
        "pane_sessions_scanned": len(active),
        "sessions_scanned": len({session for _, session in active if session}),
        "quiet_periods": quiet_periods,
        "observations": observation_count,
        "would_nudge_attempts": len(attempts),
        "stalled_background_breakdown": dict(stalled_background),
        "expected_stall_recall": [
            {"spec": row["spec"], "detected": row["detected"], "reason": row["reason"], "eligibility_block": row["eligibility_block"], "class": (row["midpoint_decision"] or {}).get("class"), "evidence": (row["midpoint_decision"] or {}).get("evidence"), "background": (row["midpoint_decision"] or {}).get("background")}
            for row in recall
        ],
        "distinct_panes_episodes_nudged": len(episode_counts),
        "max_attempts_per_episode": max(episode_counts.values(), default=0),
        "old_watchdog_nudges_same_window": old_nudge_count(cutoff, end),
        "pane_sessions_without_transcript": sum(item["events"] is None for item in active.values()),
        "binary": str(Path(args.binary).resolve()),
        "binary_sha256": hashlib.sha256(Path(args.binary).read_bytes()).hexdigest(),
        "attempt_rows": str(attempts_path),
    }
    summary_path.write_text(json.dumps(summary, indent=2) + "\n")
    lines = [
        f"# Watchdog 24-hour replay ({args.host})",
        "",
        f"Window: {summary['window_start_utc']} to {summary['window_end_utc']}.",
        f"PR build: `{summary['binary']}` (`{summary['binary_sha256']}`).",
        "",
        "## Summary",
        "",
        f"- Panes scanned: {summary['panes_scanned']} across {summary['sessions_scanned']} sessions.",
        f"- Quiet periods: {quiet_periods}; synthesized observations: {observation_count}.",
        f"- Would-nudge attempts: {len(attempts)}; distinct pane/episodes: {len(episode_counts)}; max attempts per episode: {summary['max_attempts_per_episode']}.",
        f"- Old watchdog nudge records in the same window: {summary['old_watchdog_nudges_same_window']}.",
        f"- Pane/session entries without a matching transcript: {summary['pane_sessions_without_transcript']} (conservatively ineligible).",
        f"- Stalled-observation background: live {stalled_background['live']}; none {stalled_background['none']}; unknown {stalled_background['unknown']}",
        "- Unresolved calls with no result after the 30-minute operation deadline are `unknown`; this remains ineligible for nudging.",
        "",
        "## Recall checks",
        "",
    ]
    if not recall:
        lines.append("None requested.")
    for item in recall:
        lines.append(f"- {'DETECTED' if item['detected'] else 'MISSED'} `{item['spec']}`")
        if not item["detected"]:
            decision = item["midpoint_decision"] or {}
            lines.append(f"  - Why: {item['reason']}; midpoint class `{decision.get('class', 'unavailable')}`, evidence `{decision.get('evidence', 'unavailable')}`, background `{decision.get('background', 'unavailable')}`. Eligibility blocker: {item['eligibility_block'] or 'unavailable'}.")
    lines.extend([
        "",
        "## Would-nudge rows for hand check",
        "",
    ])
    if not attempts:
        lines.append("None.")
    attempt_groups = defaultdict(list)
    for row in attempts:
        attempt_groups[(row["pane_id"], row["session_id"], row["tail_hash"])].append(row)
    for (pane, session, tail_hash), group in sorted(attempt_groups.items(), key=lambda item: (item[0][0], item[1][0]["timestamp"])):
        first = group[0]
        start_when = dt.datetime.fromtimestamp(first["timestamp"], UTC).isoformat()
        lines.extend([f"### {pane} / {session} — episode from {start_when} (`tail_hash={tail_hash}`)", ""])
        for index, row in enumerate(group, 1):
            when = dt.datetime.fromtimestamp(row["timestamp"], UTC).isoformat()
            lines.extend([
                f"#### Attempt {row['attempt']} (row {index})",
                "",
                f"- Time: {when}; idle: {row['idle_secs'] // 60} min; hook age: {(row.get('hook_age_secs') or 0) // 60} min.",
                f"- Class: {row['class']}; evidence: {row['evidence']}",
                f"- Background: {row['background']}; simulated delivered: {str(row['delivered']).lower()}.",
                "- Last 12 screen lines used:", "```text", *row["tail"], "```", "",
            ])
    report_path.write_text("\n".join(lines) + "\n")
    print(json.dumps(summary, indent=2))
    print(f"REPORT {report_path}")
    print(f"ATTEMPTS {attempts_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
