# shep-kelpie

A [shep](https://github.com/shep-pm/shep) dog that runs Claude Code workers on your projects, from a planned work item to a merged pull request. It holds the merge gate, the review budgets and the pacing in code, and calls Claude only for the work and for judgement.

This is early. It changes without notice, and there is no release yet.

## Getting started

From nothing to a worker on your repo. The examples use a project called `scratch`, on the repo `shep-pm/shep`.

### Before you start

You need these on your machine:

- macOS or Linux
- [shep](https://github.com/shep-pm/shep) 0.12 and Rust 1.88 or later, to build shep-kelpie
- Claude Code, signed in
- `node` and `npm`: `tools install` runs them, and so does every agent's sandbox
- on Linux, `bwrap` and `socat`, which the sandbox needs
- `git`, and `gh` signed in to the account that opens the pull requests
- `pi` or `codex`, only to run an agent on that harness (see Agents)
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
runner `scratch`: added with its settings, stopped until `shep kelpie start`
```

On a repo without those four labels, `add` makes them. The runner puts `in-progress` on an issue while a work item has it. The project is named after the repo, or `shep kelpie add <name>`, as `scratch` was here.

`add` writes the project's settings with these defaults:

- `merge_authority = "ask"`: you rule on every merge
- `max_items = 1`: one work item at a time. With more, their turns and reviews run at the same time, one call at a time per work item
- `ci` is on when the checkout has `.github/workflows`
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
ok       shepherd: shep 0.12.0 at /path/to/.shep
ok       dog: kelpie's dog is running and has named itself
ok       scratch: checkout: /path/to/checkout is a git checkout with an origin
ok       scratch: push access: may push to shep-pm/shep
ok       scratch: labels: shep-pm/shep has `ready-for-agent`, `ready-for-human`, `in-progress`, `review please`
ok       scratch: local review: ready
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

Put `ready-for-agent` on an issue that says what done looks like, with acceptance criteria, or have the issue writer write it (see [Writing issues](#writing-issues)). The runner gives it to a worker, which opens a draft pull request. Then the review runs, each listed reviewer once, a review bot such as CodeRabbit among them where the project lists one, then CI. Last, you get a ruling before the merge. `shep kelpie status` shows every project. A worker whose first turn ends with no pull request and no question is sent back once, and asks you for a ruling if it stops short again.

What decides whether, and when, the first issue starts:

- The board skips an issue that is assigned to anyone or already has an open pull request, and one blocked by an issue that is still open. Don't assign it to yourself
- An issue with sub-issues is never worked itself: its sub-issues are, and shep-kelpie closes it once every sub-issue is closed
- Of the rest, `priority: P0` to `P3` labels order them, then the oldest goes first. An `agent:<name>` label, such as `agent:opus-high`, picks the agent that works it from those the project lists in `agents.implementers` (see [Agents](#agents)), and a label naming one it does not list keeps the issue off the board. An old `worker:` label is no longer read: that issue runs on the default implementer, and the runner's log says so
- With a project manager set up (see [The project manager](#the-project-manager)), it picks among two or more issues the board could start, and may hold some back. Without one, or while it is down, the order above picks
- No turn starts while Claude's 5-hour window is at 50% or more, and no new work item starts once today's share of the week is spent. `shep kelpie status` says why under `pacer`, and `enabled = false` in the project's `[app.dogs.kelpie.pacing]` turns both off
- A project that lists `coderabbit` in `agents.reviewers` waits in CodeRabbit's place in the review for its window, one review an hour. shep-kelpie asks for a review by putting the `review please` label on, so the repo's `.coderabbit.yaml` must review only labelled pull requests. Without that, CodeRabbit reviews every push on its own and spends the hour a round is waiting on:

  ```yaml
  reviews:
    auto_review:
      labels:
        - "review please"
  ```

- No review bot is listed unless you list it, and a project that lists none never asks the forge about one. CodeRabbit's free plan reviews public repos only, so a repo GitHub marks private cannot list it, and lists `cubic` or `codex` instead
- Every thread a review bot leaves open goes to the worker as a finding, and shep-kelpie resolves those threads once the worker's fix moves the head. If the forge refuses three steps in a row, the review goes on with them open and the bot's next read sends them again

`shep kelpie add <issue>` opens a work item for an issue at once, without the label, ahead of the board's order. It queues nothing: while `max_items` work items are open, `add <issue>` is refused.

### 8. Answer a ruling

A ruling reaches the channel you chose in step 5. It is one of six kinds:

- `merge`: CI is green, and a yes merges the pull request
- `question`: the worker asks, and your answer is its next turn
- `stuck`: the work item cannot go on by itself, and its reason says why: `rebase`, `still-red`, `merge-refused`, `closed`, `local-model-spilled`, `fix-not-pushed`, `turn-timeout` or `turn-failed`. The question says what a yes does
- `agent-files`: the pull request changes agents' own files, and a yes accepts them
- `foreign-change`: someone else changed the pull request, and a yes accepts the change
- `follow-up`: a merged pull request left findings unfixed, and a yes files them as issues

Answer it from there, or from the terminal:

```sh
shep kelpie rule          # lists the rulings waiting
shep kelpie rule 14 yes
shep kelpie rule 14 no rename the flag
```

On ntfy you can reply in the topic after a one-time `shep kelpie totp`, as Settings below describes. In the Claude app, tap an answer or reply in words.

### 9. Pause, and find the log

```sh
shep kelpie pause
```

The calls already running finish, then the workers park. `shep kelpie start` resumes it. A runner's log is `shep bleats <project>`, and the dog's is `shep bleats kelpie`.

## Reference

### Merging

A project on `merge_authority = "auto"` merges its pull requests without asking once every gate passes, and posts a notice after. The example settings use `ask`, which raises a ruling before every merge.

### Running a project

shep-kelpie runs in your own shepherd, beside your other sheep. The adopted dog holds the leases every runner asks before a summon, and it asks shep for the channel the lease commands reach it on. `add` and `start` say how to bring the dog up when it is not running with its channel.

```sh
shep kelpie add        # labels, settings, and the runner, stopped
shep kelpie start      # starts the runner, then the project
shep kelpie pause
shep kelpie status     # every project
shep kelpie doctor     # what each project still needs on this machine
shep kelpie rule 14 yes
shep kelpie issue "<request>"   # issues for you to read, or --interactive
shep kelpie attach 7   # steer issue 7's worker in this terminal
```

Every trigger the runner takes is also a verb: `add <issue>`, `rework <pr>`, `adopt <pr>`, `gate [<issue>]`, `drop [<issue>]`, `timings [<n>]`, `tell "<note>"`, `pm` and `rule`. Each reaches the project whose repo holds the folder you run it in, a worktree of it included, or the one `-p <project>` names anywhere in the line. From anywhere else it lists the projects and guesses nothing. `shep trigger` still works.

- `drop [<issue>]` ends a work item without merging it. Its worktree, local branch and build folder go, and its pull request stays, labelled `ready-for-human`
- `gate [<issue>]` sends a work item whose worker's turn ended with a pull request into the review gate, when it was never entered
- `tell "<note>"` and `pm` reach the project manager: see [The project manager](#the-project-manager)
- `timings [<n>]` totals where the time went over the last `n` finished work items (10 when left out), in six phases: `worker`, `review`, `ci`, `ruling`, `merge` and `other` (every second lands in exactly one), and answers JSON with the totals under `seconds` and a plain-text `table`, so `shep kelpie timings 20 | jq -r .table` prints it. `status` shows each open item's split under `timings`, and the ten most recent finished items under `history`. The state file keeps the last 100

A ruling's id is unique across projects, so `rule` needs no project:

- `shep kelpie rule 14 yes`
- `shep kelpie rule 14 no rename the flag`
- `shep kelpie rule 15 use --dry-run`, for a worker's question. A `yes` there is the answer's text

Quotes are optional, but zsh still needs them around a note with `?`, `*`, `!` or an apostrophe. `-p` goes before the answer: after its first word, and after a `--`, a `-p` is part of the answer. `shep kelpie rule` alone lists the rulings waiting and asks which to answer and how. Each ruling's question says what `yes` and `no` do.

`add` names the project after the repo, or `shep kelpie add <name>`. It makes the four labels where the repo lacks them, and registers the runner, holding the project's settings as its `[app.dogs.kelpie]` table. Running `add` again changes nothing. `start` and `pause` find the project from the checkout, or take its name. `shep stop <project>` stops a runner, and `shep delete <project>` removes it.

`doctor` changes nothing and prints one line per check, each missing piece with its fix, then exits non-zero if a project needs something it lacks. It checks that `claude` is installed and logged in, that `gh` is logged in and may push to each project's repo, the sandbox runtime every agent runs in, the shepherd's version, each project's labels, CodeRabbit where a project lists it, each project's reviewers, in order, with any command or endpoint among them checked as a runner's start checks it, and which webhook, if any, rulings post to. `shep kelpie doctor <project>` checks one project. `--test-alert` posts one test alert to the webhook, which is the only post it ever makes. A line marked `unsure` could not be settled, and does not fail the run: CodeRabbit is one, since a repo it has not yet reviewed looks the same as a repo without it.

`shep describe <project>` labels each Claude session the runner starts with its issue and role, such as `#114 worker`.

shep-kelpie refuses a shepherd on another shep minor or major than the pinned one, naming both versions. shep-kelpie's commands talk to the shepherd's socket with the shep client it is built with, never a `shep` on `PATH`.

### Leases

The dog holds `cargo-test`, a share of this machine for running tests, and `gpu`, this machine's GPU lock. Workers run their test suites under `cargo-test`, and so can anything else on the machine: `shep kelpie lease run cargo-test -- cargo test` waits for a turn, runs the command and exits with its code, or 75 when it cannot reach the dog. Three hold it at once unless `[kelpie.leases]` says otherwise, the rest queue, and `shep kelpie lease status` shows who holds each lease and who waits. A command that ends or dies gives its turn back. One whose `lease run` was killed outright keeps it until the command, and anything it left running, exits: `status` shows how long each has held it.

`shep kelpie lease take gpu` holds the GPU lock for you until `shep kelpie lease return gpu`, so whatever takes that lock waits. Use it when you need the GPU to yourself.

### Settings

A project's settings are its `[app.dogs.kelpie]` table, which lookout edits in the runner's pane. `settings.example.toml` lists every key, and shows a runner's Flockfile entry for a project set up by hand. It needs `SHEP_HOME` as an absolute path in `env`, since a sheep starts without it, and `kill_timeout = "10s"` or more, since a runner needs about 7s to stop cleanly.

shep-kelpie's own settings, shared by every project, are the `[kelpie]` section of `dogs.toml`, which lookout edits in the dog's pane. Start from `kelpie-settings.example.toml`, and keep `dogs.toml` private: the webhook's URL is a credential.

- `[kelpie.webhook]` is where rulings are posted, and the only way one reaches you away from the terminal. With none, a ruling shows only in the log, `status` and `shep kelpie rule`
- `gpu_metrics_url` is the GPU's Prometheus metrics page, such as `nvidia_gpu_exporter`'s `/metrics`. `status` then shows the GPU's load, memory, power and temperature under `gpu`, read every 15 seconds
- A change reaches a running runner at its next wake, within a minute when idle. `repo` and `forge` wait for its next start. The dog reads `[kelpie.leases]`, and each review bot's window from its agent file, only when it starts, so after a change to either run `shep restart kelpie`

On an ntfy webhook, a ruling can be answered from the topic: run `shep kelpie totp` once and scan the QR code into an authenticator app, then reply with the line the alert ends on, such as `14 yes <code>`, with the app's code last. A reply takes the same answers as `shep kelpie rule`. Anyone who can read the topic can read rulings, but only a reply with the code of the moment answers one, and each code answers once. Five wrong codes turn answers off until `shep kelpie totp --unlock`, and `shep kelpie totp --rotate` replaces a secret that may have leaked.

### shep-kelpie's home

shep-kelpie keeps everything under `$SHEP_HOME/kelpie`, or the folder `KELPIE_HOME` names:

- `settings.toml`, `totp`, `tools` and `rulings`, shared by every project
- `dog`, with the dog's book and its door, `lease.sock`. The adopted dog gets `SHEP_HOME` and no `KELPIE_HOME` from shep, so it is always under `$SHEP_HOME/kelpie`, even with `KELPIE_HOME` set for your own commands
- `<project>`, with the project's `state.json`, `board.md`, worker files, `worktrees` and `builds`, and `pm`, the project manager's folder

So a project can't be named for one of shep-kelpie's own folders. Socket paths must stay under 104 bytes, so a runner with a long `SHEP_HOME` refuses to start and names the path that is too long. Keep a shepherd's `SHEP_HOME` short.

A runner or the dog refuses to start while `~/.kelpie` still holds its files from before shep-kelpie's home moved under `$SHEP_HOME`. Run the previous release once, which moves them, or move them by hand.

### Upgrading

```sh
shep kelpie upgrade --ref main          # build shep-kelpie at a git ref and install it
shep kelpie upgrade --release 0.3.0     # install a release (none is published yet)
shep kelpie upgrade --binary ./kelpie   # install a build made by hand, as it is
shep kelpie upgrade --rollback          # put back the build the last upgrade replaced
```

The installed shep-kelpie is whatever program the adopted dog runs, which the shepherd knows (`~/.cargo/bin/shep-kelpie` for the install above), and `shep kelpie add` registers runners that run the same path. An upgrade writes the new build beside that file and renames it over, as shep upgrades itself, so the running file's bytes are never edited in place. Before the swap it copies the file it replaces into `$SHEP_HOME/kelpie/builds` (resolved first if the path is a symlink), and `--rollback` puts that copy back the same way. Then it restarts the dog and each running runner one at a time, never while a work item is merging: it waits, and says what it waits on. A runner that does not answer `status` is waited on too, and named if it never does. A runner you stop while the upgrade waits stays stopped, and so does one stopped before it. A stopped runner starts on the new build when you start it.

If an upgrade stops after the swap (a merge that outlasts the wait, a sheep that does not come back, Ctrl-C), the new build is already installed and the message says so. `shep kelpie upgrade --binary <installed path>` finishes the restarts and touches no file. `--rollback` does not: it swaps the two builds again.

Every sheep it restarts must run the dog's path. If one does not, the upgrade stops before it changes anything and names the sheep and its path. A `--binary` without its executable bits is refused with the `chmod +x` that fixes it, and only one upgrade runs at a time: a second refuses while the first holds `$SHEP_HOME/kelpie/upgrade.lock`. The swap changes the file for every shepherd that adopts the same path, with no check against their shep.

Before it changes anything it asks the new build which shep it is made with (`shep-kelpie version --json`) and compares the minor with your shepherd's. If they differ it stops before restarting anything and prints the steps: upgrade shep and reload its shepherd, then run the new build's own `upgrade` with `SHEP_HOME` set to your shepherd, since the old kelpie that `shep kelpie` runs refuses the new shepherd. The upgrade prints the exact line. `--rollback` checks the previous build the same way.

### Skills

Every step shep-kelpie drives an agent through runs a skill, by default from [mattpocock/skills](https://github.com/mattpocock/skills) (MIT). shep-kelpie vendors the ones it uses in `skills/`, pinned to one upstream commit with its licence, and writes them out as a Claude Code plugin when a runner starts. A project installs nothing.

| step | default skill | where it runs |
|---|---|---|
| `triage` | `triage` | not driven yet |
| `spec` | `to-spec` | not driven yet |
| `implement` | `implement` | the worker's first turn on an issue |
| `tests` | `tdd` | named in the worker's instructions |
| `review` | `code-review` | not driven: a reviewer's prompt is its agent file's body |
| `ci` | `diagnosing-bugs` | the worker's turn on a red CI run |
| `pr` | `pr` | named in the worker's instructions, unless the repo has a pull request template |
| `reset` | `handoff` | not driven yet |
| `retro` | `retro` | not driven yet (#104) |

A step that runs a skill starts its prompt with the skill's slash command, such as `/mattpocock:implement`, and shep-kelpie's own prompt follows as its arguments. To override one, set it in the project's `[app.dogs.kelpie.skills]` table:

- `{ kind = "path", path = "..." }`: a skill folder with a `SKILL.md`, copied into a plugin of its own, `kelpie-<step>`
- `{ kind = "plugin", plugin = "...", skill = "..." }`: a skill in a Claude Code plugin's folder
- `{ kind = "none" }`: shep-kelpie's own prompt, no skill

A skill that can't load runs shep-kelpie's own prompt instead. The runner logs why, and `status` shows it under `skills`.

### The review

Each pull request goes through a review before CI. A project lists its reviewers in `agents.reviewers`, in the order the review runs them, each an agent file whose `role` is `reviewer` (see Agents below). Each listed reviewer runs once, in order, and a list changed mid-pass runs whichever listed reviewers the pass has not. A reviewer whose call fails three times in a row is passed over for the rest of the pass, and `status` lists it under `reviewers_skipped`. One that finds anything above a nit sends the worker all of its findings, nits included and at its own severity, for one fix turn, and the next reviewer reads the fix. A round of nits, or of nothing, goes straight to the next reviewer. A fix turn that pushes nothing parks on a ruling, unless the worker deferred every finding it was sent, in which case the next reviewer reads the pull request as it stands. A pass that ends with no reviewer having read the pull request, because one was down, kept failing or reviewed no file, marks the work item unreviewed: `status` shows why, the merge ruling's question says so, and under `auto` it gets the merge ruling instead of merging. An empty list, or reviewers whose `paths` all miss the change, is your choice, and the log says so once. After the last one the pull request goes to CI. There is no judge and no second pass: whatever the last fix leaves is what CI and the merge see. A pass starts again from the top only on new code the review has not seen, such as the fix a merge ruling's `no` asks for, a change you accept that someone else pushed, or a rework. An empty list reviews nothing.

A reviewer runs in one of four ways, by its file's `harness`:

- `claude-code`, `codex` or `pi`: a fresh session on its `model` and `effort`, which reads the worktree with Read, Grep and Glob and runs no command. The file's body is its prompt.
- `command`: a command of your own that keeps the contract below, and takes no body.
- `endpoint`: shep-kelpie's own reviewer, for any OpenAI-compatible server such as Ollama, LM Studio or llama.cpp's server, and takes no body.
- `bot`: a pull request review bot, CodeRabbit, cubic or Codex, summoned on the pull request (see Review bots below), and takes no body.

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

A bot's file is named for its bot, since `reviews` in `hours` is its account's window, which the dog books from that file when it starts and every project shares. Its round marks a draft ready, since a bot may skip drafts, waits for the bot's window and lease, and summons it: CodeRabbit by the `review please` label, or by `@coderabbitai full review` on a pull request it read before that would otherwise find nothing new, cubic and Codex by their comments. A summon with no sign of being heard in fifteen minutes goes out once more. Once a review covers the head, the bot's open threads are its findings, and go to one fix turn as any reviewer's do. A refusal hands the window back to the dog and the round asks again. A window that opens more than an hour on, by the dog's book or by the bot's refusal, passes the bot over for the pass, and so do two hours with no review, two hours its round could not summon it, a bot you stop listing mid-round, and CodeRabbit on a repo the forge reports not public, which is checked just before each summon; `status` lists each under `bots_skipped`, and a pass nobody else read is marked unreviewed. `rounds: 1` lets a bot read a work item's pull request once, whatever its passes, counting its reviews from before a rework or an adoption, and is unset by default. An adopted pull request goes through a pass of the listed bots alone, and until each bot answers a summon of shep-kelpie's own, none of its reviews from before the adoption stands for its read. Codex's `reviews_on_ready: true` says it reviews a pull request when it leaves draft, as the repo's Codex settings may have it: then marking ready, under its lease, is the summon, with no comment, and it is never asked again by comment. List it before any other bot, whose mark-ready would draw its review outside its lease: the runner refuses the other order.

An endpoint takes `url` (the base, up to and including `/v1`), `model`, and `context`, the context size in tokens the server gives that model. Kelpie diffs the pull request, cuts the diff to fit that context, and sends each piece with its own review prompt. Set `context` to what the server really uses: Ollama gives its OpenAI-compatible endpoint a small default context unless `OLLAMA_CONTEXT_LENGTH` says more, and drops whatever doesn't fit without saying so.

A command is run as `<command> --dir <worktree> --round <n> --diff <base>`, with:

- `QWEN_REVIEW_OUT`: the folder to write in
- `KELPIE_REVIEW_HEAD`: the commit under review
- `TMPDIR`: the folder the GPU lock lives under
- `KELPIE_REVIEW_CRITERIA`: a file holding the issue's acceptance criteria

It writes `round-<n>.txt` in that folder, one finding per line as `SEVERITY|path:line|what|why` with `HIGH`, `MEDIUM` or `LOW`, and then an empty `round-<n>.txt.done`. Kelpie reads nothing without the marker, and nothing from stdout. A nonzero exit fails the round. A command that writes `LOW|<path>:0|not reviewed: <n> lines exceeds the chunk limit|...` is run again with `--files <hunk file>` in place of `--diff`, on that file alone. If that run fails, its file is left unreviewed, as below, with the failure as the reason; only when kelpie cannot cut the hunk with `git diff` does the placeholder stay as the finding. Any other `LOW|<path>:0|not reviewed: <why>|...` line, as the script writes when the model cannot be reached, is a file left unreviewed and not a finding. A round with nothing but those lines reviewed nothing, and the review goes on to the next reviewer. A round that leaves the same files unreviewed as the same reviewer's last round counts against it too, and one that leaves none clears its count. A second such round in a row, which takes two passes, leaves the reviewer out of the review for the rest of the work item, and `status` lists it under `local_reviewers_down`. Another local reviewer, on another command or server, still runs. A round with real findings and some `not reviewed:` lines keeps its findings.

`lease` names the lock kelpie holds around each round of a command or an endpoint. `gpu` is this machine's GPU lock, the one the qwen scripts take. Any other name is a lock of its own, so a reviewer on another machine's GPU never waits on this one's. Leave it off for a command that takes the lock itself, as `qwen-review.sh` does.

With a lease, before a round against Ollama, kelpie reads the host's `/api/ps`. An endpoint's host is its `url` without the `/v1`. A command names its host with `ollama: http://localhost:11434`, which needs a lease, and its model with `ollama_model`, else every model the host has loaded is checked. A model partly or wholly on the CPU fails the round and raises a ruling, and a yes runs the round again once the model is back on the GPU. A host with no `/api/ps` is not checked, and `status` shows the model's name, its share on the GPU, its context length and when it unloads.

### The board

Issues labelled `ready-for-agent` are the board. On a pull request kelpie opened, `ready-for-agent` or a review requesting changes starts a rework of it, the same as `shep kelpie rework <pr>`. On any other open pull request of kelpie's account, `ready-for-agent` adopts it, the same as `shep kelpie adopt <pr>`. Kelpie puts `ready-for-human` on each pull request it hands back.

The runner writes the board out as `board.md` in the project's folder, and `status` names the file under `board`. It is the briefing the project manager's agent reads, so that agent never runs git or gh itself, and you can read it too. Code writes it, with no model call, at start and whenever the board changes: a call starting or ending, a phase change, a ruling raised or answered, a read of the ready queue. It is written again at least once a minute while the runner looks at the board, and each write replaces the file whole. Text that names this machine or one of the project's `private_names` is withheld from it, as from the forge. It holds:

- each open work item: its issue, phase, pull request, age and agent, the worker's last closing message, and the files its branch touches
- what each item's session is doing. A worker's turn shows when it last made a tool call or wrote output, read from its transcript (Codex's output file, pi's session file), and reads as idle after 10 minutes of neither
- the rulings waiting on you
- the ready queue in the board's order, with each issue's priority, why the board passes over it if it does, the paths its body names in backticks, and the first 600 characters of its body, quoted
- the board's events since the project manager last read it, or the last 20 before it ever has
- the overlap: for each pair of open branches, the files both touch and the files `git merge-tree` finds in conflict, and each ready issue's named paths against them. The conflict check needs git 2.38 or later

### Attaching to a worker

`shep kelpie attach <issue>` takes over a work item's worker by hand. The runner holds the work item, so no call starts for it. A call already in flight runs to its end, and `attach` says it is waiting. Then it resumes the worker's session with `claude --resume` in your terminal, in the item's worktree, with the worker's settings file and inside the worker's sandbox, so its deny rules, its guard and its fence hold while you drive. Claude Code asks before each tool call as it always does.

When you exit `claude`, the work item carries on, and its next turn resumes the same session. A commit or push you make while attached is the worker's own, as a push in a turn is, so the gate takes it as the branch's head rather than a change kelpie did not make. The runner holds the work item while either the `attach` command or the session it started runs, each known by its pid and start time, and lets it go at its next pass once both have ended. A SIGTERM or SIGHUP to `attach` is passed on to the session, which `attach` waits for. `status` shows the hold under the item's `attached`, and `drop` refuses the item while it is held.

`attach` changes nothing and says what to do instead for a work item whose worker has no session yet (its first turn starts one), one parked on a ruling (answer it first), or one merging. Only a Claude Code worker can be attached: a Codex or pi worker's session resumes another way, by hand.

### Writing issues

The issue writer turns a request into issues an agent can build from. It reads the repo, scopes the request to one pull request's worth, or splits it where each piece works, tests and ships on its own (most requests stay whole), writes acceptance criteria into every issue, and labels each `agent:<name>` with one of the project's implementers.

```sh
shep kelpie issue "let a project be paused from lookout"
shep kelpie issue --interactive "let a project be paused from lookout"
```

On its own, it runs one fresh session in a detached checkout of `main`, files what it writes as `ready-for-human`, and ends with the list of what it filed. shep-kelpie reads each issue back and prints it, with its `agent:` label and the issue it is a sub-issue of. Read them, then label them `ready-for-agent`. An issue without acceptance criteria, its status label or exactly one `agent:` label naming a listed implementer is named with what it lacks, and the command exits non-zero. A session that fails, or ends without the list, keeps its checkout and prints the `claude --resume` line that picks it up.

With `--interactive` it runs `claude` in your terminal, in the project's checkout, with the issue writer's prompt appended and the request as your first message. You plan the issues together, and it files what you agree as `ready-for-agent`, onto the board. It is your session, not a runner's: shep-kelpie starts it and gets out of the way, and Claude Code asks you before each command it runs.

Either way the session reads the checkout with Read, Grep and Glob, and nothing outside it: not gh's config, Claude Code's own files or your shell history. It edits no file. Its Bash runs only what `kelpie guard` lists for it: `gh issue create`, `gh issue view <n>` and `gh issue list` on this repo, and, for the issues it filed in this session, `gh issue edit` on labels and `gh api` on their ids and sub-issue and blocked-by links, each as plain words, with a body given as a heredoc behind a quoted delimiter (`--body-file - <<'EOF'`). It runs no git. A hook after each command records the issues it filed and the ids it read, and the guard refuses an edit or a link on any other. The guard also refuses an issue without acceptance criteria, without the status label, or without exactly one listed `agent:` label, and one whose title or body names a path on this machine. On its own the session also runs in the sandbox, with nothing to write but its own scratch folder and only GitHub to reach. Each implementer's `agent:` label is made on the repo the first time the issue writer needs it.

The issue writer is the `issue-writer` agent file: edit its body to change its prompt, or its `model` and `effort`.

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


## Agents

An agent is a file: `$SHEP_HOME/kelpie/agents/<name>.md` (or the `agents` folder of the home `KELPIE_HOME` names), YAML frontmatter and then a Markdown body. The name is the file's name without `.md`. An implementer's body is added to kelpie's own instructions for that agent, and an empty body adds nothing. A reviewer's body is its prompt (see The review above).

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

An issue labelled `agent:<name>` (the prefix in any case) runs on that agent, which the project must list, and any other issue on the first listed that is not a local model. A work item keeps the agent it opened on, and each turn runs that agent's file as it is then. `sonnet-high` alone when absent. An implementer's file must say `role: implementer`, and a reviewer's `role: reviewer`.

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

A local implementer, one whose usage is `none`, is never the default: it gets only the issues labelled `agent:<name>` for it, so you pick which issues it takes. Each account keeps its own daily allowance and 5-hour stop, shown under its name in `status.pacer`, and a new work item waits on every account its roles spend. `shep kelpie doctor` checks Codex answers for a project that spends it. A `none` agent holds `lease` (the GPU lock, `gpu`, by default) for the whole of each call instead, so a qwen round waits behind its turn, and `status.local_leases` shows who holds it.

`status` shows each role's tokens in `by_role`, with `cost_usd` only for calls whose harness reports dollars. `unpriced_calls` counts the rest.

## Design

For how it works and why:

- `CONTEXT.md`: the vocabulary
- `docs/adr/`: decisions that are hard to reverse
- `docs/design-log.md`: every decision so far, the facts behind them, and the test plan
- `docs/specs/`: each test series and its results

## License

MIT or Apache-2.0, at your option.
