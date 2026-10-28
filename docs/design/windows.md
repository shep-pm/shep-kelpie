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
  - a file or folder only its owner can use is the `owner_only` crate's (#1264), below.
- A test that reads or sets a mode, or makes a symlink, is `#[cfg(unix)]`: its body names a Unix API, and a symlink needs Developer Mode or an administrator. One that only needs a Unix script or secret is `#[cfg_attr(windows, ignore = "...")]`.
- The dog's e2e run works on Windows too (#1302):
  - `deliver` has no `openat`, so it opens each folder from the worktree down by path with `FILE_FLAG_OPEN_REPARSE_POINT` and a share mode of read only, holds every handle until the copy ends, and refuses a folder whose attributes say reparse point (a symlink or a junction). It reads the attributes of every held folder again before each file. A worker that turns a results folder into a junction in place (write-attributes access is not subject to sharing) between that read and the write can land one result file outside the worktree. Unix has no such gap. Closing it needs a handle-relative open, which needs `unsafe` or a Windows crate, so it waits on the maintainer reopening the no-new-dependency and no-`unsafe` ruling. It removes a file already there and makes it again with `create_new`, so a link put in its place fails the write. The flag values are local constants, since `dog` takes no Windows crate.
  - The `ssh` and `rsync` stand-ins are `.sh` scripts run by a `.cmd` that calls Git for Windows' `sh` with `MSYS=winsymlinks:nativestrict`. The `.cmd` is the process the dog kills, so the `ssh` script ends its command when the dog closes its input. `ended` is `!tree::alive(pid)`.
- The adapters crate gives Windows these answers (#1298):
  - the App store's folders and key use the `owner_only` crate on both platforms (#1264): `private`, `put` and the owner and mode checks of `save` call it, in place of `rustix` and `PermissionsExt`. On Windows the key and its folders are the user's alone, and a link, another owner or any other ACE is `StoreError::Exposed`.
  - `StatDisk` uses `disk` (#1300). `GetDiskFreeSpaceExW` reads caller-available bytes. One file handle supplies volume serial, index and link count (`GetFileInformationByHandle`), and `AllocationSize` (`GetFileInformationByHandleEx(FileStandardInfo)`). Hard links count once. Missing, inaccessible and delete-pending files are skipped at open.
  - `adapters::files::is_executable` is the mode bits on Unix and a regular file on Windows, and `on_path` also tries each `PATHEXT` extension there.
  - `adapters::files::link` is `symlink` on Unix, and `symlink_file` or `symlink_dir` by the target on Windows. The Codex login and both skill folders use it.
  - `adapters::mcp::live`, which binds a Unix socket named by a uid, is built on Unix only.
- The runner crate gives Windows these answers (#1299):
  - `code_graph::too_long` says `None`, since the daemon's `cbm-daemon-<uid>` folder and the socket path limit it explains are Unix ones.
  - the rig's `hook` sets no mode on Windows, and `write_totp_secret` makes its file with `owner_only::create_private`.
- The `owner_only` crate keeps a folder or file to its owner on both platforms (#1264). It is the one leaf crate that holds Windows ACL code, and denies `unsafe_code` except in `owner_only/windows.rs`, as `tree` does. `totp::private_dir` re-exports it for the runner.
  - On Unix: `private_dir` and `create_private` use mode 0700 and 0600. `is_owner_only` is the uid and `mode & 0o077 == 0`.
  - On Windows: a new folder or file takes a protected DACL of one ACE, full control for the current user, and the owner is set to that user, so one made from an elevated shell is not Administrators'. A folder's ACE is inherited by what it holds. `restrict_dir` does the same for a folder that is already there.
  - `is_owner_only` on Windows is false for a null DACL, an owner that is not the current user, or any allow ACE for another SID, Administrators and SYSTEM included. Deny ACEs are ignored.
  - `private_dir` narrows only the folders it makes, as `DirBuilder::mode` does.
  - `Secret::write` draws its bytes from `getrandom` on every platform. It and `write_atomically` skip the folder sync on Windows, which cannot open a folder to sync it and whose NTFS journals the rename and link.
  - A refusal's advice on Windows is `icacls <path> /inheritance:r /grant:r %USERNAME%:F`, or `:(OI)(CI)F` for a folder.
  - The ACL tests run in the `windows` job's non-gating step, `id: totp`.
