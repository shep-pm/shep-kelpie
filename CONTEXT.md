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
decides how a work item is split.
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

**Phase**:
A stage of a work item: plan, implement, review, fix, merge.

**Work split**:
A worker's choice of how to carry out its work item: inline, phased with
handoffs, or delegated to a crew.

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
Kelpie-granted use of a shared resource: the GPU, or the CodeRabbit or Gemini
window.
_Avoid_: lock (the file the lease is built on)

**Summon**:
Anything that makes an outside reviewer spend its review window. For
CodeRabbit on shep that is adding the `review please` label, or pushing to a
pull request that carries it. For Gemini it is a `/gemini review` comment.
Only the project manager summons.

**Ruling**:
A decision only the maintainer makes. A worker waiting on one is **parked**.

**Relay**:
The Claude Code session through which kelpie asks the maintainer for
rulings and passes the answers back. A stopgap for the first build.
