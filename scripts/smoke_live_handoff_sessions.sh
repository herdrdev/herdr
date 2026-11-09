#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SHEPHERD_BIN="${SHEPHERD_BIN:-$ROOT/target/debug/shepherd}"
BASE="${BASE:-$(mktemp -d /tmp/shepherd-handoff-smoke.XXXXXX)}"
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

cleanup() {
  set +e
  for session in "${sessions[@]}"; do
    run_shepherd "$session" server stop >/dev/null 2>&1 || true
  done
  for pid in "${server_pids[@]}"; do
    kill "$pid" >/dev/null 2>&1 || true
  done
}
trap cleanup EXIT

run_shepherd() {
  local session="$1"
  shift
  local socket
  socket="$(api_socket "$session")"
  assert_smoke_socket "$socket"
  mkdir -p "$(dirname "$socket")" "$RUNTIME_DIR" "$STATE_DIR"
  env -u SHEPHERD_SOCKET_PATH \
    -u SHEPHERD_CLIENT_SOCKET_PATH \
    -u SHEPHERD_SESSION \
    XDG_CONFIG_HOME="$CONFIG_HOME" \
    XDG_RUNTIME_DIR="$RUNTIME_DIR" \
    XDG_STATE_HOME="$STATE_DIR" \
    "$SHEPHERD_BIN" --session "$session" "$@"
}

session_dir() {
  local session="$1"
  if [[ "$session" == "default" ]]; then
    printf '%s/shepherd-dev' "$CONFIG_HOME"
  else
    printf '%s/shepherd-dev/sessions/%s' "$CONFIG_HOME" "$session"
  fi
}

api_socket() {
  printf '%s/shepherd.sock' "$(session_dir "$1")"
}

client_socket() {
  printf '%s/shepherd-client.sock' "$(session_dir "$1")"
}

assert_smoke_socket() {
  local socket="$1"
  case "$socket" in
    "$CONFIG_HOME"/shepherd-dev/shepherd.sock | "$CONFIG_HOME"/shepherd-dev/sessions/*/shepherd.sock)
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
  python3 - "$socket" "$body" <<'PY'
import socket
import sys

path, body = sys.argv[1], sys.argv[2]
client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
client.connect(path)
client.sendall(body.encode() + b"\n")
response = b""
while not response.endswith(b"\n"):
    chunk = client.recv(65536)
    if not chunk:
        break
    response += chunk
print(response.decode().strip())
PY
}

pane_id_for_session() {
  local session="$1"
  local socket
  socket="$(api_socket "$session")"
  json_request "$socket" '{"id":"smoke:workspace:create","method":"workspace.create","params":{"cwd":"/tmp","focus":true}}' \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["result"]["root_pane"]["pane_id"])'
}

send_text() {
  local session="$1"
  local pane="$2"
  local text="$3"
  local socket
  socket="$(api_socket "$session")"
  python3 - "$socket" "$pane" "$text" <<'PY'
import json
import socket
import sys

path, pane, text = sys.argv[1], sys.argv[2], sys.argv[3]
request = {
    "id": "smoke:pane:send",
    "method": "pane.send_input",
    "params": {"pane_id": pane, "text": text, "keys": ["Enter"]},
}
client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
client.connect(path)
client.sendall(json.dumps(request).encode() + b"\n")
response = b""
while not response.endswith(b"\n"):
    chunk = client.recv(65536)
    if not chunk:
        break
    response += chunk
if b'"error"' in response:
    raise SystemExit(response.decode())
PY
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
    matches="$(pgrep -fc "python3 -m http.server $port --bind 127.0.0.1" || true)"
    if [[ -n "$matches" ]]; then
      count=$((count + matches))
    fi
  done
  printf '%s\n' "$count"
}

echo "using shepherd: $SHEPHERD_BIN"
echo "smoke base: $BASE"

cargo build --locked --manifest-path "$ROOT/Cargo.toml" >/dev/null
mkdir -p "$CONFIG_HOME/shepherd-dev" "$RUNTIME_DIR" "$STATE_DIR"
printf 'onboarding = false\n' > "$CONFIG_HOME/shepherd-dev/config.toml"

for session in "${sessions[@]}"; do
  echo "starting smoke session $session at $(api_socket "$session")"
  run_shepherd "$session" server >/dev/null 2>&1 &
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

for session in "${sessions[@]}"; do
  socket="$(api_socket "$session")"
  json_request "$socket" '{"id":"smoke:handoff","method":"server.live_handoff","params":{}}' >/dev/null
  wait_for_socket "$socket"
done

for i in "${!sessions[@]}"; do
  wait_for_http "${ports[$i]}" "hello-from-${sessions[$i]}"
done

after_count="$(smoke_http_count)"
echo "smoke python http.server process count after handoff: $after_count"
echo "multi-session live handoff smoke passed"
