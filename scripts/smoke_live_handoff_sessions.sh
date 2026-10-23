#!/usr/bin/env bash

cleanup() {
  set +e
  for session in "${sessions[@]}"; do
    (exec_smoke_herdr "$session" server stop) >/dev/null 2>&1 || true
  done
  for pid in "${server_pids[@]}"; do
    [[ -n "$pid" ]] || continue
    for _ in {1..200}; do
      kill -0 "$pid" 2>/dev/null || continue 2
      sleep 0.05
    done
    kill "$pid" >/dev/null 2>&1 || true
  done
}

exec_smoke_herdr() {
  local session="$1"
  shift
  local socket
  socket="$(api_socket "$session")"
  assert_smoke_socket "$socket"
  mkdir -p "$(dirname "$socket")" "$RUNTIME_DIR" "$STATE_DIR"
  exec env -u HERDR_SOCKET_PATH \
    -u HERDR_CLIENT_SOCKET_PATH \
    -u HERDR_SESSION \
    XDG_CONFIG_HOME="$CONFIG_HOME" \
    XDG_RUNTIME_DIR="$RUNTIME_DIR" \
    XDG_STATE_HOME="$STATE_DIR" \
    "$HERDR_BIN" --session "$session" "$@"
}

session_dir() {
  local session="$1"
  if [[ "$session" == "default" ]]; then
    printf '%s/herdr-dev' "$CONFIG_HOME"
  else
    printf '%s/herdr-dev/sessions/%s' "$CONFIG_HOME" "$session"
  fi
}

api_socket() {
  printf '%s/herdr.sock' "$(session_dir "$1")"
}

assert_smoke_socket() {
  local socket="$1"
  case "$socket" in
    "$CONFIG_HOME"/herdr-dev/herdr.sock | "$CONFIG_HOME"/herdr-dev/sessions/*/herdr.sock)
      ;;
    *)
      echo "refusing to use non-smoke socket: $socket" >&2
      exit 1
      ;;
  esac
}

wait_for_socket() {
  local socket="$1"
  for _ in {1..200}; do
    [[ -S "$socket" ]] && return 0
    sleep 0.05
  done
  echo "socket did not appear: $socket" >&2
  return 1
}

wait_for_http() {
  local port="$1"
  local expected="$2"
  for _ in {1..200}; do
    if curl -fsS "http://127.0.0.1:$port/" | grep -q "$expected"; then
      return 0
    fi
    sleep 0.05
  done
  echo "http server on port $port did not return $expected" >&2
  return 1
}

json_request() {
  local socket="$1"
  local body="$2"
  local expected="$3"
  python3 - "$socket" "$body" "$expected" "${4:-30}" <<'PY'
import json
import socket
import sys

path, body, expected, timeout = sys.argv[1:]
try:
    request = json.loads(body)
    request_id = request.get("id")
    if not isinstance(request_id, str) or not request_id:
        raise ValueError("request must have a nonempty string id")
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
        client.settimeout(float(timeout))
        client.connect(path)
        client.sendall(body.encode() + b"\n")
        response = bytearray()
        while b"\n" not in response:
            chunk = client.recv(65536)
            if not chunk:
                raise ValueError("response ended before a complete JSON line")
            response.extend(chunk)
            if len(response) > 1024 * 1024:
                raise ValueError("response exceeded 1 MiB")
    line, trailing = response.split(b"\n", 1)
    if trailing.strip():
        raise ValueError("unexpected data after response")
    message = json.loads(line)
    if not isinstance(message, dict) or message.get("id") != request_id:
        raise ValueError("response id does not match request")
    if "error" in message:
        raise ValueError(f"API request failed: {message['error']}")
    result = message.get("result")
    if not isinstance(result, dict) or result.get("type") != expected:
        raise ValueError(f"expected result type {expected!r}, got {result!r}")
except (OSError, ValueError) as error:
    raise SystemExit(str(error))
print(json.dumps(message))
PY
}

pane_id_for_session() {
  local session="$1"
  local socket
  socket="$(api_socket "$session")"
  json_request "$socket" '{"id":"smoke:workspace:create","method":"workspace.create","params":{"cwd":"/tmp","focus":true}}' workspace_created \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["result"]["root_pane"]["pane_id"])'
}

send_text() {
  local session="$1"
  local pane="$2"
  local text="$3"
  local socket body
  socket="$(api_socket "$session")"
  body="$(python3 - "$pane" "$text" <<'PY'
import json
import sys

print(json.dumps({
    "id": "smoke:pane:send",
    "method": "pane.send_input",
    "params": {"pane_id": sys.argv[1], "text": sys.argv[2], "keys": ["Enter"]},
}))
PY
)"
  json_request "$socket" "$body" ok >/dev/null
}

replacement_pid_for_handoff() {
  python3 - "$1" "$2" "$HERDR_BIN" <<'PY'
import re
import subprocess
import sys
from pathlib import Path

old_pid, handoff_socket, binary = sys.argv[1:]
binary = Path(binary)
binary_prefixes = tuple(str(path) + " " for path in {binary.absolute(), binary.resolve()})
marker = re.compile(r"(?:^|\s)--handoff-import\s+" + re.escape(handoff_socket) + r"(?=\s|$)")
processes = subprocess.check_output(["ps", "-axww", "-o", "pid=", "-o", "args="], text=True)
matches = []
for line in processes.splitlines():
    fields = line.strip().split(None, 1)
    if len(fields) != 2:
        continue
    pid, command = fields
    if pid.isdecimal() and pid != old_pid and command.startswith(binary_prefixes) and marker.search(command):
        matches.append(pid)
if len(matches) != 1:
    raise SystemExit(1)
print(matches[0])
PY
}

wait_for_replacement() {
  local old_pid="$1"
  local handoff_socket="$2"
  local pid
  for _ in {1..200}; do
    if pid="$(replacement_pid_for_handoff "$old_pid" "$handoff_socket")" && kill -0 "$pid" 2>/dev/null; then
      printf '%s\n' "$pid"
      return 0
    fi
    sleep 0.05
  done
  echo "replacement did not appear for $handoff_socket" >&2
  return 1
}

wait_for_server_exit() {
  local pid="$1"
  for _ in {1..200}; do
    if ! kill -0 "$pid" 2>/dev/null; then
      wait "$pid"
      return
    fi
    sleep 0.05
  done
  echo "original server $pid did not exit" >&2
  return 1
}

wait_for_api() {
  local socket="$1"
  for _ in {1..200}; do
    if json_request "$socket" '{"id":"smoke:ping","method":"ping","params":{}}' pong 1 >/dev/null 2>&1; then
      return 0
    fi
    sleep 0.05
  done
  echo "replacement API did not become ready: $socket" >&2
  return 1
}

unused_port() {
  python3 - <<'PY'
import socket
sock = socket.socket()
sock.bind(("127.0.0.1", 0))
print(sock.getsockname()[1])
sock.close()
PY
}

smoke_http_count() {
  local count=0
  local port matches
  for port in "${ports[@]}"; do
    matches="$(pgrep -f "[pP]ython.*-m http.server $port --bind 127.0.0.1" || true)"
    if [[ -n "$matches" ]]; then
      count=$((count + $(printf '%s\n' "$matches" | wc -l)))
    fi
  done
  printf '%s\n' "$count"
}

main() (
  set -euo pipefail

  ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
  HERDR_BIN="${HERDR_BIN:-$ROOT/target/debug/herdr}"
  BASE="${BASE:-$(mktemp -d /tmp/herdr-handoff-smoke.XXXXXX)}"
  CONFIG_HOME="$BASE/config"
  RUNTIME_DIR="$BASE/runtime"
  STATE_DIR="$BASE/state"

  if [[ "$CONFIG_HOME" == "$HOME/.config" || "$CONFIG_HOME" == "$HOME/.config/"* ]]; then
    echo "refusing to run smoke test against $CONFIG_HOME" >&2
    exit 1
  fi

  sessions=("default" "work" "api")
  ports=()
  server_pids=()
  trap cleanup EXIT

  echo "using herdr: $HERDR_BIN"
  echo "smoke base: $BASE"

  cargo build --locked --manifest-path "$ROOT/Cargo.toml" >/dev/null
  mkdir -p "$CONFIG_HOME/herdr-dev" "$RUNTIME_DIR" "$STATE_DIR"
  printf 'onboarding = false\n' > "$CONFIG_HOME/herdr-dev/config.toml"

  for session in "${sessions[@]}"; do
    echo "starting smoke session $session at $(api_socket "$session")"
    exec_smoke_herdr "$session" server >/dev/null 2>&1 &
    server_pids+=("$!")
    wait_for_socket "$(api_socket "$session")"
  done

  for session in "${sessions[@]}"; do
    port="$(unused_port)"
    ports+=("$port")
    web="$BASE/web-$session"
    mkdir -p "$web"
    printf 'hello-from-%s\n' "$session" > "$web/index.html"
    pane="$(pane_id_for_session "$session")"
    send_text "$session" "$pane" "cd '$web' && python3 -m http.server $port --bind 127.0.0.1"
    wait_for_http "$port" "hello-from-$session"
  done

  before_count="$(smoke_http_count)"
  echo "smoke python http.server process count before handoff: $before_count"

  for i in "${!sessions[@]}"; do
    session="${sessions[$i]}"
    old_pid="${server_pids[$i]}"
    socket="$(api_socket "$session")"
    json_request "$socket" '{"id":"smoke:handoff","method":"server.live_handoff","params":{}}' ok >/dev/null
    replacement_pid="$(wait_for_replacement "$old_pid" "$(session_dir "$session")/herdr-handoff-$old_pid.sock")"
    server_pids+=("$replacement_pid")
    wait_for_server_exit "$old_pid"
    server_pids[$i]=""
    wait_for_api "$socket"
    echo "session $session replaced server $old_pid with $replacement_pid"
  done

  for i in "${!sessions[@]}"; do
    wait_for_http "${ports[$i]}" "hello-from-${sessions[$i]}"
  done

  after_count="$(smoke_http_count)"
  echo "smoke python http.server process count after handoff: $after_count"
  echo "multi-session live handoff smoke passed"
)

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main "$@"
fi
