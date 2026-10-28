# Starting a shepherd on Windows

What the person starting the shepherd needs on a Windows machine (#1271). Measured on a Windows 11 ARM64 VM while crabpot took Windows (shep-pm/crabpot#218).

- Start the shepherd in an interactive desktop session: signed in at the console. Not over SSH, and not as a service.
  - crabpot refuses to start a worker in session 0 with `StartError::SessionZero`. Services and OpenSSH sessions both sit in session 0, so a shepherd started either way cannot run a worker. `doctor`'s `sandbox` line names the session, and `docs/facts/worker-profile-and-sandbox.md` records the refusal.
  - `gh`'s keyring token reads as invalid from an SSH session, so `gh` there cannot open a pull request even where a worker could run.
- Turn on Developer Mode (Settings, then System, then For developers), or start the shepherd from an elevated shell.
  - `adapters::files::link` makes the Codex login and both skill folders with `symlink_file` and `symlink_dir`, and Windows allows a symlink only with Developer Mode on or an elevated token. Without either, linking fails.
  - The tests that make a symlink need the same.
- On ARM64 Windows, put `C:\Program Files\LLVM\bin` on `PATH` before `cargo install` of shep or kelpie.
  - `ring` builds with clang there. CI sets the same `PATH` entry.
- Run the one-time `crabpot windows-install` (`windows.md`), and have Git for Windows installed.
