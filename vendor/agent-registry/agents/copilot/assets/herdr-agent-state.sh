#!/bin/sh
# installed by herdr
# managed by herdr; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# HERDR_INTEGRATION_ID=copilot
# HERDR_INTEGRATION_VERSION=3

set -eu

hook_input_file="$(mktemp "${TMPDIR:-/tmp}/herdr-copilot-hook.XXXXXX")" || exit 0
trap 'rm -f "$hook_input_file"' EXIT HUP INT TERM
cat >"$hook_input_file" 2>/dev/null || true

[ "${HERDR_ENV:-}" = "1" ] || exit 0
[ -n "${HERDR_SOCKET_PATH:-}" ] || exit 0
[ -n "${HERDR_PANE_ID:-}" ] || exit 0
command -v python3 >/dev/null 2>&1 || exit 0

HERDR_HOOK_INPUT_FILE="$hook_input_file" python3 - <<'PY'
import json
import os
import random
import socket
import time

source = "herdr:copilot"
pane_id = os.environ.get("HERDR_PANE_ID")
socket_path = os.environ.get("HERDR_SOCKET_PATH")
hook_input_file = os.environ.get("HERDR_HOOK_INPUT_FILE")

if not pane_id or not socket_path:
    raise SystemExit(0)

hook_input = {}
if hook_input_file:
    try:
        with open(hook_input_file, encoding="utf-8") as handle:
            content = handle.read()
        if content.strip():
            parsed = json.loads(content)
            if isinstance(parsed, dict):
                hook_input = parsed
    except Exception:
        hook_input = {}

def first_text(*keys):
    for key in keys:
        value = hook_input.get(key)
        if isinstance(value, str) and value:
            return value
    return None

def normalize_event(event):
    return event.replace("_", "").replace("-", "").lower()

event = first_text("hook_event_name", "hookEventName")
if event:
    if normalize_event(event) != "sessionstart":
        raise SystemExit(0)
elif "prompt" in hook_input or first_text("tool_name", "toolName", "notification_type", "notificationType", "stop_reason", "stopReason", "reason"):
    raise SystemExit(0)

session_id = hook_input.get("session_id")
if not isinstance(session_id, str) or not session_id:
    session_id = hook_input.get("sessionId")
if not isinstance(session_id, str) or not session_id:
    raise SystemExit(0)

request = {
    "id": f"{source}:{int(time.time() * 1000)}:{random.randrange(1_000_000):06d}",
    "method": "pane.report_agent_session",
    "params": {
        "pane_id": pane_id,
        "source": source,
        "agent": "copilot",
        "agent_session_id": session_id,
        "seq": time.time_ns(),
    },
}

try:
    client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    client.settimeout(0.5)
    client.connect(socket_path)
    client.sendall((json.dumps(request) + "\n").encode("utf-8"))
    try:
        client.recv(4096)
    except Exception:
        pass
    client.close()
except Exception:
    pass
PY
