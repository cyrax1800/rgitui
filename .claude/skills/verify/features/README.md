# rgitui feature map

One file per user-facing feature. Each says how a user reaches the feature, how to drive it with `scripts/run.sh`, and which observable end state proves it works. A proof that drives one entry point is incomplete when the file lists others.

| Feature | File | Scenario | Status |
| --- | --- | --- | --- |
| Commit staged changes | [commit.md](commit.md) | `scenarios/commit.json` | proven 2026-11-20 |
| Create a branch | [branches.md](branches.md) | `scenarios/create-branch.json` | proven 2026-11-20 (create only) |
| Stash changes | [stash.md](stash.md) | `scenarios/stash.json` | proven 2026-11-20 (save only) |
| Stage and unstage | [staging.md](staging.md) | `scenarios/commit.json` (stage all) | stage-all proven; per-file and unstage not yet driven |

"Proven" means the scenario ran green on that date, and git plus screenshots showed the end state below. When the UI changes, rerun the scenario. If a `click` misses, take a new coordinate with `scripts/to-logical.sh` and update the date.
