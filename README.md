# shep-kelpie

A [shep](https://github.com/shep-pm/shep) dog that runs Claude Code workers on your projects, from a planned work item to a merged pull request. It holds the merge gate, the review budgets and the pacing in code, and calls Claude only for the work and for judgement.

This is early. It changes without notice, and there is no release yet.

## How it works

```
 request ──> issue writer ──> issue: acceptance criteria, agent:<name>, ready-for-agent
                                 │
                                 v
 pick:   the project manager, or the board's rule (priority, then age)
                                 │
                                 v
 build:  one worker builds it inline and opens a draft pull request
                                 │
                                 v
 review: each listed reviewer reads it once, in order ──> one fix turn per reviewer
                                 │                          with any finding
                                 v
 CI:     red goes back to the worker; red again on the same head is a ruling
                                 │ green, on the latest main
                                 v
 merge:  a ruling under `ask`, kelpie's code under `auto`, as a merge commit
```

1. **Issue.** You and the issue writer turn a request into an issue with acceptance criteria, one pull request's worth, labelled with the agent that should build it (see [Writing issues](#writing-issues)).
2. **Pick.** A `ready-for-agent` issue joins the board. The project manager's agent picks the next one, or, without one, the board takes it by priority, then age, and opens a work item for it (see [The board](#the-board)).
3. **Build.** A worker, one of the project's implementers, builds it inline, opens a draft pull request and ends its turn (see [Building](#building)).
4. **Review.** Each reviewer in the project's list reads the pull request once, in order. One that finds anything, nits included, sends the worker all its findings for one fix turn, and the next reviewer reads the fix. No loop, no judge (see [The review](#the-review)).
5. **CI.** A red run goes back to the worker. A second red run on a head the worker left alone asks you.
6. **Merge.** Under `ask` you get a ruling; under `auto` kelpie merges once every gate passes. Either way it is a merge commit of the head the gates passed (see [CI and the merge](#ci-and-the-merge)).

Anything kelpie cannot settle itself is a [ruling](#rulings) for you. Each project runs as a sheep of your own shepherd, and every agent call it starts is one of that sheep's lambs (see [What shep does for it](#what-shep-does-for-it)).

## Getting started

From nothing to a worker on your repo. The examples use a project called `scratch`, on the repo `shep-pm/shep`.

### Before you start

You need these on your machine:

- macOS or Linux
- [shep](https://github.com/shep-pm/shep) 0.12, from 0.12.4, and Rust 1.88 or later, to build shep-kelpie
- Claude Code, signed in
- `node` and `npm`: `tools install` runs them, and so does every agent's sandbox
- on Linux, `bwrap` and `socat`, which the sandbox needs
- `git`, and `gh` signed in to the account that opens the pull requests
- `pi` or `codex`, only to run an agent on that harness (see [Agents](#agents))
- a GitHub repo whose default branch is `main`, and a checkout of it

A runner is a sheep, so it gets the `PATH` your shepherd was started with, and it needs `claude`, `node`, `gh` and `git` on it. `shep kelpie doctor` runs in your shell, not the shepherd's, so it can pass while a runner fails to find one. Start the shepherd from a shell where `command -v claude node gh git` finds all four.

### 1. Install

There is no release binary yet. Build it from source:

```sh
cargo install --git https://github.com/shep-pm/shep-kelpie --locked
```

```
  Installing /path/to/bin/shep-kelpie
   Installed package `shep-kelpie v0.0.0 (https://github.com/shep-pm/shep-kelpie#61f0979f)` (executable `shep-kelpie`)
```

The binary is `shep-kelpie`, and `cargo install` puts it on your `PATH`.

### 2. Adopt it in your shepherd

```sh
shep adopt shep-kelpie --name kelpie
```

```
notice[dog_version]: kelpie reports version 0.0.0, shep protocol 11
notice[dog_channel]: kelpie asked for the shepherd channel, so it runs with channel and shutdown_with_message: `shep trigger kelpie <action>` reaches it, and shep stops it with a message rather than a signal
NAME    SOURCE   SHEPHERD  STATUS
kelpie  adopted  false     will start with the next shepherd
```

Keep the name `kelpie`: the `shep kelpie` commands reach the dog by it. If no shepherd is running, `shep muster` starts one, and shep-kelpie with it. Then `shep dogs` shows it:

```
ID  NAME    STATUS  PID    RESTARTS  EXIT  CPU  MEM   UPTIME  SOURCE
0   kelpie  online  23814  0         -     -    8.5M  4s      adopted
```

Adopt it once and leave it enabled. It is the dog that holds the leases every project's runner asks before a summon.

### 3. Install the tools

Every project needs them. Each agent runs inside the sandbox runtime they bring, and a runner won't start without it.

```sh
shep kelpie tools install
```

```
installed kelpie's tools in /path/to/.shep/kelpie/tools
```

It puts the sandbox runtime under shep-kelpie's home, `$SHEP_HOME/kelpie`.

### 4. Add your project

In the checkout:

```sh
shep kelpie add
```

```
label `ready-for-agent`: already on shep-pm/shep
label `ready-for-human`: already on shep-pm/shep
label `in-progress`: already on shep-pm/shep
label `review please`: already on shep-pm/shep
agent files: wrote sonnet-high, opus-high, defect-hunter, coderabbit, cubic, codex, issue-writer, pm in /path/to/.shep/kelpie/agents
runner `scratch`: added with its settings, stopped
`shep kelpie start scratch` runs it
```

On a repo without those four labels, `add` makes them. The runner puts `in-progress` on an issue while a work item has it. It writes kelpie's own [agent files](#agents) where they are missing, and never over one you edited. The project is named after the repo, or `shep kelpie add <name>`, as `scratch` was here.

`add` writes the project's settings with these defaults:

- `git.checkout` and `git.remote`: the checkout and the GitHub repo its `origin` names
- `git.merging = "ask"`: you rule on every merge
- `git.issues = "ask"`: you rule before deferred findings are filed as issues
- `ci.block` is on when the checkout has `.github/workflows`, and `ci.fix_attempts = -1` puts no cap on the worker's fix turns for red runs
- `concurrency.active_items = 1`: one work item working at a time. With more, their turns and reviews run at the same time, one call at a time per work item. A work item parked on a ruling gives its slot up
- `concurrency.pending_rulings = 2`: once two work items wait on your rulings, the board opens nothing new. It does not cap rulings
- `agents.implementers = ["sonnet-high"]`
- `agents.reviewers = ["defect-hunter"]`, with `qwen` first when `~/.claude/scripts/qwen-review.sh` exists, and no review bot
- `pacing.enabled = true`
- `worker.allowed_domains = []`
- `worker.turn_timeout = 60`, in minutes

The checkout is the project's repo, and shep-kelpie runs `git fetch`, `git worktree` and `git branch` against its `.git`. Its worktrees, build folders and state go under `$SHEP_HOME/kelpie/<project>`, never inside it. Add from a clone you don't work in if you'd rather keep your own checkout out of it.

### 5. Check the machine

```sh
shep kelpie doctor
```

```
ok       claude: installed and logged in
ok       gh: logged in as <you>
ok       sandbox: the sandbox runtime can run
ok       shepherd: shep 0.12.4 at /path/to/.shep
ok       dog: kelpie's dog is running and has named itself
ok       scratch: checkout: /path/to/checkout is a git checkout with an origin
ok       scratch: implementers: an issue with no `agent:` label runs on sonnet-high
ok       scratch: push access: may push to shep-pm/shep
ok       scratch: labels: shep-pm/shep has `ready-for-agent`, `ready-for-human`, `in-progress`
ok       scratch: reviewers: each pull request is read by defect-hunter
ok       scratch: rulings: no webhook, so rulings reach you only in the log, `status` and `shep kelpie rule`
nothing a project needs is missing
```

Each `MISSING` line names its fix, and `doctor` exits non-zero until they are done. A ruling is a question shep-kelpie cannot settle itself, such as a merge, and it must reach you. With no webhook it shows only in the runner's log, `shep kelpie status` and `shep kelpie rule`, so you have to look. To be told, add a `[kelpie.webhook]` table for ntfy or Discord to `dogs.toml` in the shepherd's home. Copy that table alone from `kelpie-settings.example.toml`, and put your own URL in it: the file's `url` is a public ntfy.sh topic anyone can read.

Run `shep kelpie doctor` again. With a webhook set it ends:

```
ok       scratch: rulings: rulings post to the ntfy webhook
nothing a project needs is missing
```

### 6. Open the sandbox to your registries

A worker's sandbox reaches `github.com`, `api.github.com` and the model's API, and nothing else. `add` leaves `allowed_domains` empty, so a first issue on a repo with a cold cache can't fetch crates or npm packages. The project's table is stored on its runner sheep, so edit it in `shep lookout`:

1. Open the runner's pane: the sheep named for the project, `scratch` here
2. Put the cursor on the `dogs` row and press `Enter` or `e`
3. Open the `kelpie` row, then `worker.allowed_domains`, and add the registries the project builds from
4. Press `Esc` to write the table

It ends up holding:

```toml
[app.dogs.kelpie.worker]
allowed_domains = ["crates.io", "index.crates.io", "static.crates.io"]
```

For npm, that is `registry.npmjs.org`. A change reaches a running runner at its next wake.

### 7. Start it, and label a first issue

```sh
shep kelpie start
```

It starts the project's runner, and the project runs while it does: `shep start scratch` and lookout's start do the same, and `shep ls` shows it `online`. The runner reads the board from its first pass.

Put `ready-for-agent` on an issue that says what done looks like, with acceptance criteria, or have the issue writer write it (see [Writing issues](#writing-issues)). The runner gives it to a worker, which opens a draft pull request. Then the review runs, each listed reviewer once, then CI. Last, you get a ruling before the merge. `shep kelpie status` shows every project. [The board](#the-board) says what decides whether, and when, an issue starts.

### 8. Answer a ruling

A ruling reaches the webhook you set in step 5, or waits in `status` and `shep kelpie rule`. Answer it from the terminal:

```sh
shep kelpie rule          # lists the rulings waiting
shep kelpie rule 14 yes
shep kelpie rule 14 no rename the flag
```

On ntfy you can also reply in the topic, after a one-time `shep kelpie totp`. [Rulings](#rulings) has the six kinds and how to answer each.

### 9. Pause, and find the log

```sh
shep kelpie pause
```

The runner starts no new call, the calls already running finish and a merge found in flight lands, then shep stops it, so `shep ls` shows it `stopped`. It says what it waits on meanwhile. A session you attached keeps running in your terminal. A ruling still waiting can be answered with `shep kelpie rule` while it is stopped, and the runner acts on the answer when it starts. `shep kelpie start` runs it again. A merge a ruling starts in the moment between that check and the stop can still be cut short, and its work item resumes when the runner starts. If a wait runs out, you interrupt it or shep refuses the stop, the runner keeps running. A runner's log is `shep bleats <project>`, and the dog's is `shep bleats kelpie`.

To wind a project down without watching it, finish it instead:

```sh
shep kelpie finish
```

`pause` stops after the calls running now, mid-item, and `drain` only holds back new calls and never stops anything. `finish` lets every open work item run on to its end, through review, CI, rulings and the merge, picks nothing new from the board, then stops the runner the way `pause` does once the last item has merged, closed or been dropped and any merge notice has been posted. An item parked on a ruling is still open, so finishing waits until its ruling is resolved. With nothing open it stops at once. Meanwhile `status` shows `run: finishing` with the items still open, `shep kelpie status` marks the project `(finishing)`, the board briefing says so, and `add <issue>`, `adopt <pr>` and `rework <pr>` are refused. Pull requests already adopted and waiting for a slot stay waiting until the next start, and `finish` names them. A runner restarted while finishing comes back finishing, and `shep kelpie start` cancels it, so the board picks again, except while the runner is already stopping itself: start it once it has stopped. A `pause` during finishing is the stronger stop: it ends finishing too, so the runner comes back picking when it starts.

## The flow

### Writing issues

The issue writer turns a request into issues an agent can build from. It reads the repo, scopes the request to one pull request's worth, or splits it where each piece works, tests and ships on its own (most requests stay whole), writes acceptance criteria into every issue, and labels each `agent:<name>` with one of the project's implementers.

```sh
shep kelpie issue "let a project be paused from lookout"
shep kelpie issue --interactive "let a project be paused from lookout"
```

On its own, it runs one fresh session in a detached checkout of `main`, files what it writes as `ready-for-human`, and ends with the list of what it filed. shep-kelpie reads each issue back and prints it, with its `agent:` label and the issue it is a sub-issue of. Read them, then label them `ready-for-agent`. An issue without acceptance criteria, its status label or exactly one `agent:` label naming a listed implementer is named with what it lacks, and the command exits non-zero. A session that fails, or ends without the list, keeps its checkout and prints the `claude --resume` line that picks it up.

With `--interactive` it runs `claude` in your terminal, in the project's checkout, with the issue writer's prompt appended and the request as your first message. You plan the issues together, and it files what you agree as `ready-for-agent`, onto the board. It is your session, not a runner's: shep-kelpie starts it and gets out of the way, and Claude Code asks you before each command it runs.

Either way the session reads the checkout with Read, Grep and Glob, and nothing outside it: not gh's config, Claude Code's own files or your shell history. It edits no file. Its Bash runs only what `kelpie guard` lists for it: `gh issue create`, `gh issue view <n>` and `gh issue list` on this repo, and, for the issues it filed in this session, `gh issue edit` on labels and `gh api` on their ids and sub-issue and blocked-by links, each as plain words, with a body given as a heredoc behind a quoted delimiter (`--body-file - <<'EOF'`). It runs no git. A hook after each command records the issues it filed and the ids it read, and the guard refuses an edit or a link on any other. The guard also refuses an issue without acceptance criteria, without the status label, or without exactly one listed `agent:` label, and one whose title or body names a path on this machine. On its own the session also runs in the sandbox, with nothing to write but its own scratch folder and only GitHub to reach. Each implementer's `agent:` label is made on the repo the first time the issue writer needs it.

The issue writer is the agent file `agents.issue_writer` names, `issue-writer` when it names none: edit its body to change its prompt, or its `model` and `effort`, or name a file of your own whose role is `issue-writer`.

The runner asks it too. When the board would open a `ready-for-agent` issue with no `agent:` label and the project lists more than one implementer, one issue writer call reads the issue and the repo, with read tools only, and names the implementer that should build it. shep-kelpie puts that `agent:` label on the issue, and the board opens it on its next poll. The call only picks: it files and labels nothing itself, whatever the agent file says of filing. Its reply's last line of JSON names the agent. It waits on Claude's window like any new work item, even when every role the project lists is local, and `add` refuses the issue while the call runs. The call's line goes in the usage ledger with the issue's number. A call stopped with the runner raises nothing, and the next run asks again. A call that fails, or names no listed implementer, raises a `stuck` ruling on the issue instead of guessing: the board passes the issue over, listed under `skipped` as `unlabelled`, until you answer it. Label the issue, then answer `yes`; an issue still unlabelled goes to the issue writer again. A `no` is refused, since there is no worker to take its note. A label you put on while the call runs stands, and the issue writer's pick goes unused. One labelled while the ruling waits opens anyway, and that clears the ruling. With one implementer listed there is nothing to pick, and an unlabelled issue runs on it.

### The board

Issues labelled `ready-for-agent` are the board. What decides whether, and when, an issue starts:

- The board skips an issue that is assigned to anyone or already has an open pull request, and one blocked by an issue that is still open. Don't assign it to yourself
- An issue with sub-issues is never worked itself: its sub-issues are, and shep-kelpie closes it once every sub-issue is closed
- Of the rest, `priority: P0` to `P3` labels order them, then the oldest goes first. An `agent:<name>` label, such as `agent:opus-high`, picks the agent that works it from those the project lists in `agents.implementers` (see [Agents](#agents)), and a label naming one it does not list keeps the issue off the board. With a `!` at its end, as `agent:opus-high!`, it also pins the work item to that agent, which never falls back. An issue with no `agent:` label goes to the issue writer first when the project lists more than one implementer (see [Writing issues](#writing-issues)). An old `worker:` label is no longer read: that issue counts as unlabelled, and the runner's log says so as its work item opens
- With a project manager set up (see [The project manager](#the-project-manager)), it picks among two or more issues the board could start, and may hold some back. Without one, or while it is down, the order above picks
- A slot under `concurrency.active_items` bounds model calls. A work item takes one when it opens and keeps it through CI and the merge, but one parked on a ruling gives it up, so the board opens the next issue while it waits. Once `concurrency.pending_rulings` work items are parked, the board opens nothing new, and the alert for the ruling that filled it says so. It is the point where the board stops, not a hard limit: items already working can still park past it, and `add` ignores it. `concurrency.pending_rulings = 0` stops the board while any ruling waits, and with none waiting the board opens work as ever. A merged pull request parked on its follow-up ruling counts toward neither cap. An issue whose body names a file the branch of a parked item not yet merged changes waits for that item, and `status` lists it under `skipped` as `overlap`. One whose paths the board has not read yet, or every one while such a branch's files are not known, waits a pass and is listed as `paths-unread`
- Once you answer its ruling, a work item goes on without a slot through anything that calls no model: CI, a merge, or its end. When it needs a worker's turn or a review again, such as for your `no <note>` or a red CI run, it waits for the next slot to free, ahead of any new issue. `status` lists the open items' issues under `working`, `waiting_for_slot` and `parked`
- No turn starts while Claude's 5-hour window is at 50% or more, and no new work item starts once today's share of the week is spent. `shep kelpie status` says why under `pacer`, and `enabled = false` in the project's `[app.dogs.kelpie.pacing]` turns both off

`shep kelpie add <issue>` opens a work item for an issue at once, without the label, ahead of the board's order. It queues nothing: while every slot under `concurrency.active_items` is held or waited for, `add <issue>` is refused.

On a pull request kelpie opened, `ready-for-agent` or a review requesting changes starts a rework of it, the same as `shep kelpie rework <pr>`. On any other open pull request of kelpie's account, `ready-for-agent` adopts it, the same as `shep kelpie adopt <pr>`. Adoptions and reworks go before any issue. Kelpie puts `ready-for-human` on each pull request it hands back.

The runner reads the board from GitHub at most once a minute, however often something wakes it, and at once after something that changes what the board would see, such as a work item ending, a ruling answered, an `add` or an `adopt`. `status` and `drain` answer without making it read anything, so `upgrade` and `pause` can ask them twice a second while they wait. A work item's step or a board read that fails is tried again after 15 seconds, then twice as long each time up to 10 minutes. When GitHub says the account's rate limit is used up, the runner makes no GitHub call until the limit resets, says so once in its log, and `status` shows when under `forge_held_until`.

### Building

A worker builds its work item inline, in a worktree of its own cut from `origin/main`, on the agent its issue's `agent:` label names or the project's default implementer. It commits, pushes, opens a draft pull request that ends `Resolves #<issue>`, and ends its turn. When the repo has a pull request template, it fills that. A project's `worker.instructions_file` adds rules the repo's own docs don't carry.

A worker whose first turn ends with no pull request and no question is sent back once, and asks you for a ruling if it stops short again; once you answer a question it asks, the next turn that stops short is sent back once again. A worker that needs a decision only you can make ends its turn on a question, which reaches you as a ruling, and your answer is its next turn.

A work item with no pull request ends when its issue is closed and it holds no work: no commit on its branch or in its worktree that `main` lacks, and no file left uncommitted. The worker found nothing to change, or you closed the issue. It ends as soon as the worker's turn does, with no ruling. Parked on a ruling, its issue is read at most once every five minutes, and it ends once a read finds the issue closed, its ruling withdrawn. The log, the webhook (when one is set), `status`, the board and `usage` say the issue was closed with no change, and its history entry has `closed` set, `merged` not. shep-kelpie cannot tell who closed the issue, so a worker that closes it on its own ends its work item the same way. Work on the branch or in the worktree, or an open pull request that closes the issue, leaves the work item to go on as before. A turn still running at `worker.turn_timeout` minutes is ended and asks you whether to go on.

Each worker runs inside the sandbox with its fence: it writes only its worktree and build folder, reaches only GitHub, the model's API and the project's `allowed_domains`, and never merges, marks a pull request ready or summons a review bot. `kelpie confine` checks every file write and `kelpie guard` every command. Workers run their test suites under the `cargo-test` lease (see [Leases](#leases)).

#### Skills

Every step shep-kelpie drives an agent through runs a skill, by default from [mattpocock/skills](https://github.com/mattpocock/skills) (MIT). shep-kelpie vendors the ones it uses in `skills/`, pinned to one upstream commit with its licence, and writes them out as a Claude Code plugin when a runner starts. A project installs nothing.

| step | default skill | where it runs |
|---|---|---|
| `triage` | `triage` | not driven yet |
| `spec` | `to-spec` | not driven yet |
| `implement` | `implement` | the worker's first turn on an issue |
| `tests` | `tdd` | named in the worker's instructions |
| `ci_fix` | `diagnosing-bugs` | the worker's turn on a red CI run |
| `pr` | `pr` | named in the worker's instructions, unless the repo has a pull request template |
| `reset` | `handoff` | not driven yet |
| `retro` | `retro` | not driven yet (#104) |

Some vendored skills call others, and those are vendored too: `codebase-design` (from `tdd`), `grilling` and `domain-modeling` (from `triage`) and `writing-for-agents` (from `retro`). No step names them, and a test fails when a vendored skill or its docs call one that is not vendored, except `setup-matt-pocock-skills`, which a project runs once to set itself up and a worker never runs.

A step that runs a skill starts its prompt with the skill's slash command, such as `/mattpocock:implement`, and shep-kelpie's own prompt follows as its arguments. To override one, set it in the project's `[app.dogs.kelpie.skills]` table:

- `{ kind = "path", path = "..." }`: a skill folder with a `SKILL.md`, copied into a plugin of its own, `kelpie-<step>`
- `{ kind = "plugin", plugin = "...", skill = "..." }`: a skill in a Claude Code plugin's folder
- `{ kind = "none" }`: shep-kelpie's own prompt, no skill

A skill that can't load runs shep-kelpie's own prompt instead. The runner logs why, and `status` shows it under `skills`.

### The review

Each pull request goes through a review before CI. A project lists its reviewers in `agents.reviewers`, in the order the review runs them, each an agent file whose `role` is `reviewer` (see [Agents](#agents)). Each listed reviewer runs once, in order, and a list changed mid-pass runs whichever listed reviewers the pass has not. A reviewer whose call fails three times in a row is passed over for the rest of the pass, and `status` lists it under `reviewers_skipped`. One that finds anything sends the worker all of its findings, at its own severity, for one fix turn, and the next reviewer reads the fix. A round of nothing goes straight to the next reviewer. A round of nits gets its fix turn too, and that fix starts nothing new: the pass goes on to the next reviewer as after any fix, and a bot that read the pull request before has its nits on the head that fix pushed left open, so a bot that reviews every push cannot loop on nits. A fix turn that pushes nothing parks on a ruling, unless the worker deferred every finding it was sent or every one was a nit, in which case the next reviewer reads the pull request as it stands: a ruling over nits would ask you about what never holds a merge. A file the review script skipped for its size is cut into hunks and reviewed again, and what those hunks find goes to the fix turn as any finding does; only the notice for a file it still could not review is reported with its round and never sent, since no fix of the worker's reviews it. A pass that ends with no reviewer having read the pull request, because one was down, kept failing or reviewed no file, marks the work item unreviewed: `status` shows why, the merge ruling's question says so, and under `auto` it gets the merge ruling instead of merging. An empty list, or reviewers whose `paths` all miss the change, is your choice, and the log says so once. Every round reads the head on `origin`, which is what CI and the merge take: before one runs, the worker's worktree must hold nothing uncommitted and have that head checked out. If it does not, the worker gets one turn to push or discard, and a worktree still off the pushed head after it parks on a `stuck` ruling for `unpushed`, naming the files, and both heads when they differ, whose yes gives the worker another such turn. After the last one the pull request goes to CI. There is no judge and no second pass: whatever the last fix leaves is what CI and the merge see. The merge takes only a head a round read, or one kelpie sent to CI unread on purpose: a fix turn's push, for review findings or red CI or a conflict, a pass your list gives nobody to read, or its own catch-up with `main` of such a head; an adopted pull request's head as it arrived counts too. Any other head, such as one pushed by hand, is named in the merge ruling, and under `auto` gets the ruling instead of merging. A pass starts again from the top only on new code that needs the whole review, such as the change a merge ruling's `rework` asks for, a change you accept that someone else pushed, or a rework of the pull request. The fix a merge ruling's `no` asks for goes to CI and back to the merge ruling, with no new pass. An empty list reviews nothing.

A finding the worker leaves as out of scope goes into its deferred findings file. Once the pull request merges, `git.issues` decides what becomes of each one above a nit, whatever `git.merging` is: under `ask` a `follow-up` ruling comes first and a yes files each as a `ready-for-agent` issue linking the pull request, onto the board, since your yes is its triage. Under `file` they are filed with no ruling as `needs-triage`, which keeps them off the board until you read them and label them `ready-for-agent`; a repo without `needs-triage` gets it before the first one is filed. The one `follow-up` ruling `file` can raise comes when the forge has refused to take them for six hours: a yes there retries the filing, and they still go out `needs-triage`. Under `skip` nothing is filed and they stay in the review. A nit left there is dropped.

A reviewer runs in one of four ways, by its file's `harness`:

- `claude-code`, `codex` or `pi`: a fresh session on its `model` and `effort`, which reads the worktree with Read, Grep and Glob and runs no command. The file's body is its prompt.
- `command`: a command of your own that keeps the contract below, and takes no body.
- `endpoint`: shep-kelpie's own reviewer, for any OpenAI-compatible server such as Ollama, LM Studio or llama.cpp's server, and takes no body.
- `bot`: a pull request review bot, CodeRabbit, cubic or Codex, summoned on the pull request (see [Review bots](#review-bots)), and takes no body.

A session's prompt is its file's body with the commit the change is against at `{{BASE}}` and the diff at `{{DIFF}}`. A body with no `{{DIFF}}` gets the diff after it. The body asks for kelpie's format, one finding per line as `SEVERITY|file:line|what|why`, or `CLEAN` alone. Every reviewer's prompt carries the issue's acceptance criteria: the section under an "Acceptance criteria" heading, or the whole body without one. `paths` limits a reviewer to pull requests that change a file under one of its globs, and the review skips it elsewhere. `second_look: true` runs a session twice: the second fresh session is shown what the first found and asked only for what it missed, and the worker gets one fix turn for both lists.

shep-kelpie ships `defect-hunter`: Opus 5.5 at high effort reading the whole pull request for defects, meaning a trigger and an effect a failing test could be written from, and not for style or naming, with a second look. It is a project's review when the project lists none. `qwen` runs `~/.claude/scripts/qwen-review.sh`, an optional local review script, and when that script exists a project that lists none runs `qwen` first.

A local model before the defect hunter, and a deeper read of the paths where it pays:

```toml
[app.dogs.kelpie.agents]
reviewers = ["gpu-box", "defect-hunter", "merge-reader"]
```

```markdown
---
role: reviewer
harness: endpoint
url: http://localhost:11434/v1
model: qwen2.5-coder:14b
context: 32768
lease: gpu
---
```

```markdown
---
role: reviewer
harness: claude-code
model: claude-opus-5-5
effort: high
paths: ["src/auth/**", "migrations/**"]
---

Read the change against {{BASE}} for ways it lets the wrong account in. Output
one finding per line as SEVERITY|file:line|what|why, or exactly CLEAN.
```

A missing command or an endpoint that doesn't answer stops the runner at start.

#### Review bots

A review bot reads the pull request on the forge, in its place in `agents.reviewers`, once a pass. shep-kelpie ships a file for each, `coderabbit`, `cubic` and `codex`, which `add` writes out and no project lists until you list it:

```toml
[app.dogs.kelpie.agents]
reviewers = ["defect-hunter", "coderabbit"]
```

```markdown
---
role: reviewer
harness: bot
bot: coderabbit
reviews: 1
hours: 1
---
```

A bot's file is named for its bot, since `reviews` in `hours` is its account's window, which the dog books from that file when it starts and every project shares. Its round marks a draft ready, since a bot may skip drafts, waits for the bot's window and lease, and summons it: CodeRabbit by the `review please` label, or by `@coderabbitai full review` on a pull request it read before that would otherwise find nothing new, cubic and Codex by their comments. A summon with no sign of being heard in fifteen minutes goes out once more. Once a review covers the head, the bot's open threads are its findings, and go to one fix turn as any reviewer's do. A refusal hands the window back to the dog and the round asks again. A window that opens more than an hour on, by the dog's book or by the bot's refusal, passes the bot over for the pass, and so do two hours with no review, two hours its round could not summon it, a bot you stop listing mid-round, and CodeRabbit on a repo the forge reports not public, which is checked just before each summon; `status` lists each under `bots_skipped`, and a pass nobody else read is marked unreviewed. A bot passed over that reviews the head anyway is still read: at the start of each later round, when CI goes green, and every five minutes while the merge ruling waits. Its review gets a round of its own, and its threads go to one fix turn as any review's. Once the pass has ended, that fix goes back to the merge ruling with no new pass, after the round of any other passed-over bot that has reviewed late too, and the ruling says no reviewer read it and comes to you even under `auto`. A merge ruling already waiting is withdrawn for it, which the log and the webhook say, and the work item takes a free slot for the round or waits for one. A bot that already read the pull request this pass and reviews a later head is not sent again: its open threads are named in the merge ruling, and under `auto` any above a nit hold the merge. Open nits are named in the merge ruling on their own, as in `3 nits left open (cubic)`, and never hold a merge. `rounds: 1` lets a bot read a work item's pull request once, whatever its passes, counting its reviews from before a rework or an adoption, and is unset by default. An adopted pull request goes through a pass of the listed bots alone, and until each bot answers a summon of shep-kelpie's own, none of its reviews from before the adoption stands for its read. Codex's `reviews_on_ready: true` says it reviews a pull request when it leaves draft, as the repo's Codex settings may have it: then marking ready, under its lease, is the summon, with no comment, and it is never asked again by comment. List it before any other bot, whose mark-ready would draw its review outside its lease: the runner refuses the other order.

No review bot is listed unless you list it, and a project that lists none never asks the forge about one. CodeRabbit's free plan reviews public repos only, so a repo GitHub marks private cannot list it, and lists `cubic` or `codex` instead. shep-kelpie asks CodeRabbit for a review by putting the `review please` label on, so the repo's `.coderabbit.yaml` must review only labelled pull requests. Without that, CodeRabbit reviews every push on its own and spends the hour a round is waiting on:

```yaml
reviews:
  auto_review:
    labels:
      - "review please"
```

Every thread a review bot leaves open goes to the worker as a finding, and shep-kelpie resolves those threads once the worker's fix moves the head. If the forge refuses three steps in a row, the review goes on with them open and the bot's next read sends them again.

#### Local reviewers

An endpoint takes `url` (the base, up to and including `/v1`) or a `gateway` (see [Gateways](#gateways)), `model`, and `context`, the context size in tokens the server gives that model. Kelpie diffs the pull request, cuts the diff to fit that context, and sends each piece with its own review prompt. Set `context` to what the server really uses: Ollama gives its OpenAI-compatible endpoint a small default context unless `OLLAMA_CONTEXT_LENGTH` says more, and drops whatever doesn't fit without saying so.

A command is run as `<command> --dir <worktree> --round <n> --diff <base>`, with:

- `QWEN_REVIEW_OUT`: the folder to write in
- `KELPIE_REVIEW_HEAD`: the commit under review
- `TMPDIR`: the folder the GPU lock lives under
- `KELPIE_REVIEW_CRITERIA`: a file holding the issue's acceptance criteria

It writes `round-<n>.txt` in that folder, one finding per line as `SEVERITY|path:line|what|why` with `HIGH`, `MEDIUM` or `LOW`, and then an empty `round-<n>.txt.done`. Kelpie reads nothing without the marker, and nothing from stdout. A nonzero exit fails the round. A command that writes `LOW|<path>:0|not reviewed: <n> lines exceeds the chunk limit|...` is run again with `--files <hunk file>` in place of `--diff`, on that file alone. If that run fails, its file is left unreviewed, as below, with the failure as the reason; only when kelpie cannot cut the hunk with `git diff` does the placeholder stay as the finding. Any other `LOW|<path>:0|not reviewed: <why>|...` line, as the script writes when the model cannot be reached, is a file left unreviewed and not a finding. A round with nothing but those lines reviewed nothing, and the review goes on to the next reviewer. A round that leaves the same files unreviewed as the same reviewer's last round counts against it too, and one that leaves none clears its count. A second such round in a row, which takes two passes, leaves the reviewer out of the review for the rest of the work item, and `status` lists it under `local_reviewers_down`. Another local reviewer, on another command or server, still runs. A round with real findings and some `not reviewed:` lines keeps its findings.

`lease` names the lock kelpie holds around each round of a command or an endpoint. `gpu` is this machine's GPU lock, the one the qwen scripts take. Any other name is a lock of its own, so a reviewer on another machine's GPU never waits on this one's. Leave it off for a command that takes the lock itself, as `qwen-review.sh` does. An endpoint behind a gateway takes no lease, since the gateway queues its rounds.

With a lease, before a round against Ollama, kelpie reads the host's `/api/ps`. An endpoint's host is its `url` without the `/v1`. A command names its host with `ollama: http://localhost:11434`, which needs a lease, and its model with `ollama_model`, else every model the host has loaded is checked. A model partly or wholly on the CPU fails the round and raises a ruling, and a yes runs the round again once the model is back on the GPU. A host with no `/api/ps` is not checked, and `status` shows the model's name, its share on the GPU, its context length and when it unloads.

### CI and the merge

After the review's last reviewer, kelpie waits for CI on the pull request's head. A red run is the worker's next turn, naming the failed checks; a second red run on a head the worker left alone asks you. `ci.fix_attempts` caps those fix turns for a work item: once the worker has had that many, the next red run asks you instead, and `0` asks on the first. `-1`, the default, sets no cap. A green run on a branch without the latest `main` is caught up first: kelpie rebases its own commits onto `main` and pushes with a lease, or merges `main` in where the branch holds a merge or commits that are not its own, and a conflict is the worker's next turn. A project with `ci.block = false` reads no checks, and `ci.fix_attempts` does not apply. Review rounds never wait for CI.

A project on `git.merging = "ask"` gets a ruling before every merge, and a yes merges only the head it was asked about, while CI on it is green and it has the latest `main`. A project on `"auto"` merges its pull requests without asking once every gate passes, and posts a notice after. Every other ruling still asks under `auto`, and so does the merge for a pull request no reviewer read, one with a review bot's thread above a nit open, a head no round read, the fix a merge ruling's `no` asked for, and the fix for a bot's late round: each is a safety gate, not a setting, and the ruling names which one raised it. The merge is always a merge commit of the head the gates passed, never a squash, and kelpie then removes the worktree, both branches and the build folder. With a merge queue on `main`, kelpie queues the pull request and waits on the queue.

A commit someone else pushes to a work item's branch, or a label or ready state kelpie did not set, parks it on a `foreign-change` ruling. A pull request that changes agents' own files, such as `.claude` or `.mcp.json`, parks it on an `agent-files` ruling. A pull request you merge by hand ends its work item; one closed without merging parks it.

## Agents

An agent is a file: `$SHEP_HOME/kelpie/agents/<name>.md` (or the `agents` folder of the home `KELPIE_HOME` names), YAML frontmatter and then a Markdown body. The name is the file's name without `.md`. An implementer's body is added to kelpie's own instructions for that agent, and an empty body adds nothing. A reviewer's body is its prompt (see [The review](#the-review)).

```markdown
---
role: implementer
harness: claude-code
model: claude-sonnet-5-5
effort: high
---

Extra instructions for this agent, added to kelpie's own.
```

`role` is what the agent is for: `implementer`, an agent that builds a work item, `reviewer`, one that reads a pull request, a review bot included, `issue-writer`, the one `shep kelpie issue` runs, whose body is its prompt and which runs on `claude-code` alone (see [Writing issues](#writing-issues)), or `pm`, the project manager, likewise (see [The project manager](#the-project-manager)). A key only another role takes, such as a reviewer's `paths` or `second_look`, stops the runner. `harness` is Claude Code, `claude-code`, pi, `pi`, which runs a model on an OpenAI-compatible server such as Ollama, or Codex, `codex`, on a ChatGPT plan. `model` and `effort` are what the harness runs. Any other key stops the runner, and so does a file that does not parse or misses a key its harness needs, naming the file and the key. A `.md` file whose name is no agent's, such as a `README.md`, is skipped and named in the log. A runner sees an edit to the folder the next time it wakes, as it sees a settings change.

Kelpie ships `sonnet-high`, Sonnet 5.5 at high, the default implementer, `opus-high`, Opus 5.5 at high, for work that is hard to undo, `defect-hunter`, the default reviewer, `qwen`, the reviewer that runs the qwen-review script, the review bots `coderabbit`, `cubic` and `codex`, `issue-writer`, Opus 5.5 at medium, and `pm`, the project manager, Opus 5.5 at medium. `shep kelpie add` writes out any that are missing, `qwen` only where the script exists, and never writes over one you edited, and a file named for one replaces it.

A project lists the agents that build its work items and the ones that review its pull requests, from those files:

```toml
[app.dogs.kelpie.agents]
implementers = ["sonnet-high", "opus-high"]
reviewers = ["defect-hunter"]
```

An issue labelled `agent:<name>` (the prefix in any case) runs on that agent, which the project must list. With more than one listed, the issue writer labels any other issue before the board opens it; with one, it runs on that one. Any implementer may be first, a local one included, and a list may be all local. `sonnet-high` alone when absent. An implementer's file must say `role: implementer`, and a reviewer's `role: reviewer`. When the runner starts, and when `agents.implementers` changes, it makes each listed implementer's `agent:` label the repo lacks. It never removes or changes a label, since removing one strips it from every issue, closed ones included.

A work item keeps the agent it opened on, and each turn runs that agent's file as it is then, with one exception. With `agents.fallback_after` set, in minutes, a work item's first turn that has waited that long for its model is stopped and moves to the next implementer listed after its own that no other work item is waiting on or moving to. Its first turn has no output and no session yet, so nothing is lost, and its next turn starts fresh on the new agent. The log's `fell-back` line names both agents. A later turn never moves, nor does an item whose label ends in `!`. With no implementer after its own free, it goes on waiting.

A turn waits for its model while it asks for a lease on it, from a gateway or the GPU lock, while its gateway answers busy, or while it has shown no output for `agents.fallback_after` (10 minutes with that off). `status` shows it on the work item as `waiting`, with `since` and `why` (`lease`, `busy` or `silent`), and the board as "Waiting for its model since". Its first output clears it. That covers a turn behind a gateway's lease or the GPU lock, and a harness whose output kelpie reads as it comes, such as pi. A Claude Code turn is not reliably marked: headless Claude Code retries a provider's overloaded or rate-limit answer inside the call and says so only when it ends, and kelpie reads its activity from the session's transcript, which can hold lines before any output. So a Claude Code turn may never be marked waiting, and its fallback is unreliable.

`usage` says how an agent's usage is read, and so which account paces it: `claude` (the default on Claude Code) reads `/usage`, `codex` reads Codex's own 5-hour and weekly windows, and `none` is a local model that is never paced. It must be the harness's own reader, so leave it out: Claude Code reads `claude`, pi reads `none` and Codex reads `codex`.

A pi agent also needs the model's server and the context size it gives the model:

```markdown
---
role: implementer
harness: pi
model: qwen3.8:27b
effort: medium
url: http://<host>:11434/v1
context: 65536
---
```

Kelpie runs pi with a home of its own, so your `~/.pi` is never read. pi runs no Claude Code hooks, so a worker on pi can't have `worker.guard_hooks`, and the runner refuses to start with them. Kelpie's own checks still run on every command and file write.

A pi call's sandbox allows no host of the model's, since the sandbox opens every port of an allowed host and Ollama's admin calls (pull, delete, create) answer beside chat. Kelpie runs a forwarder outside the sandbox for each call, and the sandbox allows only that. It passes `POST` to the server's `/v1/chat/completions` and refuses every other path and method, naming what was asked. The worker never sees the model's address. The forwarder dials `http://`, so `url` is the server's `http://` address: an `https://` one is refused when settings load.

#### Gateways

A gateway such as paddock, a shep dog, puts one host's model servers behind one endpoint: it loads and unloads models, queues what doesn't fit yet, and lets a long job hold a lease on its model. Name it once in shep-kelpie's own settings:

```toml
[kelpie.gateways.paddock]
url = "http://gpu-box:8700"
key_env = "PADDOCK_KEY"
```

`url` is its `http://` address, with its OpenAI routes under `/v1`. `key_env` names the variable in the runner's environment that holds the key the gateway gave kelpie, so set it in the runner sheep's `env`. A runner reads its gateways when it starts. A `pi` agent or an `endpoint` reviewer then names the gateway in place of `url`, with `model` as the gateway knows it:

```markdown
---
role: implementer
harness: pi
model: qwen3.8:27b
effort: medium
gateway: paddock
context: 65536
---
```

For a model behind a gateway:

- Kelpie takes no `gpu` lock and reads no `/api/ps`. The gateway queues each call and decides what stays loaded, so such an agent takes no `lease`.
- The key never enters the sandbox. pi's provider file holds a placeholder, and the forwarder sends the gateway's key in its place. An endpoint's round sends it from kelpie's own `curl`, on its stdin. Every process kelpie starts, an agent call or not, has `key_env`'s variable unset, and a reviewer's session can't read the shepherd's home, where the runner's `env` keeps it.
- A runner won't start while an agent it lists names a gateway its settings lack, or one whose key's variable is unset.
- A worker's turn holds a lease on its model from its start to its end, so the gateway evicts nothing it runs on midway. The turn's ceiling counts from the grant, and a gateway too busy to grant it is asked again every 30 seconds until then. A turn that ends or fails releases it, and a runner that dies stops renewing it, so it runs out two minutes later. Only a worker's turn takes one.
- A pi call's tokens reach the usage ledger unpriced, as any pi call's do. The gateway doesn't say how long a request queued, so that wait is part of the call's seconds.
- `shep kelpie doctor` checks each gateway: that it answers `GET /v1/models`, that it takes the key (unsure when the shell you run doctor from doesn't have the variable), and that it lists every agent's model.

An implementer behind a gateway is a local implementer, like any `usage: none` one. Claude Code can't run on a gateway yet. A gateway serves as many conversations at once as it allows. A project sharing one local model server without a gateway keeps `concurrency.active_items` at 1: two work items on one model that serves one sequence at a time evict each other's cached prompt, and each request reads its whole prompt again.

A Codex agent needs the `codex` command on the shepherd's `PATH` and a ChatGPT plan, and takes only `model` and `effort`:

```markdown
---
role: implementer
harness: codex
model: gpt-6.1-sol
effort: medium
---
```

`codex debug models` lists the plan's models. It runs on shep-kelpie's own ChatGPT login, in `codex_home` (`$SHEP_HOME/kelpie/codex` by default, or the path `codex_home` in the `[kelpie]` section names), never your `~/.codex`. Sign it in once:

```sh
CODEX_HOME="${SHEP_HOME:-$HOME/.shep}/kelpie/codex" codex login --device-auth
```

Each Codex call gets a home of its own with that login linked in, so no call sees another's sessions. A runner reads `codex_home` when it starts. A worker on Codex can't have `worker.guard_hooks`, the same as pi. Kelpie's checks run as Codex's own hooks: `confine` on every `apply_patch`, `guard` on every command.

A local implementer is one whose usage is `none`. Each account keeps its own daily allowance and 5-hour stop, shown under its name in `status.pacer`, and a new work item waits on every account its roles spend. `shep kelpie doctor` checks Codex answers for a project that spends it. A `none` agent holds `lease` (the GPU lock, `gpu`, by default) for the whole of each call instead, so a qwen round waits behind its turn, and `status.local_leases` shows who holds it.

`status` shows each role's tokens in `by_role`, with `cost_usd` only for calls whose harness reports dollars. `unpriced_calls` counts the rest.

## The board and the project manager

### The briefing

The runner writes the board out as `board.md` in the project's folder, and `status` names the file under `board`. It is the briefing the project manager's agent reads, so that agent never runs git or gh itself, and you can read it too. Code writes it, with no model call, at start and whenever the board changes: a call starting or ending, a phase change, a ruling raised or answered, a read of the ready queue. It is written again at least once a minute while the runner looks at the board, and each write replaces the file whole. Text that names this machine is withheld from it, as from the forge. It holds:

- each open work item: its issue, phase, pull request, age and agent, the worker's last closing message, and the files its branch touches
- what each item's session is doing. A worker's turn shows when it last made a tool call or wrote output, read from its transcript (Codex's output file, pi's session file), and reads as idle after 10 minutes of neither
- the rulings waiting on you
- the ready queue in the board's order, with each issue's priority, why the board passes over it if it does, the paths its body names in backticks, and the first 600 characters of its body, quoted
- the board's events since the project manager last read it, or the last 20 before it ever has
- the overlap: for each pair of open branches, the files both touch and the files `git merge-tree` finds in conflict, and each ready issue's named paths against them. The conflict check needs git 2.38 or later

### The project manager

A project can name an agent that manages its board, the project manager:

```toml
[app.dogs.kelpie.agents]
implementers = ["sonnet-high"]
pm = "pm"
```

Kelpie's own `pm` is Opus 5.5 at medium. Code wakes it, never a timer, when:

- a slot is free and two or more ready issues could fill it
- the branches of two open work items conflict, as `git merge-tree` finds them
- a work item is stuck: its turn failed, it stopped twice with no pull request, CI stayed red with no fix pushed (each of which already asks you a ruling), or its worker has been idle for 10 minutes
- you tell it something: `shep kelpie tell "hold #12 until the release"`

Each wake carries every reason gathered since the last one, in one call that runs as a lamb like any other and that the runner never waits on, one at a time per project. It runs in `<project>/pm`, which holds `board.md` as it stood when it woke and its own notes, `pm-notes.md`. Of kelpie's and the shepherd's homes it reads that folder alone, and it never reads your checkout, `gh`'s token or the credentials no worker reads. It reaches no host but the model's, has only Read, Glob, Grep, Edit and Write and no MCP server, runs no command, git or gh, and may only add to the end of its notes, at most 64 KiB a write, which a hook holds it to. It answers in one JSON object: a pick, the ready issues to hold until an open work item closes, an unstick (`retry`, `re-scope`, `ask` or `none`) and a reply to you.

Kelpie checks the answer against the board before it acts. A pick or a hold must name a ready issue the board shows, and an unstick a work item that is stuck now. Anything else is dropped and logged as `pm-answered`, with why. A `retry` answers the item's ruling yes (for CI still red, sends the worker back to it), or ends an idle worker's call and resumes its session. A `re-scope` or `ask` adds the project manager's words to the item's ruling, on one line after `PM says:`, and posts it to you again. A pick of none starts nothing until its next wake; with no work item open it lasts 30 minutes before the board's rule picks, and a hold over every issue the board could start goes at once, so a project with nothing in flight never stalls. Its reply shows in `status` under `pm.reply` and in the log.

It keeps one session per project, resumed on each wake. Once a wake leaves its context past 100k tokens, the session is compacted before the next wake. A session that cannot be resumed starts again from the board. `shep kelpie pm` opens that session in your terminal, in its settings and sandbox, as `attach` opens a worker's: the runner holds the project manager for that command and for the session it starts, each known by its pid and start time, so no wake starts until both have ended, and a wake already in flight runs to its end first. One terminal holds it at a time. When there is no session yet, `pm` starts the one its wakes then resume.

With no project manager named, while it is down (a failed call passes it over for 10 minutes), over Claude's 5-hour pace, held in your terminal, or when its pick is dropped, the board's rule picks and a stuck item waits on its ruling, as without one.

Measured on 33 real decision points from this repo and shep, a project manager briefed this way avoided 9 to 12 of 11 to 15 avoidable conflicts where the board's rule avoided 4 of 15, and decided as well as one that ran git and gh itself at about half the cost a wake. On a scripted day of 40 wakes, one session compacted as it went cost $3.31 a day on Opus, against $8.16 for a fresh session every wake, and kept what it knew.

## Attaching to a worker

`shep kelpie attach <issue>` takes over a work item's worker by hand. The runner holds the work item, so no call starts for it. A call already in flight runs to its end, and `attach` says it is waiting. Then it resumes the worker's session with `claude --resume` in your terminal, in the item's worktree, with the worker's settings file and inside the worker's sandbox, so its deny rules, its guard and its fence hold while you drive. Claude Code asks before each tool call as it always does.

When you exit `claude`, the work item carries on, and its next turn resumes the same session. A commit or push you make while attached is the worker's own, as a push in a turn is, so the gate takes it as the branch's head rather than a change kelpie did not make. The runner holds the work item while either the `attach` command or the session it started runs, each known by its pid and start time, and lets it go at its next pass once both have ended. A SIGTERM or SIGHUP to `attach` is passed on to the session, which `attach` waits for. `status` shows the hold under the item's `attached`, and `drop` refuses the item while it is held.

`attach` changes nothing and says what to do instead for a work item whose worker has no session yet (its first turn starts one), one parked on a ruling (answer it first), one holding no slot under `concurrency.active_items` when none is free (its session would be one more model call; with one free, `attach` takes it), or one merging. Only a Claude Code worker can be attached: a Codex or pi worker's session resumes another way, by hand.

## Rulings

A ruling is a decision only you make. The worker waiting on one is parked, and the rest of the project goes on: a parked work item gives its slot up, so the board can open another (see [The board](#the-board)). It is one of six kinds:

- `merge`: CI is green, and a yes merges the pull request. A `no <note>` sends the worker your note, and its fix goes to CI and back to you with no new pass of the review; a `rework <note>` sends your note for a change the whole review reads again. On a merge ruling that warns of open review bot threads, a pass no reviewer read or a head nothing vouches for, `no` starts a new pass too, since only a pass clears that, and the question says so. The merge ruling after a `no` fix, or after the fix for a review bot's late review (see [Review bots](#review-bots)), says its head is that fix and that no reviewer read it, and comes back to you even under `auto`. Open review bot nits are named on their own and hold nothing
- `question`: the worker asks, and your answer is its next turn
- `stuck`: the work item cannot go on by itself, and its reason says why: `rebase`, `still-red`, `merge-refused`, `closed`, `local-model-spilled`, `fix-not-pushed`, `unpushed`, `turn-timeout` or `turn-failed`. The question says what a yes does
- `agent-files`: the pull request changes agents' own files, and a yes accepts them
- `foreign-change`: someone else changed the pull request, and a yes accepts the change
- `follow-up`: a merged pull request left findings unfixed, and a yes files them as issues

A ruling shows in the runner's log, in `shep kelpie status` and in `shep kelpie rule`, and posts to the webhook when kelpie's `[kelpie.webhook]` is set (see [Settings](#settings)). The pull request gets a comment for every ruling but a merge. Answer it from the terminal:

- `shep kelpie rule 14 yes`
- `shep kelpie rule 14 no rename the flag`
- `shep kelpie rule 14 rework split the parser out`, for a merge ruling only
- `shep kelpie rule 15 use --dry-run`, for a worker's question. A `yes` there is the answer's text

A ruling's id is unique across projects, so `rule` needs no project. Quotes are optional, but zsh still needs them around a note with `?`, `*`, `!` or an apostrophe. `-p` goes before the answer: after its first word, and after a `--`, a `-p` is part of the answer. `shep kelpie rule` alone lists the rulings waiting and asks which to answer and how. Each ruling's question says what `yes` and `no`, and on a merge ruling `rework`, do. A `no` or `rework` with a note is the worker's next turn.

On an ntfy webhook, a ruling can be answered from the topic: run `shep kelpie totp` once and scan the QR code into an authenticator app, then reply with the line the alert ends on, such as `14 yes <code>`, with the app's code last. A reply takes the same answers as `shep kelpie rule`. Anyone who can read the topic can read rulings, but only a reply with the code of the moment answers one, and each code answers once. Five wrong codes turn answers off until `shep kelpie totp --unlock`, and `shep kelpie totp --rotate` replaces a secret that may have leaked.

With a project manager, it may answer a `stuck` ruling for you with a retry, or add its words to the ruling and post it again (see [The project manager](#the-project-manager)).

## Upgrading

```sh
shep kelpie upgrade --ref main          # build shep-kelpie at a git ref and install it
shep kelpie upgrade --release 0.3.0     # install a release (none is published yet)
shep kelpie upgrade --binary ./kelpie   # install a build made by hand, as it is
shep kelpie upgrade --rollback          # put back the build the last upgrade replaced
shep kelpie upgrade --ref main --now    # restart the runners without draining them first
```

The installed shep-kelpie is whatever program the adopted dog runs, which the shepherd knows (`~/.cargo/bin/shep-kelpie` for the install above), and `shep kelpie add` registers runners that run the same path. An upgrade writes the new build beside that file and renames it over, as shep upgrades itself, so the running file's bytes are never edited in place. Before the swap it copies the file it replaces into `$SHEP_HOME/kelpie/builds` (resolved first if the path is a symlink), and `--rollback` puts that copy back the same way. Then it restarts the dog and each running runner one at a time, never while a work item is merging: it waits, and says what it waits on. A runner that does not answer `status` is waited on too, and named if it never does. A runner you stop while the upgrade waits stays stopped, and so does one stopped before it. A stopped runner starts on the new build when you start it.

Before it restarts a runner, the upgrade drains it, so the restart cuts no worker's turn short. The runner's `drain` trigger tells it to start no new call (a worker's turn, a reviewer's session or local round, the project manager's wake), while the calls it has running go on to their end and its merges go on as ever. The upgrade asks again until the runner shows no call running, saying what it waits on, then waits out any merge and restarts it. It waits no longer than the runner's turn ceiling (or the project manager's, if longer) plus five minutes, counted from when the wait began. A turn's ceiling counts from when it got the lease it waited for, and a reviewer's session has no ceiling, so either can outrun that bound. Past it the upgrade sends the runner `undrain`, so it starts calls again on the old build, names it, and restarts nothing for it, as for a merge that outlasts its wait. A runner on a build from before `drain` is restarted as before, and the upgrade says the restart cuts its calls short. `--now`, first or last, restarts without draining, for when you want the build in place at once; the merge wait still holds. The dog is never drained, since the calls are the runners'. A restart ends draining. So does a Ctrl-C or SIGTERM to the upgrade, or any failure, while a runner drains: the upgrade sends it `undrain` before it exits. Only a shepherd it cannot reach leaves one drained. The upgrade prints `shep kelpie undrain -p <project>` as each drain starts, for that case, and `shep kelpie status` marks a draining runner `(draining)`.

If an upgrade stops after the swap (a merge or a call that outlasts the wait, a sheep that does not come back, Ctrl-C), the new build is already installed and the message says so. `shep kelpie upgrade --binary <installed path>` finishes the restarts and touches no file. `--rollback` does not: it swaps the two builds again.

This build writes each project's state file as version 16, and every older build refuses a version 16 file, whatever it holds, so going back by `--rollback` or any other way leaves the older build unable to read it. An older build also never knew a work item that holds no slot: parked, waiting for one, or going on without one. Finish or drop those before going back. Draining only stops new calls and leaves them in the state file.

Every sheep it restarts must run the dog's path. If one does not, the upgrade stops before it changes anything and names the sheep and its path. A `--binary` without its executable bits is refused with the `chmod +x` that fixes it, and only one upgrade runs at a time: a second refuses while the first holds `$SHEP_HOME/kelpie/upgrade.lock`. The swap changes the file for every shepherd that adopts the same path, with no check against their shep.

Before it changes anything it asks the new build which shep it is made with (`shep-kelpie version --json`) and compares it with your shepherd's. If your shepherd is on another minor, or on an earlier patch than the build's, it stops before restarting anything and prints the steps: upgrade shep and reload its shepherd, then run the new build's own `upgrade` with `SHEP_HOME` set to your shepherd, since the old kelpie that `shep kelpie` runs refuses the new shepherd. The upgrade prints the exact line. `--rollback` checks the previous build the same way.

## What shep does for it

shep-kelpie is a dog of your own shepherd, and leans on it for everything a process manager does:

- **Supervision.** The adopted `kelpie` is the dog, and each project's runner is a sheep. shep starts, restarts and stops them, and keeps them beside your other sheep. `shep stop <project>` stops a runner, and `shep delete <project>` removes it
- **Lambs.** Every agent call a runner starts, a worker's turn, a reviewer's session or the project manager's wake, is a lamb of that runner. `shep describe <project>` labels each with its issue and role, such as `#114 worker`
- **Stopping the calls.** A runner asks for `shutdown_with_message`, so a stop reaches it as a message, and it sends each call's process group SIGTERM and exits at once, without waiting for them. shep's stop then ends every lamb the runner left, whatever process group or session it is in (shep-pm/shep#688, ADR 0005), so a runner's entry takes shep's default `kill_timeout`. A runner still ends a single call itself, at its turn ceiling, through the process group each call leads
- **Triggers and status.** Every `shep kelpie` verb is a trigger on the shepherd channel, which `shep trigger <project> <action>` sends too, and the runner answers while its calls run. Runners ask the dog for leases with metrics on shep's bus
- **Settings.** A project's settings are its runner's `[app.dogs.kelpie]` table, and shep-kelpie's own are the `[kelpie]` section of `dogs.toml`. shep keeps both, and lookout edits both from shep-kelpie's settings schema
- **Logs.** `shep bleats <project>` is a runner's log, and `shep bleats kelpie` the dog's

One thing shep-kelpie still does itself, until shep can:

- **Reaching you.** Rulings go to kelpie's own webhook, ntfy or Discord, and ntfy replies are checked with kelpie's own TOTP, until shep can carry a question itself (shep-pm/shep#689)

shep-kelpie refuses a shepherd on another shep minor or major than the pinned one, or on an earlier patch, naming both versions. It needs 0.12.4 or later, the first shep whose stop ends every lamb. shep-kelpie's commands talk to the shepherd's socket with the shep client it is built with, never a `shep` on `PATH`. Keep a shepherd's `SHEP_HOME` short: see [shep-kelpie's home](#shep-kelpies-home).

## Reference

### Commands

shep-kelpie runs in your own shepherd, beside your other sheep. The adopted dog holds the leases every runner asks before a summon, and it asks shep for the channel the lease commands reach it on. `add` and `start` say how to bring the dog up when it is not running with its channel.

```sh
shep kelpie add        # labels, settings, agent files, and the runner, stopped
shep kelpie start      # starts the runner, which runs the project
shep kelpie pause      # stops it once its calls end
shep kelpie finish     # stops it once its open work items end, picking nothing new
shep kelpie status     # every project
shep kelpie doctor     # what each project still needs on this machine
shep kelpie rule 14 yes
shep kelpie issue "<request>"   # issues for you to read, or --interactive
shep kelpie attach 7   # steer issue 7's worker in this terminal
shep kelpie upgrade --ref main
shep kelpie tools install
shep kelpie totp       # once, to answer rulings from ntfy
shep kelpie lease status
```

Every trigger a person sends the runner is also a verb: `add <issue>`, `rework <pr>`, `adopt <pr>`, `gate [<issue>]`, `drop [<issue>]`, `timings [<n>]`, `tell "<note>"`, `pm`, `rule`, `drain`, `undrain` and `finish`. Each reaches the project whose repo holds the folder you run it in, a worktree of it included, or the one `-p <project>` names anywhere in the line. From anywhere else it lists the projects and guesses nothing. `shep trigger` still works.

- `drain` holds back every call the runner would start next, and answers the status with the calls still running under `draining`. `undrain`, or a restart, lets calls start again. `shep kelpie upgrade` drains each runner itself: see [Upgrading](#upgrading)
- `drop [<issue>]` ends a work item without merging it. Its worktree, local branch and build folder go, and its pull request stays, labelled `ready-for-human`
- `gate [<issue>]` sends a work item whose worker's turn ended with a pull request into the review gate, when it was never entered
- `tell "<note>"` and `pm` reach the project manager: see [The project manager](#the-project-manager)
- `timings [<n>]` totals where the time went over the last `n` finished work items (10 when left out), in six phases: `worker`, `review`, `ci`, `ruling`, `merge` and `other` (every second lands in exactly one), and answers JSON with the totals under `seconds` and a plain-text `table`, so `shep kelpie timings 20 | jq -r .table` prints it. `status` shows each open item's split under `timings`, and the ten most recent finished items under `history`. The state file keeps the last 100

`usage` reads the project's usage ledger, `<kelpie home>/<project>/usage.jsonl`, here, without the shepherd. Each model call adds a line to it as it ends: the runner writes the worker's turns, every reviewer's sessions and local rounds and the project manager's wakes and compactions, and `shep kelpie issue` the issue writer's headless runs (not `issue --interactive`, which is your own `claude` session). Each line has the call's four token counts, its units, its dollars where the harness reports them, and the pacer's last reading where there is one (the issue writer's and imported lines have none). A finished work item adds a line of its own. `shep kelpie usage [<project>] [--since <date>]` prints, for each merged pull request, its units, dollars and the calls those dollars leave out because their harness reported no cost, wall time, rulings and the worker's and reviewers' shares of the units, with their medians, the loaded units per merged pull request (every call, the project manager's, the issue writer's and those of work items dropped or closed with no change included, over the merged count), the work items that ended with their issue closed with no change and those dropped, then the project manager's and the issue writer's totals. Every dollar figure says how many unpriced calls it leaves out, as in `$4.20 + 3 unpriced calls`; with no project it prints every project that has a ledger. Units weigh tokens as the control room was measured: cache read 0.1, one-hour cache write 2, five-minute cache write 1.25, output 5, uncached input 1. `shep kelpie usage <project> --import <log file>` adds the turns a runner logged before the ledger, its `ended` and `asked` lines in `$SHEP_HOME/logs/<project>-0-out.log`, skipping any the ledger already holds. Baselines to compare with, such as the control room's own units per merged pull request, can go in `<kelpie home>/baselines/baselines.json`, which stays on your machine; `usage` holds each against kelpie's loaded figure and its median, as kelpie over the baseline, so below 1 is cheaper.

`add` names the project after the repo, or `shep kelpie add <name>`. It makes the four labels where the repo lacks them, writes kelpie's own agent files where they are missing, and registers the runner, holding the project's settings as its `[app.dogs.kelpie]` table. Running `add` again changes nothing. `start`, `pause` and `finish` find the project from the checkout, or take its name.

`doctor` changes nothing and prints one line per check, each missing piece with its fix, then exits non-zero if a project needs something it lacks. It checks that `claude` is installed and logged in, that `gh` is logged in and may push to each project's repo, the sandbox runtime every agent runs in, the shepherd's version, each project's checkout, implementers and labels, CodeRabbit where a project lists it, each project's reviewers, in order, with any command or endpoint among them checked as a runner's start checks it, Codex's usage for a project that spends it, and which webhook, if any, rulings post to. `shep kelpie doctor <project>` checks one project. `--test-alert` posts one test alert to the webhook, which is the only post it ever makes. A line marked `unsure` could not be settled, and does not fail the run: CodeRabbit is one, since a repo it has not yet reviewed looks the same as a repo without it.

### Leases

The dog holds `cargo-test`, a share of this machine for running tests, and `gpu`, this machine's GPU lock. Workers run their test suites under `cargo-test`, and so can anything else on the machine: `shep kelpie lease run cargo-test -- cargo test` waits for a turn, runs the command and exits with its code, or 75 when it cannot reach the dog. Three hold it at once unless `[kelpie.leases]` says otherwise, the rest queue, and `shep kelpie lease status` shows who holds each lease and who waits. A command that ends or dies gives its turn back. One whose `lease run` was killed outright keeps it until the command, and anything it left running, exits: `status` shows how long each has held it.

`shep kelpie lease take gpu` holds the GPU lock for you until `shep kelpie lease return gpu`, so whatever takes that lock waits. Use it when you need the GPU to yourself.

### Settings

A project's settings are its `[app.dogs.kelpie]` table, which lookout edits in the runner's pane. Every setting sits in a table of its own. `settings.example.toml` lists every key, and shows a runner's Flockfile entry for a project set up by hand. It needs `SHEP_HOME` as an absolute path in `env`, since a sheep starts without it. An older entry's `kill_timeout = "10s"` still works, and is no longer needed. The keys:

- `git.checkout`: the project's checkout
- `git.remote`: its GitHub repo as `owner/name`. Left out, the runner reads it from the checkout's `origin` when it starts, and an `origin` that is not a GitHub repo stops the runner naming the setting
- `git.merging`: who merges a green, reviewed pull request, `ask` or `auto` (see [CI and the merge](#ci-and-the-merge))
- `git.issues`: what becomes of deferred findings, `ask`, `file` or `skip` (see [The review](#the-review)). `ask` when left out
- `ci.block`: whether the repo runs CI that a merge waits for, and `ci.fix_attempts`: how many fix turns red runs get, `-1` for no cap (see [CI and the merge](#ci-and-the-merge))
- `concurrency.active_items` and `concurrency.pending_rulings`: how many work items hold a slot, and how many parked on rulings stop the board opening new ones, which is not a cap on rulings (see [The board](#the-board))
- `agents.implementers`, `agents.reviewers`, `agents.pm` and `agents.fallback_after`: see [Agents](#agents). `agents.issue_writer`: see [Writing issues](#writing-issues)
- `pacing.enabled` and `pacing.kickoff_hours`: whether usage holds work, and the hours a day the per-hour figure in `status` divides the day's allowance by
- `worker.allowed_domains`, `worker.build_env` (variables that point tool caches into the build folder), `worker.instructions_file`, `worker.guard_hooks` (Claude Code hooks of the project's own, after kelpie's guard) and `worker.turn_timeout`
- `skills.<step>`: see [Skills](#skills)

shep-kelpie's own settings, shared by every project, are the `[kelpie]` section of `dogs.toml`, which lookout edits in the dog's pane. Start from `kelpie-settings.example.toml`, and keep `dogs.toml` private: the webhook's URL is a credential.

- `[kelpie.webhook]` is where rulings are posted, and the only way one reaches you away from the terminal. With none, a ruling shows only in the log, `status` and `shep kelpie rule`
- `[kelpie.leases]` sets how many hold `cargo-test` at once
- `gpu_metrics_url` is the GPU's Prometheus metrics page, such as `nvidia_gpu_exporter`'s `/metrics`. `status` then shows the GPU's load, memory, power and temperature under `gpu`, read every 15 seconds
- `codex_home` is where shep-kelpie's own Codex login lives (see [Agents](#agents))
- `[kelpie.gateways.<name>]` is a model gateway such as paddock, with its `url` and `key_env` (see [Gateways](#gateways)). A runner reads them when it starts
- A change reaches a running runner at its next wake, within a minute when idle. `git.checkout` and `git.remote` wait for its next start. The dog reads `[kelpie.leases]`, and each review bot's window from its agent file, only when it starts, so after a change to either run `shep restart kelpie`

A key kelpie no longer reads, such as `review.reviewers`, `coderabbit` or `planning`, stops the runner, naming the key and what replaces it. So does each top-level key from before the tables: `repo`, `forge`, `merge_authority`, `ci`, `max_items`, `max_parked` and `skills.ci` name the key that took each over, and `private_names` says to guard a worker's commits and posts with a hook in `worker.guard_hooks` instead. That works for a Claude Code worker only: hooks are refused for pi and Codex implementers, and the issue writer's and kelpie's own posts never ran through them, so for those nothing replaces `private_names`.

### shep-kelpie's home

shep-kelpie keeps everything under `$SHEP_HOME/kelpie`, or the folder `KELPIE_HOME` names:

- `agents`, `builds`, `codex`, `rulings`, `tools`, `totp` and `upgrade`, shared by every project
- `dog`, with the dog's book and its door, `lease.sock`. The adopted dog gets `SHEP_HOME` and no `KELPIE_HOME` from shep, so it is always under `$SHEP_HOME/kelpie`, even with `KELPIE_HOME` set for your own commands
- `<project>`, with the project's `state.json`, `board.md`, worker files, `worktrees` and `builds`, and `pm`, the project manager's folder

So a project can't be named for one of shep-kelpie's own folders. Socket paths must stay under 104 bytes, so a runner with a long `SHEP_HOME` refuses to start and names the path that is too long. Keep a shepherd's `SHEP_HOME` short.

A runner or the dog refuses to start while `~/.kelpie` still holds its files from before shep-kelpie's home moved under `$SHEP_HOME`. The message names the folder and says what to do with it: move `projects/<project>`, which holds the project's state, to `$SHEP_HOME/kelpie/<project>`, and delete `wt/<project>`, `targets/<project>`, `shots/<project>` and `playwright/<project>`. Kelpie makes worktrees and builds again, so delete `wt` and `targets` once the project has no open work item: an open work item's state holds the old folder's path, and kelpie cannot move it. Kelpie's state for a project now goes in `$SHEP_HOME/kelpie/<project>/` by default, or under `KELPIE_HOME` when that is set. `shep kelpie doctor` reports a project whose folder is still there, and `shep kelpie add` warns when it registers one. A runner refused this way stays stopped, with the reason in `shep bleats <project>`, until you clear it and run `shep kelpie start <project>`. A runner added before this release restarts instead, until you run `shep kelpie add` again in its checkout. A runner added before this release has no stop code in its flock entry, so it still restarts under shep's backoff until its `stop_exit_codes` is set to `[78]`, which lookout's settings pane can do.

## Design

For how it works and why:

- `GLOSSARY.md`: the vocabulary
- `docs/adr/`: decisions that are hard to reverse
- `docs/design-log.md`: every decision so far, the facts behind them, and the test plan
- `docs/specs/`: each test series and its results

## License

MIT or Apache-2.0, at your option.
