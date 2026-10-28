# Kelpie

Kelpie runs Claude Code workers on a maintainer's projects, holds their merge
gates and shared review resources, and keeps each agent's context small. Named
for the working sheepdog that works a mob on its own initiative. Vocabulary
accepted 2026-09-25, revised 2026-10-05 for the flow in ADR 0006, and
revised 2026-10-06 for the code as it stands after the cleanup.

_Avoid_: harness (the working name before 2026-09-25), control room

## The flow

1. **Issue.** The maintainer, or any agent session, writes an issue with
   kelpie's issue skill: acceptance criteria, one pull request's worth, and
   the agent that should build it as a label where one is wanted. The
   **issue check** says it holds every part.
2. **Pick.** The project manager's agent picks the next ready issue, or,
   without one, the board takes it by priority, then age; either opens a
   work item for it.
3. **Build.** A worker implements it inline, opens a draft pull request and
   ends its turn.
4. **Review.** Each reviewer in the project's list reads the pull request
   once, in order, and one with any finding, nits included, is followed by
   one fix turn. The pass ends where it ends: no loop, no judge. No
   reviewer reads a head until CI has passed on it, so the worker's push and
   each fix turn's head wait for green first, and a red run goes to the
   worker's CI fix turn before any reviewer reads.
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
The ready issues the PM keeps waiting, each until the open work item it names
(`until`) closes. A hold ends when that work item closes or the issue's text
changes, and on no other close; one naming none ends at any close. An answer
with a hold list replaces the last; one without leaves the holds as they are.

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
A goose session is named by kelpie's id.
_Avoid_: shift

**Turn**:
One run of an agent on one prompt, until the agent stops on its own or the
turn timeout ends it. A turn holds many model replies and tool calls. Each
later turn resumes the session, and a work item takes many turns: the
first, a fix turn per review round, a CI fix turn, one after each ruling
answer.
_Avoid_: step, reply

**Paused turn**:
A worker turn that a pause ended at its next tool call, on Claude Code or
pi. It is not a finished turn: no unpushed check, no review, no ruling, and
the work item keeps its stage. It holds no slot or lease, and the next start
resumes its session with one message and a fresh turn ceiling. A Codex or
goose call is not paused this way, and runs to its end.

**Agent**:
A definition file, `<kelpie home>/agents/<name>.md`, naming what it is for
(its **role**: `implementer`, `reviewer`, `pm` and the
others), its harness, the model and effort it runs on, and its prompt as the
body. A file is a **baseline** plus a **binding**, or says both itself.
An implementer or reviewer file may also say what it is chosen by:
`description`, `kinds` and `tier`. Kelpie ships whole agents and baselines,
and `add` writes the whole ones out. A project names its implementers, its
reviewers and its PM by agent.
_Avoid_: model (one part of an agent), bot (a review bot is one kind of
reviewer)

**Tier**:
How much work an implementer or reviewer is trusted with: `light`,
`standard` or `heavy`, in that order, set by its file's `tier`. An agent
whose file names none is `standard`.
_Avoid_: size, level

**Baseline**:
A shipped agent file that says what a role does: its `role` and a body, and
none of the binding keys. `extends: kelpie:<name>` reaches it. A role may
have several (`reviewer` has `defect-hunter`, `standards`, `spec` and
`security-reviewer`), and `implementer` has none. A file that extends one and
binds nothing is unbound: the book leaves it out and `shep kelpie agents new`
binds it. While a baseline keeps its binding keys it runs with no file.
_Avoid_: default, template

**Binding**:
The keys that say what runs an agent: `harness`, `model` and `effort`, and
`second_look`, `usage`, `url`, `gateway`, `context` and `tools` around them.
The operator's file carries them, so kelpie's files need not choose the
operator's model.

**Lineup**:
The agents a project lists for a role, such as `agents.implementers` or
`agents.reviewers`, in order.
_Avoid_: roster, pool

**Harness**:
What runs an agent's session: Claude Code (`claude-code`), Codex (`codex`),
pi (`pi`, a model on an OpenAI-compatible server) or goose (`goose`, a model
on an OpenAI-compatible server too). A reviewer may also
run as a `command`, an `endpoint` or a `bot`.

**Gateway**:
One endpoint in front of a host's model servers, such as paddock,
named once in kelpie's own settings with the variable holding its key. A
pi agent or an endpoint reviewer names one in place of its server's URL.
It queues its calls, and with it kelpie holds no lock on the GPU. A worker's turn
on one holds a lease of the gateway's own on its model, so the gateway
evicts nothing it runs on midway; a reviewer's round takes no lease at all.
_Avoid_: proxy, router

**Implementer**:
An agent kelpie can run a work item on, from a file whose role is
`implementer`. A project lists the ones an issue runs on, and the chain its
first turn **falls back** along. An issue's `agent:<name>` label picks one,
listed or not. An issue without one runs on the **default implementer**,
the first listed, which may be a local model. A work item keeps the
implementer it opened on, unless its first turn **falls back**: with
`agents.fallback_after` set, a first turn that waited that long for its
model moves to the next implementer listed. A label ending in `!` **pins**
the work item, which never falls back, and an unlisted implementer never
falls back either.
_Avoid_: worker model, local worker

**Waiting for model**:
A worker's turn that has not yet heard from its model: asking for a lease on
it, told its gateway is busy, or silent past `agents.fallback_after` (ten
minutes with that off). `status` and the board show since when, and why.
Its first output clears it.
_Avoid_: stalled, hung

**Issue check**:
Kelpie's code check that an issue holds every part an implementer builds
from: a `Touches:` line, what to build, acceptance criteria, harness reach
when it touches one harness's adapter, blockers when it names any, one
priority label, and a kind: its `kind:<type>` label, else its title's
conventional prefix, one of nine conventional types. It reads structure, not quality.
`shep kelpie issue check <n>` runs it, and the issue skill holds the same
format for the agent that writes an issue. The board runs it on every new or
edited open issue, and the **triager** runs only on one that fails it.
_Avoid_: lint, validation, triage

**Reviewer**:
An agent in a project's review list, `agents.reviewers`. Each reads the
pull request once, in the list's order, against the issue's acceptance
criteria. It runs a fresh session on a harness, whose file's body is its
prompt, or a command, or kelpie's own endpoint reviewer. One with a
**second look** reads twice, the second time shown what it found the first
and asked only for what it missed, before its one fix turn. Kelpie ships
`defect-hunter`, which does, and `defect-hunter-lite`, which does not. A **local reviewer** is a command or an
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

**Comment round**:
A resumed turn that sends the worker what people with write access wrote on
its pull request since kelpie last looked: new comments and review threads,
numbered as requests. Kelpie reads at the start of a review round, at green
CI and while a merge ruling waits; a comment landing under a merge ruling
withdraws it and is handled as its `no <note>`. A work item gets three before
a `question` ruling takes over. Apps and people without write access are
ignored and logged.
_Avoid_: comment fix, auto-fix

## In shep's terms

Kelpie is a **dog**: it watches its projects rather than being one. The
adopted dog, `kelpie`, holds the leases. Each **project** runs as a
**sheep**, kelpie's **runner**, and every agent call it starts, a worker's
turn, a reviewer's session or the PM's wake, is that sheep's **lamb**.
A stop is shep's: the runner exits, shep's stop ends every lamb it left
(ADR 0005), and a restart resumes each work item from its session. The sessions the
maintainer opens by hand, `attach` and `shep kelpie pm`,
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

**Contribution**:
An open pull request whose author is not the maintainer, not kelpie and not
a bot. Kelpie reviews it and talks with its **contributor**, and the pull
request stays theirs until an **adoption**.
_Avoid_: outside PR, external PR, community PR

**Contributor**:
The author of a **contribution**.
_Avoid_: outside contributor, external author

**Security review**:
A read of each head a **contributor** pushes, before anything of theirs
runs, for lines that could reach the maintainer's machine, secrets or
agents. A head it flags runs only after the maintainer trusts it in a
ruling.
_Avoid_: trust check, safety scan

**Release**:
A **contributor**'s OK for kelpie to finish their **contribution**, said
out loud or given by staying quiet once asked. A released contribution is
kelpie's, and its next step is an **adoption**.
_Avoid_: handover, consent, abandonment

**Representative**:
The agent that writes kelpie's prose for GitHub in one personality,
starting with everything a **contributor** reads. Others draft the facts,
and the representative words them. It speaks as kelpie, never as the
maintainer.
_Avoid_: rep (reads as repo), liaison, voice

**Triager**:
The agent that researches an issue another person opened, read-only on
every **harness**, and reports what it found. It changes and posts nothing:
kelpie's code does.
_Avoid_: triage bot, classifier (that word is the **spam check**'s model)

**Triage**:
Kelpie's handling of issues other people open, chosen by `triage.pick`. An
issue the **issue check** refused is **untouched** until it carries one of
kelpie's triage labels, and only one opened after `triage.since` is looked at.
_Avoid_: grooming, intake

**Spam check**:
The classifier a **gateway** runs on an issue after its triage limits and
before the **triager**'s call, under `[app.dogs.kelpie.triage.spam]`. At or
above `triage.spam.threshold` the issue gets `possible-spam` and no comment,
and the maintainer decides. It is no **lamb**: no session, no harness, no
lease, and no place of a triage cap.
_Avoid_: spam filter, spam bot

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
files as follow-ups, all but its nits. A finding the worker shows is
wrong goes in the **disputed findings** file with its reason: the next
reviewer is shown it, and it is never filed.

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

**Browser**:
The Playwright MCP server a worker's `Work` turn drives to look at the UI it
builds, when the project turns on `browser`. It runs outside the worker's
sandbox, since Chromium cannot start inside it, and the worker reaches it
over a socket kelpie filters: only a list of tools passes, and a URL only to
the project's `browser.domains`.

**Hook**:
A project's own command, which kelpie runs itself at one of its points for
every harness and role: `publish` (text bound for the forge), `commit`,
`push` and `handover`. It refuses with exit 2 and a reason, and a hook that
cannot run refuses too. Apart from `worker.guard_hooks`, which are Claude
Code hooks that only a Claude Code worker runs.

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
the dog's **door**. There is no lease on the GPU: a **gateway** queues it,
and `shep paddock run` holds a turn on a model by hand. A bot's round
summons only under its lease.
_Avoid_: lock

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
installation token minted from it, one per repo. Kelpie's rulings, notices
and review rounds are posted as the App, and the issues and labels it makes.
Merging, a review bot's summons and a pull request's labels stay on the
maintainer's own `gh` login. On a repo it covers, a **worker** holds a token
of the App's too, for that repo alone and able to push and nothing more,
never the maintainer's login.
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
`merge-refused`, `closed`, `fix-not-pushed`,
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
nits never do. Where an **App** covers the repo, a ruling is also answered by
a reply in its thread that starts with the ruling's number, as `shep kelpie rule`
does (`99 yes`), from the one login the ruling mentions, if it has write
access to the repo: a bare `yes` answers nothing, and a ruling that mentions
no one is answered at the terminal only. The first answer, from the thread or
the terminal, wins.

A ruling's option can name an **action** kelpie carries out in code when the
option is chosen, by the maintainer or the panel: `rerun` on a `still-red`
ruling, which reruns the head's failed jobs, and `restore` on a `turn-failed`
one, which puts an uncommitted agent file back before the turn resumes. The
option's text says what kelpie will do, and the board's events name the option
that asked for it.

**Merge authority**:
Who merges a green, reviewed pull request, a project's `git.merging`. `ask`
raises a ruling before every merge. `auto` has kelpie merge once every gate
passes, and replaces only that ruling: the merge still asks where a safety
gate holds it, such as a pull request no reviewer read. Deferred findings
follow `git.issues`, not it: a `follow-up` ruling comes only under `ask`.
Every other ruling still asks.

**Notice**:
What kelpie sends after a merge under `auto`, a work item closed with no
change, a withdrawn merge ruling or a finished runner, through the same
webhook as rulings. All but the finished runner's are also a comment on the
work item's thread where an App covers the repo. Not a ruling: it has no id
and takes no answer.

**Draining**:
A runner told to start no new call, while the calls it has running go on to
their end, except a worker turn on Claude Code or pi, which a pause ends at
its next tool call as a **paused turn**. `shep kelpie upgrade` hands each
runner over, and drains and restarts one that cannot be.

**Finishing**:
A runner whose board picks nothing new while its open work items run on to
their end, merged, closed, dropped or stopped by a `no`, after which it
stops its own sheep as a pause does. It outlasts a restart but not a pause, and `shep kelpie start`
ends it.
_Avoid_: winding down
