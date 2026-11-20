# Branches

Create, switch, rename, delete and merge local branches, from the sidebar's Branches section, the toolbar, and the command palette.

## Sub-features

- `branch-create`: the New Branch dialog creates a branch at HEAD and **does not check it out**.
- `branch-switch`: activating a branch row in the sidebar checks it out.
- `branch-rename`, `branch-delete`, `branch-merge`: commands with no default key (`rgitui::RenameBranch`, `DeleteBranch`, `MergeBranch`).
- `branch-previous`: `rgitui::SwitchToPreviousBranch`, like `git switch -`.

## How to get to it (user POV)

- `cmd-b`, or the toolbar's **Branch** button, opens the branch dialog with the name field focused. Type a name and press Enter.
- `alt-1` focuses the sidebar. Arrow to a branch and press Enter to switch (`menu::Confirm`).
- The command palette lists every branch command.

## Driving it with run.sh

Preconditions: the `fixtures/basic.sh` repo, which has branches `main` (HEAD) and `feature/login`.

- **Open dialog.** `{"key": {"key": "secondary-b", "settle": "frame"}}`. The dialog's name input has focus.
- **Name it.** `{"type": {"text": "verify/topic"}}`. An invalid name shows an inline error and Enter won't create anything.
- **Create.** `{"key": {"key": "enter", "settle": "frame"}}` then `wait_for operation_idle`.
- **Proof.** `scenarios/create-branch.json` does all of the above. Then `git -C <run>/repo branch --format='%(HEAD) %(refname:short) %(objectname:short)'` lists `verify/topic` at the same sha as `main`, with `*` still on `main`. The `created` screenshot shows Branches at 3 with a `verify/topic` row, a `Success: Branch 'verify/topic' created` toast, and the status bar reading `Created branch 'verify/topic'`.
- **Switch (not yet scripted).** Send `alt-1`, then `down`/`up` to the row, then `enter`. Prove it with `git -C <run>/repo symbolic-ref --short HEAD`. The fixture's dirty `src/main.rs` doesn't conflict with `feature/login`, so the switch should carry the change across. If a dialog about local changes appears instead, that's the behaviour to verify. Don't change the fixture to avoid it.

## Gotchas

- Create does not switch branches. Asserting `HEAD == verify/topic` would be wrong.
- `/` in a branch name is safe inside the dialog. Outside a text input, `/` is bound to Search.
