# Contributions

Whether and how kelpie picks up other people's pull requests. It sees and lists the others (below), reads each new head for security (`contribution-security.md`), hears what they say (`contribution-talk.md`), asks for a release (`contribution-release.md`), runs nothing of theirs but the CI runs of a head that passed the security review (below), records the reviewers' pass over a green head (`contribution-pass.md`), rules every merge (`contribution-merge.md`), and takes a released one over as its own work item (`contribution-release.md`). #617 to #619, #622 to #625, #634

## Settings

- `[app.dogs.kelpie.contributions]`, `#[serde(default)]`: an absent table is `pick = "off"`. `deny_unknown_fields`, so a mistyped key is refused naming it. `crates/core/src/settings/contributions.rs`
- `pick`: `off` (default, kelpie's own pull requests only), `label` (those the maintainer labels for it) or `all`. Kebab-case, any other word refused.
- `reviewers`: ordered agent files whose role is `reviewer`, bots welcome, each once. Absent, it is kelpie's default pass: `iq3-xxs-review` where kelpie holds a `paddock` gateway, then `defect-hunter-lite`.
- `security_reviewer`: a reviewer agent file, `security-reviewer` when absent, shipped as `agents/security-reviewer.md`. It must run sessions: a command, endpoint or bot is refused.
- `quiet_days` (7) and `nudge_days` (3): whole days above 0. `nudge_days` must be below `quiet_days`: the nudge comes before a contribution counts as quiet. A zero is refused by the parser, the ordering by `Contributions::check`, called from `Settings::from_table`, so a bad pair is refused by key when the settings are read and a reread keeps the old settings.

## Checks at start

- `Settings::contribution_lineup` shares its body with `lineup` (`crates/core/src/settings/reviewers/mod.rs`), so the bot-order check applies. Its refusals name `contributions.reviewers` when that key is set, and kelpie's default reviewers when it falls back. #1544
- Agent files are checked only when `pick` is not `off`, so a project that never opts in needs none.
- `Runner::open` refuses a `pick` other than `off` when `ports.voice.covers(&remote)` is false: `SettingsError::Invalid` on `contributions.pick`. The representative would otherwise post as the maintainer, which no worker or reviewer may do. `pick = "off"` starts on any repo. `crates/runner/src/runner/mod.rs`
- `Settings::security_reviewer` checks, whenever `pick` is not `off`, that `security_reviewer` has a file whose role is `reviewer` and that runs sessions. `Runner::open` calls it.

## Rereads

- A changed table takes effect at the next poll: `reread` runs the checks above and refuses it as a start does. A call running keeps its reviewer; `off` forgets the followed. `runner/reread/`

## Detection

- A contribution is an open pull request, not a draft, whose author is not the maintainer (`Runner::maintainer`), not the account kelpie acts as (`viewer`), and not a bot, and not one the maintainer left open (`left_open`). Logins compare ignoring case. `Runner::is_contribution`, `runner/contributions/`
- Under `pick = "label"` it also carries `ready-for-agent`; under `"all"` it needs no label; under `"off"` nothing is one.
- The author decides, never the branch name or the fork flag: a fork can name its branch `kelpie/5`. `OpenPullRequest.author_is_bot` is the listing's `author { __typename }` being `Bot`. `crates/adapters/src/adapters/gh/board.rs`
- `ProjectState.contributions` holds `Contribution { pull_request, author, head, seen }`, one per pull request, oldest first; `head` is the head commit, `seen` when that head was first seen. Absent in a file saved before version 40. `crates/core/src/state/contributions.rs`
- `Runner::follow_contributions` runs once a board poll before the rotation, so it runs with every slot held. It reads the pass's listing, which the board then shares. A new head updates the record; a pull request closed, merged, a draft again, or unlabelled under `label` drops it, with no post. An unreadable listing or login changes nothing.
- `adopt_waiting` leaves a labelled contribution alone when `pick` is not `off`. With `off` a labelled fork is refused as before. `rework_asked` still refuses a fork.
- A released contribution is queued for adoption by `follow_adoption` and adopted in the next free slot, ahead of a new issue: `contribution-release.md`. Its record is gone once its work item is open.
- `status` lists the records as `contributions`, absent when empty.

## Remote

- `worktree::Remote`, where a work item's branch lives (`Origin` or a contributor's fork), and the adoption of a released contribution: `contribution-release.md`. #631, #632, #634

## CI on a trusted head

- GitHub holds a fork's first CI runs until a maintainer approves them, and approving runs the contributor's code. `Forge::approve_workflow_runs(repo, head)` lists the runs with `status=action_required` and that `head_sha`, drops any whose `head_sha` differs or whose `status` and `conclusion` are neither `action_required` (the query matches either field), and approves each. Kind `approve_workflow_runs`, REST. `crates/adapters/src/adapters/gh/runs/mod.rs`
- Kelpie approves as the maintainer's `gh` login, from the runner outside any sandbox, as it merges. Moving to the App's token, if it can approve, is #636.
- `follow_ci` (`runner/contributions/ci.rs`) runs at the end of `follow_contributions`, once a poll. A head is approved only when it equals `passed`: a flagged, unread or stale head never is, and a yes to a flagged head's ruling makes it `passed`, so the next poll approves it. A new push clears `ci`, so the new head waits for its own pass.
- `contributions[].ci` is `{ head, since, state }`, left out until the approval succeeds. `since` is the approval, and `state` is `waiting`, `green` or `{ red: [failed checks' names] }`.
- A rollup is trusted `CHECKS_SETTLE` (two minutes) after `since`, as at the gate, and only when `Forge::pull_request` reads the same head. No check or a pending one stays `waiting`. A skipped rollup is `green`: a contribution is never a draft. With `ci.block` off the state is `green` at the approval, as the gate passes a project without CI.
- A refused approval or an unreadable pull request logs and changes nothing: the next poll tries again, and no poll fails. A call that listed no held run is an approval. A run GitHub holds after that listing is approved by listing again at each poll while the settled rollup shows no check or a pending one. State version 50.
- No reviewer reads a contribution before green (ADR 0008; the pass, `contribution-pass.md`, enforces it).

## A review bot in the list

- A bot in `contributions.reviewers` reads in its turn, through the work item's round (`runner/contributions/bots.rs`). Its turn is no call: the runner steps it each board poll, after `follow_ci`, with no slot. The walk waits in `contributions[].walking` (`past`, `ran`, `missed`, and `bot`, the turn's `ReviewStage`) until the bot answers or is passed over.
- It is summoned on the head by its label (off, then on) or by its full-review comment, under the dog's lease. The `Summoned` stage is saved before the comment is posted, so a restart never posts it twice, and an unanswered summon is not sent again. The lease row has no issue. `paths` are not checked.
- Passed over, so the next reviewer reads and the local agent at the end is the backstop: a bot whose window opens more than `FAR` (an hour) on, one that refuses with such a window or declines, and one silent, processing or failed its `review_wait` after its summon (45 minutes unless its agent file says), and CodeRabbit where the repo is not public. A pass every reviewer was passed over for ends unread, with each reason.
- A review of the head ends the pass once its threads settle (two reads that agree, 60 to 120 seconds, as `lands`). Its open threads above a nit are `pass.findings`, its nits are `pass.nits`, and the bot is `last_answered`. Kelpie posts nothing for it.
- A push or a dropped pull request clears the walk, takes the label off and returns the lease or the ask, unless another turn needs it. Threads recorded on a head are not carried to the next unless the bot reads that head too.
