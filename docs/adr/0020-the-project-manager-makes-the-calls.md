# The project manager makes the calls, and settings hold only what it cannot

Amends three lines of ADR 0018: the agent board's ranking at dispatch, the ruling kinds `rulings.pm_decides` refuses to the project manager, and the review count keys with the light review's HIGH step-up. It records a direction and moves no code: kelpie's settings have grown into rules that do the project manager's job, and the deciding moves back to it (the maintainer, 2026-11-05).

## Why

ADR 0018 cut the roles that decide down to one, the project manager, and moved agent choice from a guess made when an issue is labelled to a reading made when the work starts. It left the settings as they were, and those have grown the same job back:

- `[kelpie.agent_board] prefer` ranks the agents, first most wanted, ahead of cost. That is a pick, written in a file, for work items nobody has seen yet.
- `agents.slack` says how long an item may wait for a cheaper agent. `reviews.need`, `reviews.light`, `reviews.light_paths`, `reviews.full_paths`, `reviews.pm_lightens` and `reviews.light_max_lines` say how many reviewers an item gets and which changes count as small.
- `budget.per_item_usd` says what an item may spend, and `turn_timeout` when a turn is more likely stuck than busy.
- `settings.example.toml` holds 111 project keys and `kelpie-settings.example.toml` 12, counted by `scripts/settings-count.py`. `CODING_STANDARDS.md`'s "Fewer settings" already says a key is a fact kelpie cannot detect or a safety limit and that a judgement goes to the project manager. The keys above are judgements.

The maintainer's path is already the project manager: a ruling's thread, a comment on a pull request, `shep kelpie tell`. A settings file is a second path, and it makes the call for every item at once, including the ones nobody is looking at.

## Decision

### The maintainer talks to the project manager

- Free text on a ruling or on a pull request goes to the project manager, and it acts: park the item, send the worker back with the note, settle a finding, merge. `runner/pm/`, `runner/ruling/`, `runner/talk/`
- What it cannot carry out it names, and the item waits for the maintainer. A call it can make is never put in front of them.

### Each item's plan is the project manager's, set at the pick

- At the pick it names the item's agent and its review plan, how many reads it wants and what it wants them for, beside the tier and the hold it already sets. `runner/pm/picked.rs`
- `reviews.need`, `reviews.light`, `reviews.light_paths`, `reviews.full_paths`, `reviews.pm_lightens` and `reviews.light_max_lines` stop deciding that plan, and ADR 0018's rule that a HIGH finding makes a light review full for the rest of an item goes with them: a finding above a nit now asks the project manager instead. What stays is the code that weighs a diff and reports it, so the project manager sees what the change touches.
- After a round, kelpie asks for one more read only when that round sent a finding above a nit, medium or high, and the project manager rules on it. A round of nits, or of nothing, ends the pass as it does today. `runner/review/lineup/`

### No ruling reaches the maintainer when the map says so

- `rulings.pm_decides` may hold `true` for every kind it knows. With that, every ruling is answered by the project manager and none reaches the maintainer. `crates/core/src/settings/rulings.rs`
- This amends ADR 0018's refusal of `true` for `merge`, `security` and `contribution-merge`. What keeps a bad head out is the gate, not who answers the card: CI green on the latest `main` (ADR 0008), the review pass, the unread-head and borrowed-head checks. A contribution's security review still reads every head its author pushes before any of kelpie's calls run on it; the ruling its flag raises is now the project manager's to answer.
- A ruling the project manager answers keeps its posted answer and its `no <note>` reopen, as it does today.

### The dog reports who is available, and ranks no more

- The dog keeps its reading of every agent across projects: which files exist, which can start now, when each could finish and what its work has cost. It answers the runner's `pick` ask with that reading and chooses nobody. `crates/dog/src/dog/board/`
- The ranking goes: the slack window, the cost and tier order, and `[kelpie.agent_board] prefer`. `agents.slack` goes with them. An item the project manager wants to wait for an agent is held, the way it holds an issue.
- A handoff has no ranking to fall back on, so an item whose agent goes out waits for the project manager's next pick, which its wake carries.

### What a setting may hold

- The agent files, `rulings.pm_decides`, and facts kelpie cannot detect: the checkout path and the remote, a gateway's URL and the variable holding its key, a webhook's URL and token, the host that runs the end-to-end tests, whether the repo runs CI at all, which hosts beyond the package registries a worker may reach, which agents the project's owner lets its code run on (`agents.allow`), and which logins are bots.
- Nothing else is a setting. A key that decides is dropped, and either the project manager decides it for the item in front of it or kelpie derives it from what it can read.

### Hard limits stay in code, with no setting

- Red CI blocks a merge, and nothing turns that off. `ci.block` stays as the fact it names, that a repo runs no CI at all, which GitHub cannot tell kelpie; it is not a way to merge past a red run.
- The fence, the leases and the spend limits hold. No setting widens a worker's write grants or the hosts its call reaches, and no setting lifts a lease or an item's spend cap.
- The reason is one: the project manager reads text a stranger can write, an issue body, a comment, a bot's thread, a contributor's pull request. A limit that is to keep a bad call out holds whatever that text says, so it is code and not a knob.

### A decision rides on a wake that already runs

- Where kelpie already wakes the project manager, the decision goes into that wake: a free slot, a ruling to answer, a round's findings, a stuck item, a lease wait. No wake is added for a call a running wake could make. `runner/pm/wakes.rs`
- Each decision keeps its reason, on the ruling answer's `why` or in the journal, so the batch retro reads what was decided and what came of it. `runner/ruling/history.rs`, `runner/retro/`

## Considered options

- **Keep the settings as the rulebook.** Every call would need a rule written before the work, so the maintainer reads a file to change how one item runs, and the change lands on every item at once.
- **Keep the dog's ranking and let the project manager override it.** Two pickers on one item: the code's pick starts it and the project manager's changes it, and a wrong start costs the turn the override waits for. The availability reading is what both need, and the dog can report it without choosing.
- **Wake the project manager for each decision.** A model call for a call a wake already running could make, on a board it already read.
- **Let the project manager relax a gate.** It reads text a stranger writes. A gate that holds only while nothing suspicious was written is not a gate.
- **A per-item setting the maintainer edits.** `shep kelpie set` on one work item keeps the maintainer in the loop and leaves the deciding in a file, which is what this record moves.
- **Keep `merge` out of `rulings.pm_decides`.** The card is where a merge is asked about, and the safety that keeps a bad head out sits in the gate. Refusing the kind leaves the maintainer answering a form while the control lives elsewhere.

## Consequences

- This record moves no code. Each part becomes its own issue: the availability report in place of the ranking, the review plan at the pick, the kinds made settable, and the settings pass that drops the keys that decide.
- `scripts/settings-count.py`'s baseline comes down as those keys go, and a key that decides never raises it.
- The board has to show what the project manager decides from: each agent's availability, each round's findings with their severities, and each item's spend against its cap. Where it leaves a list out, the wake says how many it left out.
- A project with no `agents.pm` file keeps what it has today: kelpie's own rule picks, and every ruling reaches the maintainer. This record is about a project that has one.

## Not decided here

- The app's stack, and where shep fits (ADR 0001 and 0003). This record says who makes the calls and what a setting may hold, and nothing about either.
- Which key goes and which stays, key by key. That is the settings pass's own call.
