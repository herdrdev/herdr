#!/usr/bin/env bash
# Resurrect integration test — verifies workspace layout + agent session restoration.
#
# Spins up an isolated named herdr session, creates 2 workspaces with pi
# agents, captures session refs, kills the server, restarts it, then asserts
# both workspaces AND the agent session refs survive the restart.
set -euo pipefail

HERDR_FIX="${HERDR_FIX:-herdr}"
TEST_SESSION="herdr-resurrect-test-$$"
WS1="/tmp/herdr-resurrect-ws1-$$"
WS2="/tmp/herdr-resurrect-ws2-$$"
LOG="/tmp/herdr-resurrect-test-$$.log"

cleanup() {
    pkill -f "$TEST_SESSION" 2>/dev/null || true
    rm -rf "$WS1" "$WS2" "$LOG" 2>/dev/null || true
    rm -rf "$HOME/.config/herdr/sessions/$TEST_SESSION" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

echo "=== Herdr resurrect integration test ==="
echo "Binary: $HERDR_FIX"
echo "Session: $TEST_SESSION (isolated)"

mkdir -p "$WS1" "$WS2"
( cd "$WS1" && git init -q && echo "test1" > f1.txt && git add . && git commit -qm "init1" )
( cd "$WS2" && git init -q && echo "test2" > f2.txt && git add . && git commit -qm "init2" )

"$HERDR_FIX" --session "$TEST_SESSION" server >"$LOG" 2>&1 &
SRV_PID=$!
sleep 5
kill -0 "$SRV_PID" || { echo "FAIL: server did not start"; cat "$LOG"; exit 1; }

"$HERDR_FIX" --session "$TEST_SESSION" workspace create --cwd "$WS1" --label "test-ws1" --no-focus >/dev/null
"$HERDR_FIX" --session "$TEST_SESSION" workspace create --cwd "$WS2" --label "test-ws2" --no-focus >/dev/null

W1=$("$HERDR_FIX" --session "$TEST_SESSION" workspace list 2>/dev/null | python3 -c "import json,sys;d=json.load(sys.stdin);print([w['workspace_id'] for w in d['result']['workspaces'] if w.get('label')=='test-ws1'][0])")
W2=$("$HERDR_FIX" --session "$TEST_SESSION" workspace list 2>/dev/null | python3 -c "import json,sys;d=json.load(sys.stdin);print([w['workspace_id'] for w in d['result']['workspaces'] if w.get('label')=='test-ws2'][0])")

"$HERDR_FIX" --session "$TEST_SESSION" agent start pi-1 --kind pi --pane "$W1:p1" >/dev/null
"$HERDR_FIX" --session "$TEST_SESSION" agent start pi-2 --kind pi --pane "$W2:p1" >/dev/null

"$HERDR_FIX" --session "$TEST_SESSION" agent prompt "$W1:p1" "echo hello" >/dev/null 2>&1 || true
"$HERDR_FIX" --session "$TEST_SESSION" agent prompt "$W2:p1" "echo hello" >/dev/null 2>&1 || true

sleep 20   # wait for session refs to register

REFS_BEFORE=$("$HERDR_FIX" --session "$TEST_SESSION" agent list 2>/dev/null | python3 -c "
import json,sys
d=json.load(sys.stdin)
agents=[a for a in d['result']['agents'] if a.get('name') in ('pi-1','pi-2')]
print(sum(1 for a in agents if a.get('agent_session')))
")
echo "Session refs before restart: $REFS_BEFORE (expect >= 1)"

kill "$SRV_PID" 2>/dev/null || true
sleep 3
wait "$SRV_PID" 2>/dev/null || true

"$HERDR_FIX" --session "$TEST_SESSION" server >>"$LOG" 2>&1 &
SRV_PID=$!
sleep 10

WS_AFTER=$("$HERDR_FIX" --session "$TEST_SESSION" workspace list 2>/dev/null | python3 -c "
import json,sys
d=json.load(sys.stdin)
print(len([w for w in d['result']['workspaces'] if w.get('label','').startswith('test-ws')]))
")
REFS_AFTER=$("$HERDR_FIX" --session "$TEST_SESSION" agent list 2>/dev/null | python3 -c "
import json,sys
d=json.load(sys.stdin)
agents=[a for a in d['result']['agents'] if a.get('name') in ('pi-1','pi-2')]
print(sum(1 for a in agents if a.get('agent_session')))
")

echo "Workspaces restored: $WS_AFTER (expect 2)"
echo "Session refs after restart: $REFS_AFTER (expect >= 1)"

PASS=true
[ "$WS_AFTER" -ge 2 ] || PASS=false
[ "$REFS_AFTER" -ge 1 ] || PASS=false

if [ "$PASS" = "true" ]; then
    echo "PASS: workspace layout + agent session restored"
    exit 0
else
    echo "FAIL"
    exit 1
fi
