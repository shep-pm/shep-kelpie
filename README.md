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

It downloads a headless Chrome of about 95 MiB, and puts it under shep-kelpie's home, `$SHEP_HOME/kelpie`.

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
- `max_items = 1`: one work item at a time
- `ci` is on when the checkout has `.github/workflows`
- `coderabbit.enabled` is on for a public repo, off for a private one
- `review.reviewers = ["deep"]`, unless `~/.claude/scripts/qwen-review.sh` exists, which then runs first
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
ok       scratch: coderabbit: CodeRabbit has commented on a pull request of shep-pm/shep
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

Put `ready-for-agent` on an issue that says what done looks like, with acceptance criteria. The runner gives it to a worker, which opens a draft pull request. Then the review loop runs, then CI, then on a repo with CodeRabbit on, CodeRabbit. Last, you get a ruling before the merge. `shep kelpie status` shows every project. A worker whose first turn ends with no pull request and no question is sent back once, and asks you for a ruling if it stops short again.

What decides whether, and when, the first issue starts:

- The board skips an issue that is assigned to anyone or already has an open pull request, and one blocked by an issue that is still open. Don't assign it to yourself
- An issue with sub-issues is never worked itself: its sub-issues are, and shep-kelpie closes it once every sub-issue is closed
- Of the rest, `priority: P0` to `P3` labels order them, then the oldest goes first. A `worker:<model>-<effort>` label, such as `worker:opus-high`, picks the model that works it, from `opus`, `sonnet`, `haiku` and `fable`, each run as the id `[app.dogs.kelpie.models.labels]` gives it. `worker:local` picks the project's local worker
- No turn starts while Claude's 5-hour window is at 50% or more, and no new work item starts once today's share of the week is spent. `shep kelpie status` says why under `pacer`, and `enabled = false` in the project's `[app.dogs.kelpie.pacing]` turns both off
- On a public repo, after CI each pull request waits for CodeRabbit, at one review an hour. shep-kelpie asks for a review by putting the `review please` label on, so the repo's `.coderabbit.yaml` must review only labelled pull requests. Without that, CodeRabbit reviews every push on its own and spends the hour a round is waiting on:

  ```yaml
  reviews:
    auto_review:
      labels:
        - "review please"
  ```

- `coderabbit.enabled` is the switch for every pull request reviewer, cubic and Codex too. A private repo has it off, so a private repo that wants cubic or Codex turns it on

`shep kelpie add <issue>` opens a work item for an issue at once, without the label, ahead of the board's order. It queues nothing: while `max_items` work items are open, `add <issue>` is refused.

### 8. Answer a ruling

A ruling reaches the channel you chose in step 5. Answer it from there, or from the terminal:

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

The current turn finishes, then the worker parks. `shep kelpie start` resumes it. A runner's log is `shep bleats <project>`, and the dog's is `shep bleats kelpie`.

## Reference

### Merging

A project on `merge_authority = "auto"` merges its pull requests without asking once every gate passes, and posts a notice after. The example settings use `ask`, which raises a ruling before every merge.

Before either, once CI is green, a fresh Opus session reads the work as a whole: the issue with whatever its body points to (an issue or a pull request, with its latest review and its unresolved comments), the pull request's body and the final diff. It answers two questions. For each acceptance criterion, is it met, and where? And what does the change assume about the world outside the repo (labels, files, settings, other services), and does the code or a test check each assumption against the real thing, rather than a fake that accepts anything? A criterion not met or an assumption nothing real checks goes to the worker as its next turn, with the gap named, and what it pushes goes through the review loop, CI and the check again, like any other fix. After two such trips the next gap is a ruling: a yes sends the worker the gaps once more, the same way, and merging by hand overrules the check. `[models.auditor]` picks its model (Opus 5.5 at high effort when left out), an agent can run it through `[agents] auditor`, and its time shows as `audit` under `timings`.

### Running a project

shep-kelpie runs in your own shepherd, beside your other sheep. The adopted dog holds the leases every runner asks before a summon, and it asks shep for the channel the lease commands reach it on. `add` and `start` say how to bring the dog up when it is not running with its channel.

```sh
shep kelpie add        # labels, settings, and the runner, stopped
shep kelpie start      # starts the runner, then the project
shep kelpie pause
shep kelpie status     # every project
shep kelpie doctor     # what each project still needs on this machine
shep kelpie rule 14 yes
```

Every trigger the runner takes is also a verb: `add <issue>`, `rework <pr>`, `adopt <pr>`, `gate [<issue>]`, `drop [<issue>]`, `timings [<n>]` and `rule`. Each reaches the project whose repo holds the folder you run it in, a worktree of it included, or the one `-p <project>` names anywhere in the line. From anywhere else it lists the projects and guesses nothing. `shep trigger` still works.

- `drop [<issue>]` ends a work item without merging it. Its worktree, local branch and build folder go, and its pull request stays, labelled `ready-for-human`
- `gate [<issue>]` sends a work item whose worker's turn ended with a pull request into the review gate, when it was never entered
- `timings [<n>]` totals where the time went over the last `n` finished work items (10 when left out), and answers JSON with the totals under `seconds` and a plain-text `table`, so `shep kelpie timings 20 | jq -r .table` prints it. `status` shows each open item's split under `timings`, and the ten most recent finished items under `history`. The state file keeps the last 100

A ruling's id is unique across projects, so `rule` needs no project:

- `shep kelpie rule 14 yes`
- `shep kelpie rule 14 no rename the flag`
- `shep kelpie rule 15 use --dry-run`, for a worker's question. A `yes` there is the answer's text

Quotes are optional, but zsh still needs them around a note with `?`, `*`, `!` or an apostrophe. `-p` goes before the answer: after its first word, and after a `--`, a `-p` is part of the answer. `shep kelpie rule` alone lists the rulings waiting and asks which to answer and how. Each ruling's question says what `yes` and `no` do.

`add` names the project after the repo, or `shep kelpie add <name>`. It makes the four labels where the repo lacks them, and registers the runner, holding the project's settings as its `[app.dogs.kelpie]` table. Running `add` again changes nothing. `start` and `pause` find the project from the checkout, or take its name. `shep stop <project>` stops a runner, and `shep delete <project>` removes it.

`doctor` changes nothing and prints one line per check, each missing piece with its fix, then exits non-zero if a project needs something it lacks. It checks that `claude` is installed and logged in, that `gh` is logged in and may push to each project's repo, the sandbox runtime every agent runs in, the shepherd's version, each project's labels, CodeRabbit where a project turns it on, the local review command or endpoint where one is set, the preview tools for a project that shows its UI, and which webhook, if any, rulings post to. `shep kelpie doctor <project>` checks one project. `--test-alert` posts one test alert to the webhook, which is the only post it ever makes. A line marked `unsure` could not be settled, and does not fail the run: CodeRabbit is one, since a repo it has not yet reviewed looks the same as a repo without it.

`shep describe <project>` labels each Claude session the runner starts with its issue and role, such as `#114 worker`.

shep-kelpie refuses a shepherd on another shep minor or major than the pinned one, naming both versions. shep-kelpie's commands talk to the shepherd's socket with the shep client it is built with, never a `shep` on `PATH`.

### Leases

The dog holds `cargo-test`, a share of this machine for running tests, and `gpu`, this machine's GPU lock. Workers run their test suites under `cargo-test`, and so can anything else on the machine: `shep kelpie lease run cargo-test -- cargo test` waits for a turn, runs the command and exits with its code, or 75 when it cannot reach the dog. Three hold it at once unless `[kelpie.leases]` says otherwise, the rest queue, and `shep kelpie lease status` shows who holds each lease and who waits. A command that ends or dies gives its turn back. One whose `lease run` was killed outright keeps it until the command, and anything it left running, exits: `status` shows how long each has held it.

`shep kelpie lease take gpu` holds the GPU lock for you until `shep kelpie lease return gpu`, so whatever takes that lock waits. Use it when you need the GPU to yourself.

### Settings

A project's settings are its `[app.dogs.kelpie]` table, which lookout edits in the runner's pane. `settings.example.toml` lists every key, and shows a runner's Flockfile entry for a project set up by hand. It needs `SHEP_HOME` as an absolute path in `env`, since a sheep starts without it, and `kill_timeout = "10s"` or more, since a runner needs about 7s to stop cleanly.

shep-kelpie's own settings, shared by every project, are the `[kelpie]` section of `dogs.toml`, which lookout edits in the dog's pane. Start from `kelpie-settings.example.toml`, and keep `dogs.toml` private: the webhook's URL is a credential.

- `[kelpie.webhook]` is where rulings are posted, and the only way one reaches you away from the terminal. With none, a ruling shows only in the log, `status` and `shep kelpie rule`
- `[kelpie.reviewers]` defines the pull request reviewers, CodeRabbit, cubic and Codex, by their review windows, and a project's `pull_request_reviewers` lists the ones it uses in preference order: each round goes to the first whose window is free. Codex's table also takes `reviews_on_ready`, off when absent: turn it on only where Codex's automatic reviews are enabled. Then marking a draft ready is its summon and shep-kelpie posts no comment on top, so one round spends one review
- `gpu_metrics_url` is the GPU's Prometheus metrics page, such as `nvidia_gpu_exporter`'s `/metrics`. `status` then shows the GPU's load, memory, power and temperature under `gpu`, read every 15 seconds
- A change reaches a running runner at its next wake, within a minute when idle. `repo` and `forge` wait for its next start. The dog reads `[kelpie.leases]` and `[kelpie.reviewers]` only when it starts, so after a change run `shep restart kelpie`

On an ntfy webhook, a ruling can be answered from the topic: run `shep kelpie totp` once and scan the QR code into an authenticator app, then reply with the line the alert ends on, such as `14 yes <code>`, with the app's code last. A reply takes the same answers as `shep kelpie rule`. Anyone who can read the topic can read rulings, but only a reply with the code of the moment answers one, and each code answers once. Five wrong codes turn answers off until `shep kelpie totp --unlock`, and `shep kelpie totp --rotate` replaces a secret that may have leaked.

### shep-kelpie's home

shep-kelpie keeps everything under `$SHEP_HOME/kelpie`, or the folder `KELPIE_HOME` names:

- `settings.toml`, `totp`, `tools` and `rulings`, shared by every project
- `dog`, with the dog's book and its door, `lease.sock`. The adopted dog gets `SHEP_HOME` and no `KELPIE_HOME` from shep, so it is always under `$SHEP_HOME/kelpie`, even with `KELPIE_HOME` set for your own commands
- `<project>`, with the project's `state.json`, worker files, `worktrees`, `builds`, `shots` and `playwright`

So a project can't be named for one of shep-kelpie's own folders. Socket paths must stay under 104 bytes, so a runner with a long `SHEP_HOME` refuses to start and names the path that is too long. Keep a shepherd's `SHEP_HOME` short.

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
| `review` | `code-review` | each Claude review round |
| `ci` | `diagnosing-bugs` | the worker's turn on a red CI run |
| `pr` | `pr` | named in the worker's instructions, unless the repo has a pull request template |
| `reset` | `handoff` | not driven yet |
| `retro` | `retro` | not driven yet (#104) |

A step that runs a skill starts its prompt with the skill's slash command, such as `/mattpocock:implement`, and shep-kelpie's own prompt follows as its arguments. To override one, set it in the project's `[app.dogs.kelpie.skills]` table:

- `{ kind = "path", path = "..." }`: a skill folder with a `SKILL.md`, copied into a plugin of its own, `kelpie-<step>`
- `{ kind = "plugin", plugin = "...", skill = "..." }`: a skill in a Claude Code plugin's folder
- `{ kind = "none" }`: shep-kelpie's own prompt, no skill

A skill that can't load runs shep-kelpie's own prompt instead. The runner logs why, and `status` shows it under `skills`.

### The review loop

Each pull request goes through a review loop before CI. A project lists its reviewers in `review.reviewers`, in the order the loop runs them, and shep-kelpie's own settings define each one by name in `[kelpie.local_reviewers.<name>]`:

- `kind = "command"`: a command of your own that keeps the contract below
- `kind = "endpoint"`: shep-kelpie's own reviewer, for any OpenAI-compatible server such as Ollama, LM Studio or llama.cpp's server
- `kind = "claude"`: a fresh Claude session on its own `model` and `effort`
- `kind = "session"`: a fresh session on the agent its `agent` names (see Agents below)

`claude` is always defined: the project's own Claude round on its reviewer's agent, `models.reviewer` unless the project names one. The loop ends once two rounds in a row, from two different reviewers, find nothing above a nit, and those nits get fixed with no further round. Where only one reviewer can run, one clean round ends it.

`deep` is always defined too, and it is a new project's review: one deep round instead of a loop of shallow ones. A fresh session on `models.deep_reviewer` (Opus 5.5 at high effort unless the project says otherwise) reads the whole pull request for defects, meaning a trigger and an effect that a failing test could be written from, and not for style or naming. A second fresh session on the same model is shown what the first found and asked only for what it missed. Each HIGH they hold is then confirmed by a session that may run commands in the worktree, which writes a failing test for it, and a HIGH it cannot confirm goes on marked unconfirmed. One worker turn fixes everything held, both readers' lists with the tests. One re-check then reads only the fix commits against the findings and runs each failing test, since a fixer's word that it fixed something is not evidence. A finding it finds unfixed goes back to the worker once, and after that to a ruling, whose yes sends the worker it once more. A fix that passes ends the loop. The local reviewers' rounds and CodeRabbit's stay as they are, and `deep` ends the loop whenever it is listed, so `reviewers = ["qwen", "claude", "opus"]` is still the loop of alternating rounds for a project that wants it. Its time is the `deep_round` timing phase.

A local model alone:

```toml
# the project's [app.dogs.kelpie.review]
reviewers = ["qwen"]

# shep-kelpie's [kelpie] section
[kelpie.local_reviewers.qwen]
kind = "endpoint"
url = "http://localhost:11434/v1"
model = "qwen2.5-coder:14b"
context = 32768
lease = "gpu"
```

Claude alone:

```toml
reviewers = ["claude"]
```

Both, with a deeper Claude round only where it pays:

```toml
reviewers = ["qwen", "claude", "opus"]

[kelpie.local_reviewers.opus]
kind = "claude"
model = "claude-opus-5-5"
effort = "high"
paths = ["src/auth/**", "migrations/**"]
```

`paths` limits a reviewer to pull requests that change a file under one of its globs, and the loop skips it elsewhere. Every round's prompt carries the issue's acceptance criteria: the section under an "Acceptance criteria" heading, or the whole body without one.

A project that lists none and sets no `review.local` runs `~/.claude/scripts/qwen-review.sh`, an optional local review script, when it exists, and then the deep round. One that sets `review.local` keeps the older loop of that round and `claude`. `review.local` is the older form: it takes the same keys as a definition, or `kind = "off"`, and the runner says so at start. `review.local_rounds` caps the rounds from local reviewers per work item. Once they are spent, only Claude reviewers run.

A missing command or an endpoint that doesn't answer stops the runner at start.

An endpoint takes `url` (the base, up to and including `/v1`), `model`, and `context`, the context size in tokens the server gives that model. Kelpie diffs the pull request, cuts the diff to fit that context, and sends each piece with its own review prompt. Set `context` to what the server really uses: Ollama gives its OpenAI-compatible endpoint a small default context unless `OLLAMA_CONTEXT_LENGTH` says more, and drops whatever doesn't fit without saying so.

A command is run as `<command> --dir <worktree> --round <n> --diff <base>`, with:

- `QWEN_REVIEW_OUT`: the folder to write in
- `KELPIE_REVIEW_HEAD`: the commit under review
- `TMPDIR`: the folder the GPU lock lives under
- `KELPIE_REVIEW_CRITERIA`: a file holding the issue's acceptance criteria

It writes `round-<n>.txt` in that folder, one finding per line as `SEVERITY|path:line|what|why` with `HIGH`, `MEDIUM` or `LOW`, and then an empty `round-<n>.txt.done`. Kelpie reads nothing without the marker, and nothing from stdout. A nonzero exit fails the round. A command that writes `LOW|<path>:0|not reviewed: <n> lines exceeds the chunk limit|...` is run again with `--files <hunk file>` in place of `--diff`, on that file alone. If that run fails, its file is left unreviewed, as below, with the failure as the reason; only when kelpie cannot cut the hunk with `git diff` does the placeholder stay as the finding. Any other `LOW|<path>:0|not reviewed: <why>|...` line, as the script writes when the model cannot be reached, is a file left unreviewed and not a finding. A round with nothing but those lines reviewed nothing: it is not judged and counts as neither clean nor a reviewer's turn, and it is run once more. A second such round in a row leaves the loop to the other reviewers for the rest of the work item, and `status` lists the reviewer under `local_reviewers_down`. Another local reviewer, on another command or server, still runs. A round with real findings and some `not reviewed:` lines keeps its findings, but neither it nor any round after it counts as clean until a later local round leaves no file unreviewed.

`lease` names the lock kelpie holds around each round of a command or an endpoint. `gpu` is this machine's GPU lock, the one the qwen scripts take. Any other name is a lock of its own, so a reviewer on another machine's GPU never waits on this one's. Leave it off for a command that takes the lock itself, as `qwen-review.sh` does. `gpu_lease = true` is the older spelling of `lease = "gpu"`.

With a lease, before a round against Ollama, kelpie reads the host's `/api/ps`. An endpoint's host is its `url` without the `/v1`. A command names its host with `ollama = "http://localhost:11434"`, which needs a lease, and its model with `ollama_model`, else every model the host has loaded is checked. A model partly or wholly on the CPU fails the round and raises a ruling, and a yes runs the round again once the model is back on the GPU. A host with no `/api/ps` is not checked, and `status` shows the model's name, its share on the GPU, its context length and when it unloads.

### The board

Issues labelled `ready-for-agent` are the board. On a pull request kelpie opened, `ready-for-agent` or a review requesting changes starts a rework of it, the same as `shep kelpie rework <pr>`. On any other open pull request of kelpie's account, `ready-for-agent` adopts it, the same as `shep kelpie adopt <pr>`. Kelpie puts `ready-for-human` on each pull request it hands back.

## Agents

An agent is a harness plus the model and effort it runs on. The harnesses are Claude Code, `claude-code`, pi, `pi`, which runs a model on an OpenAI-compatible server such as Ollama, and Codex, `codex`, on a ChatGPT plan. Kelpie's own settings define agents by name, and a project names one per role, over its `models` entry:

```toml
# kelpie's [kelpie] section
[kelpie.agents.opus-high]
harness = "claude-code"
model = "claude-opus-5-5"
effort = "high"

# the project's table
[app.dogs.kelpie.agents]
judge = "opus-high"
auditor = "opus-high"
```

A role left out keeps its `models` entry, so a project that names none runs as before. A local reviewer of kind `session` names an agent from the same list. An agent nobody defines stops the runner at start, naming it.

`usage` says how an agent's usage is read, and so which account paces it: `claude` (the default on Claude Code) reads `/usage`, `codex` reads Codex's own 5-hour and weekly windows, and `none` is a local model that is never paced. It must be the harness's own reader, so leave it out: Claude Code reads `claude`, pi reads `none` and Codex reads `codex`.

A pi agent also needs the model's server and the context size it gives the model:

```toml
[kelpie.agents.qwen]
harness = "pi"
model = "qwen3.8:27b"
effort = "medium"
url = "http://<host>:11434/v1"
context = 65536
```

Kelpie runs pi with a home of its own, so your `~/.pi` is never read. pi starts no MCP servers and runs no Claude Code hooks, so a worker on pi can't have the preview or `worker.guard_hooks`, and the runner refuses to start with either. Kelpie's own checks still run on every command and file write.

A pi call's sandbox allows no host of the model's, since the sandbox opens every port of an allowed host and Ollama's admin calls (pull, delete, create) answer beside chat. Kelpie runs a forwarder outside the sandbox for each call, and the sandbox allows only that. It passes `POST` to the server's `/v1/chat/completions` and refuses every other path and method, naming what was asked. The worker never sees the model's address. The forwarder dials `http://`, so `url` is the server's `http://` address: an `https://` one is refused when settings load.

A Codex agent needs the `codex` command on the shepherd's `PATH` and a ChatGPT plan, and takes only `model` and `effort`:

```toml
[kelpie.agents.gpt]
harness = "codex"
model = "gpt-6.1-sol"
effort = "medium"
```

`codex debug models` lists the plan's models. A project names the agent for a role in its `[app.dogs.kelpie.agents]` table, as above. It runs on shep-kelpie's own ChatGPT login, in `codex_home` (`$SHEP_HOME/kelpie/codex` by default, or the path `codex_home` in the `[kelpie]` section names), never your `~/.codex`. Sign it in once:

```sh
CODEX_HOME="${SHEP_HOME:-$HOME/.shep}/kelpie/codex" codex login --device-auth
```

Each Codex call gets a home of its own with that login linked in, so no call sees another's sessions. A runner reads `codex_home` when it starts. A worker on Codex can't have the preview or `worker.guard_hooks`, the same as pi. Kelpie's checks run as Codex's own hooks: `confine` on every `apply_patch`, `guard` on every command.

A local worker gets only the issues labelled `worker:local`. The rest run on `models.worker`, so you pick which issues it takes. Each account keeps its own daily allowance and 5-hour stop, shown under its name in `status.pacer`, and a new work item waits on every account its roles spend. `shep kelpie doctor` checks Codex answers for a project that spends it. A `none` agent holds `lease` (the GPU lock, `gpu`, by default) for the whole of each call instead, so a qwen round waits behind its turn, and `status.local_leases` shows who holds it.

`status` shows each role's tokens in `by_role`, with `cost_usd` only for calls whose harness reports dollars. `unpriced_calls` counts the rest.

## Design

For how it works and why:

- `CONTEXT.md`: the vocabulary
- `docs/adr/`: decisions that are hard to reverse
- `docs/design-log.md`: every decision so far, the facts behind them, and the test plan
- `docs/specs/`: each test series and its results

## License

MIT or Apache-2.0, at your option.
