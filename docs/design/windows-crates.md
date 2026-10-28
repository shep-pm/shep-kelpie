# Windows crates

What each crate gives Windows (#241), apart from the shell and test fakes in `docs/design/windows.md`.

## What the crates cover

- The adapters crate gives Windows these answers (#1298):
  - the App store's folders and key use the `owner_only` crate on both platforms (#1264): `private`, `put` and the owner and mode checks of `save` call it, in place of `rustix` and `PermissionsExt`. On Windows the key and its folders are the user's alone, and a link, another owner or any other ACE is `StoreError::Exposed`.
  - `StatDisk` uses `disk` (#1300). `GetDiskFreeSpaceExW` reads caller-available bytes. One file handle supplies volume serial, index and link count (`GetFileInformationByHandle`), and `AllocationSize` (`GetFileInformationByHandleEx(FileStandardInfo)`). Hard links count once. Missing, inaccessible and delete-pending files are skipped at open.
  - `adapters::files::is_executable` is the mode bits on Unix and a regular file on Windows, and `on_path` also tries each `PATHEXT` extension there. `spawn::command` resolves a bare name through `PATH` and `PATHEXT` there (std adds only `.exe`, so npm's `.cmd` shims were "program not found"), and `spawn::program_names` is the one copy of the lookup.
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
