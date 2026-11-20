#!/usr/bin/env bash
# Runs one rgitui verification scenario in an isolated run directory.
#
#   run.sh <scenario.json> [--fixture <script>] [--repo <existing-repo>] [--timeout <secs>] [--no-build]
#
# Creates  target/verify-runs/<stamp>-<scenario-name>/
#            repo/     the fixture repository the app opens (unless --repo)
#            state/    settings + caches for this run only (RGITUI_PERF_STATE)
#            out/      the replay report and screenshots/ (RGITUI_PERF_OUT)
#            evidence  symlink to out/<stamp>-<name>/: report.json, screenshots/
#            app.log   the app's log at RUST_LOG=info
#            pid       the app's PID while it runs
# and prints the run directory on its last line of stdout.
#
# Exit status: 0 when the scenario ran every step, 1 when a step failed or the
# app exited early, 2 on bad usage, 124 on timeout (the app is killed).
set -euo pipefail

skill_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
repo_root="$(cd "$skill_dir/../../.." && pwd)"

scenario="" fixture="$skill_dir/fixtures/basic.sh" repo="" timeout=180 build=1
while (($#)); do
  case "$1" in
    --fixture) fixture="$2"; shift 2 ;;
    --repo) repo="$2"; shift 2 ;;
    --timeout) timeout="$2"; shift 2 ;;
    --no-build) build=0; shift ;;
    -*) echo "unknown flag $1" >&2; exit 2 ;;
    *) scenario="$1"; shift ;;
  esac
done
[[ -n "$scenario" && -f "$scenario" ]] || { echo "usage: run.sh <scenario.json> [...]" >&2; exit 2; }
scenario="$(cd "$(dirname "$scenario")" && pwd)/$(basename "$scenario")"

if ((build)); then
  (cd "$repo_root" && cargo build -q -p rgitui --features perf-capture) >&2
fi
bin="$repo_root/target/debug/rgitui"

name="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["name"])' "$scenario")"
run="$repo_root/target/verify-runs/$(date +%Y%m%d-%H%M%S)-$name"
mkdir -p "$run/state" "$run/out"

if [[ -z "$repo" ]]; then
  repo="$run/repo"
  bash "$fixture" "$repo" >&2
fi

echo "run dir: $run" >&2
RGITUI_PERF="replay:$scenario" \
RGITUI_PERF_STATE="$run/state" \
RGITUI_PERF_OUT="$run/out" \
RUST_LOG="${RUST_LOG:-info}" \
  "$bin" "$repo" >"$run/app.log" 2>&1 &
pid=$!
echo "$pid" >"$run/pid"

raise() {
  # gpui stops drawing a window macOS reports as occluded (covered, another
  # Space, minimized), and a background launch cannot bring itself forward,
  # so the replay would wait forever for a first frame. Keep it in front.
  osascript -e "tell application \"System Events\" to set frontmost of (first process whose unix id is $pid) to true" \
    >/dev/null 2>"$run/raise.err"
}

status=0
deadline=$((SECONDS + timeout))
next_raise=0
while kill -0 "$pid" 2>/dev/null; do
  if ((SECONDS >= next_raise)); then
    if ! raise && grep -qiE "not allowed|assistive|-1719|-25211" "$run/raise.err"; then
      echo "osascript may not control System Events: grant Accessibility to the app running this shell" >&2
      echo "(System Settings > Privacy & Security > Accessibility), or keep the rgitui window uncovered by hand." >&2
    fi
    next_raise=$((SECONDS + 2))
  fi
  if ((SECONDS > deadline)); then
    echo "timed out after ${timeout}s; killing pid $pid" >&2
    kill "$pid" 2>/dev/null || true
    sleep 1
    kill -9 "$pid" 2>/dev/null || true
    status=124
    break
  fi
  sleep 0.5
done
wait "$pid" 2>/dev/null || true
rm -f "$run/pid"

if ((status == 0)); then
  if grep -q "perf scenario failed" "$run/app.log"; then
    grep "perf scenario failed" "$run/app.log" >&2
    status=1
  elif ! grep -q "scenario \"$name\" complete" "$run/app.log"; then
    echo "app exited before the scenario completed; see $run/app.log" >&2
    status=1
  fi
fi

report=$(find "$run/out" -mindepth 1 -maxdepth 1 -type d | head -1)
[[ -n "$report" ]] && ln -sfn "${report#$run/}" "$run/evidence"
shots=$(find "$run/out" -name '*.png' 2>/dev/null | sort)
[[ -n "$shots" ]] && printf 'screenshot: %s\n' $shots >&2
echo "$run"
exit "$status"
