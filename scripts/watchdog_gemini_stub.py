#!/usr/bin/env python3
"""Deterministic watchdog oracle. It never invokes tools or accesses Gemini."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import re
import sys
import os
import time


ROOT = Path(__file__).resolve().parents[1]
FIXTURE = ROOT / "tests/fixtures/watchdog/evidence-escalation.json"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("-m", "--model", required=True)
    parser.add_argument("-p", "--prompt", required=True)
    parser.add_argument("-o", "--output-format", required=True)
    parser.add_argument("--approval-mode", required=True)
    parser.add_argument("--policy", type=Path, required=True)
    parser.add_argument("--skip-trust", action="store_true")
    parser.add_argument("-e", dest="extensions", nargs="?")
    parser.add_argument("--allowed-mcp-server-names", nargs="?")
    args = parser.parse_args()

    policy = args.policy.read_text()
    if 'toolName = "*"' not in policy or 'decision = "deny"' not in policy:
        print("watchdog Gemini policy did not deny every tool", file=sys.stderr)
        return 3
    mode = os.environ.get("WATCHDOG_STUB_MODE", "")
    if mode == "timeout":
        time.sleep(120)
        return 0
    status_to_class = {"working": "working", "blocked": "waiting_human", "done": "finished_idle"}
    expected = {
        case["id"]: status_to_class.get(case["expected_state"], "unknown")
        for case in json.loads(FIXTURE.read_text())["cases"]
    }
    expected.update({"human-wait": "waiting_human", "quiet-stall": "working"})
    for match in re.finditer(r"^OBSERVATION (\S+)$", args.prompt, re.MULTILINE):
        observation_id = match.group(1)
        pane_id = observation_id.split("#", 1)[0]
        state = expected.get(pane_id, "unknown")
        if mode == "stale":
            observation_id += "-stale"
        if mode == "malformed":
            print("not a bound observation reply")
        else:
            print(f"{observation_id}\t{state}\tdeterministic watchdog fixture oracle")
    return 0


if __name__ == "__main__":
    sys.exit(main())
