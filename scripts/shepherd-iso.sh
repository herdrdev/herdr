#!/usr/bin/env bash
# Isolated herdr session for Shepherd checks. Never touches the real HOME/XDG state.
# usage: shepherd-iso.sh start|stop|keys <tmux keys...>|screen|call <method> <json-params>|seed <agents.json>|reset
# BIN defaults to ./target/debug/herdr (run ./cargo-fork.sh build first).
set -u
R=/tmp/shfiso; BIN=$(readlink -f "${BIN:-./target/debug/herdr}"); S=iso
mkdir -p $R/home $R/cfg/herdr-dev $R/state $R/data $R/run $R/repo; chmod 700 $R/run
[ -d $R/repo/.git ] || (cd $R/repo && git init -q && git -c user.email=a@b -c user.name=t commit -q --allow-empty -m init)
[ -f $R/cfg/herdr-dev/config.toml ] || printf 'onboarding = false\n[shepherd]\nenabled = true\n' > $R/cfg/herdr-dev/config.toml
E="HOME=${SHF_HOME:-$R/home} XDG_CONFIG_HOME=$R/cfg XDG_STATE_HOME=$R/state XDG_DATA_HOME=$R/data XDG_RUNTIME_DIR=$R/run"
SOCK=$R/cfg/herdr-dev/sessions/$S/herdr.sock
case "${1:-}" in
 start) tmux -L shfiso kill-server 2>/dev/null
   tmux -L shfiso new-session -d -s t -x 200 -y 50 -c $R/repo "env -u HERDR_ENV -u HERDR_SOCKET_PATH -u HERDR_SESSION -u HERDR_PANE_ID $E $BIN --session $S 2>$R/err.log"
   for i in $(seq 1 30); do [ -S $SOCK ] && break; sleep 0.5; done; sleep 2; echo started;;
 stop) env $E $BIN --session $S server stop >/dev/null 2>&1; tmux -L shfiso kill-server 2>/dev/null; echo stopped;;
 keys) shift; tmux -L shfiso send-keys -t t "$@";;
 screen) tmux -L shfiso capture-pane -p -t t;;
 call) python3 - "$SOCK" "$2" "$3" <<'PY'
import socket,sys,json
s=socket.socket(socket.AF_UNIX); s.settimeout(float(__import__("os").environ.get("SHF_TIMEOUT","20"))); s.connect(sys.argv[1])
s.sendall((json.dumps({"id":"1","method":sys.argv[2],"params":json.loads(sys.argv[3])})+"\n").encode())
d=b""
while not d.endswith(b"\n"):
    c=s.recv(65536)
    if not c: break
    d+=c
print(d.decode()[:200000])
PY
 ;;
 seed) mkdir -p $R/cfg/herdr-dev/sessions/$S/shepherd && cp "$2" $R/cfg/herdr-dev/sessions/$S/shepherd/agents.json; echo seeded;;
 reset) "$0" stop; rm -rf $R/cfg/herdr-dev/sessions $R/repo/.shepherd; echo reset;;
 *) sed -n 2,4p "$0"; exit 2;;
esac
