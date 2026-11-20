# Stage and unstage

Move changes between the working tree and the index: all at once, per file from the Changes list, or per hunk and line from the diff viewer.

## Sub-features

- `stage-all` / `unstage-all`: `cmd-s` (`rgitui::StageAll`), and `cmd-shift-s` or `cmd-u` (`rgitui::UnstageAll`).
- `stage-file`: the checkbox on a row in the Changes tab, or `s` on the selected sidebar row (`sidebar::ToggleStageRow`).
- `stage-hunk` / `stage-lines`: in the diff viewer, `alt-s` stages the hunk under the cursor, `p` turns on line selection, and `s` stages the selection.
- `discard`: `x` on a selected row (`sidebar::DiscardRow`), or `rgitui::DiscardAll`. This is destructive, but only to the fixture.

## How to get to it (user POV)

- The **Select all N changed files** checkbox at the top of the Changes tab, or `cmd-s`.
- The per-file checkboxes in the Changes tab.
- Click a file to open its diff, then use the diff-viewer keys above (after `alt-4`, which focuses the diff viewer).

## Driving it with run.sh

Preconditions: the `fixtures/basic.sh` repo, with modified `src/main.rs` and untracked `notes.txt`.

- **Stage all.** `{"action": {"action": "rgitui::StageAll", "settle": "frame"}}` then `wait_for operation_idle`. Proof: `git -C <run>/repo status --short` → `A  notes.txt` and `M  src/main.rs`, and the status bar reads `2 staged`. This journey is proven as the first half of `scenarios/commit.json`.
- **Unstage all.** `{"action": {"action": "rgitui::UnstageAll", "settle": "frame"}}` then `wait_for`. Proof: status back to ` M src/main.rs` and `?? notes.txt`.
- **Per file (not yet scripted).** At 1280×800, take the row checkbox coordinates from a screenshot with `to-logical.sh` and `click` one. Prove it with `git status --short`, where only that file moves to the index column.
- **Per hunk (not yet scripted).** Click `main.rs` in the Changes list, send `alt-4`, then `alt-s`. Prove it with `git diff --cached`.

## Gotchas

- The perf scenario `crates/rgitui_perf/scenarios/staging-churn.json` exercises the same commands for performance. It proves nothing about correctness, so use git status as the proof.
- After a stage, the watcher refresh is debounced by 200 ms. Always put `wait_for operation_idle` before a screenshot.
