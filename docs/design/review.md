# Review

Review passes and the review bots. Rulings are in rulings.md, and CI and the merge in merge.md.

## Review

- One pass at green CI on draft head: the item's review plan's reads, `need_of` where its pick planned none (`review-pick.md`), once each; no loop or judge. A pass's candidates are every reviewer file `agents.allow` holds but `contributions.security_reviewer`, and a project naming no agent in `agents.allow` takes only those its gateways can route. ADR 0006, ADR 0008, ADR 0018, #1544, `runner/review/`
- A review call ends at its own ceiling, `REVIEW_CEILING` (1h, no setting): past it the round fails like any failed round, and its ledger line reads `timed-out`. The ask reaches a session call, not a local round's command. `runner/flight/` #1711
- Each round is the agent board's pick, and a finding above a nit may ask for one more read (`review-more.md`). #1539
- Any finding, nits included, goes to one fix turn; next reviewer reads the fix (skipped nits left open threads). Nit-only fix starts nothing new; bot nits on a head it read stay open. ADR 0007, #354
- A round waits for CI on its head (red costs no read or window); `doctor` names CI that skips drafts. A head behind `main` whose checks settle with none (a conflict runs no CI; a draft's lone skipped bot check counts) is caught up and returns; one with CI is not. None at all: merge.md. #423, #1163, #1194
- A contribution's head gets its held runs approved only after the security review, CI in `contributions[].ci`, and no reviewer reads it before `green` (ADR 0008); one pass of `contributions.reviewers` records findings, posting none. A released one's work item runs the project's whole review pass (`contribution-release.md`). #622, #623, #634
- Pass restarts only on a new head that needs it (merge `rework`, accepted foreign change, rework). ADR 0006
- A head no reviewer read, and a fix no reviewer read, go back through a pass of the review where a project manager rules on the item's threads: gate-unread.md. #290, #354, #1826
- Reviewer = agent file on session harness (read-only), `command`, `endpoint` or `bot`; runs no commands (running tests found fewer). #314
- `paths` limit a reviewer to matching changes; every round reads `<base>...HEAD`. Gets the issue title and acceptance criteria (4,000 bytes). #314, #484
- `defect-hunter` (the maintainer's prompt verbatim) is a baseline that binds nothing; `agents/models.toml` binds its claude-code row with `second_look`. No reviewer is a default: `contributions.reviewers` is refused while unset. #1224, `crates/core/src/settings/reviewers/mod.rs`
- `standards` and `spec` (one read each, no `second_look`) are the two axes of the vendored `code-review` skill, one agent each, no sub-agents. Smells are `LOW`; `add` writes no file for either, and each is a candidate like every other reviewer file once its own file binds it. `agents/standards.md`, `agents/spec.md`, `crates/core/src/agents/baseline.rs`. #363
- Reviewer failing 3 times passed over; pass nobody read marks item unreviewed (an outage can't auto-merge). #314 A pass short of its item's reads marks it too, naming the count and who did not read ("1 of 2 reviewers read it: ..."); a bot's closed window is no read; a pass the project's own list leaves short (`paths`, a bot's `rounds`) is not marked. A local round that reviewed no file is no read, and a reviewer passed over in one pass counts again in the next. #1595, `runner/review/lineup/mod.rs` (`pass_unread`)
- Round reads only pushed head; dirty worktree gets one turn to push, then parks `unpushed` (CI and merge take `origin`'s head). #290
- Fix turn counts only if it moves `origin` head, unless every finding is deferred or disputed with a reason (`disputed-findings.md`), or it got only nits; else parks `fix-not-pushed`, naming what was disputed and deferred. A dispute goes to the next reviewer, or one pull request comment when none follows, naming build-folder files by name alone; a comment the guard or a hook refuses raises the merge ruling, whose yes merges without posting (even under `auto`), and any other post failure retries; a second dispute of the same finding parks. #40, #354, #802, #1622, `runner/review/disputes.rs`
- Clean reply is exactly `CLEAN`. #314
- Local reviewers (`command`, `endpoint`), their lock and the rounds kept for replay: `local-review.md`. #107, #145, #502, #849
- One finding shape for every reviewer kind; each bot has a tested profile. #123
- With `ci.test_command`, a reviewer on any harness may attach a failing test, held to `review-tests` in the build folder by `kelpie confine`: Claude Code `Write`, pi `write` (guard extension), goose `write` and Codex `apply_patch` (hooks; shells read only). The test runs before/after fix and is recorded. Still failing after the fix: worker gets one more fix turn on it (`sent_back` on the check), then `stuck` `test-still-fails`; yes = one more turn, no = check ends and pass goes on. A timeout or no run never acts. The public line names no output. #517, #518, #519
- Deferred findings, in kelpie's words, no nits or disputed ones: with `agents.pm`, ruled before the merge (review-more.md), else after it as a `follow-up` ruling. Yes files `ready-for-agent`, or unread `needs-approval` as the PM asks. `git.issues` is refused. ADR 0020, #1776, `runner/follow_up/`

## Bots

- Bots = `harness: bot` reviewer files named for the bot (its account owns the window): CodeRabbit 1 in 1h, cubic 20 in 720h, Codex 10 in 168h. #315
- Bot round marks draft ready (CodeRabbit skips drafts), takes dog lease, summons; never summons a head a review covers. #65, #315
- Threads read until two reads agree (forge lists review before threads). A round's reads run off the runner lock (`runner-lock.md`). #350, #1208
- Worker marks each bot thread `fixed` or `declined`; kelpie replies and resolves fixed ones, worker never posts. #367, `crates/core/src/work_item/threads.rs`
- Merge yes accepts the threads it named. #455
- Writer comments and review threads (owner, member, collaborator; Apps ignored) read at round start, green CI and while a merge ruling waits, one noted turn; ambiguous ones a `question`; replies as #367; a comment under a waiting merge ruling withdraws it as `no <note>`, with `agents.pm` the maintainer's text waits for the PM; 3 rounds, then a `question`. #368, `runner/talk/`
- Bot passed over after far window, its `review_wait` (45 min) with no review, or decline; at `ANSWER_WAIT` if it never reviewed there (`not-installed`); re-read at green CI and every 5 min while merge ruling waits. #351, #399, #1320
- A bot passed over as `not-installed`, or because the repo is not public, is left out of every pick for 7 days (`bots_left_out`), and comes back sooner when kelpie sees it reviewing; one passed over for its window, its silence or its settings stays a candidate. A pass left with nobody but a left-out bot marks the item unreviewed naming that bot, why, and when it comes back. #1541, `core/state/bots.rs`, `runner/review_bot/`
- Open bot threads above nit block `auto`; nits named apart, never block. #350, #351
- Unheard summon resent once after 15 min, same lease. #122
- CodeRabbit gets `full review` on a PR it read or after catch-up (label sees only new commits). Clean catch-up with `main` summons no bot (own diff unchanged; CI checks combined code). #100, #106
- cubic summoned by comment (no label); Codex by `@codex review` or marking ready; `P0` to `P3` badges are severities. #124, #126
- cubic's refusal is read from its check run on the head titled "AI review line limit reached", which kelpie reads beside its comments, reviews and threads, after any review covering the head. Its summary names the day reviews resume and the dog holds cubic until then, past an hour on, the pass goes on without it; a run whose named day has come and gone is no refusal, and the window's own definition decides then. A comment naming a limit is a refusal too, with no opening. #1641, `core/cubic.rs`
- Bot `rounds` caps reads per PR; an adopted or released PR owes each bot a fresh summon (`contribution-release.md`). #84, #158, #632
