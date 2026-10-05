# One agent builds, reviewers run once in series

Kelpie's flow grew ahead of the evidence: a planning call, a review loop that alternates reviewers until two clean rounds, a judge on every finding, the deep round's confirming and re-check sessions, a separate whole-issue check, three review bot profiles, three harnesses, 21 ruling kinds and 15 timing phases. The experiments repo has since measured most of it (series 2 to 6, 2026-09-29 to 10-04):

- No plan-then-implement arm beat one agent with no plan, and a shared plan spread its bugs into every build made from it. The issue's acceptance criteria moved correctness more than anything else.
- Kelpie's own review gate found 0 of 52 known bugs, and its judge kept 69% of the findings that were not real. A defect-hunting prompt on Opus 5.5 at high effort found 27% at 82% precision in one round, and a second fresh session shown the first's findings added 11 points at 95%.
- A reviewer that runs code proved 26 of 26 of its findings real, but found fewer. It confirms rather than replaces.
- A fixer's "fixed" was wrong for one bug in four.
- qwen found 1 of 52 at 8% precision. No pull request bot showed a measured gain.

So the flow is: the issue says what done means, one agent builds it inline, and a list of reviewers each read it once, in order, each followed by one fix turn. Whatever the last fix leaves is what goes to CI and the merge. There is no loop and no judge. Where a reviewer leaves a failing test, kelpie runs it before and after the fix and reports both, in place of a session that asks whether the fix worked.

An agent is a definition file, like Claude Code's own agents: harness, model, effort, prompt, tools and what it is for. Kelpie embeds its defaults at compile time and `add` writes them out, so the maintainer edits a file rather than a settings table. A project names its implementers and its reviewers in order. A pull request bot is a reviewer on a bot harness, off by default.

Splitting and picking the implementer move to the issue: an issue-writer agent researches a request, scopes it to one pull request's worth or splits it, and labels the agent that should build it, before kelpie sees it.

## Considered options

- **Keep the deep round (shep-pm/shep-kelpie#284).** It is the closest of the current shapes to what was measured, but it is a state machine of five session kinds with its own snapshots and pins, and its re-check session asks a model what kelpie can learn by running the tests.
- **Keep the review loop with a better prompt.** Its end condition, two clean rounds in a row, spends rounds on a reviewer that keeps finding nits, and the measured gain is in the first two reads.
- **Keep the planning call, off by default.** It already ships off. Leaving it in keeps its sub-issue, blocker and ruling code alive for a feature nobody runs, and the issue-writer does the same job before the work item exists.

## Consequences

- Removed: the planning call, the judge, the review loop and its guards, the deep round's confirming and re-check sessions, the separate whole-issue check (each reviewer checks the acceptance criteria in its own pass), the GPU-wait timing, shots and the preview (parked, not designed out).
- Rulings come down to five kinds: `merge`, `question`, `stuck` with its reason as a field, `agent-files` and `foreign-change`. Timing phases come down to six: `worker`, `review`, `ci`, `ruling`, `merge` and `other`.
- Claude Code, Codex and pi stay behind the agent port as they are. A harness is not added or changed in this cleanup.
- The state file and settings change shape. A project in flight is drained before the upgrade, and an older state file is refused with a message saying so.
- The project manager's agent, which picks work and unsticks items, waits on the experiments repo's series 7. Until it reports, the board picks by priority, then age, as it does now.
