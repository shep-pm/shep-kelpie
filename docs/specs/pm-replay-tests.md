# Project manager replay test

Which values of `RulingName::pm_default` change, kind by kind (#1814). Each ruling the maintainer answered by hand is run again through the project manager as it runs today, and its answer is compared with the one that stood. Nothing here is written after a result: this file stands first, and the case file and wake prompt it names are those #1805 to #1810 leave standing. If one of those names moves before a run, this file moves with it first.

The code, the rebuilt case files and the raw answers live in the experiments repo, under `pm-replay/`. Its `PLAN.md` holds the same rules as this file, and the bar each kind must pass.

## Material

Lines of `<project>/rulings.jsonl` the maintainer answered by hand, joined to the `Ruling` step of `<project>/steps.jsonl` by the ruling's `id` and the item's `issue`. A line holds the `kind`, `id`, `issue` and `pull_request`, when it was `raised`, the `answer` that stood, `chose` when that answer was one of the ruling's options, and `stem` for a failed turn. Its `asked` is the card's opening only: `asked_of` cuts the options table at `| | Option` and clips what is left at 1200 characters. The question as it was worded comes from the step, which holds it whole, appended and never rewritten.

- A ruling the project manager answered is not material: it has no human answer to compare against.
- A line with no `raised` is not material either, since there is no moment to cut the case at. The file holds only the rulings answered from the build that added it.
- A line naming no `issue` is a contribution's ruling, or an older line with no work item open. The project manager answers neither. A contribution's `kind` can still read `question`, since `RulingName::of` maps its question, waiting, no-edits and cannot-adopt rulings onto that name.
- A `turn-failed` line whose `asked` carries the card's setup title, the one that names a turn kelpie could not set up, is not material either: that ruling is never the project manager's.
- A line whose `stem` matches an earlier one the project manager answered on the same item is not material either: kelpie never asks it about the same failure twice.
- Two things no record keeps are left to the hand reading under "The compare". A drain is runner memory, in no line of either file, so a ruling raised in one reads like any other. And `pm_answers_ruling` puts no ruling with fewer than two options to the project manager, a count read off the rebuilt card rather than off the line. Both stay in the material and in the counts, and neither counts toward a kind's bar, which only disagreements can meet.

## The case

Each ruling's case is rebuilt from `cases/<issue>.md`'s parts, each as it stood at `raised`:

- the ruling's card: the question as it was worded at `raised`, from the `Ruling` step's text, with its title, facts and options as `Card::of(kind)` words them at the run's commit (`crates/runner/src/runner/voice/card/mod.rs`). The effect kelpie worded for an option when it raised the ruling sits in `Ruling.effects`, kept in the state file while the ruling waits and gone after, so the call sees each option worded as the kind words it by default.
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

Where a part the ruling turns on cannot be rebuilt, the ruling is left out of the run and counted, and the results file says which part was missing and how many rulings each part cost. An issue edited after `raised` with no history kept, a transcript no longer on disk, a spend the ledger cannot rebuild, a branch rewritten since `raised`, and a ruling whose `id` has no `Ruling` step to read its question from are the likeliest.

## The call

One call per ruling, in a fresh session: no resume, and no other ruling in view.

- The project manager's agent file (`agents/pm.md`) and wake prompt (`crates/runner/src/runner/pm/prompt/`) at one named commit, the same for every ruling in the run, recorded in the results file.
- The harness, model and effort the plan names, the same for every ruling.
- A folder holding the rebuilt case, the item's diff as its branch stood at `raised`, and a board cut the same way: the item's own line, and the rulings waiting at `raised`, read from the `rulings.jsonl` lines with `at` before it. The ready queue and the other open items are not rebuilt, so a ruling whose answer turns on what else was open on the board is left out and counted. A branch rewritten since `raised` holds no state to diff, and its ruling is left out too.
- No write reach, and no worktree or checkout either: those hold the code as it stands now, which is after `raised`. The answer is read from the reply, never from a file the call wrote.
- The answer is the JSON the wake asks for, read as `crates/core/src/pm_answer/` reads it. An answer kelpie cannot read is an abstention, counted per kind, never a wrong answer.
- Nothing is filed, posted, retried or merged. The replay answers, and acts on nothing.

## The compare

Per kind, the replay's answer against the answer that stood. `chose` is the letter the maintainer's answer was, worked out from the kind as it stood when the ruling was answered, and the replay's answer is read against the options the rebuilt card lists. The same letter, or the same yes or no where the card lists none, is an agreement. Anything else is a disagreement. A letter the rebuilt card does not carry is read by hand first, and the results file counts those apart, since they can come from the card's wording changing rather than from the project manager.

A disagreement is read by hand, since the project manager may have been right. Each is marked by what the wrong answer would have done:

- harmless: nothing the maintainer would have had to see changes
- a cost: a read the maintainer did not need, a turn a worker did not need, a settled ruling reopened, or money spent
- a harm: something merged that should not have been, a stranger's text reaching an agent that answers it, a spend cap passed, or a decision that is the maintainer's answered as if it were not

The maintainer reads each disagreement and gives its mark, with the reason, kept in the experiments repo. The same reading says whether the project manager would have received that ruling at all, which is how a ruling raised in a drain, or one whose card listed fewer than two options, is told apart. Such a ruling stays in the counts and never counts toward a kind's bar.

## Deciding rules

Written before any run.

1. A kind's default changes on its disagreements alone, never on its agreements: only when they pass the bar the plan sets for it, and only when the material holds the number of rulings the plan asks for. Both numbers are in the plan, not here.
2. A kind short of that number keeps its default, and the results file names how many rulings it had.
3. A kind whose disagreements are all harmless keeps its default.
4. `security` and `contribution-merge` hold no default to change: `pm_default` names none for them and `Rulings::refusal` refuses either as `true`, so the replay reports on them and moves nothing. Changing one needs a code change in an issue of its own first. `merge` changes only by the maintainer's word, whatever the replay says: it is the one key for unattended merging, and a warned head still goes to the maintainer.
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
