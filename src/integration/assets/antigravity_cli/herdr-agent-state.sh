#!/bin/sh
# installed by herdr
# managed by herdr; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# HERDR_INTEGRATION_ID=antigravity_cli
# HERDR_INTEGRATION_VERSION=1

set -eu

action="${1:-}"
hook_input_file="$(mktemp "${TMPDIR:-/tmp}/herdr-antigravity-cli-hook.XXXXXX")" || exit 0
trap 'rm -f "$hook_input_file"' EXIT HUP INT TERM
cat >"$hook_input_file" 2>/dev/null || true

case "$action" in
  working|idle|release) ;;
  *) exit 0 ;;
esac

[ "${HERDR_ENV:-}" = "1" ] || exit 0
[ -n "${HERDR_SOCKET_PATH:-}" ] || exit 0
[ -n "${HERDR_PANE_ID:-}" ] || exit 0
command -v python3 >/dev/null 2>&1 || exit 0

HERDR_ACTION="$action" HERDR_HOOK_INPUT_FILE="$hook_input_file" python3 - <<'PY'
import json
import os
import random
import socket
import time

source = "herdr:antigravity_cli"
agent = "antigravity-cli"
action = os.environ.get("HERDR_ACTION", "")
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
            hook_input = json.loads(content)
    except Exception:
        hook_input = {}

conversation_id = hook_input.get("conversationId")
agent_session_id = conversation_id if isinstance(conversation_id, str) and conversation_id else None
transcript_path = hook_input.get("transcriptPath")
agent_session_path = transcript_path if isinstance(transcript_path, str) and transcript_path else None

report_seq = time.time_ns()

# Always report/refresh the session first if we have the session ID
if agent_session_id:
    session_request = {
        "id": f"{source}:session:{int(time.time() * 1000)}:{random.randrange(1_000_000):06d}",
        "method": "pane.report_agent_session",
        "params": {
            "pane_id": pane_id,
            "source": source,
            "agent": agent,
            "agent_session_id": agent_session_id,
            "seq": report_seq,
        },
    }
    if agent_session_path:
        session_request["params"]["agent_session_path"] = agent_session_path

    try:
        client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        client.settimeout(0.5)
        client.connect(socket_path)
        client.sendall((json.dumps(session_request) + "\n").encode())
        try:
            client.recv(4096)
        except Exception:
            pass
        client.close()
    except Exception:
        pass

# Send the state update (or release)
request_id = f"{source}:{int(time.time() * 1000)}:{random.randrange(1_000_000):06d}"
if action == "release":
    request = {
        "id": request_id,
        "method": "pane.release_agent",
        "params": {
            "pane_id": pane_id,
            "source": source,
            "agent": agent,
            "seq": report_seq,
        },
    }
else:
    params = {
        "pane_id": pane_id,
        "source": source,
        "agent": agent,
        "state": action,
        "seq": report_seq,
    }
    if agent_session_id:
        params["agent_session_id"] = agent_session_id
    request = {
        "id": request_id,
        "method": "pane.report_agent",
        "params": params,
    }

try:
    client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    client.settimeout(0.5)
    client.connect(socket_path)
    client.sendall((json.dumps(request) + "\n").encode())
    try:
        client.recv(4096)
    except Exception:
        pass
    client.close()
except Exception:
    pass
PY
