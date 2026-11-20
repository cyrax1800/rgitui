#!/usr/bin/env bash
# Builds the default verification repo at $1: three commits on main, a
# `feature/login` branch one commit ahead, a tag, and a dirty working tree
# (one modified tracked file, one untracked file). Deterministic dates so the
# graph looks the same on every run.
set -euo pipefail
repo="$1"
mkdir -p "$repo"
cd "$repo"

export GIT_AUTHOR_NAME="Verify Bot" GIT_AUTHOR_EMAIL="verify@rgitui.invalid"
export GIT_COMMITTER_NAME="Verify Bot" GIT_COMMITTER_EMAIL="verify@rgitui.invalid"
git init -q -b main
git config user.name "Verify Bot"
git config user.email "verify@rgitui.invalid"
git config commit.gpgsign false

commit() { # commit <date> <message>
  GIT_AUTHOR_DATE="$1" GIT_COMMITTER_DATE="$1" git commit -q -m "$2"
}

printf '# Fixture\n\nA repository for rgitui verification runs.\n' > README.md
commit_all() { git add -A; commit "$@"; }
commit_all "2025-01-01T10:00:00Z" "Initial commit"

mkdir -p src
printf 'fn main() {\n    println!("hello");\n}\n' > src/main.rs
commit_all "2025-01-02T10:00:00Z" "Add main"

printf 'pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n' > src/lib.rs
commit_all "2025-01-03T10:00:00Z" "Add lib"
git tag v0.1.0

git switch -q -c feature/login
printf 'pub fn login() -> bool {\n    true\n}\n' > src/login.rs
commit_all "2025-01-04T10:00:00Z" "Add login"
git switch -q main

# Dirty working tree: one modified tracked file, one untracked file.
printf 'fn main() {\n    println!("hello, verify");\n}\n' > src/main.rs
printf 'scratch\n' > notes.txt
