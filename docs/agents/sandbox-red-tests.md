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

### Measured on [run 41032543922](https://github.com/shep-pm/shep-kelpie/actions/runs/41032543922/job/123985536432) (#1575)

The step did not finish. No test hung beyond the six skipped, but the runner crate's 3035 tests ran at 0.4 a second, 1941 of them in the step's 90 minutes, and `--test-threads=32` ran no faster than the default (run 41026010104: 725 in 30 minutes). The step now has 180 minutes. The table is every crate that reached `test result`. `shep-kelpie-test-support`, `-tree`, `-trim` and `-worker-folder` come after the runner and did not run:

| Crate | Passed | Failed | Ignored |
|---|---|---|---|
| `shep-kelpie` (root) | 543 | 107 | 4 |
| `shep-kelpie-adapters` | 788 | 133 | 21 |
| `shep-kelpie-client` | 11 | 0 | 0 |
| `shep-kelpie-core` | 1412 | 50 | 5 |
| `shep-kelpie-disk` | 4 | 0 | 0 |
| `shep-kelpie-dog` | 170 | 5 | 2 |
| `shep-kelpie-facts` | 77 | 0 | 0 |
| `shep-kelpie-owner-only` | 7 | 0 | 0 |
| `shep-kelpie-process` | 63 | 51 | 0 |
| `shep-kelpie-protocol` | 16 | 0 | 0 |
| `shep-kelpie-runner` | 1802 passed and 139 failed of 1941 run, no `test result` | | |

The `lib` step skips these tests, which hang on Windows:

- `upgrade::tests::handover::a_runner_stopped_while_it_hands_over_stays_stopped` (run 40990662373)
- `upgrade::tests::sheep::a_runner_stopped_while_the_upgrade_waits_stays_stopped` (run 40990662373)
- `runner::ci_help::tests::the_red_prompt_names_the_e2e_command_only_when_configured` (run 41020519313)
- `runner::claude_files::tests::a_trigger_is_answered_while_the_changed_files_are_read` (run 41020519313)
- `runner::closed::tests::a_trigger_is_answered_while_the_worktree_is_checked_for_work` (run 41020519313)
- `runner::contributions::pass_tests::a_pass_past_its_ceiling_ends_unread_and_counts_what_it_spent` (run 41020519313)

Five `sheep::handover::tests` ran past 60 seconds in the root package on both runs and then ended: `codex::a_codex_turn_whose_record_is_gone_refuses_the_handover_and_the_old_image_keeps_its_call`, the goose twin, and three `pi` tests. They are slow, not skipped.

The failures by cause, counted from the panic messages of run 41026010104 for the crates above. The runner's 139 failures so far are ungrouped, but its panics are mostly the first row (about 40) and the third (about 5):

| Cause | Panic message | Panics |
|---|---|---|
| A `\\?\` path handed to git | `git init --bare` or `worktree add`: "cannot mkdir" or "could not create leading directories of '//?/C:/...'", and `fetch` of a fork | about 60 |
| "Access is denied" flushing a file | `cannot flush ...\bin\.shep-kelpie.staged.<pid>: Access is denied. (os error 5)` in the `upgrade` tests | about 62 |
| A stand-in started as an exe | `%1 is not a valid Win32 application. (os error 193)`: a script with no `.exe`, `.cmd` or `.bat` name | about 30 |
| A stand-in started through `/usr/bin/env` | `/usr/bin/env: '\\?C:Users...pi': No such file or directory` (Pi, Goose, Codex), the `\\?\` path stripped of its separators | about 20 |
| A file not found | `Os { code: 2, kind: NotFound }` | 11 |
| A platform without the feature | `handing over is not supported on this platform`, `the images agree on the state: Unsupported` | 6 |
| Smaller ones | a process the test never saw, `Unreachable(23)`, a TOML escape, an absolute-path check, a `curl: (23)` write error, a hook run through `sh`, and others | the rest |

The counts are replaced by those of a run in which the runner reaches `test result`.

### Reading the source (#1149)

Windows runs a file only if its name ends in `.cmd`, `.bat` or `.exe`, so a `#!/bin/sh` script a test starts itself cannot run there. A test that makes its stand-in with `write_fake` runs on both. `write_script` writes a script Windows runs only through Git for Windows' `sh.exe`, which `spawn::sh` finds (a hook, `sh -c`). A test that needs a FIFO is `#[cfg_attr(windows, ignore = "a FIFO is a Unix file")]`.

| Needs | Modules |
|---|---|
| A shebang script the test starts itself | `adapters::claude` (`tests`, `sandbox`, `calls`), `adapters::codex`, `adapters::goose`, `adapters::pi`, `adapters::local`, `adapters::dev_server`, `adapters::bridge` (some: the stand-in browser server), `adapters::process` (some), `process::tools::install`, `process::terminal` (some), `process::spawn` (some: the environment probe), `core::guard::tests::manager`, `runner::code_graph`, `runner::core_tests::metrics`, `runner::review::local`, `runner::triage::research_tests`, `sheep::handover::tests`, `upgrade::tests`, `issues::tests`, `flock::attach`, `flock::pm` |
| `cat` or `sleep` as a live process, and `ps` or `kill` to look at it (#1148) | `runner::attach`, `runner::flight::carry`, `runner::pm::tests::attach`, `runner::turn::tests::held`, `adapters::process::adopt`, `adapters::dev_server` (some), `tests/dog_smoke.rs` |
| A symlink or a mode bit the test reads or sets (`#[cfg(unix)]`, so it is not built there), | `core::confine` (some), `core::fence` (some), `core::guard` (some), `core::home` (some), `core::local_paths` (some), `core::profile::login` (some), `core::profile::housekeeping`, `core::totp` (some), `core::totp::answers` (some), `adapters::files`, `adapters::disk` (some), `adapters::codex` (some), `adapters::goose` (some), `adapters::claude::sandbox` (some), `adapters::claude::settings` (some), `adapters::dev_server` (some), `github` (some), `upgrade::install::tests` (some: the mode, `ino` and symlink reads, and `a_real_executable_is_staged_and_still_runs`, whose staged name has no `.exe`), `upgrade::tests::files` (some: the `ino` reads and the two symlink tests, through the `linked` rig path), `upgrade::tests::refusals` (some: `a_binary_without_its_executable_bits_is_refused_naming_chmod`), `tests/upgrade.rs` (some: the test of that name), `tests/dog_smoke.rs` (some: `stand_in_runner_with_lambs`) |
| A Unix socket, a signal or a process group (#1147, #1148). Every crate compiles for Windows now (`core` since #1262, `dog` #1274, `adapters` #1298, `runner` #1299, the root package #1392), so these only keep tests from running | `runner` |
| `env` as a command (the live sandbox probes) | `adapters::pi::escapes`, `adapters::goose::escapes` |

## Keeping the list true

A change that moves a test into or out of either list updates the corresponding table in the same pull request. A new test that binds a socket or a port belongs in the sandbox table ("What fails, and why") when it lands.

The sandbox table alone determines what `kelpie_ci_failures_total` counts as a sandbox failure: `SANDBOX_RED` in `crates/runner/src/runner/ci_failure/mod.rs` holds that table's whole entries, and `the_doc_and_the_list_agree` fails when the two part ways. A whole entry added to that table and not to the list, or the other way round, is a failing test, not a note. A `some:` entry belongs in that table and not in the list: it says the module is red in part, which is not enough to call a failing test in it one a worker could not run. The Windows table does not affect this classification.

The list is of this crate's modules, so kelpie uses it only on this repo. On a project kelpie holds no list of, every failing test of a red run counts as one its worker could have run, and `sandbox-only` is never used there. Giving another project a list of its own is unbuilt.
