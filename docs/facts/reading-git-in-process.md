# Reading git in process

- Measured 2026-10-31 on macOS 26.2 with git 2.50.1 and gix 0.90.0 (`default-features = false`, features `revision` and `sha1`), for #1594. The runs set `GIT_TRACE2_EVENT` to a folder and counted `cmd_name` events by name.
- `cargo test --lib -p shep-kelpie-runner -- runner::merge runner::gate runner::dependencies` (231 tests) started 5550 `rev-parse` and 2722 `merge-base` processes before the in-process reads, and 1279 and 556 after. The full `runner::` run did not finish inside the 10 minute call limit, so it was not counted.
- `cargo test --lib -p shep-kelpie-process -- worktree::` started 276 `rev-parse` and 18 `merge-base` before, and 285 and 20 after. The five tests added to that run start about 9 `rev-parse` and 6 `merge-base` themselves.
- gix adds 72 packages to `Cargo.lock`. A clean `cargo build --locked` of the workspace took 28.7 s and 50.6 s before (two runs under different load) and 57.2 s and 57.9 s after, with user CPU of 107 s and 113 s before and 130 s and 132 s after.
- gix needs a hash feature: with none, `gix-hash` does not compile. Without `sha256` it cannot open a sha256 repo, so a read there falls back to git.
