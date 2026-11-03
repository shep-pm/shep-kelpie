# Reading git in process

- Measured 2026-10-31 on macOS 26.2 with git 2.50.1 and gix 0.90.0 (`default-features = false`, features `revision` and `sha1`), for #1594. The runs set `GIT_TRACE2_EVENT` to a folder and counted `cmd_name` events by name.
- The full `cargo test --lib -- worktree:: runner::`, run once before and once after with the trace folder set, started 12492 `rev-parse` and 6138 `merge-base` processes before and 12518 and 4256 after. `merge-base` fell by 31% and `rev-parse` did not move. Each run had sandbox-only failures (48 before, 41 after, none new after), and the two ran under different load, which changes how often a polling test starts git.
- The subset `runner::merge runner::gate runner::dependencies` (231 tests, `-p shep-kelpie-runner`) started 5550 `rev-parse` and 2722 `merge-base` before and 1279 and 556 after. Those modules are where the reads land. The full run's flat `rev-parse` count means the rest of `runner::` started about 4300 more after than before. Load or a change to those tests is the likely cause; it was not isolated.
- `cargo test --lib -p shep-kelpie-process -- worktree::` started 276 `rev-parse` and 18 `merge-base` before, and 285 and 20 after. The five tests added to that run start about 9 `rev-parse` and 6 `merge-base` themselves.
- gix adds 79 packages to `Cargo.lock` (213 to 292, none removed): 45 `gix*` crates and 34 others, among them a second `defmt` (0.3.101) and `hashbrown` (0.16.1). A clean `cargo build --locked` of the workspace took 28.7 s and 50.6 s before (two runs under different load) and 57.2 s and 57.9 s after, with user CPU of 107 s and 113 s before and 130 s and 132 s after.
- gix needs a hash feature: with none, `gix-hash` does not compile. Without `sha256` it cannot open a sha256 repo, so a read there falls back to git.

## Step 2: the tree, blob, config and common dir reads (#1630)

- Measured 2026-11-03 on macOS 26.2 with git 2.50.1 and gix 0.90.0, with no gix feature added to `revision` and `sha1`, so `Cargo.lock` is unchanged. Each run was `runner::slots_tests::a_full_pending_rulings_opens_nothing_and_its_alert_counts_the_rulings` alone (`--exact`, the test binary run by hand), with `GIT_TRACE2_EVENT` set to a folder and the `cmd_name` events counted whose `hierarchy` equals their `name`, so a git that git starts is not counted.
- Before (the commit `#1629` merged at) the test started 51 git processes, the same on two runs: 7 `fetch`, 7 `rev-parse`, 6 `config`, 5 `update-ref`, 3 each of `cat-file`, `symbolic-ref`, `status`, `init` and `ls-tree`, 2 each of `remote` and `reset`, and 1 each of `merge-base`, `worktree`, `clone`, `commit`, `add`, `push` and `diff`. After, it started 43: 7 `fetch`, 6 `rev-parse`, 5 `update-ref`, 4 `config`, 3 each of `symbolic-ref`, `status` and `init`, 2 each of `remote` and `reset`, and 1 each of `ls-tree`, `merge-base`, `worktree`, `clone`, `commit`, `add`, `push` and `diff`. `cat-file` went from 3 to 0, `ls-tree` from 3 to 1, `config` from 6 to 4 and `rev-parse` from 7 to 6.
- The issue's 58 was not reproduced: the 51 is top-level sessions only, and it includes the test's own setup (`init`, `update-ref`, `commit`, `add`, and the clone and worktree it makes). Of the 8 starts left among the reads, 2 are `config core.bare false` and `config remote.origin.url` run on a clone's git dir by `clone::make`, 4 are `rev-parse --verify` of a peeled ref (`refs/remotes/origin/main^{commit}` twice, `refs/heads/kelpie/8^{commit}`) and of `refs/heads/kelpie/7` and `/8`, 1 is `rev-parse --is-inside-work-tree`, and 1 is `ls-tree -z <commit>` from `fence/commit.rs`. None of those is a read this step moves. The `ls-tree` is not a read of a worker's clone: its `-C` names the checkout (step 3 below counts it as the checkout's).
- The build script (`crates/core/build.rs`) is counted apart: one run starts 7 git processes (4 `rev-parse`, `status`, `ls-tree` and `symbolic-ref`, counted from its source, not traced), and it runs when cargo rebuilds `shep-kelpie-core`, not when a test runs. This change did not touch it.
- gix reads each of the four without a feature beyond `revision` and `sha1`: `revision` brings `index`, which holds the tree traversal and the revision parse, and the config and discovery are in `gix` always.
- gix applies `refs/replace`, and `git --no-replace-objects` (the board's reads) does not, so a repo with a replace ref is left to git. A `config --local` read of a file with an include is left to git too, as is a `[section.sub]` header, which git reads with a lower-cased subsection, and a key with no `=`, which gix leaves out of its values.

## Step 3: what reading a worker's clone in process would cost and save (#1631)

- Measured 2026-11-03 on macOS 26.2 with git 2.50.1 (Apple Git-155), rustc 1.98.1 and gix 0.90.0 (the version `Cargo.lock` pins, and the one whose source is read below). No code changed: the build numbers come from a `Cargo.toml` edit that was reverted, never committed.
- The starts are counted as step 2 counted them: `GIT_TRACE2_EVENT` set to a folder, the test binary run by hand under the test lease, `cmd_name` events whose `hierarchy` equals their `name`. A start is the clone's when its `-C` or `--git-dir` names a worktree or its sibling `<worktree>.git`. A worktree is a path that a start of the same run passes as `--work-tree`, which `trusted_git` always does. Each run was made twice.

### The clone's git starts, by call site

- `runner::slots_tests::a_full_pending_rulings_opens_nothing_and_its_alert_counts_the_rulings`: 43 top-level starts, 18 of them the clone's, the same on both runs. `runner::merge runner::gate runner::dependencies` (234 tests, `-p shep-kelpie-runner`, all passing; #1594 counted 231): 12130 and 12131 top-level starts, 3789 and 3790 the clone's. The two subset runs differ by one `merge-base`.
- Reads, the ones gix could answer (subset counts are run 1 and run 2 where they differ):

| Call site | git | Single test | Subset |
| --- | --- | --- | --- |
| `worktree::uncommitted` (`crates/process/src/worktree/mod.rs`) | `status --porcelain=v2 -z --untracked-files=all` | 3 | 1472 |
| `catch_up.rs`, `adopt.rs` | `status --porcelain --untracked-files=no` | 0 | 37 |
| `fence::commit::cut_at` (`crates/core/src/fence/commit.rs`) | `merge-base <head> <main>` | 1 | 376, 377 |
| `worktree::change_start` | `merge-base <base> HEAD` | 0 | 173 |
| `review/calls.rs` | `diff --no-ext-diff --no-textconv <base>...HEAD` | 0 | 172 |
| `catch_up.rs` | `diff --name-only --diff-filter=U` | 0 | 42 |
| `worktree::prepare` | `symbolic-ref --quiet HEAD` | 1 | 91 |
| `holds_work`, `head`, `branch_head` | `rev-parse HEAD` (51), `rev-parse --quiet --verify <ref>` (37) | 0 | 88 |
| `catch_up.rs`, `adopt.rs`, `active.rs` | `rev-list --merges --max-count=1` (31), `log -1 --format=%cn` and `%ce` (62), `log -1 --format=%ct` (1) | 0 | 94 |

- That is 5 reads of 18 in the single test and 2545 (2546 on run 2) of 3789 in the subset. The other starts are writes and setup, which stay on git:

| Call site | git | Single test | Subset |
| --- | --- | --- | --- |
| `clone::make` | `config core.bare false`, `config <key> <value>` | 4 | 356 |
| `clone::make` | `symbolic-ref HEAD <ref>`, `update-ref <ref> <commit>` | 4 | 356 |
| `clone::make`, `clone::complete` | `reset --quiet --hard HEAD` | 2 | 182 |
| `clone::mirror` | `update-ref --no-deref` | 3 | 259 |
| `catch_up.rs` | `rebase --quiet` (30), `rebase --abort` (22), `merge --quiet --no-commit --no-ff` (23), `commit --quiet --no-edit` (3) | 0 | 78 |
| `abort_merge`, `push.rs` | `merge --abort` (3), `push` (10) | 0 | 13 |

- Reads of a worker's commits that the rule does not count as the clone's, because their `-C` is the checkout (the fence fetches the head into the checkout, and the board names each clone's objects to the checkout's git as alternates). Single test: 1 `diff --name-only --no-renames -z`, 1 `ls-tree -z <commit>`. Subset, the same on both runs: 1319 `diff --name-only --no-renames -z` (`fence::changed`, `worktree/changed.rs`, the board's `files`), 261 `merge-tree --write-tree --name-only --no-messages -z` (the board's `conflicts`) and 30 without `--name-only`, 196 `ls-tree -z <commit>` (`fence/commit.rs`) and 32 `ls-tree --name-only <ref> -- <path>`.
- The issue named `ls-tree`, `diff` and the conflict check as clone reads. In these runs only the clone's `diff` for a reviewer (172) is the clone's; the rest read the checkout.

### What each feature weighs

- `status` and `merge` each switch on `blob-diff` in gix's own features (`gix-0.90.0/Cargo.toml`: `status = [.., "blob-diff", ..]`, `merge = ["blob-diff", ..]`), so "alone" overlaps. Each row is today's `revision` and `sha1` plus the named features. `cargo metadata` rewrote `Cargo.lock` (it only added packages), then `cargo build --locked` of the workspace (dev profile) ran twice into an empty target folder, with the crates already downloaded.
- The registry came from a copy of `~/.cargo/registry` under the build folder, since the sandbox cannot write `~/.cargo`. The machine's load average was 4 to 10 from other workers.

| gix features | Packages in `Cargo.lock` | Gained | Wall (s) | User CPU (s) |
| --- | --- | --- | --- | --- |
| `revision`, `sha1` (today) | 292 | 0 | 24.5, 23.5 | 106.2, 107.6 |
| plus `status` | 300 | 8 | 24.3, 24.6 | 110.9, 111.2 |
| plus `blob-diff` | 298 | 6 | 24.5, 25.3 | 110.0, 113.0 |
| plus `merge` | 299 | 7 | 24.6, 24.9 | 112.5, 113.0 |
| plus all three | 301 | 9 | 24.7, 26.6 | 114.1, 116.0 |

- The packages gained: `blob-diff` adds `dashmap`, `gix-ignore`, `gix-imara-diff`, `gix-pathspec`, `gix-submodule` and `gix-worktree`. `status` adds those six and `gix-dir` and `gix-status`. `merge` adds the six and `gix-merge`. All three add `status`'s eight and `gix-merge`. `gix-filter` and `gix-attributes` are in the lock already.
- Wall time did not move by more than the noise: the build's longest chain is the same. User CPU rose 4 s for `status`, 5 s for `blob-diff`, 6 s for `merge` and 8 s for all three, 4% to 8% of today's 107 s. Two runs of one set differ by up to 3 s (`blob-diff`: 110.0 and 113.0), so the gaps between the three single features are not separable; only today against all three is.
- Step 1's 79 packages bought `revision`; these add 6 to 9 more.

### What each feature would let move

- No feature beyond today's: the `revision` feature brings `index`, the merge-base and the revision parse. `merge-base` (549), `symbolic-ref --quiet HEAD` (91), `rev-parse` (88), `rev-list --merges` (31) and `log -1` (63) read commits and refs. `diff --name-only --diff-filter=U` (42) lists the index's entries at a stage above 0. That is 864 of the subset's 12130 starts (7.1%). None of this was tried: it needs gix to open the clone after `worktree::trusted`, alternates included.
- `status`: both `status` shapes, 1509 starts, 12.4% of the subset's 12130 and 40% of the clone's. It also brings `blob-diff`.
- `blob-diff`: the tree-to-tree name lists in the checkout (`diff --name-only --no-renames -z`, 1319 starts, 10.9%), since `Repository::diff_tree_to_tree` is behind it (`gix-0.90.0/src/repository/mod.rs`). The reviewer's patch text (172 starts) stays on git: gix-diff has a unified-diff writer for hunks, and no `diff --git`, `index` or mode header was found in it.
- `merge`: `merge-tree --write-tree` in the checkout (291 starts, 2.4%), through `Repository::merge_trees` and `merge_commits`. Whether it names the same conflicted paths as `merge-tree --name-only` (renames, several merge bases, merge drivers) is not tested.
- Where git answers, whatever the feature:
  - A `filter=` attribute, in the tree or in git's global attributes. Git runs a driver defined in global config, and an isolated open reads none, so the two disagree on a file the driver changes.
  - A submodule (a gitlink in the index or a `.gitmodules`), since gix's status opens the submodule's own repo, which sits in the worker's folder.
  - A sparse checkout (`core.sparseCheckout`, skip-worktree entries, a sparse index).
  - A global ignore file (`core.excludesFile`, `~/.config/git/ignore`), which git reads for `--untracked-files=all` and an isolated gix does not.
  - A `merge=` attribute for `merge`.
  - A repo with `refs/replace`.
  - Every write above, 1244 of 3789.
- `trusted_git` passes `core.autocrlf=false` and `core.eol=lf`; an isolated open would need the same two values, or `text=auto` files differ on Windows.

### Whether gix's `status` can start a program

Read at gix 0.90.0, gix-status 0.37.0, gix-filter 0.36.1, gix-diff 0.70.0 and gix-merge 0.23.0, on docs.rs. Not run.

- A filter driver: yes, if config defines one. For a tracked file whose stat differs, gix converts the worktree file to its git form through the filter pipeline, in `ToGit` mode ([`gix/src/status/index_worktree.rs`](https://docs.rs/crate/gix/0.90.0/source/src/status/index_worktree.rs), lines 106 to 114; [`gix-diff/src/blob/pipeline.rs`](https://docs.rs/crate/gix-diff/0.70.0/source/src/blob/pipeline.rs), `convert_to_git`; [`gix-status/src/index_as_worktree/function.rs`](https://docs.rs/crate/gix-status/0.37.0/source/src/index_as_worktree/function.rs), line 620). The drivers are the `filter.<name>.clean`, `smudge`, `process` and `required` keys of config sections the trust filter keeps (full trust, or a source that is not the repo's own config; [`gix/src/filter.rs`](https://docs.rs/crate/gix/0.90.0/source/src/filter.rs), lines 225 to 266; [`gix/src/open/options.rs`](https://docs.rs/crate/gix/0.90.0/source/src/open/options.rs), line 146). An attribute naming a driver that no config defines starts nothing ([`gix-filter/src/pipeline/convert.rs`](https://docs.rs/crate/gix-filter/0.36.1/source/src/pipeline/convert.rs), lines 55 to 91). A defined one is spawned by [`gix-filter/src/driver/init.rs`](https://docs.rs/crate/gix-filter/0.36.1/source/src/driver/init.rs) (lines 75 to 90) through `gix_command::prepare(..).command_may_be_shell_script()`, so possibly through a shell.
- In a kelpie clone that needs a driver in the clone's own config: an isolated open reads that file alone, `clone::make` writes three keys into it (`remote.origin.url`, `remote.origin.pushurl`, `core.hooksPath`) and the fence denies the worker its `config`. So gix's status would start no filter program there. Git's would, for a driver in the maintainer's global config.
- `core.fsmonitor`: no. Nothing under `gix/src/config`, `gix-status/src` or `gix-dir/src` names the key, a hook or a daemon. gix's status skips an index entry flagged `FSMONITOR_VALID`, `ASSUME_VALID`, `SKIP_WORKTREE` or `UPTODATE` ([`function.rs`](https://docs.rs/crate/gix-status/0.37.0/source/src/index_as_worktree/function.rs), lines 276 to 283). `FSMONITOR_VALID` is bit 21 of gix-index's entry flags ([`gix-index/src/entry/flags.rs`](https://docs.rs/crate/gix-index/0.58.0/source/src/entry/flags.rs), line 32), beside flags that are not stored. Whether reading an index can set it was not traced. Git clears the bit when `core.fsmonitor` is off.
- A textconv or `diff.<name>.command` program: not in status. The drivers are loaded ([`gix/src/config/cache/access.rs`](https://docs.rs/crate/gix/0.90.0/source/src/config/cache/access.rs), `diff_drivers`, line 44), but a binary-to-text command runs only when the mode converts to the worktree form, and `ToGit` does not (`pipeline.rs`, `Mode::to_worktree`).
- Any other program: gix-status has two `Command::new` calls (`fsutil.exe`, `cmd.exe`), both in its Windows test module ([`gix-status/src/fscache.rs`](https://docs.rs/crate/gix-status/0.37.0/source/src/fscache.rs), lines 119 to 177). The `merge` feature adds external merge drivers, `merge.<name>.driver` ([`gix-merge/src/blob/platform/merge.rs`](https://docs.rs/crate/gix-merge/0.23.0/source/src/blob/platform/merge.rs), line 79), which a clone's config does not define either.
