# Commit staged changes

The commit panel, at the bottom of the right-hand Changes tab, turns staged changes into a commit using the Summary (and optional Description) the user typed.

## Sub-features

- `commit-summary`: Summary plus Commit creates a commit with that message.
- `commit-description`: Description text becomes the message body.
- `commit-amend`: the Amend checkbox rewrites HEAD instead of adding a commit.
- `commit-button`: the commit button at the bottom right is labelled with the commit state (`No Selected Changes`, `No Message`, or ready to commit).

## How to get to it (user POV)

- Stage changes (`cmd-s` stages all), click the **Summary** field, type, then press `cmd-enter` (`rgitui::Commit`).
- Same, but click the commit button at the bottom right of the panel instead of pressing `cmd-enter`.
- The command palette (`cmd-shift-p`) has "Commit".

## Driving it with run.sh

Preconditions: the `fixtures/basic.sh` repo, with modified `src/main.rs` and untracked `notes.txt`. `doctor.sh` prints `ready`.

- **Stage.** `{"action": {"action": "rgitui::StageAll", "settle": "frame"}}` then `wait_for operation_idle`. The panel header reads `2 files selected` and the status bar reads `2 staged`.
- **Focus Summary.** No command focuses it. Use `{"click": {"x": 1069, "y": 556}}`, which is valid at `resize` 1280×800. The field gets a purple focus ring and a caret.
- **Type.** `{"type": {"text": "Verify commit"}}`. The text appears in Summary and the button changes from `No Message` to the commit label.
- **Commit.** `{"action": {"action": "rgitui::Commit", "settle": "frame"}}` then `wait_for operation_idle`.
- **Proof.** `scenarios/commit.json` does all of the above. Then:
  - `git -C <run>/repo log -1 --format=%s` → `Verify commit`
  - `git -C <run>/repo show --stat --format= HEAD` → `notes.txt` and `src/main.rs`
  - `git -C <run>/repo status --short` → empty
  - The `committed` screenshot shows `Created commit <sha>` in the status bar and `Working tree clean`.

## Gotchas

- The panel opens with **Description** focused, not Summary. Typing without the click puts the text in the body, and Commit then refuses for lack of a summary. That's what happened in the first attempt at this journey.
- `rgitui::Commit` with an empty Summary does nothing visible beyond the button label. Assert on git, not on the absence of an error.
- The Summary coordinate holds only at 1280×800. Retake it with `to-logical.sh` if the panel layout changes.
- "Add an API key" (AI message) calls an external provider. Leave it out of verification unless the user asks.
