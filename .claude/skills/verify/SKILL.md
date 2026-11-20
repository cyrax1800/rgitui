---
name: verify
description: Drive the real rgitui desktop app (GPUI, macOS) through scripted user input and capture proof. Each run gets a fresh fixture repo and its own settings, and the evidence is screenshots plus git state. Use after changing any user-facing behaviour (commit panel, staging, branches, stash, dialogs, keybindings, layout) to prove it works the way a user would hit it, or to reproduce a UI bug.
---

# Verify rgitui

rgitui is a desktop app. Nothing here touches the user's own settings, keychain entries, or repositories, and there is no server to keep alive. Each verification is one **run**: build once, then launch the app on a fresh fixture repo with a JSON **scenario**. The app replays the scenario through its real input path, saves screenshots, and quits. You then check the fixture repo with `git`.

The driver is the replay harness in `crates/rgitui_perf` (`docs/PERFORMANCE.md`). Keystrokes go through the real keymap (`Window::dispatch_keystroke`), commands through gpui's action registry, and clicks as synthesized platform mouse events. Nothing goes through test doubles or internal setters. The `perf-capture` cargo feature adds the `screenshot` step, which renders the window's own frame offscreen. That step needs no Screen Recording permission.

All paths below are relative to the repo root.

## Launch

```bash
cargo build -p rgitui --features perf-capture         # once per code change; run.sh does it unless --no-build
.claude/skills/verify/scripts/run.sh <scenario.json> [--fixture <script>] [--repo <path>] [--timeout <secs>] [--no-build]
```

`run.sh` creates `target/verify-runs/<stamp>-<scenario-name>/` containing:

- `repo/`: the fixture repo the app opens. It's built by `fixtures/basic.sh` unless you pass `--fixture` or `--repo`.
- `state/`: this run's settings and caches (`RGITUI_PERF_STATE`), so it can't affect the user's own app.
- `evidence/`: the replay report (`report.json`, `report.md`) and `screenshots/NN-<name>.png`.
- `app.log`: the app's log at `RUST_LOG=info`.

Its last stdout line is the run directory, and screenshot paths go to stderr. Exit codes:

- `0`: every step ran.
- `1`: a step failed, or the app quit early. The reason is in `app.log`; grep `perf scenario failed`.
- `124`: timed out, and the app was killed.
- `2`: bad usage.

Ready means the replay has started. The harness waits until the workspace has rendered before the first step. Begin every scenario with `{"wait_for": {"condition": "operation_idle"}}` so the first refresh finishes, then `resize` before any mouse step.

**The window must stay visible.** gpui stops drawing a window that macOS reports as occluded: covered, on another Space, minimized, or behind a locked screen. The replay then dies with `the workspace never rendered`. `run.sh` brings the app to the front by PID every 2 seconds through `osascript`/System Events, which needs Accessibility permission for the app hosting the shell. Runs take 5 to 15 seconds and pull focus while they run.

There's no long-lived instance, so two runs never share state. Even so, run one at a time: each one fights for the front window.

## Doctor

```bash
.claude/skills/verify/scripts/doctor.sh
```

Read-only with respect to the user's state. It checks, in order:

1. The platform is macOS.
2. No earlier verify run is still live.
3. The `perf-capture` build is current.
4. The `scenarios/doctor.json` run opens the fixture, renders, finishes its first refresh, pins the window to 1280×800, and writes a screenshot.

It ends with `ready`. Run it first, and again whenever a run fails for a reason you don't understand. `never rendered` means the window was occluded or the screen was locked.

## Drive

A scenario is `{"name": ..., "description": ..., "steps": [...]}`. Each step is a single-key object:

| Step | Shape | Use |
| --- | --- | --- |
| `wait_for` | `{"condition": "operation_idle", "timeout_ms": 30000}` | After anything that runs git. Waits until no operation has been in flight for 0.5 s. |
| `action` | `{"action": "rgitui::StageAll", "settle": "frame"}` | Any command by name. Names come from `docs/KEYBINDINGS.md` (generated from `crates/rgitui_workspace/src/keymap/registry.rs`), and an unknown name fails the run before step 1. |
| `key` | `{"key": "secondary-b", "settle": "frame"}` | One keystroke through the keymap (`secondary` = cmd). Use it for `enter`, `escape`, and shortcuts. |
| `type` | `{"text": "Fix login"}` | Text into the focused input, one keystroke per character. Uppercase and punctuation work. |
| `resize` | `{"width": 1280, "height": 800}` | Pins the window in logical px. **Required before `click`/`hover`/`scroll`.** The app maximizes into whatever room the Dock leaves, so coordinates otherwise shift between runs. |
| `click` | `{"x": 1069, "y": 556}` | Left click at logical window coordinates. Add `"button": "right"` to right-click, which opens context menus. |
| `scroll` / `hover` | see `docs/PERFORMANCE.md` | Wheel and pointer sweeps. |
| `screenshot` | `{"screenshot": "after-commit"}` | Saves `evidence/screenshots/NN-after-commit.png` after a fresh frame. |
| `wait` | `{"ms": 500}` | A fixed sleep. Use only when no condition fits. |

Prefer handles in this order: `action` (stable name), then `key` (the shortcut a user presses), then `click`. A click is the only way to reach controls with no command, such as the commit Summary field.

**Getting a click coordinate.** Run the scenario up to a `screenshot`, then read the PNG. Image viewers, the agent's Read tool included, often downscale large PNGs, so convert the point you measured:

```bash
.claude/skills/verify/scripts/to-logical.sh <screenshot.png> <x> <y> <width-it-was-shown-at>
# → {"click": {"x": 1069, "y": 556}}
```

The script reads the real PNG size and the scale factor that `app.log` recorded for that screenshot. At 1280×800 the layout is fixed, so a coordinate found once stays good until someone changes the layout.

Text inputs that open focused (branch dialog, stash dialog) take `type` directly. Enter in a single-line input submits it. The commit Summary field needs a `click` first.

Scenarios for mapped features live in `scenarios/`. The feature map in `features/` says which scenario proves which journey. Write new scenarios there too, and name them for the journey.

## Evidence

A proof has three parts, and all three must agree:

1. **The side effect, read from the fixture repo with git.** Use `git -C <run>/repo log/status/branch/stash list/show --stat`. This is the strongest evidence because the app wrote it through its real git path. Assert exact values (message, file list, ref target), not just that something changed.
2. **Screenshots of the action and the result.** Capture one before the trigger (for example a filled dialog) and one after (for example the status bar reading `Created commit de2ebde`). A final screen alone doesn't prove the user path ran.
3. **A clean run:** `run.sh` exit 0, and no `perf scenario failed` in `app.log`. Check `app.log` for `ERROR` lines from the feature's own module.

Standards:

- Drive the user's path: the shortcut or command they would use, then the dialog, then Enter.
- Never set state through test-only hooks.
- Never edit the fixture repo mid-run to fake a precondition. Put it in a fixture script instead.
- Network features (fetch, pull, push, PRs) talk to a real remote. Give them a local bare repo as `origin` in a fixture script, never a hosted remote.
- AI commit messages call an external provider. Don't drive them unless the user provides a key for the purpose.

Evidence stays in `target/verify-runs/<run>/evidence` and `app.log`, and cleanup keeps it. Cite the paths when reporting.

## Cleanup

```bash
.claude/skills/verify/scripts/cleanup.sh         # kill any live verify app (by its recorded PID), drop repo/ and state/, keep evidence
.claude/skills/verify/scripts/cleanup.sh --all   # also delete target/verify-runs entirely
```

A normal run quits on its own and `run.sh` kills it on timeout, so cleanup matters only after an interrupted run. Never kill rgitui by process name: the user may have their own instance open.

## Helpers

| Script | What it does |
| --- | --- |
| `scripts/run.sh` | Builds, sets up the fixture and isolated state, launches, keeps the window in front, waits, reports. |
| `scripts/doctor.sh` | Readiness check (above). |
| `scripts/to-logical.sh` | Converts a screenshot point into a `click` step. |
| `scripts/cleanup.sh` | Teardown (above). |
| `fixtures/basic.sh <dir>` | The default repo. `main` has 3 commits (tag `v0.1.0` on the third); `feature/login` is one commit ahead; `src/main.rs` is modified and `notes.txt` is untracked. Commit dates are fixed. |
| `fixtures/tracking.sh <dir>` | `basic.sh` plus a local bare `origin`. Before a fetch, `main` is 1 ahead. Another clone has already pushed, so after a fetch `main` reads 1 ahead and 2 behind, and `feature/login` reads 1 behind. Use it for fetch, pull, push, and the ahead/behind counts. |

On Linux there's no capture support in this skill. `scripts/headless-smoke-test.sh` only proves that the app starts under Xvfb.
