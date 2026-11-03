# CodeRabbit

- The free plan reviews public repos only.
- The included quota on shep was 1 review an hour when last read.
- The control room measured about six real findings from a full review to one from an incremental review on shep (#100). It has changed without notice before, so kelpie reads it from each review's footer.
- Kelpie's `coderabbit` agent file holds that window, 1 review in 1 hour, and a footer's quota still overrides it in the dog's book (shep-pm/shep-kelpie#315).

Read 2026-09-27 from CodeRabbit's 504 comments on shep since 2026-08-01, and its reviews and threads on shep#598, #615 and #617. The recorded ones are in `fixtures/`.

- A clean review posts no review object. shep#615 was reviewed clean at its head, and only its walkthrough comment says so, footer included.
- The walkthrough carries the commit it read in hidden markers: `final_review_risk_coverage` with a `coveredCommitId` (129 comments, 126 of kind `reviewed` and 3 `target_branch_merge_carry_forward`) and `change_assessment_commit` (88). On shep#598 the coverage marker named ce143d9 while the limit block beside it named the head.
- Footers read "up to 1 included review per hour" 40 times and "up to 10 included reviews per hour" 91 times, each with "N remain(s) after this review".
- Waits are quoted in minutes ("in 49 minutes", "in 1 minute") or seconds ("in 59 seconds"), in the walkthrough as "Next included review available in" and in a reply to a comment summon as "next included review will be available in". No wait of an hour or more was seen.
- Inline findings open with a label: Minor 523 times, Major 185 and Trivial 65. Kelpie reads Critical and Major as high, Minor as medium and Trivial as low, and the worker is sent that severity.
- The inline comment's shape changed twice. Read 2026-11-02 from CodeRabbit's comments on #1602:
  - On 2026-10-15 the first line was a label, `**🎯 Functional Correctness** | **🟡 Minor** | **⚡ Quick win**`, with no HTML comment above it.
  - Since 2026-10-30 the first line is `<!-- coderabbit-ai-generated:v1 -->`, and the label line under it is `**<picture><img src="…/major-16.svg" …></picture> Major** · **Stability & Availability** · **Heavy lift**`, the severity word beside an icon. The bold title and the reasoning paragraph follow, then `<!-- ai-agent-prompt:start -->` and a `<details>` block.
  - Kelpie reads the label as the first line past the HTML comments, takes the severity from the word it names, and takes the title and reasoning from the paragraphs after it. `fixtures/coderabbit-inline-major.md` and `fixtures/coderabbit-inline-minor.md` hold the shape; their reasoning paragraphs are rewritten, not recorded.
- A review in progress was never recorded, since CodeRabbit edits it over. Kelpie reads the shape the maintainer's review driver documents.

Live run on #12's build, 2026-09-28, a throwaway project on the public scratch repo shep-pm/kelpie-scratch under its own shepherd with this build's dog:

- The worker, the qwen and Claude review rounds, green CI, the dog's grant and the summon all ran end to end on kelpie-scratch#2. CodeRabbit never answered: no comment, no check, not even the skip notice it posts on every shep pull request. The org's CodeRabbit install covered selected repos, and kelpie-scratch was not one of them until the maintainer added it afterwards.
- With no answer, the round counted the summon as spent after ten minutes and gave the lease back. The dog's window showed that summon and opened an hour after it.
- A worker whose kelpie home sits under `~/.claude` cannot write its worktree or build folder: the sandbox refused both although both were on its allowlist. It ended its turn on a question about it, twice.
- CodeRabbit skips a draft on the label (#65, from the run on #17). The maintainer's call: a round marks the pull request ready before its first summon.
  - On kelpie-scratch#3 kelpie's summon at 20:59:52Z, on a draft, drew "Draft PR not reviewed", and the round would have waited for a review that never came. Marking it ready and summoning by comment at 21:00:33Z was answered at 21:00:44Z.
  - CodeRabbit edits the skip notice over with the review, so it is not in the fixtures.

Read 2026-09-29 from shep#614 after kelpie's first adoption (#100). The comments and statuses are recorded in `fixtures/`.

- Kelpie's owed summon put the label on at 05:53:24Z, on a head that only merged `main` and changed nothing but `Cargo.lock`. CodeRabbit set its commit status to "Review in progress" at 05:53:31Z and "Review completed" at 05:53:45Z, posted no review, and edited its walkthrough to a "Review skipped" notice whose coverage marker still named the older head.
- CodeRabbit sets a commit status named `CodeRabbit` on the head for every summon: "Review in progress", then "Review completed", "Review rate limited" or "Review skipped: ...". The REST statuses of `refs/pull/<n>/head` list them with the commit in each status's URL.
- On a review with findings (shep#614 at 7850d0c) the review posted at 10:58:49Z and the "Review completed" status followed at 10:58:58Z.
- The forge can list a review before its threads (shep-pm/shep-kelpie#350).
  - On shep#703 CodeRabbit submitted its review at 14:04:31Z with two inline threads, both published at 14:04:31Z, and the runner's read at 14:04:33Z found the review and no open thread, so the round ended clean. Read afterwards, both threads were there, unresolved, not outdated and by `coderabbitai`, so it was not a login mismatch.
  - On the same pull request cubic was passed over at 14:07:56Z, its window by the dog's book opening more than an hour on, and reviewed the head anyway at 14:08:46Z with three threads, one P2. The merge ruling at 14:10:04Z named none of the five.
- A repo whose `.coderabbit.yaml` does not let the `review please` label start a review gets a commit status that declines the summon, and a "Review skipped" comment: `Review skipped: automatic reviews are disabled` or `Review skipped: excluded by label configuration`.
  - The second was recorded on shep#614 at 04:57:57Z, set by a push 55 minutes before the owed summon; the first is as the maintainer quoted it in shep-pm/shep-kelpie#399, with its fixture `fixtures/coderabbit-statuses-399.jsonl` made up around that text.
  - Kelpie reads such a status, newest on the head and set at or after the summon with no clock slack, as `Reading::Declined` and passes CodeRabbit over at once (`BotSkipped::Declined`), where it waited the two hours before.
  - A skip set by a push just before the label is not the answer to it. A newer "Review in progress" or "Review completed" wins, and a head marked done stays a clean read.
- CodeRabbit can miss a summon altogether. On shep#644 the runner added `review please` at 11:30:54Z and CodeRabbit set no status, edited no comment and posted no rate-limit notice afterwards, while it was answering elsewhere. Kelpie waited the two hours for the `coderabbit-silent` ruling. The summon it did hear on shep#614 set "Review in progress" 7 seconds after the label. Fifteen minutes is well past that, so no sign by then is read as unheard.
- Where the time went, read by hand from the runner's log and the sessions' transcripts on 2026-09-29: CodeRabbit's hourly window was 66% of a shep pull request's time, and qwen rounds 59% of kelpie's own. `timings` exists so nobody reads that by hand again; what it measures on the next live run belongs here.
- `@coderabbitai full review` is answered in a reply comment: "Full review finished." under "Action performed", or "Review rate limited" with "next included review will be available in N minutes" under "Action not completed". shep's `.coderabbit.yaml` gates automatic reviews on the label, and a full-review comment made on 2026-09-24 with the label off was still answered.
