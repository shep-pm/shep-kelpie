# Dependencies

Whether kelpie merges the bumps bots open (Dependabot), and closes one that breaks with an issue filed. #855

## Settings

- `[app.dogs.kelpie.dependencies]`, `#[serde(default)]`, `deny_unknown_fields`: an absent table is `pick = "off"`. `crates/core/src/settings/dependencies.rs`
- `pick`: `off` (default; every bot's pull request is left alone) or `on`.
- `bots`: logins whose pull requests are bumps, `["dependabot"]` when absent. Compared ignoring case and a `[bot]` suffix. Not the contributions' bot rule: a bump never waits on the maintainer's labels.
- There is no `on_red`: a red bump is always closed (ADR 0020). The key is refused by name, and `settings migrate` deletes it, naming its value.
- `web`: paths that make a bump a web bump. A path ending `/` is a folder, any other is a file. Empty means no bump is one.
- `wait_hours` (24): above 0. How long a head's checks may stay pending or never start before the bump is red.
- A changed table takes effect at the next poll: `reread` lists `dependencies` as in effect, and the runner builds nothing from it at start. Turning `pick` on starts recording bumps, and turning it off forgets the records as a start with `pick` off does; `wait_hours` applies to a bump already recorded. `runner/reread/`

## Detection

- A bump is an open pull request, not a draft, from a branch of the repo itself, whose author is a listed bot. `is_bump`, `runner/dependencies/`. A bump takes no slot, no ruling and no model call.
- `ProjectState.bumps` holds `Bump { pull_request, author, branch, head, seen, rebase_asked, e2e, standing, red }`, oldest first. A new head replaces `head` and `seen` and clears `e2e`; `rebase_asked` names the head last asked, so the new head is asked afresh. `crates/core/src/state/bumps.rs`
- `follow_dependencies` runs once a board poll at the start of a pass, with the lock let go: it lists the open pull requests itself and reads each bump's base and paths, and the comments, merges and closes run on the lock. A merge moves `main`, so the bumps after it wait and the next pass polls again. A bump that left the board is dropped, unless kelpie had decided to close it and still owes its issue; that record is kept however it left. One closed by someone else before kelpie's comment or close skips them and gets the issue; one merged by hand gets none, and a pull request that cannot be read waits. `pick = "off"` forgets all, close steps still owed included; an e2e run in flight finishes and its result is dropped.

## Proving a head

- Behind `origin/main` (`worktree::base_of`): kelpie asks for a rebase only when the head conflicts with `main` (`git merge-tree`) or `main`'s rules require a pull request's branch to be up to date with its base (`Forge::requires_up_to_date`; on GitHub the branch's rulesets, then its classic protection unless one requires it, `adapters/gh/rules.rs`, read once a poll when behind). A bump only behind, under a rule that does not require it, is proved as it stands. The ask is `@<author> rebase` (the login without a `[bot]` suffix, which a mention does not resolve), posted once per head (`rebase_asked`) as the maintainer's `gh` login; nothing else is proved on it. When git or the forge cannot say, kelpie asks, as it always did.
- A head being proved waits `CHECKS_SETTLE` (120 s) from `seen`, as the gate does, then reads the checks. `Passed` and `Skipped` are green. Failed is red.
- Pending or absent checks are red once `wait_hours` have passed since `seen`.
- A web bump (`changed_paths` of the head against `origin/main` meets `web`) also needs a proof of its head: the local browser where the project has one, the e2e host where it has one, both when it has both. `runner/flight/proving.rs`
- No run starts while the runner drains, and a run in flight shows in the drain's `calls` as role `e2e`, so an upgrade or pause waits for it. `runner/flight/calls.rs`
- The proof is one thread at a time: check the head out at `worktrees/bump-<n>`, run the browser proof if planned, ask the dog (`E2eRunner`, `lease e2e`'s ask), remove the checkout, send the verdict as news. The dog finds the project from that folder. `crates/adapters/src/adapters/e2e_runner.rs`
- Outcomes (`Ended`, `crates/core/src/lease/cli/e2e.rs`, not the exit code, which can equal the dog's): the command exiting 0 passes and is saved; exiting anything else is red and saved with the end of its output; the dog saying there is no host leaves the bump unmerged and `status` says so, and the dog is not asked again for that head until the runner restarts; the dog unreached, gone, or refusing the ask is no run, asked again at the next poll.
- The project has the browser when `browser.enabled`, this is macOS, the tools are installed and `main` has a launch file with a configuration (#861). Without a launch file the e2e host alone proves it; a launch file that cannot be read or a port that cannot be had leaves the bump open, `status` saying why, asked again at the next poll. `Runner::browser_proof`, `runner/dev_server/bump/`
- The browser proof starts that dev server in the bump's checkout, in a sandbox that writes only the checkout and `builds/bump-<n>`, reaches only `browser.domains` and cannot read credentials, and opens its port to the browser only once it answers. The browser is headless whatever `headed` says, behind the same bridge, filter and proxy as a worker's (`PlaywrightPages`, `crates/adapters/src/adapters/pages.rs`). Before the server starts, the same sandbox installs the packages from the lockfile in or above the server's folder (`npm ci`, or pnpm, yarn or bun's frozen install, with `--ignore-scripts`), with the package manager's registry added to the hosts and its cache in the build folder; with no lockfile the server starts as is. `runner/dev_server/install/`. Kelpie speaks MCP to its server with no model (`crates/adapters/src/adapters/bridge/client/mod.rs`, a recorded session in `fixtures/playwright-mcp-session.jsonl`).
- Each of `browser.routes` is opened at `http://localhost:<port><route>`. A route passes when the page loads, no request to the dev server's origin answers 5xx, and the console logs no error. Chromium's log of a resource at another origin that failed to load is not one. A failed route is red like a failed e2e run (the browser proof ends the head's proof, so no e2e run follows), and the failure names the route and the status, never a URL or a path: ``route `/` had a request answer 503``, ``route `/` logged 2 console errors``, ``route `/` did not load``. It is posted and not saved apart from `Red`.
- A browser proof is not saved when it passes: the e2e run follows in the same thread, and the pass is saved in `Bump.e2e` when that ends (a host that is not there counts as a pass when the browser passed). A restart repeats both. A dev server that does not answer, or cannot start, leaves the bump open and is not tried again for that head until the runner restarts. A browser that cannot be driven, or fewer pages than asked, is tried again at the next poll.
- A proved bump is merged with `Forge::merge` (a merge commit, `--match-head-commit`). Merges come one at a time without a queue: the next bump is behind the moved `main`, proved or asked as the rule says. A merge the forge refuses on a behind bump asks for the rebase, once per head; on one on `main` it is the standing, tried again.

## Red

- A red bump is always closed. Steps are saved one at a time in `Red.step`, so a restart repeats none: comment naming each failed check (the e2e output tail is not posted), close the pull request (`Forge::close_pull_request`), then open an issue labelled `needs-triage` and `dependencies`. A refused step is tried again at the next poll.
- The issue puts the call in front of the project manager, who decides per item on the free-slot wake. A close is undone by the bot's next bump, which is a new pull request.
- `RedStep::Left` stays as a saved state value: no new bump gets it. A bump saved as left stays open until the bot pushes a new head, which gives it another chance.
- Out of scope: a fix attempt on a red bump.

## Log and status

- One log line each: rebase ask, browser proof and e2e run (passed, failed), merge, red, close, issue filed. `bump #<n>: ...`
- A bump behind `main` says which rule applied, in its standing and its log line: `main` requires it up to date, it conflicts, the rule cannot be read, or it is not required.
- `status` lists `bumps` with the pull request, author, head and standing, left out when empty.
