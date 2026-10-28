# Tests a worker's sandbox cannot run

A worker's sandbox binds no TCP port by design (`a_worker_binds_no_port` in `crates/runner/src/runner/core_tests/profile.rs`), and on macOS binds Unix sockets only in its own scratch folder, the one its `TMPDIR` names (#956). So some tests fail inside it and pass in CI. A red run of these in a worker is expected. It is not a bug in your change.

- Run what can run: `cargo test --lib <module path>` on the modules you changed.
- Name in the pull request body the tests only CI ran.
- Do not skip, `#[ignore]` or loosen a test to turn a sandbox run green.
- Make a test's temp folder with `crate::test::short_tempdir()`, never `tempdir_in("/tmp")`. In a crate under `crates/` it is `shep_kelpie_test_support::short_tempdir()`. It sits under `TMPDIR`, which a worker can write, and stays short enough for a socket. `scripts/no-tmp-tempdir.sh` fails CI on a new `tempdir_in("/tmp")` in `src/` or `crates/*/src/`.

## What fails, and why

The doctor, flock, dog (`left_over`), `lease::cli`, sheep, shepherd and upgrade tests run in a worker: they dial `FakeShepherd`, which answers in memory through the `Dial` port, and `lease::cli::e2e` reads the dog's answers off a connected pair of streams. What is listed below binds on purpose or for a reason of its own.

Measured 2026-10-21 on macOS 26.2 (#1085): `cargo test --lib` and `cargo test --test upgrade` under srt with a worker's policy (its worktree, target and scratch folder to write, Unix sockets in the scratch folder, the keychain blocked) and `TMPDIR` set to a 60-byte scratch folder, `~/.shep/kelpie/shep-kelpie/worker/settings-<n>.tmp`. 5148 tests pass, 193 fail and 28 are ignored, and `tests/upgrade.rs` passes. Every test that bound a Unix socket under `TMPDIR` now passes, the shep-client fake shepherd's included, so no row for a Unix socket is left. A module's name covers its child modules. A listed module that lives in a crate under `crates/` (`shepherd::connection` in `crates/core`, `adapters::*` and `github` in `crates/adapters`, `dog::*` in `crates/dog`, `runner::*` in `crates/runner`) runs under that crate's `cargo test -p <package> --lib`, and a bare `cargo test --lib` covers it too.

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

Not a sandbox list: `kelpie_ci_failures_total` and `SANDBOX_RED` do not read it. The Windows CI job runs `cargo check` and the gating `save` step (`lease::saved::`, `state::tests::release::` and one atomic-write test) only, so this is read from the source, not measured (#1149). Windows runs a file only if its name ends in `.cmd`, `.bat` or `.exe`, so a `#!/bin/sh` script a test starts itself cannot run there. A test that makes its stand-in with `write_fake` runs on both. `write_script` writes a script Windows runs only through Git for Windows' `sh.exe`, which `spawn::sh` finds (a hook, `sh -c`). A test that needs a FIFO is `#[cfg_attr(windows, ignore = "a FIFO is a Unix file")]`.

| Needs | Modules |
|---|---|
| A shebang script the test starts itself | `adapters::claude` (`tests`, `sandbox`, `calls`), `adapters::codex`, `adapters::goose`, `adapters::pi`, `adapters::local`, `adapters::dev_server`, `adapters::bridge` (some: the stand-in browser server), `adapters::process` (some), `process::tools::install`, `process::terminal` (some), `process::spawn` (some: the environment probe), `core::guard::tests::manager`, `runner::code_graph`, `runner::core_tests::metrics`, `runner::review::local`, `runner::triage::research_tests`, `sheep::handover::tests`, `upgrade::tests`, `issues::tests`, `flock::attach`, `flock::pm` |
| `cat` or `sleep` as a live process, and `ps` or `kill` to look at it (#1148) | `runner::attach`, `runner::flight::carry`, `runner::pm::tests::attach`, `runner::turn::tests::held`, `adapters::process::adopt`, `adapters::dev_server` (some), `tests/dog_smoke.rs` |
| A symlink or a mode bit the test reads or sets (`#[cfg(unix)]`, so it is not built there), | `core::confine` (some), `core::fence` (some), `core::guard` (some), `core::home` (some), `core::local_paths` (some), `core::profile::login` (some), `core::profile::housekeeping`, `core::totp` (some), `core::totp::answers` (some), `adapters::files`, `adapters::disk` (some), `adapters::codex` (some), `adapters::goose` (some), `adapters::claude::sandbox` (some), `adapters::claude::settings` (some), `adapters::dev_server` (some), `github` (some), `upgrade::install::tests` (some: the mode, `ino` and symlink reads, and `a_real_executable_is_staged_and_still_runs`, whose staged name has no `.exe`), `upgrade::tests::files` (some: the `ino` reads and the two symlink tests, through the `linked` rig path), `upgrade::tests::refusals` (some: `a_binary_without_its_executable_bits_is_refused_naming_chmod`), `tests/upgrade.rs` (some: the test of that name), `tests/dog_smoke.rs` (some: `stand_in_runner_with_lambs`) |
| A Unix socket, a signal or a process group (#1147, #1148). Every crate compiles for Windows now (`core` since #1262, `dog` #1274, `adapters` #1298, `runner` #1299, the root package #1392), so these only keep tests from running | `runner` |
| `env` as a command (the live sandbox probes) | `adapters::pi::escapes`, `adapters::goose::escapes` |

## Keeping the list true

A change that moves a test into or out of this list updates the table in the same pull request. A new test that binds a socket or a port belongs in the table when it lands.

The table is also what `kelpie_ci_failures_total` counts a red run on: `SANDBOX_RED` in `crates/runner/src/runner/ci_failure/mod.rs` holds the table's whole entries, and `the_doc_and_the_list_agree` fails when the two part ways. A whole entry added to the table and not to the list, or the other way round, is a failing test, not a note. A `some:` entry belongs in the table and not in the list: it says the module is red in part, which is not enough to call a failing test in it one a worker could not run.

The list is of this crate's modules, so kelpie uses it only on this repo. On a project kelpie holds no list of, every failing test of a red run counts as one its worker could have run, and `sandbox-only` is never used there. Giving another project a list of its own is unbuilt.
