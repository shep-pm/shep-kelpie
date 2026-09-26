# Implementer calibration

Six small, well-specified changes from shep's history (`cases.jsonl`). Each run starts from the case's parent commit with its tests absent, gives `claude -p` the spec, and then applies the hidden test patch and runs those tests exactly. One sample per cell, so a single flipped cell is noise: compare totals.

## Pass/fail by case

| config | wire-reply-id-shape | channel-non-utf8-frame | outbox-zero-capacity | env-u64-beyond-i64 | graph-dogs-last-with-cycle | paths-user-home |
|---|---|---|---|---|---|---|
| claude-opus-5-5@low | pass | pass | pass | pass | pass | pass |
| claude-opus-5-5@medium | pass | pass | pass | pass | pass | pass |
| claude-sonnet-5@medium | pass | pass | pass | pass | pass | pass |
| claude-sonnet-5@high | pass | pass | pass | pass | pass | pass |
| claude-haiku-4-5-20251001@low | pass | pass | pass | pass | pass | pass |
| claude-fable-5-1@medium | pass | pass | pass | pass | pass | pass |

## Per config

| config | passed | runs | total units | total cost USD | median wall s | median turns | denied tool calls |
|---|---|---|---|---|---|---|---|
| claude-opus-5-5@low | 6/6 | 6 | 644,507 | 2.33 | 33 | 7.0 | 3 |
| claude-opus-5-5@medium | 6/6 | 6 | 702,170 | 2.50 | 34 | 8.0 | 9 |
| claude-sonnet-5@medium | 6/6 | 6 | 634,676 | 1.27 | 27 | 6.5 | 4 |
| claude-sonnet-5@high | 6/6 | 6 | 660,879 | 1.32 | 25 | 8.0 | 3 |
| claude-haiku-4-5-20251001@low | 6/6 | 6 | 693,287 | 0.69 | 40 | 6.0 | 3 |
| claude-fable-5-1@medium | 6/6 | 6 | 854,763 | 6.53 | 37 | 9.5 | 15 |

Units are input x1 + cache creation x2 + cache read x0.1 + output x5, summed over every model in `modelUsage` (the sibling harness's weighting). Wall is the `claude -p` call alone, not the hidden test run.

## Flags (generated)

None: no run errored, touched its test module or another file, needed a fuzzy hidden-patch apply, failed to compile the hidden tests, or broke the crate's suite.

## Notes

Run 2026-09-26, 07:38 to 07:51, `implement.py launch` (three configs at once, 4.5 to 7 minutes per config). Claude Code 2.1.283.

- **Ceiling.** 36 of 36 pass. At pass/fail this set does not separate the six configs, so it cannot say which one should implement. What does separate them is cost and turns. A set that discriminates needs harder cases: more than one file, more than about 60 lines, or a spec that does not name the file.
- **Cost against units.** Dollars (`total_cost_usd`) run from 0.69 (haiku) to about 1.3 (sonnet) to about 2.4 (opus) to 6.53 (fable). Units barely move (635k to 855k) because they carry no per-model price, and every session starts from a fixed floor of the system prompt, the worker repo's CLAUDE.md and the tool definitions. The cheapest run in each config sits at 85k to 109k units.
- **Where configs diverged.** `wire-reply-id-shape` was the only case with a spread: fable took 17 turns and 118 s, haiku 15 turns and 76 s, everything else 6 to 10 turns. Opus (low and medium) and sonnet (medium and high) narrowed `result` to the `Ok`/`Err` shape. That is stricter than the commit's accept-any-value check but still matches the protocol's externally tagged `Result`. Fable did what the commit did (`IgnoredAny`). Haiku used `serde_json::Value`, which is correct but deserializes the whole result, and its `#[allow(dead_code)]` carries no reason.
- **Windows branch.** The macOS run cannot execute `paths-user-home`'s Windows test, so every config's diff was also run through `cargo check -p shep-core --lib --target x86_64-pc-windows-gnu`. All six compile with no warnings (`results/paths-user-home.windows-check.json`). By reading, every Windows branch has the right order and treats empty as unset. Sonnet@medium wrote two `cfg`'d copies of the function and haiku a nested `if let` chain. The rest took the commit's shape.
- **Denied tool calls (37, none fatal).** 13 were `cargo test`, denied by design since the tests are hidden. 10 were `cargo check` with a pipe or `echo $?` attached, which `Bash(cargo check*)` does not cover. 7 were `cargo fmt`, clippy or a Windows cross-check, all from fable and opus, following the worker repo's CLAUDE.md gate. 6 were python or sed edit scripts. 1 was a `cd <worktree> && ...`. Fable asked for the most (15).
- **Skill loads.** 12 of 36 runs loaded `shep-idiomatic-rust`, which the worker repo's CLAUDE.md makes a hard trigger: sonnet@high 6 of 6, fable 3, sonnet@medium 2, opus@medium 1, haiku and opus@low never. Those runs pay for the extra context.
- **Nothing odd in the edits.** No run touched its file's test module, added a `#[test]`, or changed another file, and none broke the crate's existing lib suite (run after the hidden tests, unscored). Every hidden patch applied with plain `git apply`. There were no refusals, errors, timeouts or usage-limit failures, though the sibling harness was running at the same time. No transcript shows a read outside the run's worktree.
- **Cargo.lock.** Cargo rewrote `Cargo.lock` in all six `outbox-zero-capacity` runs, and in no other case. That parent's lock trails a version bump (`shep-channel` 0.2.2 in the lock against 0.4.3 in the manifest). It is recorded as `cargo_lock_changed` and not counted as the model touching another file.
- **`--effort`.** Every model accepted it, `claude-haiku-4-5-20251001` included, so nothing was dropped.
- **Harness caveats.** The haiku `outbox-zero-capacity` cell was the smoke test. It ran before two harness edits: Cargo.lock excluded from other-files, and the guard that keeps a failed-before-start call from being saved as a result. Its record was brought up to the new fields and the run is otherwise identical. `paths-user-home` goes red at the parent by compile error (`user_home` does not exist), which a new-API case cannot avoid. Its hidden patch adds `use std::ffi::OsString;` to the test module, so a correct signature spelled in full still compiles. There is one sample per cell.
