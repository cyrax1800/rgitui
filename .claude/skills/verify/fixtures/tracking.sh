#!/usr/bin/env bash
# Builds the basic repo at $1 with a local bare `origin` beside it, then has a
# second clone push to that origin after the fixture repo last fetched:
#
#   main           1 local commit not pushed; origin has 2 the repo has not
#                  fetched yet. Reads "1 ahead" until a fetch, then diverged
#                  (1 ahead, 2 behind).
#   feature/login  level until a fetch, then 1 behind.
#
# So the counts the app shows before and after a fetch differ, which is what
# proves that a fetch (manual or automatic) ran and the sidebar picked it up.
set -euo pipefail
repo="$1"
here="$(cd "$(dirname "$0")" && pwd)"
bash "$here/basic.sh" "$repo"

base="$(cd "$repo/.." && pwd)"
origin="$base/origin.git"
peer="$base/peer"

export GIT_AUTHOR_NAME="Verify Bot" GIT_AUTHOR_EMAIL="verify@rgitui.invalid"
export GIT_COMMITTER_NAME="Verify Bot" GIT_COMMITTER_EMAIL="verify@rgitui.invalid"

git init -q --bare -b main "$origin"
git -C "$repo" remote add origin "$origin"
git -C "$repo" push -q -u origin main feature/login

# A local commit on main that origin does not have.
git -C "$repo" stash -q -u
printf 'local\n' > "$repo/LOCAL.md"
git -C "$repo" add LOCAL.md
GIT_AUTHOR_DATE="2025-01-05T10:00:00Z" GIT_COMMITTER_DATE="2025-01-05T10:00:00Z" \
  git -C "$repo" commit -q -m "Local only"
git -C "$repo" stash pop -q

# Someone else pushes after the repo's last fetch.
git clone -q "$origin" "$peer"
git -C "$peer" config commit.gpgsign false
for n in 1 2; do
  printf 'remote %s\n' "$n" > "$peer/REMOTE$n.md"
  git -C "$peer" add "REMOTE$n.md"
  GIT_AUTHOR_DATE="2025-01-06T1$n:00:00Z" GIT_COMMITTER_DATE="2025-01-06T1$n:00:00Z" \
    git -C "$peer" commit -q -m "Remote $n"
done
git -C "$peer" push -q origin main
git -C "$peer" switch -q feature/login
printf 'remote login\n' > "$peer/LOGIN.md"
git -C "$peer" add LOGIN.md
GIT_AUTHOR_DATE="2025-01-07T10:00:00Z" GIT_COMMITTER_DATE="2025-01-07T10:00:00Z" \
  git -C "$peer" commit -q -m "Remote login"
git -C "$peer" push -q origin feature/login
