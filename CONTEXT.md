# Kelpie

Kelpie runs Claude Code workers on a maintainer's projects, holds their merge
gates and shared review resources, and keeps each agent's context small. Named
for the working sheepdog that works a mob on its own initiative. Vocabulary
accepted 2026-09-25, and revised 2026-10-05 for the flow in ADR 0006.

_Avoid_: harness (the working name before 2026-09-25), control room

## The flow

1. **Issue.** The maintainer and the issue writer turn a request into an
   issue with acceptance criteria, one pull request's worth, labelled with
   the agent that should build it.
2. **Pick.** The project manager's agent picks the next ready issue, or,
   without one, the board takes it by priority, then age; either opens a
   work item for it.
3. **Build.** A worker implements it inline, opens a draft pull request and
   ends its turn.
4. **Review.** Each reviewer in the project's list reads the pull request
   once, in order, and each is followed by one fix turn. A failing test a
   reviewer left is run by kelpie before and after the fix. The chain ends
   where it ends: no loop, no judge.
5. **CI.** A red run goes back to the worker, twice at most, then a ruling.
6. **Merge.** A ruling under `ask`, kelpie's code under `auto`, as a merge
   commit of the head the gates passed.

## Who

**Project**:
A repo under kelpie, with its settings, board and state file. You start or
pause a project; its settings (merge authority, agents, budgets) live on it.
_Avoid_: shift

**Project manager (PM)**:
The role that owns a project's merge queue, git state and gates. Kelpie's
code does all of that. The PM's agent, an agent file whose role is `pm`
that a project names in `agents.pm`, picks work, holds it back, unsticks
items and answers the maintainer. Code wakes it on a free slot with two or
more ready issues, two open branches in conflict, a stuck work item, or a
**tell** (`shep kelpie tell "<note>"`), and it reads only the board and its
own notes, never running git or gh. Kelpie checks each answer against the
board before acting on it. One session per project is resumed on each wake
and compacted past 100k tokens of context. Without one, or while it is
down, the board's rule picks and stuck items wait on their rulings.
_Avoid_: lead, control room, control center

**Worker**:
Owns one work item: its branch, pull request, budget and sessions. Picks the
work split, and outlives any one of its sessions.
_Avoid_: middle manager, PR owner, chip

**Crew**:
The executors a worker delegates to for one work item: subagents or teammates.
_Avoid_: team (Claude's agent teams are one kind of crew)

**Session**:
One Claude Code conversation, identified by its session id. Claude's word, kept
as Claude uses it.
_Avoid_: shift

**Agent**:
A definition file naming a harness (Claude Code, Codex, or pi on a local
model), the model and effort it runs on, its prompt and tools, and what it is
for. Kelpie ships defaults and `add` writes them out. A project names its
implementers and its reviewers by agent.
_Avoid_: model (one part of an agent), bot (a review bot is one kind of
reviewer)

**Implementer**:
An agent a project lists to build its work items. An issue's `agent:<name>`
label picks one, and an issue without one runs on the first listed that is
not a local model. A work item keeps the implementer it opened on.
_Avoid_: worker model, local worker

**Issue writer**:
The agent that turns a request into an issue: it researches, scopes the work
to one pull request or splits it, writes the acceptance criteria and labels
the implementer. `shep kelpie <project> issue "<request>"` runs it headless
and files the issue for the maintainer to read; with `--interactive` it runs
in the maintainer's terminal and files it ready for a worker.
_Avoid_: planner, planning call

**Reviewer**:
An agent in a project's review list, `agents.reviewers`. Each reads the
pull request once, in the list's order, against the issue's acceptance
criteria, and its file's body is its prompt. It runs a fresh session on a
harness, or a command, or kelpie's own endpoint reviewer. One with a
**second look** reads twice, the second time shown what it found the first
and asked only for what it missed, before its one fix turn. Kelpie ships
`defect-hunter`, which does. A **review bot** (CodeRabbit, cubic, Codex) is
a reviewer on the bot harness, summoned on the pull request and answering
there within a rate window of its own. Its file is named for it and holds
that window. A listed bot reads once a pass in its place, and its open
threads are its findings; one whose window opens more than an hour on, or
that never answers, is passed over for the pass. No bot is listed unless
the project lists it.
_Avoid_: judge, local reviewer, round, deep round, pull request reviewer

## In shep's terms

Kelpie is a **dog**: it watches its projects rather than being one. Each
**project** runs as a **sheep** (kelpie's project runner), and every agent
session it starts, worker or reviewer, is that sheep's **lamb**. Stopping
the sheep stops every lamb (ADR 0005). Crew members are usually not
processes at all; one started as its own process is a deeper lamb. The
sessions the maintainer opens by hand, `--interactive`, `attach` and the
PM's (`shep kelpie pm`), run in the maintainer's terminal and are not
lambs. The PM's woken calls are lambs.

## What they handle

**Work item**:
What a worker is handed: one issue, on one branch and pull request.
_Avoid_: task, job, ticket

**Adoption**:
Kelpie taking over an open pull request it didn't open, as a work item for
the issue that pull request closes.
_Avoid_: takeover, import

**Phase**:
A stage of a work item: implement, review, fix, CI, merge.

**Work split**:
A worker's choice of how to carry out its work item: inline, phased with
handoffs, or delegated to a crew. Inline unless the worker has a reason; the
experiments found no split that beat it.

**Reset**:
Ending a session's context at a phase boundary, by compact (same session) or
clear (a new session that starts from a handoff). The worker stays the same.

**Handoff**:
The document a session writes so the next session or a crew member can continue.
_Avoid_: summary (Claude's compaction output)

**Attach**:
`shep kelpie attach <issue>` holds a work item, so no call starts for it,
and resumes its worker's session in the maintainer's terminal, to steer it
by hand. Exiting the session lets the work item go.

## Kelpie's mechanics

**Board**:
The project's open work items and ready issues, as kelpie's code sees them.
Kelpie's code writes it out as `<kelpie home>/<project>/board.md` whenever it
changes, for the PM's agent to read: each open work item with its session's
activity, the rulings waiting, the ready queue, the board's events since the
PM's last wake, and the files open branches and ready issues share.

**Board event**:
One change to the board, such as a turn ending or a ruling raised, with an
id that only grows. The PM's cursor names the last one it has read.

**Gate**:
The checks a pull request must pass before it merges.

**Timings**:
Where a work item's wall time went, by timing phase: `worker`, `review`,
`ci`, `ruling`, `merge` and `other`. Every second lands in exactly one. A
timing phase is a bucket of time, not the stage a work item is in (its
**phase**). `status` shows the open items' timings, and `timings <n>` totals
the last `n` finished ones.
_Avoid_: profile, metrics

**Lease**:
Kelpie-granted use of a shared resource: a review bot's window, such as
CodeRabbit's, which the dog books from the bot's file, or a share of the
machine for running tests (`cargo-test`), which a few commands hold at once.
A bot's round summons only under its lease.
_Avoid_: lock (the file the lease is built on)

**Summon**:
Anything that makes a review bot spend its review window. For CodeRabbit on
shep that is adding the `review please` label, pushing to a pull request that
carries it, or asking for a full review in a comment. For cubic it is the
`@cubic-dev-ai review` comment, and for Codex the `@codex review` comment, or
marking a draft ready where its file says it reviews on ready. Only kelpie's
code summons, in the listed bot's round of a review pass.

**Ruling**:
A decision only the maintainer makes, one of five kinds: `merge`,
`question`, `stuck` (with its reason), `agent-files` and `foreign-change`. A
worker waiting on one is **parked**. The one exception: the PM may retry a
stuck item, which answers its `stuck` ruling for the maintainer, with a yes
or, for CI still red, a note sending the worker back.

**Merge authority**:
A project's setting for who decides a merge. `ask` raises a ruling before
every merge. `auto` has kelpie merge once every gate passes, and replaces only
that ruling: every other ruling still asks.

**Notice**:
What kelpie sends after a merge under `auto`, through the same channel as
rulings. Not a ruling: it has no id and takes no answer.
