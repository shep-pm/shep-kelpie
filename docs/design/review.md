# Review

Review passes and the review bots. Rulings are in rulings.md, and CI and the merge in merge.md.

## Review

- One pass at green CI on draft head: `reviews.need` reviewers (2), once each; no loop or judge. A pass's candidates are every reviewer file `agents.allow` holds but `contributions.security_reviewer`, and a project naming no agent in `agents.allow` takes only those its gateways can route. ADR 0006, ADR 0008, ADR 0018, #1544, `runner/review/`
- Each round is the agent board's pick; the pass ends at `reviews.need` reads. `review-pick.md`. #1539
- Any finding, nits included, goes to one fix turn; next reviewer reads the fix (skipped nits left open threads). Nit-only fix starts nothing new; bot nits on a head it read stay open. ADR 0007, #354
- A round waits for CI on its head (red costs no read or window); `doctor` names CI that skips drafts. A head behind `main` whose checks settle with none (a conflict runs no CI; a draft's lone skipped bot check counts) is caught up and returns; one with CI is not. None at all: merge.md. #423, #1163, #1194
- A contribution's head gets its held runs approved only after the security review, CI in `contributions[].ci`, and no reviewer reads it before `green` (ADR 0008); one pass of `contributions.reviewers` records findings, posting none. A released one's work item runs the project's whole review pass (`contribution-release.md`). #622, #623, #634
- Pass restarts only on a new head that needs it (merge `rework`, accepted foreign change, rework). ADR 0006
- Merge ruling `no <note>` = fix turn back to the ruling, no new pass; restarts pass only after unread/open-thread warning. #354
- Reviewer = agent file on session harness (read-only), `command`, `endpoint` or `bot`; runs no commands (running tests found fewer). #314
- `paths` limit a reviewer to matching changes; every round reads `<base>...HEAD`. Gets the issue title and acceptance criteria (4,000 bytes). #314, #484
- `defect-hunter` (Opus 5.5 high, maintainer's prompt verbatim) uses `second_look`. Contribution default: `iq3-xxs-review` (endpoint on `paddock`) where kelpie holds that gateway, then `defect-hunter-lite` (same prompt, medium, none); `add` writes it. #1224, `crates/core/src/settings/reviewers/mod.rs`
- `standards` and `spec` (Opus 5.5 high, one read, no `second_look`) are the two axes of the vendored `code-review` skill, one agent each, no sub-agents. Smells are `LOW`; `add` writes both, and they are candidates like every other reviewer file. `agents/standards.md`, `agents/spec.md`, `crates/core/src/agents/mod.rs`. #363
- Reviewer failing 3 times passed over; pass nobody read marks item unreviewed (an outage can't auto-merge). #314
- Round reads only pushed head; dirty worktree gets one turn to push, then parks `unpushed` (CI and merge take `origin`'s head). #290
- Merge trusts only heads a round read or kelpie sent to CI unread on purpose; others raise `unread_head`. #290
- Fix turn counts only if it moves `origin` head, unless every finding is deferred or disputed with a reason (`disputed-findings.md`), or it got only nits; else parks `fix-not-pushed`, naming what was disputed and deferred. A dispute goes to the next reviewer, or one pull request comment when none follows; a second dispute of the same finding parks. #40, #354, #802
- Clean reply is exactly `CLEAN`. #314
- Local reviewers (`command`, `endpoint`), their lock and the rounds kept for replay: `local-review.md`. #107, #145, #502, #849
- One finding shape for every reviewer kind; each bot has a tested profile. #123
- With `ci.test_command`, a reviewer on any harness may attach a failing test, held to `review-tests` in the build folder by `kelpie confine`: Claude Code `Write`, pi `write` (guard extension), goose `write` and Codex `apply_patch` (hooks; shells read only). The test runs before/after fix and is recorded. Still failing after the fix: worker gets one more fix turn on it (`sent_back` on the check), then `stuck` `test-still-fails`; yes = one more turn, no = check ends and pass goes on. A timeout or no run never acts. The public line names no output. #517, #518, #519
- Deferred finding kelpie sent is filed after merge in kelpie's words (nits dropped); a disputed one never, even if also deferred. `git.issues`: `ask` (default, `follow-up` ruling), `file` (as `needs-triage`; auto-filed were often false), `skip`. #142, #380, #384, `runner/follow_up/`

## Bots

- Bots = `harness: bot` reviewer files named for the bot (its account owns the window): CodeRabbit 1 in 1h, cubic 20 in 720h, Codex 10 in 168h. #315
- Bot round marks draft ready (CodeRabbit skips drafts), takes dog lease, summons; never summons a head a review covers. #65, #315
- Threads read until two reads agree (forge lists review before threads). A round's reads run off the runner lock (`runner-lock.md`). #350, #1208
- Worker marks each bot thread `fixed` or `declined`; kelpie replies and resolves fixed ones, worker never posts. #367, `crates/core/src/work_item/threads.rs`
- Merge yes accepts the threads it named. #455
- Writer comments and review threads (owner, member, collaborator; other Apps ignored and logged) read at round start, green CI and while a merge ruling waits, batched into one noted turn; ambiguous ones become a `question` ruling; replies as #367; a comment while a merge ruling waits withdraws it as `no <note>`; 3 rounds per item then a `question` ruling. #368, `runner/talk/`
- Bot passed over after far window, its `review_wait` (45 min) with no review, or decline; at `ANSWER_WAIT` if it never reviewed there (`not-installed`); re-read at green CI and every 5 min while merge ruling waits. #351, #399, #1320
- A bot passed over as `not-installed`, or because the repo is not public, is left out of every pick for 7 days (`bots_left_out`), and comes back sooner when kelpie sees it reviewing; one passed over for its window, its silence or its settings stays a candidate. A pass left with nobody but a left-out bot marks the item unreviewed naming that bot, why, and when it comes back. #1541, `core/state/bots.rs`, `runner/review_bot/`
- Open bot threads above nit block `auto`; nits named apart, never block. #350, #351
- Unheard summon resent once after 15 min, same lease. #122
- CodeRabbit gets `full review` on a PR it read or after catch-up (label sees only new commits). Clean catch-up with `main` summons no bot (own diff unchanged; CI checks combined code). #100, #106
- cubic summoned by comment (no label); Codex by `@codex review` or marking ready; `P0` to `P3` badges are severities. #124, #126
- Bot `rounds` caps reads per PR; an adopted or released PR owes each bot a fresh summon (`contribution-release.md`). #84, #158, #632
