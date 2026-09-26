# Implementation test series

One set of harder cases answers two questions left open by the first series:

- **Model defaults for the implementer role.** The first implementer set passed 36 of 36, so it could not separate the configs. These cases are harder.
- **The work split for implementation.** The first work-split grid measured reading tasks only. Kelpie's workers mostly implement.

## Cases

- Six shep commits that change 2 to 5 source files and 60 to 300 source lines, with tests in the commit that fail at the parent with only the tests applied, and pass at the commit. Prefer shep-core, shep-channel and shep-client; shep-daemon and shep-cli are allowed if their builds fit.
- Each spec describes behaviour in 5 to 12 sentences. It names the crates but not the files or functions to change, except the name of a new public item the tests call.
- Tests stay hidden until scoring: inline test hunks and any `tests/` files.
- Validation works as in the first implementer set (the experiments repo's `calibration/implementer/make_cases.py`), with the evidence kept per case.

## Runs

Every run starts `claude -p` in a worktree at the parent with `--permission-mode acceptEdits`, allows only `cargo check`, `cargo build`, `cargo fmt`, `git diff` and `git status` through Bash, and uses its own `CARGO_TARGET_DIR`. Every run uses the same worker settings profile, `--settings '{"disableAllHooks": true}'`, so no strategy runs under different hooks.

Each run records: hidden tests passed, files touched, whether a test file was touched (a violation), units from `modelUsage`, `total_cost_usd`, wall seconds and turns.

**Calibration grid:** inline, one run per case, on Opus 5.5 low, Sonnet 5 medium, Haiku 4.5 low and Fable 5.1 medium. 24 runs.

**Work-split grid:** Sonnet 5 medium, two runs per case per strategy. 48 runs, with the calibration grid's Sonnet run counting as inline's first.

- **inline:** one session implements.
- **phased:** session A writes a plan (files, steps, risks) as its final message without editing. Session B implements from that plan.
- **crew:** one lead session plans, dispatches Sonnet subagents with the Agent tool (one per group of files), then integrates and runs `cargo check`.
- **inline_cbm:** inline, with codebase-memory-mcp loaded against an index of the parent.

## How the results decide

Proposed, for the maintainer to confirm:

- The implementer default is the cheapest config, in dollars, whose pass count is within one case of the best.
- The work split for implementation is the cheapest strategy per passed case, unless another strategy passes at least two more cases.
