# Stash

Save the working tree to a stash with an optional message, then pop, apply, drop, or branch from it later.

## Sub-features

- `stash-save`: the Create Stash dialog stashes tracked changes with the typed message, or `WIP on <branch>` when empty.
- `stash-pop`: `cmd-shift-z` applies `stash@{0}` and drops it.
- `stash-apply`, `stash-drop`, `stash-branch`: palette commands, also in the sidebar's Stashes section and the stashes panel (`alt-8`).

## How to get to it (user POV)

- `cmd-z`, or the toolbar's **Stash** button, opens **Create Stash** with the Message field focused. Enter confirms and Esc cancels.
- The toolbar's **Pop** button and `cmd-shift-z` pop. Pop is greyed out when there are no stashes.

## Driving it with run.sh

Preconditions: the `fixtures/basic.sh` repo, with modified `src/main.rs` and untracked `notes.txt`.

- **Open dialog.** `{"key": {"key": "secondary-z", "settle": "frame"}}`.
- **Message.** `{"type": {"text": "verify stash"}}`.
- **Save.** `{"key": {"key": "enter", "settle": "frame"}}` then `wait_for operation_idle`.
- **Proof.** `scenarios/stash.json` does all of the above. Then:
  - `git -C <run>/repo stash list` → `stash@{0}: On main: verify stash`
  - `git -C <run>/repo status --short` → only `?? notes.txt`
  - The `stashed` screenshot shows Stashes at 1, the toolbar's Pop enabled, an `Info: Stash created "verify stash"` toast, and the status bar reading `Saved stash`.
- **Pop (not yet scripted).** Append `{"key": {"key": "secondary-shift-z", "settle": "frame"}}` and `wait_for`. Prove it with an empty `git stash list`, and with `git diff` showing the `hello, verify` line again.

## Gotchas

- `cmd-z` means **stash**, not undo. Undo is the unbound `rgitui::Undo`.
- The dialog has no include-untracked option, so untracked files stay in the tree. Don't assert a clean tree after saving.
