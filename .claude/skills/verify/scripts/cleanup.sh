#!/usr/bin/env bash
# Tears down what verify runs created, keeping the evidence.
#
#   cleanup.sh            kill any still-live app this skill started (by the
#                         PID recorded in its run dir), then delete each run's
#                         repo/ and state/ scratch directories
#   cleanup.sh --all      also delete target/verify-runs entirely (evidence too)
#
# Evidence (out/<run>/screenshots, report.json, app.log) is kept by default.
# Never kills by process name: a user's own rgitui is left alone.
set -euo pipefail
skill_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
runs="$(cd "$skill_dir/../../.." && pwd)/target/verify-runs"
[[ -d "$runs" ]] || { echo "nothing to clean"; exit 0; }

for f in $(find "$runs" -maxdepth 2 -name pid); do
  p=$(cat "$f")
  if kill -0 "$p" 2>/dev/null; then
    echo "killing verify app pid $p ($(dirname "$f"))"
    kill "$p" 2>/dev/null || true
    sleep 1
    kill -9 "$p" 2>/dev/null || true
  fi
  rm -f "$f"
done

if [[ "${1:-}" == "--all" ]]; then
  rm -rf "$runs"
  echo "removed $runs"
  exit 0
fi

for d in "$runs"/*/; do
  rm -rf "$d/repo" "$d/state" "$d/raise.err"
done
echo "scratch removed; evidence kept under $runs/*/out and $runs/*/app.log"
