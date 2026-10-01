#!/usr/bin/env bash
# Run `herdr watchdog` as a per-user service: a systemd --user unit on Linux, a LaunchAgent on macOS.
# Defaults: every 60 s, model calls off (--with-model opts in to Gemini).
# Logs: ~/.local/state/herdr/watchdog.log (Linux) or ~/Library/Logs/herdr-watchdog.log (macOS);
# status records: ~/.local/state/herdr/watchdog-status.log. Stop and remove: scripts/watchdog_service.sh uninstall.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage:
  scripts/watchdog_service.sh install [--herdr-bin PATH] [--interval-secs N] [--with-model] [--dry-run] [--print]
  scripts/watchdog_service.sh uninstall
  scripts/watchdog_service.sh status
EOF
}

fail() { printf 'watchdog service: %s\n' "$*" >&2; exit 1; }

detect_os() {
  if [[ -n ${WATCHDOG_SERVICE_OS:-} ]]; then
    case $WATCHDOG_SERVICE_OS in linux|darwin) printf '%s\n' "$WATCHDOG_SERVICE_OS" ;; *) fail "unsupported WATCHDOG_SERVICE_OS: $WATCHDOG_SERVICE_OS" ;; esac
  else
    case $(uname -s) in Linux) printf linux ;; Darwin) printf darwin ;; *) fail "unsupported operating system: $(uname -s)" ;; esac
  fi
}

xml_escape() {
  local value=$1
  value=${value//&/\&amp;}; value=${value//</\&lt;}; value=${value//>/\&gt;}
  value=${value//\"/\&quot;}; value=${value//\'/\&apos;}
  printf '%s' "$value"
}

systemd_quote() {
  local value=$1
  value=${value//\\/\\\\}; value=${value//\"/\\\"}
  printf '"%s"' "$value"
}

render() {
  local os=$1 binary=$2 interval=$3 model=$4 dry=$5 home=$6
  local bindir state status_log service_log args path
  bindir=$(dirname "$binary")
  state="$home/.local/state/herdr"
  status_log="$state/watchdog-status.log"
  args="$(systemd_quote "$binary") watchdog --interval-secs $interval"
  [[ $model == yes ]] || args+=" --no-model"
  [[ $dry == yes ]] && args+=" --dry-run"
  args+=" --status-log $(systemd_quote "$status_log")"
  if [[ $os == linux ]]; then
    service_log="$state/watchdog.log"
    # systemd takes append: paths verbatim; quotes would make them relative.
    [[ $service_log != *[[:space:]]* ]] || fail "log path must not contain whitespace: $service_log"
    path="$bindir:/usr/local/bin:/usr/bin:/bin"
    cat <<EOF
[Unit]
Description=Herdr watchdog
StartLimitIntervalSec=600
StartLimitBurst=5

[Service]
Type=simple
ExecStart=$args
Restart=on-failure
RestartSec=10
Environment=PATH=$(systemd_quote "$path")
StandardOutput=append:$service_log
StandardError=append:$service_log

[Install]
WantedBy=default.target
EOF
  else
    service_log="$home/Library/Logs/herdr-watchdog.log"
    path="$bindir:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"
    cat <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>so.scalable.herdr-watchdog</string>
  <key>ProgramArguments</key>
  <array>
    <string>$(xml_escape "$binary")</string>
    <string>watchdog</string><string>--interval-secs</string><string>$interval</string>
$(if [[ $model != yes ]]; then printf '    <string>--no-model</string>\n'; fi)
$(if [[ $dry == yes ]]; then printf '    <string>--dry-run</string>\n'; fi)
    <string>--status-log</string><string>$(xml_escape "$status_log")</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
  <key>ThrottleInterval</key><integer>10</integer>
  <key>StandardOutPath</key><string>$(xml_escape "$service_log")</string>
  <key>StandardErrorPath</key><string>$(xml_escape "$service_log")</string>
  <key>EnvironmentVariables</key><dict>
    <key>HOME</key><string>$(xml_escape "$home")</string>
    <key>PATH</key><string>$(xml_escape "$path")</string>
  </dict>
</dict>
</plist>
EOF
  fi
}

command_name=${1:-}
[[ -n $command_name ]] || { usage >&2; exit 2; }
shift
os=$(detect_os)
label=so.scalable.herdr-watchdog
unit=herdr-watchdog.service
linux_unit=${HOME:?HOME is required}/.config/systemd/user/$unit
mac_plist=${HOME:?HOME is required}/Library/LaunchAgents/$label.plist
state_dir=$HOME/.local/state/herdr
service_log=$state_dir/watchdog.log
status_log=$state_dir/watchdog-status.log
if [[ $os == darwin ]]; then service_log=$HOME/Library/Logs/herdr-watchdog.log; fi

case $command_name in
  install)
    herdr_bin='' interval=60 model=no dry=no print=no
    while (($#)); do
      case $1 in
        --herdr-bin) (($# >= 2)) || fail "--herdr-bin requires a path"; herdr_bin=$2; shift 2 ;;
        --interval-secs) (($# >= 2)) || fail "--interval-secs requires a value"; interval=$2; shift 2 ;;
        --with-model) model=yes; shift ;;
        --dry-run) dry=yes; shift ;;
        --print) print=yes; shift ;;
        *) fail "unknown install option: $1" ;;
      esac
    done
    [[ $interval =~ ^[1-9][0-9]*$ ]] || fail "interval must be a positive integer"
    if [[ -z $herdr_bin ]]; then herdr_bin=$(command -v herdr || true); fi
    [[ -n $herdr_bin && -x $herdr_bin ]] || fail "herdr binary is missing or not executable: ${herdr_bin:-herdr}"
    herdr_bin=$(cd "$(dirname "$herdr_bin")" && printf '%s/%s\n' "$PWD" "$(basename "$herdr_bin")")
    if [[ $print == yes ]]; then
      render "$os" "$herdr_bin" "$interval" "$model" "$dry" "$HOME"
      exit 0
    fi
    if [[ $os == linux ]]; then
      command -v systemctl >/dev/null || fail "systemctl not found"
      mkdir -p "$(dirname "$linux_unit")" "$state_dir"
      render "$os" "$herdr_bin" "$interval" "$model" "$dry" "$HOME" > "$linux_unit"
      systemctl --user daemon-reload
      if systemctl --user is-active --quiet "$unit"; then systemctl --user enable "$unit"; systemctl --user restart "$unit"; else systemctl --user enable --now "$unit"; fi
      printf 'stop/uninstall: scripts/watchdog_service.sh uninstall; systemctl --user disable --now %s\n' "$unit"
    else
      command -v launchctl >/dev/null || fail "launchctl not found"
      mkdir -p "$(dirname "$mac_plist")" "$HOME/Library/Logs" "$state_dir"
      render "$os" "$herdr_bin" "$interval" "$model" "$dry" "$HOME" > "$mac_plist"
      launchctl bootout "gui/$UID/$label" >/dev/null 2>&1 || true
      launchctl bootstrap "gui/$UID" "$mac_plist"
      printf 'stop/uninstall: scripts/watchdog_service.sh uninstall; launchctl bootout gui/%s/%s\n' "$UID" "$label"
    fi
    ;;
  uninstall)
    if [[ $os == linux ]]; then
      if command -v systemctl >/dev/null; then systemctl --user disable --now "$unit" >/dev/null 2>&1 || true; systemctl --user daemon-reload >/dev/null 2>&1 || true; fi
      rm -f "$linux_unit"
    else
      if command -v launchctl >/dev/null; then launchctl bootout "gui/$UID/$label" >/dev/null 2>&1 || true; fi
      rm -f "$mac_plist"
    fi
    ;;
  status)
    if [[ $os == linux ]]; then
      systemctl --user status "$unit" --no-pager || true
      printf 'PID: '; systemctl --user show "$unit" --property=MainPID --value 2>/dev/null || true
    else
      launchctl print "gui/$UID/$label" 2>/dev/null | sed -n '1,20p' || printf 'launchd service is not loaded\n'
      printf 'PID: '; launchctl list "$label" 2>/dev/null | awk 'NR == 2 {print $1}' || true
    fi
    printf '%s\n' 'Last 5 log lines:'
    if [[ -f $service_log ]]; then tail -n 5 "$service_log"; else printf 'No log file: %s\n' "$service_log"; fi
    ;;
  *) usage >&2; exit 2 ;;
esac
