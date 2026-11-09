#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 8 ]]; then
  echo "usage: $0 <binary> <variant> <scenario> <round> <seconds> <warmup> <output-root> <platform>" >&2
  exit 2
fi

bin_dir=$(cd "$(dirname "$1")" && pwd)
bin="$bin_dir/$(basename "$1")"
variant=$2
scenario=$3
round=$4
seconds=$5
warmup=$6
out_root=$(cd "$7" && pwd)
platform=$8
script_dir=$(cd "$(dirname "$0")" && pwd)
producer="$script_dir/release_perf_producer.pl"
cols=86
rows=47

case "$scenario" in
  visible30) scenario_tag=v3; total_panes=1; writers=visible; rate=30 ;;
  hidden50) scenario_tag=h5; total_panes=50; writers=hidden; rate=60 ;;
  *) echo "unknown scenario: $scenario" >&2; exit 2 ;;
esac
case "$platform" in linux) platform_tag=l ;; macos) platform_tag=m ;; *) exit 2 ;; esac
variant_tag=${variant:0:1}
name="rps${platform_tag}${variant_tag}${scenario_tag}r${round}x$$"
state="/var/tmp/shepherd-release-perf-$name"
xdg="$state/xdg"
runtime="$state/run"
gate="$state/start-output"
out="$out_root/$variant/$scenario/r$round"
mkdir -p "$xdg" "$runtime" "$out"

launch_env=(env -u SHEPHERD_BIN_PATH -u SHEPHERD_ENV -u SHEPHERD_SOCKET_PATH -u SHEPHERD_CLIENT_SOCKET_PATH -u SHEPHERD_SESSION -u SHEPHERD_STARTUP_CWD -u SHEPHERD_WORKSPACE_ID -u SHEPHERD_TAB_ID -u SHEPHERD_PANE_ID XDG_CONFIG_HOME="$xdg" XDG_RUNTIME_DIR="$runtime" SHEPHERD_DISABLE_SOUND=1 SHELL=/bin/sh)
control_env=(env -u SHEPHERD_BIN_PATH -u SHEPHERD_ENV -u SHEPHERD_SOCKET_PATH -u SHEPHERD_CLIENT_SOCKET_PATH -u SHEPHERD_STARTUP_CWD -u SHEPHERD_WORKSPACE_ID -u SHEPHERD_TAB_ID -u SHEPHERD_PANE_ID XDG_CONFIG_HOME="$xdg" XDG_RUNTIME_DIR="$runtime" SHEPHERD_DISABLE_SOUND=1 SHELL=/bin/sh SHEPHERD_SESSION="$name")

cleaned=0
cleanup() {
  if [[ $cleaned -eq 1 ]]; then return; fi
  cleaned=1
  "${control_env[@]}" "$bin" session stop "$name" >/dev/null 2>&1 || true
  for _ in $(seq 1 50); do
    if "${control_env[@]}" "$bin" session delete "$name" >/dev/null 2>&1; then break; fi
    sleep 0.1
  done
  tmux kill-session -t "$name" >/dev/null 2>&1 || true
  rm -rf "$state"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

printf -v launch 'exec '
printf -v quoted '%q ' "${launch_env[@]}" "$bin" --session "$name"
launch+=$quoted
tmux new-session -d -s "$name" -x "$cols" -y "$rows" "$launch"

panes_json=
for _ in $(seq 1 150); do
  if panes_json=$("${control_env[@]}" "$bin" pane list 2>/dev/null); then break; fi
  sleep 0.1
done
[[ -n "$panes_json" ]] || { echo "session API did not become ready" >&2; exit 1; }
root_pane=$(printf '%s\n' "$panes_json" | jq -r '.result.panes[0].pane_id')
workspace_id=$("${control_env[@]}" "$bin" workspace list | jq -r '.result.workspaces[0].workspace_id')
[[ -n "$root_pane" && "$root_pane" != null ]] || { echo "session did not report a root pane" >&2; exit 1; }
[[ -n "$workspace_id" && "$workspace_id" != null ]] || { echo "session did not report a workspace" >&2; exit 1; }
pane_file="$state/pane-ids.txt"
printf '%s\n' "$root_pane" > "$pane_file"

for ((index = 2; index <= total_panes; index++)); do
  created=$("${control_env[@]}" "$bin" tab create --workspace "$workspace_id" --label "bench-$index" --no-focus)
  pane_id=$(printf '%s\n' "$created" | jq -r '.result.root_pane.pane_id')
  [[ -n "$pane_id" && "$pane_id" != null ]] || { echo "tab $index did not return a pane" >&2; exit 1; }
  printf '%s\n' "$pane_id" >> "$pane_file"
done

index=0
while IFS= read -r pane_id; do
  index=$((index + 1))
  if [[ $writers == visible && $index -eq 1 ]] || [[ $writers == hidden && $index -gt 1 ]]; then
    "${control_env[@]}" "$bin" pane run "$pane_id" "$producer" "$rate" "$gate" "p$index" >/dev/null
  fi
done < "$pane_file"
touch "$gate"

socket="$xdg/shepherd/sessions/$name/shepherd.sock"
server_pid=
for _ in $(seq 1 80); do
  server_pid=$(lsof -t "$socket" 2>/dev/null | head -n1 || true)
  [[ -n "$server_pid" ]] && break
  sleep 0.1
done
[[ -n "$server_pid" ]] || { echo "could not find server pid" >&2; exit 1; }
client_pid=$(tmux list-panes -s -t "$name" -F '#{pane_pid}')
[[ -n "$client_pid" ]] || { echo "could not find client pid" >&2; exit 1; }
all_pids=("$server_pid" "$client_pid")

sleep "$warmup"
raw="$out/cpu-raw.txt"
if [[ $platform == linux ]]; then
  pid_csv=$(IFS=,; echo "${all_pids[*]}")
  LC_ALL=C pidstat -h -u -p "$pid_csv" 1 "$seconds" > "$raw"
else
  top_args=(top -l $((seconds + 1)) -s 1 -stats pid,cpu,time -n 2)
  for pid in "${all_pids[@]}"; do top_args+=(-pid "$pid"); done
  LC_ALL=C "${top_args[@]}" > "$raw"
fi

mean_linux() {
  awk -v target="$2" '
    /^Linux/ || /^#/ || NF < 5 { next }
    $3 == target && $(NF-2) ~ /^[0-9]+([.][0-9]+)?$/ { sum += $(NF-2); count++ }
    END { if (!count) exit 1; printf "%.6f,%d", sum/count, count }
  ' "$1"
}
mean_macos() {
  awk -v target="$2" '
    $1 == target && $2 ~ /^[0-9]+([.][0-9]+)?%?$/ {
      seen++; if (seen == 1) next; value=$2; gsub(/%/, "", value); sum += value; count++ }
    END { if (!count) exit 1; printf "%.6f,%d", sum/count, count }
  ' "$1"
}

total=0
for pid in "${all_pids[@]}"; do
  if [[ $platform == linux ]]; then parsed=$(mean_linux "$raw" "$pid"); else parsed=$(mean_macos "$raw" "$pid"); fi
  mean=${parsed%,*}
  samples=${parsed#*,}
  [[ $samples -eq $seconds ]] || { echo "expected $seconds samples for pid $pid, got $samples" >&2; exit 1; }
  total=$(awk -v total="$total" -v mean="$mean" 'BEGIN { printf "%.6f", total + mean }')
done

index=0
while IFS= read -r pane_id; do
  index=$((index + 1))
  if [[ $writers == visible && $index -eq 1 ]] || [[ $writers == hidden && $index -gt 1 ]]; then
    read_file="$out/pane-$index.txt"
    "${control_env[@]}" "$bin" pane read "$pane_id" --source visible --format text > "$read_file"
    grep -q 'bench-output-' "$read_file" || { echo "writer pane $index produced no output" >&2; exit 1; }
  fi
done < "$pane_file"

printf '%s\n' "$total" > "$out/total-cpu.txt"
printf '%s,%s,%s,%s\n' "$variant" "$scenario" "$round" "$total"
