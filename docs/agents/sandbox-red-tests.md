# Tests a worker's sandbox cannot run

A worker's sandbox binds no TCP port by design (`a_worker_binds_no_port` in `crates/runner/src/runner/core_tests/profile.rs`), and on macOS binds Unix sockets only in its own scratch folder, the one its `TMPDIR` names (#956). So some tests fail inside it and pass in CI. A red run of these in a worker is expected. It is not a bug in your change.

- Run what can run: `cargo test --lib <module path>` on the modules you changed.
- Name in the pull request body the tests only CI ran.
- Do not skip, `#[ignore]` or loosen a test to turn a sandbox run green.
- Make a test's temp folder with `crate::test::short_tempdir()`, never `tempdir_in("/tmp")`. In a crate under `crates/` it is `shep_kelpie_test_support::short_tempdir()`. It sits under `TMPDIR`, which a worker can write, and stays short enough for a socket. `scripts/no-tmp-tempdir.sh` fails CI on a new `tempdir_in("/tmp")` in `src/` or `crates/*/src/`.

## What fails, and why

The doctor, flock, dog (`left_over`), `lease::cli`, sheep, shepherd and upgrade tests run in a worker: they dial `FakeShepherd`, which answers in memory through the `Dial` port, and `lease::cli::e2e` reads the dog's answers off a connected pair of streams. What is listed below binds on purpose or for a reason of its own.

Measured 2026-10-21 on macOS 26.2 (#1085): `cargo test --lib` and `cargo test --test upgrade` under srt with a worker's policy (its worktree, target and scratch folder to write, Unix sockets in the scratch folder, the keychain blocked) and `TMPDIR` set to a 60-byte scratch folder, `~/.shep/kelpie/shep-kelpie/worker/settings-<n>.tmp`. 5148 tests pass, 193 fail and 28 are ignored, and `tests/upgrade.rs` passes. Unix socket binds under `TMPDIR` now pass, the shep-client fake shepherd's included. The overlong socket path in the table below still fails. A module's name covers its child modules. A listed module that lives in a crate under `crates/` (`shepherd::connection` in `crates/core`, `adapters::*` and `github` in `crates/adapters`, `dog::*` in `crates/dog`, `runner::*` in `crates/runner`) runs under that crate's `cargo test -p <package> --lib`, and a bare `cargo test --lib` covers it too.

An entry whose parenthesis begins `some:` lists only part of the module: kelpie cannot tell which of its tests a worker cannot run. An entry with no such parenthesis, or one whose parenthesis says all, covers the module whole, and an entry listed whole in any row is whole. `kelpie_ci_failures_total` reads this table: only a whole entry makes a failing test in it `sandbox-only`, and a failing test in a `some:` entry counts as one the worker could have run. `SANDBOX_RED` in `crates/runner/src/runner/ci_failure/mod.rs` holds the whole entries, and `the_doc_and_the_list_agree` fails when the two part ways.

| Reason | Modules |
|---|---|
| Loopback TCP bind refused | `agent_verbs::tests` (some: 3 tests, the stand-in gateway `agents init --detect` asks for its models), `adapters::dev_server` (all of its tests, which find a free port), `adapters::local::endpoint`, `adapters::forwarder` (some: 23 tests, the stand-in model server in `crates/core/src/test/endpoint.rs` or one of the tests' own), `adapters::gpu`, `adapters::paddock`, `adapters::classifier` (some: 6 tests, the stand-in gateway its tests ask on the classifier's route), `adapters::curl`, `adapters::pi::gateway_tests`, `adapters::pi::attach_tests` (some: the forwarder test's stand-in model server), `adapters::goose::gateway_tests`, `adapters::goose::attach_tests` (some: the forwarder test's stand-in model server), `adapters::openai_gateway`, `adapters::bridge` (some: 8 of 12 tests: those that open the browser's proxy, which binds a loopback TCP port), `adapters::bridge::proxy::tests` (some: all but the address check, the dev-origin tests included), `adapters::srt::live` (some: 2 tests, whose inner runtime's proxy cannot listen on loopback inside the worker's), `adapters::goose::tests` (some: the stand-in paddock), `adapters::pi::adopt_tests` (some: the one test that starts a stand-in gateway), `adapters::goose::adopt_tests` (some: the one test that starts a stand-in gateway), `sheep::handover::tests::pi`, `sheep::handover::tests::goose`, `doctor::gateways`, `runner::gpu_tests`, `runner::limits_tests` (some: the paddock holder status test), `runner::pm::tests::lease` (some: 1 test, the lease wait test's stand-in model server), `runner::review::local`, `github` (some: 2 tests) |
| `ps` cannot say whether a process runs | `runner::attach`, `runner::flight::carry` (some: the 6 tests that attach a pi, goose or Codex session), `runner::pm::tests::attach`, `runner::slots_tests`, `runner::turn::tests::held` (some: the 2 tests that attach) |
| Process groups: a group check or signal the sandbox does not allow | `adapters::process` (some: 4 tests, the fourth the group an unserved handover ends), `adapters::dev_server` (some: the tests that start a server, whose group the guard stops), `adapters::claude::calls` (some: 1 test, the stop) |
| A socket path past 104 bytes once a worker's long `TMPDIR` is in front of it | `runner::code_graph` (some: 1 test, whose path that fits outside a worker does not inside one) |
| The temp folder is under `~/.shep`, which these tests' fences refuse to read | `issues::tests` (some: 2 tests), `runner::pm::tests::code` (some: 1) |

## The tests behind `short_tempdir`

Of the sites that used `tempdir_in("/tmp")`:

- Pass in a worker: `adapters::claude::sandbox`, `adapters::codex` and `adapters::pi::tests`.
- Pass by skipping: the `adapters::srt::live` tests on a machine without macOS, kelpie's tools or a keychain (with them, two fail in a worker, as the table says), and `adapters::mcp::live`, which needs macOS, kelpie's tools with the code graph and a short `TMPDIR`. Each says why on stderr.
- Fail in a worker for a reason above: the 23 `adapters::forwarder::tests` with a stand-in model server on loopback.
- `#[ignore]`d, and run by the `sandbox` CI job: `adapters::claude::escapes`, `adapters::claude::ecosystems` and `adapters::pi::ecosystems`. The last two copy a fixture of `fixtures/ecosystems` (Rust, Go with `golangci-lint`, bun, npm, and a hook script) into a worktree, fetch its dependencies outside the fence, then build, test, lint and `git push` inside it. They need `cargo`, `go`, `golangci-lint`, `bun` and `node` on `PATH` and the network for the fetch, so a worker's sandbox cannot run them. The job runs them on Linux only. A macOS run is by hand, after `cargo build`, with `cargo test --lib -- --ignored ecosystems`.

## Tests Windows cannot run yet

The Windows CI job runs every crate's lib tests in the non-gating `lib` step, after `door`, `totp` and the gating `save` step. Not a sandbox list: `kelpie_ci_failures_total` and `SANDBOX_RED` do not read it.

Measured on [run 41044428502](https://github.com/shep-pm/shep-kelpie/actions/runs/41044428502/job/124024950902) (#1575), the first in which every crate reached `test result`. The runner took 7383 seconds of the step's 180 minutes (it started at 11:12 and ended at 13:15 UTC). Only the root package's `sheep::handover` tests and the runner's slow ones ran past 60 seconds:

| Crate | Passed | Failed | Ignored |
|---|---|---|---|
| `shep-kelpie` (root) | 546 | 104 | 4 |
| `shep-kelpie-adapters` | 789 | 132 | 21 |
| `shep-kelpie-client` | 11 | 0 | 0 |
| `shep-kelpie-core` | 1412 | 50 | 5 |
| `shep-kelpie-disk` | 4 | 0 | 0 |
| `shep-kelpie-dog` | 170 | 5 | 2 |
| `shep-kelpie-facts` | 77 | 0 | 0 |
| `shep-kelpie-owner-only` | 7 | 0 | 0 |
| `shep-kelpie-process` | 63 | 51 | 0 |
| `shep-kelpie-protocol` | 16 | 0 | 0 |
| `shep-kelpie-runner` | 2877 | 156 | 2 |
| `shep-kelpie-test-support` | 7 | 0 | 3 |
| `shep-kelpie-tree` | 7 | 0 | 0 |
| `shep-kelpie-trim` | 3 | 0 | 0 |
| `shep-kelpie-worker-folder` | 11 | 0 | 0 |

The runner's 3035 tests run at 0.4 a second, and 32 test threads ran no faster than the default. [Windows runner test time](../facts/windows-tests.md) records the timing probes and what reducing that time needs. The most failing modules are `upgrade::tests` (74), `worktree::remote` (32), `adapters::goose` (31), `adapters::codex` (28), `sheep::handover` (22), `runner::points_tests` (21), `runner::core_tests` (20), `runner::attach` (17) and `adapters::local` (17).

The `lib` step skips these tests, which hang on Windows:

- `upgrade::tests::handover::a_runner_stopped_while_it_hands_over_stays_stopped` (run 40990662373)
- `upgrade::tests::sheep::a_runner_stopped_while_the_upgrade_waits_stays_stopped` (run 40990662373)
- `runner::ci_help::tests::the_red_prompt_names_the_e2e_command_only_when_configured` (run 41020519313)
- `runner::claude_files::tests::a_trigger_is_answered_while_the_changed_files_are_read` (run 41020519313)
- `runner::closed::tests::a_trigger_is_answered_while_the_worktree_is_checked_for_work` (run 41020519313)
- `runner::contributions::pass_tests::a_pass_past_its_ceiling_ends_unread_and_counts_what_it_spent` (run 41020519313)

23 tests ran past 60 seconds and then ended, so they are slow and not skipped:

- 5 `sheep::handover::tests` (`codex`, `goose` and three `pi`): a turn whose record is gone or invalid.
- 18 in the runner: `flight::tests::reads`, five in `gate::tests`, `merge::tests`, two in `pm::tests`, three in `replies::tests`, `review_bot::thread_replies`, three in `review::`, two in `turn::tests`. Most are the "a trigger is answered while ..." tests, which wait on a read the run never reached.

The failures by cause, counted from the panic messages (a failing test can print more than one):

| Cause | Panic message | Panics |
|---|---|---|
| A Windows path in TOML | `Error { message: "too few unicode value digits, expected unicode hexadecimal value" ... }`: a `C:\Users\...` path written into a TOML string unescaped | 56 |
| "Access is denied" flushing a file | `cannot flush ...\bin\.shep-kelpie.staged.<pid>: Access is denied. (os error 5)` in the `upgrade` tests | about 62 |
| A `\\?\` path handed to git | `git init --bare`, `worktree add` and `fetch`: "cannot mkdir ... Invalid argument" or "could not create leading directories of '//?/C:/...'" | about 60 |
| A stand-in started as an exe | `%1 is not a valid Win32 application. (os error 193)`: a script with no `.exe`, `.cmd` or `.bat` name | about 40 |
| A stand-in started through `/usr/bin/env` | `/usr/bin/env: '\\?C:Users...pi': No such file or directory` (Pi, Goose, Codex): the path has lost its separators | about 20 |
| A process the test looks at with `ps` | `no process <pid> is running`, `NotRunning(<pid>)`, `never saw the turn's child` | about 20 |
| A file not found | `Os { code: 2, kind: NotFound }` | 11 |
| A run that never reached its read | `the run never reached the read`, `the turn never reached the fetch that prepares its worktree` | about 15 |
| A platform without the feature | `handing over is not supported on this platform`, `the images agree on the state: Unsupported` | 6 |
| Smaller ones | `Unreachable(23)`, `EOF while parsing a value`, an absolute-path check on `/srv/shep`, a `curl: (23)` write error, a hook run through `sh`, a missing `sh` or `env`, and others | the rest |

Windows runs a file only if its name ends in `.cmd`, `.bat` or `.exe`, so a `#!/bin/sh` script a test starts itself cannot run there. A test that makes its stand-in with `write_fake` runs on both. `write_script` writes a script Windows runs only through Git for Windows' `sh.exe`, which `spawn::sh` finds (a hook, `sh -c`). A test that needs a FIFO is `#[cfg_attr(windows, ignore = "a FIFO is a Unix file")]`. A test that needs a symlink or a mode bit is `#[cfg(unix)]`, so it is not built there. Fixing a group is its own issue.

## Keeping the list true

A change that moves a test into or out of the sandbox table updates it in the same pull request. A new test that binds a socket or a port belongs in the sandbox table ("What fails, and why") when it lands.

The sandbox table alone determines what `kelpie_ci_failures_total` counts as a sandbox failure: `SANDBOX_RED` in `crates/runner/src/runner/ci_failure/mod.rs` holds that table's whole entries, and `the_doc_and_the_list_agree` fails when the two part ways. A whole entry added to that table and not to the list, or the other way round, is a failing test, not a note. A `some:` entry belongs in that table and not in the list: it says the module is red in part, which is not enough to call a failing test in it one a worker could not run. The Windows section does not affect this classification.

The list is of this crate's modules, so kelpie uses it only on this repo. On a project kelpie holds no list of, every failing test of a red run counts as one its worker could have run, and `sandbox-only` is never used there. Giving another project a list of its own is unbuilt.
