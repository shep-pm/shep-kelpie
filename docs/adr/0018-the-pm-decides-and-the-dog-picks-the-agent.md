# The project manager decides, and the dog picks the agent

This reopens two lines of ADR 0006: "a list of reviewers each read it once, in order", and "an issue-writer agent ... labels the agent that should build it". Reviewers still read once each with one fix turn, and ADR 0008's green head still comes first.

Around the work item loop, kelpie grew roles that decide or prepare work: the ruling panel's advisor and researcher, the issue writer, and the triager. The ledgers and ruling logs for Oct 20 to 26, across every project (`kelpie-report roles`, in the maintainer's scripts), show what each one changed:

- **The panel.** Of 99 rulings the panel decided, the project manager and the worker already agreed on the chosen answer in 92, so the advisor and researcher could have changed at most 7. On the 18 the maintainer answered, the project manager matched 8 of 11, the advisor 12 of 18 and the researcher 11 of 18: neither read the maintainer better. The two cost $55 that week.
- **The issue writer.** It filed no issue that week. All 228 of its calls picked an implementer, and it sent 157 of them to `iq3-xxs`, the one local model on one GPU, with no view of that model's queue. `iq3-xxs` merged 173 of 181 items at a median of 1.6 hours and $0.22, against 0.7 hours and $1.22 for `sonnet-high`. The local model was good enough, and its picks backlogged it.
- **The triager.** It cost $61, more than the other three together. On 47 issues it asked 247 questions: 234 were design choices and it recommended an answer for 246. Of 30 answered issues, 20 were a plain "ok". A triaged issue was ready 1.5 to 15 hours after filing, against at once for an issue its filer labelled.
- **Who decides what.** Each project names its implementers and reviewers, so 17 projects drifted into eight reviewer lines, and a pick never sees another project's load on the same model. `agents.fallback_after` covers only a first turn with no output, so later turns queued behind the GPU and timed out (34 on Oct 25). A review bot with a spent window held the item behind it, about an hour each time.

So the roles that decide are cut to one, the project manager, and choosing an agent moves from a guess when an issue is labelled to a reading when the work starts.

## Decision

### Rulings

- The panel goes: the advisor and researcher roles, their agent files and folders, the worker's vote, and `rulings.decide`.
- `rulings.pm_alone`, a list, becomes `rulings.pm_decides`, a map from every ruling kind to `true` or `false`. `true` means the project manager answers that kind, and `false` means the maintainer does.
- `upgrade` writes every kind the build knows into each project's map, so a new kind shows up in the file. Operational kinds (`rebase`, `still-red`, `turn-timeout`, `turn-failed`, `over-budget`, `follow-up` and the rest) start `true`, while `question`, `foreign-change`, `agent-files` and the new `agent-fit` start `false`. The kinds the code already refuses to the project manager (`merge`, `security`, `contribution-merge`, `local-model-spilled`) are refused when set to `true`, naming the kind.
- An old list loads for one release, read as `true` for each kind it names.

### Issues

- The issue writer role goes. In its place kelpie ships an issue skill that holds the format and requirements of an issue a kelpie implementer can work: what to build, acceptance criteria, the files it touches, harness reach, blockers, and the priority and kind labels.
- `upgrade` links the skill into each harness's skills folder, so an implementer, the project manager and any agent session the maintainer runs can call it. An issue meant for kelpie is written the same way wherever it comes from.
- `shep kelpie issue check <n>` is the same check in code: structure, not quality. The board runs it on every new or edited issue, and the skill runs it before filing. An issue that passes is ready.
- The triager runs only on an issue that fails the check, whoever opened it. It rewrites the issue to the format. A question it has a good answer for becomes a stated assumption in the body ("Assumed: X. Say otherwise.") instead of a question, and only a question it has no good answer for holds the issue. An issue another person opened still waits on `triage.hand_ready`.
- `agents.issue_writer`, the `shep kelpie issue "<request>"` verb and the `unlabelled` ruling go.

### Agents

- An agent file carries what the project manager and the board choose by, so the maintainer edits files, not project settings:
  - `description`: free text for the project manager (absorbs #1378).
  - `kinds`: the kinds of work it takes, such as `docs` or `mechanical` (absorbs #1379).
  - `tier`: `light`, `standard` or `heavy`.
- Concurrency is not in the file. The board reads it from where it lives: a gateway model's slots, queue and load time from the paddock's status, a hosted plan's windows from its meter, and a review bot's window from the lease dog.
- A project names no agents. It keeps `agents.allow`, an optional list of agents it may use, so a private project can keep its code to the models its owner chooses, and `reviews.need`. This finishes #1232's last slice (#1287) without shep-pm/shep#1079, since no project lineup is left to inherit, and kelpie keeps no agent names in code.

### The agent board

- The dog keeps one board for every runner, since agents are shared across projects. For each agent it holds the availability above and a typical time per kind of work: the median from the ledger once the agent has 5 finished items of that kind, the median of its tier until then.
- The project manager tags each work item with a kind and a minimum tier, and may name an agent outright. A pinned agent is the only one tried.
- At dispatch, the runner asks the board. Of the agents that are allowed, take the item's kind and meet its tier, with a window left, the board estimates when each would finish (queue wait plus typical time). It picks the cheapest whose finish is within `agents.slack` (default 1 hour) of the soonest. The project manager may tighten `slack` for one item, to 0 for a hotfix.
- A started item keeps its implementer. When its next turn would queue, the same estimate runs again, and a wait past the slack hands the item to the board's next pick with a handoff. `agents.implementers`, `implementer_choice` and `fallback_after` go.
- The board fills every free slot it has a capable agent for. The project manager says what an item needs, never how many items run or whether an agent sits idle.

### Track record

- Each work item's finished line records its kind and tier beside its agent. The board keeps each agent's record per kind: items, merge rate, median hours, review rounds, fix turns, rulings and cost. The runner sends the same as `kelpie_*` series with agent and kind as labels, both bounded (ADR 0012).
- A record reaches a pick only one way: through the typical time, since an agent stuck in fix rounds finishes later. No rule from the record drops an agent, skips a slot or reorders the pool.
- Who may take a kind changes only in the agent file, by the maintainer. When an agent has at least 10 items of a kind and does clearly worse than another agent that takes that kind (merge rate, hours, or share of items with 3 or more fix turns), the project manager raises an `agent-fit` ruling with both records and a suggested `kinds` change. It never applies the change itself, and does not raise the same one again for the next 10 items.

### Reviews

- `reviews.need` (default 2) reviewers read each item, still once each and in series, so each later one sees the earlier one's fix. Each reviewer is the board's pick at that moment, from reviewers that are allowed and fit the item's tier. One with a spent window is skipped, not waited on. A late bot review is still read (ADR 0007). `agents.reviewers` goes.

## Considered options

- **Keep the panel with fewer voters.** The project manager and the worker already agreed on 92 of 99 answers, and the maintainer's answers favoured no voter, so a smaller panel keeps a wait and a cost for the same answer.
- **Keep the issue writer and give it the queue.** It would then repeat the board's reading in a model, at the time an issue is labelled, hours before the work starts and the reading is true.
- **Triage every issue, with fewer questions.** An issue that already meets the format gains nothing from a model reading it, and the code check says which ones do.
- **A `max_wait` per agent.** The wait worth taking depends on the other agents, not on the agent: 20 minutes on a local model is fine when the next would take an hour, and too long when it would take ten.
- **Concurrency in the agent file.** It is a fact of the gateway, already reported there, and a copy in a file goes stale the day `sequences` changes.
- **Reviewers at once on one green head.** It saves wall time, but no reviewer would read another's fix, and ADR 0006's measured gain from a second read came from one shown the first's findings.
- **Project lineups inherited from the operator's defaults (shep-pm/shep#1079).** It shares one list across projects, but each project still names agents and the pick still ignores load.
- **Let the project manager weigh each agent's record in every pick.** It has held slots idle before on a reading of its own. A record that moves picks only through the typical time, and changes an agent's kinds only through the maintainer, cannot do that.
- **A project manager that watches every lock and meter itself.** Waking a model on each change in availability is slow and costs tokens, for what code reads exactly. The project manager says what an item needs, and the code finds who is free.

## Consequences

- The cuts land in order, each its own pull request: the panel and `pm_decides`; the issue skill and `issue check`; the triager's narrowing; the agent file keys; the board with the pick at dispatch; `reviews.need`.
- `upgrade` migrates each project's settings: it writes the `pm_decides` map, moves `implementers` and `reviewers` into `agents.allow`, and drops the keys that go. A work item in flight keeps its implementer, so no drain is needed.
- The glossary drops panel, advisor, researcher and issue writer, and adds agent board, slack, tier, track record and issue check. Prose names an agent by its role, implementer or reviewer, and keeps worker for the owner of a running work item.
- With the panel gone, a ruling the map leaves to the maintainer comes with no pick, and the maintainer asks for one per ruling in its thread: `<id> pm recommend` for the project manager's recommendation, `<id> pm options` for other approaches when none of the options is right, or `<id> pm decide` to hand that ruling over as if the map gave it. No wake runs and no pick anchors the maintainer's answer unless they ask, and the untrusted text a ruling quotes reaches the project manager only when they send it.
- The board estimates from the ledger, so a new agent starts on its tier's median and its picks improve as it finishes items.
