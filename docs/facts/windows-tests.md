# Windows runner test time

Measured 2026-10-30 on GitHub's `windows-11-arm` runner, image
`windows-11-vs2026-arm64`, Rust 1.99.1 (`aarch64-pc-windows-msvc`) and Git
2.56.0.windows.3 (`cpu: aarch64`).

Repeated Git subprocess calls dominate the measured runner test. Its 77 timed
Git calls consumed 7.03 of 8.91 seconds. A temporary directory took less than
one millisecond. Reducing the Git calls is the follow-up work described below.

## Baseline

[Run 41044428502, job 124024950902](https://github.com/shep-pm/shep-kelpie/actions/runs/41044428502/job/124024950902)
at #1575's head ran the runner's lib tests in 7382.72 seconds: 2877 passed,
156 failed, two ignored and four filtered out. That is 0.41 completed tests
per second. Core took 185.49 seconds and adapters 41.51 seconds in that run.
The runner duration is the test harness's `finished in`, excluding compilation.
The Windows failure groups remain in [the platform list](../agents/sandbox-red-tests.md#tests-windows-cannot-run-yet).

## A fixture and one runner test

[Run 41064538355, job 124092379306](https://github.com/shep-pm/shep-kelpie/actions/runs/41064538355/job/124092379306),
commit `2f6b9a3b2`, built only the runner's lib test binary. A temporary
`test::windows_timing::windows_timing_probe` ran three samples serially with
`--nocapture`. `Instant` timers surrounded each operation. The fixture's
`test_support::git` helper also printed each Git call's elapsed time.

| Operation | Windows, range over three samples |
|---|---:|
| Create a temp directory | 0.31 to 0.65 ms |
| Write 100 small files | 32.85 to 34.24 ms |
| `git --version`, including process start and wait | 45.34 to 51.11 ms |
| Git for Windows' `usr/bin/sh.exe -c 'exit 0'` | 46.72 to 56.63 ms |
| Start and wait for an empty `.cmd` stand-in | 31.37 to 39.14 ms |
| `Rig::new` | 0.782 to 1.185 s |
| `Rig::open` | 0.880 to 0.933 s |
| `rig.ask(..., "add", Some("7"))` | 10.33 to 13.93 ms |
| First `runner::step` | 2.041 to 2.075 s |
| Drop the runner | 0.25 to 0.44 ms |
| Drop the rig and remove its files | 28.75 to 30.12 ms |

The five Git calls that make each rig's repository (`init`, `clone`, `add`,
`commit`, `push`) account for 98 to 99% of `Rig::new`. Individual calls range
from 68.51 ms for `add` to 507.70 ms for one `init`. Creating the temporary
directory and writing ordinary files are much cheaper than starting processes.

The existing test
`runner::slots_tests::a_full_pending_rulings_opens_nothing_and_its_alert_counts_the_rulings`
passed in 6.88 seconds, running alone with `--test-threads=1`. Its fixture's
five Git calls took 0.839 seconds. The same test passed locally on macOS in
1.13 seconds under the worker's test lease, with 0.099 seconds in those calls.
These local numbers are a comparison, not a Windows runner measurement.

Defender's `AntivirusEnabled` and `RealTimeProtectionEnabled` were both true.
That observation alone does not attribute process time to Defender. Git reports
native ARM64, so the Git version check does not support an x64 emulation cause.

Cargo repeated about 92 and 96 seconds of build work before the two filtered
runs, despite the preceding `--no-run`. Those durations are excluded from the
table and the 6.88-second result. Subsequent probes invoke the executable from
Cargo's `compiler-artifact` JSON directly, to separate build time from test time.

## Git calls and concurrency

[Run 41065936797, job 124097225018](https://github.com/shep-pm/shep-kelpie/actions/runs/41065936797/job/124097225018),
commit `37333f558`, added an elapsed-time print around
`crates/process/src/worktree/mod.rs`'s `output` and enabled `GIT_TRACE2_EVENT`
for the same existing test. It passed in 8.91 seconds. Instrumentation and a
different VM make this a separate measurement from the preceding 6.88 seconds.

- 77 calls timed by the Rust helpers total 7.0318573 seconds, 79% of the test.
  This is a lower bound: other Git call sites were not timed by those helpers.
- The timed calls include 34 `rev-parse`, ten `config`, seven `fetch`, five
  `update-ref` and three `init` calls. A `rev-parse` typically takes 63 to 72 ms.
- Trace2 records 95 top-level Git starts and exits, plus 23 nested Git
  processes. Count top-level sessions by a `sid` with no `/` to avoid counting
  nested work twice. The top-level `exit.t_abs` values total 5.247120 seconds,
  measured inside Git, excluding the surrounding launchers and Rust's wait.

The next step ran twelve Windows-passing tests in `runner::slots_tests` from
the same compiled executable, without Trace2 or timing prints. The three tests
that failed in the baseline were excluded from this measurement only:
`an_answered_ruling_waits_for_the_slot_and_goes_before_a_new_issue`,
`a_still_red_yes_goes_to_ci_with_every_slot_taken` and
`an_attach_takes_a_free_slot_for_an_item_going_on_without_one`.

| Test threads | Passed | Harness duration |
|---|---:|---:|
| 1 | 12 | 137.71 s |
| 4 | 12 | 60.11 s |
| 32 | 12 | 62.12 s |

Four threads help this subset, so a lock serialising all its tests does not
explain these results. Increasing to 32 adds no throughput. This is one sample
per thread count, in the order shown, not a measurement of the whole suite.

The same job timed three `git --version` calls per executable from PowerShell:

| Git executable under `C:\Program Files\Git` | Range |
|---|---:|
| `bin\git.exe`, the one on `PATH` | 46.09 to 52.99 ms |
| `clangarm64\bin\git.exe` | 30.21 to 35.07 ms |
| `clangarm64\libexec\git-core\git.exe` | 29.58 to 32.48 ms |
| `cmd\git.exe` | 47.43 to 94.02 ms |

The launcher contributes overhead, and the direct native binary still costs
about 30 ms for an empty operation. No integrated runner test used the direct
binary, so this does not establish a suite speedup from changing `PATH`.

The job is red because its final Defender comparison stopped at
`Get-Help Add-MpPreference -Parameter ExclusionPath`: the image's help answered
`No parameter matches criteria ExclusionPath`. The traced test, all three
module runs and the executable comparisons completed before that failure.

## Defender comparison

[Run 41067470581, job 124102561574](https://github.com/shep-pm/shep-kelpie/actions/runs/41067470581/job/124102561574),
commit `bd3a5b909`, used PowerShell's command metadata instead of its missing
parameter help. This VM reports a Cobalt 100 with four logical processors.
It timed Git work before, during and after Defender path exclusions for
`TEMP`, `GITHUB_WORKSPACE` and `C:\Program Files\Git`. `Get-MpPreference`
confirmed the exclusions, and `finally` removed them before the last sample.
No Rust compilation or test binary was involved in this comparison.

| Work | Before | Excluded | Restored |
|---|---:|---:|---:|
| 50 `git --version` calls | 2.443 s | 2.397 s | 2.410 s |
| 50 `git rev-parse --git-dir` calls | 2.574 s | 2.492 s | 2.530 s |
| 20 `git init` calls, each a new repository | 2.397 s | 1.612 s | 1.587 s |

The empty and read-only Git calls retain almost all their cost with exclusions.
The `init` improvement persists after restoration, so warming is confounded
with the exclusion change. This comparison does not show Defender exclusions
fixing the slowdown. It also does not measure every Defender hook or the
runner suite with Defender disabled.

## What fixing it needs

The measured bottleneck is repeated Git subprocess round trips: fixture
creation and the runner's worktree/ref reads and writes. The same test spends
seconds in Git while directory creation, ordinary file writes and cleanup take
milliseconds. Four test threads improve throughput, and 32 do not improve on
four. The probes found no single shared lock or missing global cache that
accounts for this work.

A separate performance issue should reduce those round trips. Candidates are
batching ref reads and keeping a step's read results until a Git write makes
them stale. Cache invalidation must cover fetches, commits, resets and ref
updates, and preserve the checks in `worktree::trusted`. A copied immutable
fixture repository could reduce setup calls while each test keeps its own
home, but setup alone was only 0.839 of the untraced test's 6.88 seconds.

Measure the changed subset first, then the runner's entire lib binary on
Windows. No full-suite run under 45 minutes was measured here, so the `lib`
step's 180-minute limit and the Windows job's 200-minute limit stay. The
Windows failure groups are unchanged. The final change retains only these
facts and their index and platform-list links; temporary probes are available
in the cited commits.

Logs were read with `gh run view <run> --job <job> --log`. The worker's command
guard refused the issue's suggested `gh api` invocation. The raw job log still
contains GitHub's timestamps and the measurements quoted here.
