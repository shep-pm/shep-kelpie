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
2. **Pick.** The board takes the next ready issue by priority, then age, and
   opens a work item for it.
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
code does all of that. The PM's agent, woken only on events and briefed from
the board, picks work, unsticks items and answers the maintainer; it never
runs git or gh. Its shape waits on the experiments repo's series 7.
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
An agent in a project's review list. Each reads the pull request once, in
the list's order, against the issue's acceptance criteria. A **review bot**
(CodeRabbit, cubic, Codex) is a reviewer on the bot harness, summoned on the
pull request and answering there within a rate window of its own.
_Avoid_: judge, local reviewer, round

## In shep's terms

Kelpie is a **dog**: it watches its projects rather than being one. Each
**project** runs as a **sheep** (kelpie's project runner), and every agent
session it starts, worker or reviewer, is that sheep's **lamb**. Stopping
the sheep stops every lamb (ADR 0005). Crew members are usually not
processes at all; one started as its own process is a deeper lamb. The
sessions the maintainer opens by hand, `--interactive`, `attach` and the
PM's, run in the maintainer's terminal and are not lambs.

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
`shep kelpie <project> attach <issue>` pauses a work item and resumes its
worker's session in the maintainer's terminal, to steer it by hand.

## Kelpie's mechanics

**Board**:
The project's open work items and ready issues, as kelpie's code sees them.
Kelpie writes it out as `board.md`, with the files each open branch touches,
for the PM's agent to read.

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
CodeRabbit's, or a share of the machine for running tests (`cargo-test`),
which a few commands hold at once.
_Avoid_: lock (the file the lease is built on)

**Summon**:
Anything that makes a review bot spend its review window. For CodeRabbit on
shep that is adding the `review please` label, pushing to a pull request that
carries it, or asking for a full review in a comment. For cubic it is the
`@cubic-dev-ai review` comment, and for Codex the `@codex review` comment.
Only kelpie's code summons.

**Ruling**:
A decision only the maintainer makes, one of five kinds: `merge`,
`question`, `stuck` (with its reason), `agent-files` and `foreign-change`. A
worker waiting on one is **parked**.

**Merge authority**:
A project's setting for who decides a merge. `ask` raises a ruling before
every merge. `auto` has kelpie merge once every gate passes, and replaces only
that ruling: every other ruling still asks.

**Notice**:
What kelpie sends after a merge under `auto`, through the same channel as
rulings. Not a ruling: it has no id and takes no answer.
