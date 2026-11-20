#!/usr/bin/env bash
# Answers "can I drive rgitui right now?" without touching the user's own
# settings or repositories. Prints one line per check and exits non-zero on
# the first failure. Builds the perf-capture binary if it is stale.
set -uo pipefail
skill_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
repo_root="$(cd "$skill_dir/../../.." && pwd)"
fail() { echo "FAIL: $*"; exit 1; }

[[ "$(uname)" == Darwin ]] || fail "this skill drives the macOS build; on Linux use scripts/headless-smoke-test.sh"
command -v git >/dev/null || fail "git not on PATH"

leftover=$(find "$repo_root/target/verify-runs" -name pid 2>/dev/null)
for f in $leftover; do
  p=$(cat "$f")
  if kill -0 "$p" 2>/dev/null; then
    fail "a verify run is still live (pid $p, $(dirname "$f")); let it finish or run scripts/cleanup.sh"
  fi
done
echo "ok: no other verify run is live"

if ! (cd "$repo_root" && cargo build -q -p rgitui --features perf-capture 2>/tmp/rgitui-doctor-build.log); then
  tail -30 /tmp/rgitui-doctor-build.log
  fail "cargo build -p rgitui --features perf-capture"
fi
echo "ok: target/debug/rgitui built with perf-capture"

run=$("$skill_dir/scripts/run.sh" "$skill_dir/scenarios/doctor.json" --no-build --timeout 90 2>/tmp/rgitui-doctor.err)
code=$?
if ((code != 0)); then
  cat /tmp/rgitui-doctor.err
  if [[ -n "$run" ]] && grep -q "never rendered" "$run/app.log" 2>/dev/null; then
    fail "the window never drew a frame: it was covered or on another Space. See the Accessibility note above, or leave the screen uncovered and unlocked"
  fi
  fail "doctor scenario exited $code; see ${run:-the stderr above}/app.log"
fi
shot=$(find "$run/out" -name '*-doctor.png' | head -1)
[[ -s "$shot" ]] || fail "scenario passed but no screenshot was written under $run/out"
size=$(sips -g pixelWidth -g pixelHeight "$shot" | awk '/pixel/ {printf "%s ", $2}')
echo "ok: workspace rendered, screenshot $shot ($size device px)"
echo "ready"
