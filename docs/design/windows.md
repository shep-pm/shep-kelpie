# Windows

Kelpie is being ported (#241). This area holds the shell-outs and the tests' stand-in commands (#1149).

## Starting a shepherd on Windows

What the person starting the shepherd needs on a Windows machine (#1271). Measured on a Windows 11 ARM64 VM while crabpot took Windows (shep-pm/crabpot#218).

- Start the shepherd in an interactive desktop session: signed in at the console. Not over SSH, and not as a service.
  - crabpot refuses to start a worker in session 0 with `StartError::SessionZero`. Services and OpenSSH sessions both sit in session 0, so a shepherd started either way cannot run a worker. `doctor`'s `sandbox` line names the session, and `docs/facts/worker-profile-and-sandbox.md` records the refusal.
  - `gh`'s keyring token reads as invalid from an SSH session, so `gh` there cannot open a pull request even where a worker could run.
- Turn on Developer Mode (Settings, then System, then For developers), or start the shepherd from an elevated shell.
  - `adapters::files::link` makes the Codex login and both skill folders with `symlink_file` and `symlink_dir`, and Windows allows a symlink only with Developer Mode on or an elevated token. Without either, linking fails.
- On ARM64 Windows, put `C:\Program Files\LLVM\bin` on `PATH` before `cargo install` of shep or kelpie.
  - `ring` builds with clang there. CI sets the same `PATH` entry.
- Run the one-time `crabpot windows-install` (below), and have Git for Windows installed.

## What the port covers

- `spawn::sh()` and `spawn::env()` are the only way to start a shell or `env`. On Unix they are `/bin/sh` and `/usr/bin/env`. On Windows they find Git for Windows through a `git.exe` on `PATH`, then its default install folders, and fall back to the bare name.
- A hook, the test-runner, the dev server's install and the Claude sandbox's `env` run the maintainer's POSIX shell line, so they require Git for Windows.
- `upgrade` reaches `tar` and `shasum` only for a release asset, so it keeps them Unix. `kill` and `ps` wait on #1148, sockets on #1147.
- `upgrade` compiles on Windows and does not install there yet (#1392). `--binary` takes a regular file as it is, and the staged copy gets no mode. `--release` refuses before `tar`: there is no Windows asset. `--ref` adds `EXE_SUFFIX`. The staged copy's name ends in the pid, never `.exe`, so `Build::of` cannot run it, and a running `.exe` cannot be replaced.
- Workers run through crabpot here as on every platform (#1217): the wrap is kelpie's own binary with `--crabpot`, and the machine needs a one-time `crabpot windows-install`, which fixes the loopback mode of every session after it. `crabpot::windows_status` reads what the install says, `Host::windows_status` carries it to `doctor`, and `docs/design/fence.md` holds the loopback rule.
- A test's stand-in command is `write_fake` and `Fake` from `test_support`: an `sh` script on Unix, a `.cmd` file on Windows. `write_script` is for a script that needs real shell, which Windows runs through `spawn::sh()`. A FIFO test is `#[cfg_attr(windows, ignore = "a FIFO is a Unix file")]`.
- `docs/agents/sandbox-red-tests.md` lists what Windows cannot run, read from the source, since the Windows job runs only `cargo check` and the `save` step.
- `core` and the crates below it (#1262), `dog` (#1274), `adapters` (#1298), `runner` (#1299) and the root package (#1392) compile for Windows, tests included, each in the gating `cargo check`. The root package's tests do not run there yet (#241).
- A file's mode is a Unix answer, and `core` gives Windows one of its own where it can (#1262):
  - the home stamp reads `Metadata::modified`.
  - the fence takes no executable bit from a Windows disk, since git there keeps none: a fenced file matches either regular mode of the commit.
  - a file or folder only its owner can use is the `owner_only` crate's (#1264), in `docs/design/windows-crates.md`.
- A test that reads or sets a mode, or makes a symlink, is `#[cfg(unix)]`: its body names a Unix API, and a symlink needs Developer Mode or an administrator. One that only needs a Unix script or secret is `#[cfg_attr(windows, ignore = "...")]`.
- The dog's e2e run works on Windows too (#1302):
  - `worker_folder` has no `openat` on Windows, so its `Held` walk opens each folder from the root down by path with `FILE_FLAG_OPEN_REPARSE_POINT` and a share mode of read only, holds every handle until it drops, and refuses a folder whose attributes say reparse point (a symlink or a junction) or that is not a folder. `read` and `write` use it, and so does the dog's `deliver`, which was the walk's first home (#1302, #1324). It reads the attributes of every held folder again before each file. A worker that turns a held folder into a junction in place (write-attributes access is not subject to sharing) between that read and the write can land one file outside the folder. Unix has no such gap. Closing it needs a handle-relative open, which needs `unsafe` or a Windows crate, so it waits on the maintainer reopening the no-new-dependency and no-`unsafe` ruling. `deliver` removes a file already there and makes it again with `create_new`; `write` makes `name.tmp` with `create_new` and renames it over, so a link or hard link in place is replaced and never written through. Safe std cannot read a file's hard link count or owner, so `read` does not refuse a hard link there and `write_private` sets no access list (#1467). The flag values are local constants, since `worker_folder` takes no Windows crate.
  - The `ssh` and `rsync` stand-ins are `.sh` scripts run by a `.cmd` that calls Git for Windows' `sh` with `MSYS=winsymlinks:nativestrict`. The `.cmd` is the process the dog kills, so the `ssh` script ends its command when the dog closes its input. `ended` is `!tree::alive(pid)`.
- A cold start reads `BootError::Unsupported` from `shep_handover::boot` as no arrival, since Windows has no `execve` and no handover to arrive from (#1448). `handover_boot::arrival` maps it, and any other `BootError` stays fatal.
- The adapters, runner and `owner_only` crates' Windows answers are in `docs/design/windows-crates.md`.
- `Path::canonicalize` gives a `\\?\` path on Windows, which Git for Windows refuses (#1464). `process::canonical` drops the prefix when the path has a plain form (a drive letter or a UNC share, no component or length that needs it) and is `canonicalize` elsewhere. A path that reaches another program, or is compared with one that program printed, goes through it: git's dirs, the sandbox and settings rules, the e2e copy's root, the guard's `pwd -P` match, the repo match for a folder. A path compared only with another `canonicalize` result, or used only for kelpie's own file calls, stays on `canonicalize`. `crates/process/src/canonical.rs`
