#!/usr/bin/env bash
# Converts a point you read off a screenshot into the logical window
# coordinates that `click`, `hover` and `scroll` steps take.
#
#   to-logical.sh <screenshot.png> <x> <y> [<shown-width>]
#
# <shown-width> is the width of the image as it was displayed to you (image
# viewers, including the agent's Read tool, often downscale large PNGs). Omit
# it if you measured on the full-size PNG. The scale factor comes from the
# matching "screenshot ... (scale factor N)" line in the run's app.log, found
# next to the screenshot.
set -euo pipefail
png="$1" x="$2" y="$3"
real_w=$(sips -g pixelWidth "$png" | awk '/pixelWidth/ {print $2}')
shown_w="${4:-$real_w}"
run_dir="$(cd "$(dirname "$png")/../../.." && pwd)"
scale=$(grep -F "$(basename "$png")" "$run_dir/app.log" | sed -n 's/.*scale factor \([0-9.]*\).*/\1/p' | tail -1)
scale="${scale:-2}"
python3 -c "
import sys
x, y, real, shown, scale = map(float, sys.argv[1:])
k = real / shown / scale
print(f'{{\"click\": {{\"x\": {x*k:.0f}, \"y\": {y*k:.0f}}}}}')
" "$x" "$y" "$real_w" "$shown_w" "$scale"
