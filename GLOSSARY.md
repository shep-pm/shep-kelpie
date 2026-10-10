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
   once, in order, and one with any finding, nits included, is followed by
   one fix turn. The pass ends where it ends: no loop, no judge.
5. **CI.** A red run goes back to the worker. A second red run on a head the
   worker left alone is a `stuck` ruling.
6. **Merge.** A ruling under `ask`, kelpie's code under `auto`, as a merge
   commit of the head the gates passed.

## Who

**Project**:
A repo under kelpie, with its settings, board and state file. It runs while
its runner's sheep runs: you start it, or pause it, which waits for its
calls and any merge in flight to end and then stops that sheep. A wait that
runs out, an interrupt or a refused stop leaves it running. Or you finish
it, and it stops itself once its open work items end. Its settings
(merge authority, CI, agents, pacing) live on it, each in a table of its own.
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

**Gateway**:
One endpoint in front of a host's model servers, such as paddock,
named once in kelpie's own settings with the variable holding its key. A
pi agent or an endpoint reviewer names one in place of its server's URL.
It queues its calls, so kelpie takes no GPU lock for them, and a worker's
turn on one holds a lease of the gateway's own on its model.
_Avoid_: proxy, router

**Implementer**:
An agent a project lists to build its work items. An issue's `agent:<name>`
label picks one. With several listed, the issue writer labels an issue
without one before the board opens it; with one, it runs on that one, the
**default implementer**, the first listed, which may be a local model. A
work item keeps the implementer it opened on, unless its first turn **falls
back**: with `agents.fallback_after` set, a first turn that waited that long
for its model moves to the next implementer listed. A label ending in `!`
**pins** the work item, which never falls back.
_Avoid_: worker model, local worker

**Waiting for model**:
A worker's turn that has not yet heard from its model: asking for a lease on
it, told its gateway is busy, or silent past `agents.fallback_after` (ten
minutes with that off). `status` and the board show since when, and why.
Its first output clears it.
_Avoid_: stalled, hung

**Issue writer**:
The agent that turns a request into an issue: it researches, scopes the work
to one pull request or splits it, writes the acceptance criteria and labels
the implementer. `shep kelpie issue "<request>"` runs it headless and files
the issue for the maintainer to read; with `--interactive` it runs in the
maintainer's terminal and files it ready for a worker. With several
implementers listed, the runner also asks it to pick the implementer for a
ready issue with no `agent:` label, and puts that label on the issue itself.
`agents.issue_writer` names its agent file.
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
or that never answers, is passed over for the pass. One passed over that
reviews the head anyway gets a **late round** of its own: kelpie reads for
its review at the start of each round, at green CI, and while the merge
ruling waits, which that review withdraws. After the pass, a late round's
fix goes to CI and back to the merge ruling with no new pass. No bot is listed unless the project lists it.

## In shep's terms

Kelpie is a **dog**: it watches its projects rather than being one. The
adopted dog, `kelpie`, holds the leases. Each **project** runs as a
**sheep**, kelpie's **runner**, and every agent call it starts, a worker's
turn, a reviewer's session or the PM's wake, is that sheep's **lamb**.
A stop is shep's: the runner exits, shep's stop ends every lamb it left
(ADR 0005), and a restart resumes each work item from its session. The sessions the
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
again from the top only on new code that needs the whole review, such as
a merge ruling's `rework`.

**Finding**:
One thing a reviewer reports, as `SEVERITY|file:line|what|why`, with a
severity of high, medium or low (a **nit**). A review bot's open thread is
a finding at the bot's own severity.

**Fix turn**:
The one worker turn after a reviewer's findings, which counts only if it
moves the branch's head. A fix turn sent only nits starts nothing new: no
reviewer reads again because of it, and a bot that read the pull request
before has its nits on that fix's head left open. Findings the worker
leaves out of scope go in the **deferred findings** file, which the merge
files as follow-ups, all but its nits.

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

**Usage ledger**:
A project's record of every model call it made, one line each as the call
ends, and one per finished work item, in `usage.jsonl` in its folder.
**Units** weigh a call's tokens as the control room was measured: cache read
0.1, one-hour cache write 2, five-minute cache write 1.25, output 5,
uncached input 1. `shep kelpie usage` reads it.
_Avoid_: spend log, cost log

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

**App**:
Kelpie's GitHub App, one per repo owner, which the maintainer registers
with `shep kelpie github setup` and installs on the repos kelpie works. Its
private key stays in kelpie's home, and kelpie acts as the App with an
installation token minted from it, one per repo. Merging stays on the maintainer's
own `gh` login.
_Avoid_: bot account, kelpie's account

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
`unpushed`, `turn-timeout` and `turn-failed`. A worker waiting on one is
**parked**, and its work item gives up its slot under `concurrency.active_items`, which
bounds model calls. `concurrency.pending_rulings` is a threshold for opening
new work items, not a cap on parking: once that many work items are parked
on rulings that hold their work back, the board opens no new one, while
items already open can still park past it. 0 is taken as 1: the board opens
nothing while any such ruling waits. It leaves out the `follow-up`
ruling of a merged pull request. The one exception: the PM may retry a stuck item, which
answers its `stuck` ruling for the maintainer, with a yes or, for CI still
red, a note sending the worker back. A merge ruling takes three answers:
`yes`, `no <note>`, whose fix goes to CI and back to the ruling, and
`rework <note>`, whose change starts a new pass, as a `no` does on a
merge ruling that warns of open bot threads, an unread pass or an unread
head. It names open bot nits apart from the threads that hold a merge, and
nits never do.

**Merge authority**:
Who merges a green, reviewed pull request, a project's `git.merging`. `ask`
raises a ruling before every merge. `auto` has kelpie merge once every gate
passes, and replaces only that ruling: the merge still asks where a safety
gate holds it, such as a pull request no reviewer read. Deferred findings
follow `git.issues`, not it: a `follow-up` ruling comes only under `ask`.
Every other ruling still asks.

**Notice**:
What kelpie sends after a merge under `auto`, through the same webhook as
rulings. Not a ruling: it has no id and takes no answer.

**Draining**:
A runner told to start no new call, while the calls it has running go on to
their end. `shep kelpie upgrade` drains each runner before it restarts it.

**Finishing**:
A runner whose board picks nothing new while its open work items run on to
their end, merged, closed, dropped or stopped by a `no`, after which it
stops its own sheep as a pause does. It outlasts a restart but not a pause, and `shep kelpie start`
ends it.
_Avoid_: winding down
