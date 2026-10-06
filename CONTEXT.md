# Kelpie

Kelpie runs Claude Code workers on a maintainer's projects, holds their merge
gates and shared review resources, and keeps each agent's context small. Named
for the working sheepdog that works a mob on its own initiative. Vocabulary
accepted 2026-09-25, revised 2026-10-05 for the flow in ADR 0006, and
revised 2026-10-06 for the code as it stands after the cleanup.

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
   once, in order, and one with any finding above a nit is followed by one
   fix turn. The pass ends where it ends: no loop, no judge.
5. **CI.** A red run goes back to the worker. A second red run on a head the
   worker left alone is a `stuck` ruling.
6. **Merge.** A ruling under `ask`, kelpie's code under `auto`, as a merge
   commit of the head the gates passed.

## Who

**Project**:
A repo under kelpie, with its settings, board and state file. You start or
pause a project; its settings (merge authority, agents, pacing) live on it.
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

**Pick**:
The PM's choice of which ready issue the next free slot takes, or none.

**Hold**:
The ready issues the PM keeps waiting until an open work item closes. Each
answer replaces the last.

**Worker**:
Owns one work item: its branch, pull request and sessions. Builds it
inline, and outlives any one of its sessions.
_Avoid_: middle manager, PR owner, chip

**Crew**:
Subagents or teammates a session could hand work to. Kelpie's calls start
none: a worker builds inline, and a reviewer's or the PM's tools cannot
start one.
_Avoid_: team (Claude's agent teams are one kind of crew)

**Session**:
One agent conversation, identified by its session id. Claude's word, kept
as Claude uses it. A Codex session is a thread kelpie maps to its own id.
_Avoid_: shift

**Agent**:
A definition file, `<kelpie home>/agents/<name>.md`, naming what it is for
(its **role**: `implementer`, `reviewer`, `issue-writer` or `pm`), its
harness, the model and effort it runs on, and its prompt as the body.
Kelpie ships defaults and `add` writes them out. A project names its
implementers, its reviewers and its PM by agent.
_Avoid_: model (one part of an agent), bot (a review bot is one kind of
reviewer)

**Harness**:
What runs an agent's session: Claude Code (`claude-code`), Codex (`codex`)
or pi (`pi`, a model on an OpenAI-compatible server). A reviewer may also
run as a `command`, an `endpoint` or a `bot`.

**Implementer**:
An agent a project lists to build its work items. An issue's `agent:<name>`
label picks one, and an issue without one runs on the **default
implementer**, the first listed that is not a local model. A work item
keeps the implementer it opened on.
_Avoid_: worker model, local worker

**Issue writer**:
The agent that turns a request into an issue: it researches, scopes the work
to one pull request or splits it, writes the acceptance criteria and labels
the implementer. `shep kelpie issue "<request>"` runs it headless and files
the issue for the maintainer to read; with `--interactive` it runs in the
maintainer's terminal and files it ready for a worker.
_Avoid_: planner, planning call

**Reviewer**:
An agent in a project's review list, `agents.reviewers`. Each reads the
pull request once, in the list's order, against the issue's acceptance
criteria. It runs a fresh session on a harness, whose file's body is its
prompt, or a command, or kelpie's own endpoint reviewer. One with a
**second look** reads twice, the second time shown what it found the first
and asked only for what it missed, before its one fix turn. Kelpie ships
`defect-hunter`, which does. A **local reviewer** is a command or an
endpoint, run on a model of the maintainer's own.
_Avoid_: judge, round guard, deep round, pull request reviewer

**Review bot**:
A reviewer on the `bot` harness: CodeRabbit, cubic or Codex, summoned on the
pull request and answering there within a rate **window** of its own,
which its file holds. A listed bot reads once a pass in its place, and its
open threads are its findings; one whose window opens more than an hour on,
or that never answers, is passed over for the pass. No bot is listed unless
the project lists it.

## In shep's terms

Kelpie is a **dog**: it watches its projects rather than being one. The
adopted dog, `kelpie`, holds the leases. Each **project** runs as a
**sheep**, kelpie's **runner**, and every agent call it starts, a worker's
turn, a reviewer's session or the PM's wake, is that sheep's **lamb**.
Until shep sweeps a sheep's lamb tree on every stop (ADR 0005), the runner
ends the calls it started with its own stop ladder before it exits, and a
restart resumes each work item from its session. The sessions the
maintainer opens by hand, `--interactive`, `attach` and `shep kelpie pm`,
run in the maintainer's terminal and are not lambs.

## What they handle

**Work item**:
What a worker is handed: one issue, on one branch and pull request.
_Avoid_: task, job, ticket

**Rework**:
A work item made of a pull request kelpie opened, again, because the
maintainer asked with `ready-for-agent` or a review requesting changes.

**Adoption**:
Kelpie taking over an open pull request it didn't open, as a work item for
the issue that pull request closes.
_Avoid_: takeover, import

**Phase**:
A stage of a work item: `implement`, `review`, `ci`, `ruling`, `merge` and
`done`. A fix turn is a stage of the review phase.

**Reset**:
Ending a session's context, by compact (same session) or clear (a new
session that starts from a handoff). The PM's session is compacted past
100k tokens of context. The `reset` step's skill is vendored, and no step
drives it yet.

**Handoff**:
The document a session writes so the next session can continue: the
`reset` step's skill.
_Avoid_: summary (Claude's compaction output)

**Attach**:
`shep kelpie attach <issue>` holds a work item, so no call starts for it,
and resumes its worker's session in the maintainer's terminal, to steer it
by hand. Exiting the session lets the work item go.

**Follow-up**:
A finding a reviewer sent the worker and the worker deferred, filed as an
issue once the pull request merges.

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

**Pass**:
One run down a project's reviewers, each once, in order. A pass starts
again from the top only on new code the review has not seen.

**Finding**:
One thing a reviewer reports, as `SEVERITY|file:line|what|why`, with a
severity of high, medium or low (a **nit**). A review bot's open thread is
a finding at the bot's own severity.

**Fix turn**:
The one worker turn after a reviewer's findings, which counts only if it
moves the branch's head. Findings the worker leaves out of scope go in
the **deferred findings** file, which the merge files as follow-ups.

**Unreviewed**:
The mark on a work item whose pass ended with no reviewer having read the
pull request, because one was down, kept failing or reviewed no file.
Under `auto` it gets the merge ruling instead of merging.

**Gate**:
The checks a pull request must pass before it merges: the review pass, CI
on the latest `main`, and the checks for agents' own files and foreign
changes.

**Foreign change**:
A head, label or ready state on a work item's pull request that neither
kelpie nor its worker made.

**Agents' own files**:
The files in a worktree that make a harness run code: Claude Code's
`.claude` folders and `.mcp.json`, Codex's `.codex` and `.agents`. No
worker writes them, and a pull request that changes them waits on an
`agent-files` ruling.

**Fence**:
What a worker's call may write, read and reach, which kelpie's OS
**sandbox** enforces around the whole call, with **confine** (on every file
write) and **guard** (on every command) as kelpie's own checks inside it.

**Timings**:
Where a work item's wall time went, by timing phase: `worker`, `review`,
`ci`, `ruling`, `merge` and `other`. Every second lands in exactly one. A
timing phase is a bucket of time, not the stage a work item is in (its
**phase**). `status` shows the open items' timings, and `timings <n>` totals
the last `n` finished ones.
_Avoid_: profile, metrics

**Lease**:
Kelpie-granted use of a shared resource. The dog's **book** holds a review
bot's window, which it books from the bot's file, and `cargo-test`, a share
of the machine for running tests that a few commands hold at once through
the dog's **door**. `gpu` and any other lock a reviewer names are file
locks on this machine, which the qwen scripts take too. A bot's round
summons only under its lease.
_Avoid_: lock (the file a lock lease is built on)

**Summon**:
Anything that makes a review bot spend its review window. For CodeRabbit on
shep that is adding the `review please` label, pushing to a pull request that
carries it, or asking for a full review in a comment. For cubic it is the
`@cubic-dev-ai review` comment, and for Codex the `@codex review` comment, or
marking a draft ready where its file says it reviews on ready. Only kelpie's
code summons, in the listed bot's round of a review pass.

**Pacing**:
How kelpie spends each **account** (`claude`, `codex`, or `none` for a local
model) against its usage windows: a daily **allowance** of the weekly
window, which holds new work items, and a stop at 50% of the 5-hour
window, which holds every turn.

**Ruling**:
A decision only the maintainer makes, one of six kinds: `merge`,
`question`, `stuck` (with its reason), `agent-files`, `foreign-change` and
`follow-up`. A `stuck` ruling's reason is one of `rebase`, `still-red`,
`merge-refused`, `closed`, `local-model-spilled`, `fix-not-pushed`,
`turn-timeout` and `turn-failed`. A worker waiting on one is **parked**. The
one exception: the PM may retry a stuck item, which answers its `stuck`
ruling for the maintainer, with a yes or, for CI still red, a note sending
the worker back.

**Merge authority**:
A project's setting for who decides a merge. `ask` raises a ruling before
every merge. `auto` has kelpie merge once every gate passes, and replaces only
that ruling: every other ruling still asks.

**Notice**:
What kelpie sends after a merge under `auto`, through the same webhook as
rulings. Not a ruling: it has no id and takes no answer.

**Draining**:
A runner told to start no new call, while the calls it has running go on to
their end. `shep kelpie upgrade` drains each runner before it restarts it.
