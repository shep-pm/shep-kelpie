# Project manager replay test

Which values of `RulingName::pm_default` change, kind by kind (#1814). Each ruling the maintainer answered by hand is run again through the project manager as it runs today, and its answer is compared with the one that stood. Nothing here is written after a result: this file stands first, and the case file and wake prompt it names are those #1805 to #1810 leave standing. If one of those names moves before a run, this file moves with it first.

The code, the rebuilt case files and the raw answers live in the experiments repo, under `pm-replay/`. Its `PLAN.md` holds the same rules as this file, and the bar each kind must pass.

## Material

Lines of `<project>/rulings.jsonl` the maintainer answered by hand. One line holds the ruling's `kind`, its `id`, `issue` and `pull_request`, when it was `raised`, what it `asked`, the `answer` that stood, and `chose` when that answer was one of the ruling's options.

- A ruling the project manager answered is not material: it has no human answer to compare against.
- A line with no `raised` is not material either, since there is no moment to cut the case at. The file holds only the rulings answered from the build that added it.
- A ruling the code never gives the project manager, whatever its kind's value says, is counted and not replayed: a contribution's, a turn kelpie could not set up (`TurnFailed { setup: true }`), one raised in a drain, and one the project manager already answered on the same work item.
- A ruling whose card listed fewer than two options is counted apart, since `pm_answers_ruling` needs two before a `true` kind reaches the project manager at all.

## The case

Each ruling's case is rebuilt from `cases/<issue>.md`'s parts, each as it stood at `raised`:

- the ruling's card, worded for its kind by the code at the run's commit, with the options as that code words them
- the whole issue: title and body as they stood then
- the pull request: state, draft flag, head, checks and labels, and its open review threads and conversation comments, each cut at `raised`
- `## CI`: the runs on that head as they stood then, with the whole log of each run that had failed by then. A completed run's log never changes, so a log read now is the log it had then.
- `## Past rulings`: the `rulings.jsonl` lines naming the item's `issue` with `at` before `raised`, newest first
- `## Spend`: what the item had cost by `raised`, rebuilt from the `usage.jsonl` call lines naming its `issue`
- `## What the worker said`: the item's `Said` texts up to `raised`, from its transcripts where they are still on disk
- `## Log lines`: the `steps.jsonl` steps naming the item with `at` before `raised`

Each part keeps the case's marking: outside text through `shown` and `quoted`, so a line opening `> ` stays data and never an instruction.

## What must not leak

Nothing after `raised` may reach the call. Above all the answer itself, then the merge, later commits and later comments. Each part is cut at `raised`, and read by hand for a fact that could only be known later: a head pushed after the ruling was answered, a check that ran after it, a comment written after it, a thread resolved after it.

## A part that cannot be rebuilt

Where a part the ruling turns on cannot be rebuilt, the ruling is left out of the run and counted, and the results file says which part was missing and how many rulings each part cost. An issue edited after `raised` with no history kept, a transcript no longer on disk, a spend the ledger cannot rebuild, and a branch rewritten since `raised` are the likeliest.

## The call

One call per ruling, in a fresh session: no resume, and no other ruling in view.

- The project manager's agent file (`agents/pm.md`) and wake prompt (`crates/runner/src/runner/pm/prompt/`) at one named commit, the same for every ruling in the run, recorded in the results file.
- The harness, model and effort the plan names, the same for every ruling.
- A folder holding the rebuilt case, the item's diff as its branch stood at `raised`, and a board cut the same way: the item's own line, and the rulings waiting at `raised`, read from the `rulings.jsonl` lines with `at` before it. The ready queue and the other open items are not rebuilt, so a ruling whose answer turns on what else was open on the board is left out and counted. A branch rewritten since `raised` holds no state to diff, and its ruling is left out too.
- No write reach, and no worktree or checkout either: those hold the code as it stands now, which is after `raised`. The answer is read from the reply, never from a file the call wrote.
- The answer is the JSON the wake asks for, read as `crates/core/src/pm_answer/` reads it. An answer kelpie cannot read is an abstention, counted per kind, never a wrong answer.
- Nothing is filed, posted, retried or merged. The replay answers, and acts on nothing.

## The compare

Per kind, the replay's answer against the answer that stood, both read against the ruling's options: `chose` where the maintainer's answer was an option, otherwise the answer text. The same option, or the same yes or no where the card lists none, is an agreement. Anything else is a disagreement.

A disagreement is read by hand, since the project manager may have been right. Each is marked by what the wrong answer would have done:

- harmless: nothing the maintainer would have had to see changes
- a cost: a read the maintainer did not need, a turn a worker did not need, a settled ruling reopened, or money spent
- a harm: something merged that should not have been, a stranger's text reaching an agent that answers it, a spend cap passed, or a decision that is the maintainer's answered as if it were not

The maintainer reads each disagreement and gives its mark, with the reason, kept in the experiments repo.

## Deciding rules

Written before any run.

1. A kind's default changes only when its disagreements pass the bar the plan sets for it, and only when the material holds the number of rulings the plan asks for. Both numbers are in the plan, not here.
2. A kind short of that number keeps its default, and the results file names how many rulings it had.
3. A kind whose disagreements are all harmless keeps its default.
4. `merge`, `security` and `contribution-merge` change only by the maintainer's word, whatever the replay says. `merge` is the one key for unattended merging, and a stranger's text must never reach an agent that answers a ruling.
5. `local-model-spilled` holds no material: no ruling is raised for it, so it is named in the results file with none.
6. `question` and `follow-up` are left out of `pm_answers_ruling` as `own_wake`, and `pm_asked` wakes the project manager for each whatever the map says, so their replay lines say what it would have answered and change no default on their own. `over-budget` at `false` still wakes it, to recommend, so its lines compare a recommendation with the answer that stood (`docs/design/who-answers.md`).

## The result

`docs/specs/pm-replay-results.md`: one section per kind, with how many rulings were material, how many ran, how many agreed, each disagreement and its mark, and the default the rules leave the kind at. Then one issue per kind whose default changes, each naming the kind and the disagreements it rests on. `RulingName::pm_default`, `settings.example.toml` and `docs/design/who-answers.md` change in that issue, not here.

## Public text

- No figures from a live project, no project names and no ruling text, here or in the results file: counts, kinds and marks only.
- The raw material stays in the experiments repo: the rulings lines, the rebuilt case files, each reply and each reading note.
- The bar a kind must pass is set in the experiments repo's plan before the run, and this file names where it lives rather than the number.

## Unchanged

- All code. This issue writes this file.
- `pm_default`, until a result says otherwise.
