# Kelpie

Kelpie runs Claude Code workers on a maintainer's projects, holds their merge
gates and shared review resources, and keeps each agent's context small. Named
for the working sheepdog that works a mob on its own initiative. Vocabulary
accepted 2026-09-25.

_Avoid_: harness (the working name before 2026-09-25), control room

## Who

**Project**:
A repo under kelpie, with its settings, board and state file. You start or
pause a project; its settings (merge authority, models, budgets) live on it.
_Avoid_: shift

**Project manager (PM)**:
The role that owns a project's merge queue, git state and gates. It never
decides how a work item is split. It does plan, which is a level above.
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

## In shep's terms

Kelpie is a **dog**: it watches its projects rather than being one. Each
**project** runs as a **sheep** (kelpie's project runner), and its **workers**
are that sheep's **lambs**. Crew members are usually not processes at all;
one started as its own process is a deeper lamb.

## What they handle

**Work item**:
What a worker is handed: one or more issues bundled into one branch and pull
request.
_Avoid_: task, job, ticket

**Adoption**:
Kelpie taking over an open pull request it didn't open, as a work item for
the issue that pull request closes.
_Avoid_: takeover, import

**Phase**:
A stage of a work item: plan, implement, review, fix, merge.

**Work split**:
A worker's choice of how to carry out its work item: inline, phased with
handoffs, or delegated to a crew.

**Planning**:
Splitting an issue into sub-issues before any work item exists, one pull
request each, linked by what blocks what. A one-shot call the project
manager makes when the board picks an issue; most issues stay whole. It
decides how many work items an issue becomes. The work split inside each
one stays the worker's.
_Avoid_: breakdown, decomposition, work split (the worker's choice inside one work item)

**Reset**:
Ending a session's context at a phase boundary, by compact (same session) or
clear (a new session that starts from a handoff). The worker stays the same.

**Handoff**:
The document a session writes so the next session or a crew member can continue.
_Avoid_: summary (Claude's compaction output)

## Kelpie's mechanics

**Gate**:
The checks a pull request must pass before it merges.

**Lease**:
Kelpie-granted use of a shared resource: the GPU, or a pull request
reviewer's window, such as CodeRabbit's.
_Avoid_: lock (the file the lease is built on)

**Summon**:
Anything that makes a pull request reviewer spend its review window. For
CodeRabbit on shep that is adding the `review please` label, pushing to a pull
request that carries it, or asking for a full review in a comment. For
cubic it is the `@cubic-dev-ai review` comment.
Only the project manager summons.

**Shots**:
Screenshots kelpie takes of a work item's routes, from the dev server its
repo's `.claude/launch.json` names, at a phone and a desktop width, light and
dark. A worker takes them with kelpie's shots tool; kelpie takes them before
each Claude review round and the merge ruling.
_Avoid_: preview (Claude Desktop's pane), snapshots (Playwright's page trees)

**Local reviewer**:
A reviewer in a project's review loop, named in kelpie's settings: a local
model through kelpie's own reviewer against an OpenAI-compatible server, a
command such as the maintainer's qwen-review script, or a Claude session on
its own model. A project lists its local reviewers in the order the loop
runs them, and `claude` is always one: the project's own Claude round.

**Local round**:
One round of the review loop, by one local reviewer. The loop ends once two
in a row, from two different local reviewers, find nothing above a nit.
_Avoid_: qwen round (qwen is one model a local reviewer can run)

**Pull request reviewer**:
A reviewer summoned on the pull request that answers there, within a rate
window of its own. CodeRabbit and cubic are two. A **review bot** is one
that works the GitHub way, by label or comment, status and review threads,
and a **profile** says how each bot does it. A project lists its pull
request reviewers in preference order, and each round goes to the first
whose window is free. The other kind is a local reviewer.
_Avoid_: outside reviewer, remote reviewer

**Ruling**:
A decision only the maintainer makes. A worker waiting on one is **parked**.

**Merge authority**:
A project's setting for who decides a merge. `ask` raises a ruling before
every merge. `auto` has the project manager merge once every gate passes,
and replaces only that ruling: every other ruling still asks.

**Notice**:
What kelpie sends after a merge under `auto`: a post to the webhook, or where
the webhook is off a push through the relay. Not a ruling: it has no id and
takes no answer.

**Relay**:
The Claude Code session through which kelpie asks the maintainer for
rulings and passes the answers back. A stopgap for the first build.
