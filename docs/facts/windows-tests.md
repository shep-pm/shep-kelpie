# Windows runner test time

Measured 2026-10-30 on GitHub's `windows-11-arm` runner, image
`windows-11-vs2026-arm64`, Rust 1.99.1 (`aarch64-pc-windows-msvc`) and Git
2.56.0.windows.3 (`cpu: aarch64`).

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
