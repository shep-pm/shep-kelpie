# Each worker has a clone of its own

Part of #241. A worker's worktree stops being a linked worktree of the project's checkout and becomes a clone that borrows the checkout's objects.

A linked worktree writes into the checkout's `.git`: its branch ref, the lock beside it, its reflog and its objects. The fence grants the worker those paths one by one, and `ref_folders::ensure` makes the folders they sit in before each handover, since the fence cannot grant a folder that is not there yet.

On Windows that does not hold. srt-win drops a write grant on a path that does not exist yet. A worker cannot make `refs/heads/kelpie/22.lock` without Modify on `refs/heads/kelpie`, and Modify on that folder lets it rewrite `refs/heads/kelpie/23`, another worker's branch. No narrower grant exists. This was measured for #241, and `docs/facts/worker-profile-and-sandbox.md` holds the other srt-win behaviour found there.

## Decision

- Each worker has a clone of its own on every platform, not on Windows alone.
- The clone's git dir is the sibling `<worktree>.git`. It is made by `git init --bare`, then `core.bare=false`, and its `objects/info/alternates` names the checkout's objects, so nothing is fetched and the clone costs the files and little else.
- The worker may write its whole git dir and nothing of the checkout's. Its branch, lock and reflog sit in folders it already owns, so the grant needs no path that is not there yet.
- The clone copies three settings from the checkout: `remote.origin.url`, `remote.origin.pushurl` and `core.hooksPath`. It holds no ref but its branch. A ref kelpie wants in it, such as `origin/main`, is set by `update-ref` from the checkout's commit.
- Kelpie's git reads a clone through the same trusted call it uses for a linked worktree, with the clone's git dir named as both git dirs. A clone whose alternates name another repo's objects is refused.
- Kelpie never reads `origin/<base>` from a clone, whose refs the worker writes. It reads the commit from the checkout and passes it.
- A linked worktree turns into a clone in place, keeping its branch, its index and a merge in progress. The checkout keeps `refs/heads/<branch>`, so its gc keeps what the clone borrows.

This record covers the first slice, which teaches kelpie's git to read a clone. Making one at `prepare`, the fence's grants and the worker's profile follow in later slices.

## Considered options

- **`CREATOR OWNER` and `OWNER RIGHTS` on the refs folder.** The idea is that a worker owns what it creates, so it needs no grant on a path that is not there yet. It fails because every session runs as the one user `srt-sandbox`, so the files of every worker have the same owner and one worker can rewrite another's.
- **A Windows-only fork.** Clones on Windows and linked worktrees elsewhere keeps the second path alive and untested on the machines the maintainer works on. One layout on every platform costs the same clone once.

## Consequences

- A repo-local setting of the checkout other than the three copied, such as a credential helper, does not reach a clone. A push of kelpie's from a clone reads the machine's and the clone's config.
- The checkout keeps a branch ref for each converted worktree until kelpie removes it.
- Objects a worker writes sit in the clone, and go with it when the work item is removed.
