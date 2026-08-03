#!/usr/bin/env bash
# Mixed pi + hermes resurrect test (10 agents, 3 workspaces)
set -euo pipefail

HERDR="${HERDR_FIX:-/home/bhd/.local/bin/herdr-fix}"
SESS="mixed-$$"
BASE="/tmp/mixed-$$"
LOG_DIR="/tmp/mixed-logs-$$"
mkdir -p "$LOG_DIR"

cleanup() {
    pkill -f "herdr-fix --session $SESS" 2>/dev/null || true
    pkill -9 -f "hermes-agent/bin/hermes" 2>/dev/null || true
    sleep 1
    rm -rf "$BASE" "$LOG_DIR" 2>/dev/null || true
    rm -rf "$HOME/.config/herdr/sessions/$SESS" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

echo "=== Mixed pi+hermes resurrect test ===" >&2
echo "Binary: $HERDR" >&2
echo "Session: $SESS" >&2

# Setup 3 workspaces
mkdir -p "$BASE/ws1" "$BASE/ws2" "$BASE/ws3"
for i in 1 2 3; do
    cd "$BASE/ws$i" && git init -q && echo "ws$i" > README.md && git add . && git commit -qm "init ws$i"
done
cd "$HOME"

"$HERDR" --session "$SESS" server >"$LOG_DIR/server-1.log" 2>&1 &
SRV=$!
sleep 6
if ! kill -0 "$SRV" 2>/dev/null; then
    echo "FAIL: server didn't start" >&2
    cat "$LOG_DIR/server-1.log" >&2
    exit 1
fi

# Create 3 workspaces
for i in 1 2 3; do
    "$HERDR" --session "$SESS" workspace create --cwd "$BASE/ws$i" --label "ws$i" --no-focus >"$LOG_DIR/ws$i.log" 2>&1
done
sleep 2

# Get workspace IDs
get_wid() {
    "$HERDR" --session "$SESS" workspace list 2>/dev/null | python3 -c "
import json,sys
d=json.load(sys.stdin)
for w in d['result']['workspaces']:
    if w.get('label')=='$1': print(w['workspace_id']); break
"
}
W1=$(get_wid ws1); W2=$(get_wid ws2); W3=$(get_wid ws3)
echo "Workspace IDs: $W1 $W2 $W3" >&2

# Split to 10 panes: ws1=4, ws2=3, ws3=3
echo "=== Split panes ===" >&2
for _ in 1 2 3; do
    "$HERDR" --session "$SESS" pane split "$W1:p1" --direction right >"$LOG_DIR/split.log" 2>&1
    sleep 0.5
done
for _ in 1 2; do
    "$HERDR" --session "$SESS" pane split "$W2:p1" --direction right >>"$LOG_DIR/split.log" 2>&1
    sleep 0.5
done
for _ in 1 2; do
    "$HERDR" --session "$SESS" pane split "$W3:p1" --direction right >>"$LOG_DIR/split.log" 2>&1
    sleep 0.5
done
sleep 2

# Get all pane IDs via pane list (live API, not session.json)
get_panes_in_ws() {
    "$HERDR" --session "$SESS" pane list --workspace "$1" 2>/dev/null | python3 -c "
import json,sys
d=json.load(sys.stdin)
for p in d['result']['panes']:
    print(p['pane_id'])
"
}
PANES_WS1=$(get_panes_in_ws "$W1")
PANES_WS2=$(get_panes_in_ws "$W2")
PANES_WS3=$(get_panes_in_ws "$W3")
echo "WS1 panes: $(echo "$PANES_WS1" | wc -l)" >&2
echo "WS2 panes: $(echo "$PANES_WS2" | wc -l)" >&2
echo "WS3 panes: $(echo "$PANES_WS3" | wc -l)" >&2

# Start agents: 7 pi + 3 hermes
echo "=== Start agents (target: 10) ===" >&2
STARTED=0
IDX=0
for pane in $PANES_WS1 $PANES_WS2 $PANES_WS3; do
    IDX=$((IDX+1))
    if [ $IDX -le 7 ]; then
        KIND="pi"
        ARGS=()
    else
        KIND="hermes"
        ARGS=(-- --profile hermes-manager)
    fi
    echo "  [$IDX] $KIND → $pane" >&2
    if "$HERDR" --session "$SESS" agent start "a$IDX" --kind "$KIND" --pane "$pane" "${ARGS[@]}" >"$LOG_DIR/start-$IDX.log" 2>&1; then
        STARTED=$((STARTED+1))
    else
        echo "  WARN: $KIND start failed" >&2
    fi
    sleep 3
done
echo "Started: $STARTED / $IDX" >&2

# Send prompts
echo "=== Send prompts ===" >&2
for pane in $PANES_WS1 $PANES_WS2 $PANES_WS3; do
    "$HERDR" --session "$SESS" agent prompt "$pane" "say hi" >/dev/null 2>&1 || true
done

# Wait for sessions to register
echo "=== Wait 45s for session refs ===" >&2
sleep 45

# Get session refs via agent list (live API, not session.json)
echo "=== Session refs BEFORE restart ===" >&2
PI_BEFORE=$("$HERDR" --session "$SESS" agent list 2>/dev/null | python3 -c "
import json,sys
d=json.load(sys.stdin)
agents = [a for a in d['result']['agents'] if a.get('name','').startswith('a')]
pi = sum(1 for a in agents if a.get('agent')=='pi' and a.get('agent_session'))
hermes = sum(1 for a in agents if a.get('agent')=='hermes' and a.get('agent_session'))
print(f'{pi} {hermes}')
")
echo "Refs before: pi hermes = $PI_BEFORE" >&2

# Kill server
echo "=== Kill server ===" >&2
kill "$SRV" 2>/dev/null || true
sleep 3
wait "$SRV" 2>/dev/null || true

# Restart server
echo "=== Restart server ===" >&2
RUST_LOG=info "$HERDR" --session "$SESS" server >"$LOG_DIR/server-2.log" 2>&1 &
SRV=$!
sleep 15

# Verify restoration
echo "=== AFTER restart ===" >&2
WS_AFTER=$("$HERDR" --session "$SESS" workspace list 2>/dev/null | python3 -c "
import json,sys
d=json.load(sys.stdin)
print(len([w for w in d['result']['workspaces'] if w.get('label','').startswith('ws')]))
" || echo 0)
echo "Workspaces after restart: $WS_AFTER" >&2

PANES_AFTER=$("$HERDR" --session "$SESS" pane list 2>/dev/null | python3 -c "
import json,sys
d=json.load(sys.stdin)
n = sum(1 for p in d['result']['panes'] if p['pane_id'].split(':')[0] in ['$W1','$W2','$W3'])
print(n)
" || echo 0)
echo "Panes after restart: $PANES_AFTER" >&2

# Get session refs after restart
PI_AFTER=$("$HERDR" --session "$SESS" agent list 2>/dev/null | python3 -c "
import json,sys
d=json.load(sys.stdin)
agents = [a for a in d['result']['agents'] if a.get('name','').startswith('a')]
pi = sum(1 for a in agents if a.get('agent')=='pi' and a.get('agent_session'))
hermes = sum(1 for a in agents if a.get('agent')=='hermes' and a.get('agent_session'))
print(f'{pi} {hermes}')
" || echo "0 0")
echo "Refs after: pi hermes = $PI_AFTER" >&2

# Resume log entries
RESUME_LOGS=$(grep -cE "agent resume skipped|restore_plan|persist.restore" "$LOG_DIR/server-2.log" 2>/dev/null || echo 0)
echo "Resume log entries: $RESUME_LOGS" >&2

# Result
echo "" >&2
echo "=== RESULT ===" >&2
PASS=true
[ "$STARTED" -ge 10 ] || { echo "FAIL: STARTED=$STARTED / 10"; PASS=false; }
[ "$WS_AFTER" -ge 3 ] || { echo "FAIL: WS=$WS_AFTER < 3"; PASS=false; }
[ "$PANES_AFTER" -ge 10 ] || { echo "FAIL: PANES=$PANES_AFTER < 10"; PASS=false; }
# Both pi and hermes must have session refs
PI_BEFORE_COUNT=$(echo $PI_BEFORE | awk '{print $1}')
HERMES_BEFORE_COUNT=$(echo $PI_BEFORE | awk '{print $2}')
[ "$PI_BEFORE_COUNT" -gt 0 ] || { echo "FAIL: pi refs=0 before"; PASS=false; }
[ "$HERMES_BEFORE_COUNT" -gt 0 ] || { echo "FAIL: hermes refs=0 before"; PASS=false; }
PI_AFTER_COUNT=$(echo $PI_AFTER | awk '{print $1}')
HERMES_AFTER_COUNT=$(echo $PI_AFTER | awk '{print $2}')
[ "$PI_AFTER_COUNT" -ge 5 ] || { echo "FAIL: pi refs after=$PI_AFTER_COUNT < 5"; PASS=false; }
[ "$HERMES_AFTER_COUNT" -ge 2 ] || { echo "FAIL: hermes refs after=$HERMES_AFTER_COUNT < 2"; PASS=false; }

if [ "$PASS" = "true" ]; then
    echo "PASS: $STARTED/10 agents (7 pi + 3 hermes), 3 WS, $PANES_AFTER panes, refs: pi=$PI_AFTER_COUNT hermes=$HERMES_AFTER_COUNT" >&2
    exit 0
else
    echo "FAIL" >&2
    exit 1
fi
