#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 <candidate-binary>" >&2
  exit 2
fi

candidate=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
[[ -x "$candidate" ]] || { echo "candidate binary is not executable: $candidate" >&2; exit 1; }
script_dir=$(cd "$(dirname "$0")" && pwd)
repo_root=$(cd "$script_dir/.." && pwd)
baseline=${HERDR_PERF_BASELINE_BIN:-}
for command in jq lsof perl tmux; do
  command -v "$command" >/dev/null || { echo "required command not found: $command" >&2; exit 1; }
done
if [[ -z "$baseline" ]]; then
  command -v curl >/dev/null || { echo "required command not found: curl" >&2; exit 1; }
fi

case "$(uname -s)" in
  Linux) platform=linux; command -v pidstat >/dev/null || { echo "required command not found: pidstat" >&2; exit 1; } ;;
  Darwin) platform=macos ;;
  *) echo "release performance smoke supports Linux and macOS" >&2; exit 1 ;;
esac
case "$(uname -m)" in
  x86_64|amd64) arch=x86_64 ;;
  arm64|aarch64) arch=aarch64 ;;
  *) echo "unsupported architecture: $(uname -m)" >&2; exit 1 ;;
esac

root=$(mktemp -d /var/tmp/herdr-release-perf-smoke.XXXXXX)
cleanup() { rm -rf "$root"; }
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
mkdir -p "$root/results"

if [[ -z "$baseline" ]]; then
  baseline_version=$(jq -er '.version' "$repo_root/distribution/latest.json")
  baseline="$root/herdr-baseline"
  curl -fL --retry 3 \
    "https://github.com/herdrdev/herdr/releases/download/v${baseline_version}/herdr-${platform}-${arch}" \
    -o "$baseline"
  chmod +x "$baseline"
else
  baseline=$(cd "$(dirname "$baseline")" && pwd)/$(basename "$baseline")
fi
[[ -x "$baseline" ]] || { echo "baseline binary is not executable: $baseline" >&2; exit 1; }

case_script="$script_dir/release_perf_case.sh"
seconds=${HERDR_PERF_SAMPLE_SECONDS:-10}
warmup=${HERDR_PERF_WARMUP_SECONDS:-3}

for round in 1 2; do
  if [[ $round -eq 1 ]]; then variants="baseline candidate"; else variants="candidate baseline"; fi
  for scenario in hidden50 visible30; do
    for variant in $variants; do
      if [[ $variant == baseline ]]; then binary=$baseline; else binary=$candidate; fi
      "$case_script" "$binary" "$variant" "$scenario" "$round" "$seconds" "$warmup" "$root/results" "$platform"
    done
  done
done

mean_total() {
  awk '{ sum += $1; count++ } END { if (!count) exit 1; printf "%.3f", sum/count }' \
    "$root/results/$1/$2/r1/total-cpu.txt" \
    "$root/results/$1/$2/r2/total-cpu.txt"
}

failed=0
printf '\nrelease performance smoke (%s/%s, two rounds, %ss samples)\n' "$platform" "$arch" "$seconds"
printf '%-12s %12s %12s %12s\n' scenario baseline candidate change
for scenario in hidden50 visible30; do
  baseline_total=$(mean_total baseline "$scenario")
  candidate_total=$(mean_total candidate "$scenario")
  if awk -v before="$baseline_total" -v after="$candidate_total" 'BEGIN { exit !(before <= 0 || after <= 0) }'; then
    echo "error: $scenario measured no CPU usage; the benchmark did not exercise the binaries" >&2
    failed=1
  fi
  change=$(awk -v before="$baseline_total" -v after="$candidate_total" 'BEGIN { if (before == 0) print "n/a"; else printf "%+.1f%%", (after-before)/before*100 }')
  printf '%-12s %12s %12s %12s\n' "$scenario" "$baseline_total" "$candidate_total" "$change"
  if awk -v before="$baseline_total" -v after="$candidate_total" 'BEGIN { exit !(after > before * 1.25 && after - before > 0.5) }'; then
    echo "error: $scenario candidate CPU exceeds baseline by more than 25% and 0.5 CPU points" >&2
    failed=1
  fi
done

if [[ $failed -ne 0 ]]; then
  exit 1
fi
echo "release performance smoke passed"
