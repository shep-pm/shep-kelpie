# Budget, time and usage

Where an item's time and a call's usage are kept. Pacing and settings are in `budget.md`, the dog and its leases in `budget-forge.md`.

## Where the time goes

- Every second of an item lands in one of six phases (`worker`, `review`, `ci`, `ruling`, `merge`, `other`), totals add up. #141, #331
- Charged at each save to the prior phase; stopped time = `other` (decided 2026-10-01); saves about every 30s. GPU wait is `review`. #141, `crates/runner/src/runner/mod.rs`
- `history` keeps last 100 finished items for `timings [n]`. #141

## Where the usage goes

- Every model call is a line in `<kelpie home>/<project>/usage.jsonl`, append-only. `crates/core/src/usage/mod.rs`, #347
- Each step is a JSON object in `<kelpie home>/<project>/steps.jsonl` (`at` first), append-only, which `watch` and `usage --import` read; the out log gets one plain line per event, never JSON. `runner/step_log.rs`, #491
- Units = `input + 2 x hour cache writes + 1.25 x five-minute writes + 0.1 x cache reads + 5 x output` (control room weights). #347
- Call never reaching a model gets no line; call cut by stop written from launch draft; finished item adds `finished` line. #347
- Unwritable ledger fails no call or step. `shep kelpie usage` compares units per merged PR against the maintainer's baselines. #347
