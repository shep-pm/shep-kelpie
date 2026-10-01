# shep-kelpie

A [shep](https://github.com/shep-pm/shep) dog that runs Claude Code workers on your projects, from a planned work item to a merged pull request. It holds the merge gate, the review budgets and the pacing in code, and calls Claude only for the work and for judgement.

This is early. It changes without notice, and there is no release yet.

## Getting started

From nothing to a worker on your repo. The output below is what each command printed on a scratch shepherd and a fresh checkout.

### Before you start

You need these on your machine:

- [shep](https://github.com/shep-pm/shep) 0.12 and Rust 1.88 or later, to build kelpie
- Claude Code, signed in
- `git`, and `gh` signed in to the account that opens the pull requests
- a GitHub repo whose default branch is `main`, and a checkout of it

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

If no shepherd is running, `shep muster` starts one, and kelpie with it. Then `shep dogs` shows it:

```
ID  NAME    STATUS  PID    RESTARTS  EXIT  CPU  MEM   UPTIME  SOURCE
0   kelpie  online  23814  0         -     -    8.5M  4s      adopted
```

Adopt it once and leave it enabled. It is the dog that holds the leases every project's runner asks before a summon.

### 3. Install kelpie's tools

Every project needs them. Each agent runs inside the sandbox runtime they bring, and a runner won't start without it.

```sh
shep kelpie tools install
```

```
installed kelpie's tools in /path/to/.kelpie/tools
```

It downloads a headless Chrome of about 95 MiB, and puts it under kelpie's home.

### 4. Add your project

In the checkout:

```sh
shep kelpie add
```

```
label `ready-for-agent`: already on shep-pm/shep
label `ready-for-human`: already on shep-pm/shep
label `review please`: already on shep-pm/shep
runner `scratch`: added with its settings, stopped until `shep kelpie start`
```

On a repo without those labels, `add` makes them. The project is named after the repo, or `shep kelpie add <name>`, as `scratch` was here.

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
ok       scratch: labels: shep-pm/shep has `ready-for-agent`, `ready-for-human`, `review please`
ok       scratch: coderabbit: CodeRabbit has commented on a pull request of shep-pm/shep
ok       scratch: local review: ready
MISSING  scratch: rulings: rulings go to the webhook, and kelpie's settings name none. Fix: add a `webhook` table to kelpie's [kelpie] section of dogs.toml, as kelpie-settings.example.toml shows, or drop `webhook` from `ruling_channels`
1 thing a project needs is missing
```

Each `MISSING` line names its fix, and `doctor` exits non-zero until they are done. A ruling is a question kelpie cannot settle itself, such as a merge, and it must reach you. Pick where:

- ntfy or Discord: add the `[kelpie.webhook]` table to `dogs.toml` in the shepherd's home, from `kelpie-settings.example.toml`
- the Claude app on your phone: put `ruling_channels = ["relay"]` in the `[kelpie]` section of that file instead

Run `shep kelpie doctor` again. With the relay alone it ends:

```
ok       scratch: rulings: rulings go to the relay session, and the webhook is off
nothing a project needs is missing
```

### 6. Start it, and label a first issue

```sh
shep kelpie start
```

Put `ready-for-agent` on an issue that says what done looks like, with acceptance criteria. The runner gives it to a worker, which opens a draft pull request. Kelpie then runs the review loop and CI, and asks you for a ruling before it merges. `shep kelpie status` shows every project, and `shep kelpie add <issue>` puts an issue on the board without the label. A worker whose first turn ends with no pull request and no question is sent back once, and asks you for a ruling if it stops short again.

### 7. Answer a ruling

A ruling reaches the channel you chose in step 5. Answer it from there, or from the terminal:

```sh
shep kelpie rule          # lists the rulings waiting
shep kelpie rule 14 yes
shep kelpie rule 14 no rename the flag
```

On ntfy you can reply in the topic after a one-time `shep kelpie totp`, as the reference below describes. In the Claude app, tap an answer or reply in words.

### 8. Pause

```sh
shep kelpie pause
```

The current turn finishes, then the worker parks. `shep kelpie start` resumes it.

## Reference

### What it needs

- shep 0.12
- Rust 1.88 or later, to build it
- Claude Code, signed in
- `git`, and `gh` signed in to the account that opens the pull requests
- a GitHub repo per project, where `add` makes the `ready-for-agent`, `ready-for-human` and `review please` labels it lacks
- a local review command or an OpenAI-compatible endpoint, or a project that lists `claude` alone

### Merging

A project on `merge_authority = "auto"` merges its pull requests without asking once every gate passes, and posts a notice after. The example settings use `ask`, which raises a ruling before every merge.

### Planning

When the board picks an issue, a planning call on Opus reads the repo at `main` and decides whether it is one pull request or several. Most stay one. Several become sub-issues of the issue, each with its labels and blocked by the pieces it needs first, and the issue gets one comment with the plan.

- Under `auto` the split happens on its own. Under `ask` it's a ruling: `yes` opens the sub-issues, `no` works the issue whole, and `answer <note>` plans it again with your note
- An issue with sub-issues is never worked itself, and kelpie closes it once every sub-issue is closed
- A sub-issue is never planned again, and neither is an issue added with `add`
- Off by default until the sub-issue and blocked-by calls have run against a real repo: `[planning] enabled = true` turns it on, and `[models.planner]` picks the model
- A split or a parent close the forge refuses three times in a row waits on a ruling, and the board goes on

### Running a project

Kelpie needs shep 0.12 and runs in your own shepherd, beside your other sheep. Adopt it once and leave it enabled: the adopted kelpie is the dog that holds the leases every runner asks before a summon, and it asks shep for the channel the lease commands reach it on.

```sh
shep adopt /path/to/shep-kelpie --name kelpie
```

A kelpie adopted before it asked for the channel has none until it is adopted again: run the same `shep adopt`, then `shep disable kelpie` and `shep enable kelpie`. A `kelpie-dog` sheep left from before is removed when the adopted kelpie starts, and its book at `~/.kelpie/dog/book.json` is kept as it is. The adopted kelpie gets no `KELPIE_HOME` from shep, so its book is always under `~/.kelpie`.

The dog also holds `cargo-test`, a share of this machine for running tests. Workers run their test suites under it, and so can anything else on the machine: `shep kelpie lease run cargo-test -- cargo test` waits for a turn, runs the command and exits with its code, or 75 when it cannot reach the dog. Three hold it at once unless `[kelpie.leases]` says otherwise, the rest queue, and `shep kelpie lease status` shows who holds it and who waits. A command that ends or dies gives its turn back. One whose `lease run` was killed outright keeps it until the command, and anything it left running, exits: `status` shows how long each has held it.

Then, in the checkout of any GitHub repo whose default branch is `main`:

```sh
shep kelpie add        # labels, settings, and the runner, stopped
shep kelpie start      # starts the runner, then the project
shep kelpie pause
shep kelpie status     # every project
shep kelpie doctor     # what each project still needs on this machine
shep kelpie rule 14 yes
```

Every trigger the runner takes is also a verb: `add <issue>`, `rework <pr>`, `adopt <pr>`, `gate [<issue>]`, `drop [<issue>]` and `rule`. Each reaches the project whose repo holds the folder you run it in, a worktree of it included, or the one `-p <project>` names anywhere in the line. From anywhere else it lists the projects and guesses nothing. `shep trigger` still works.

A ruling's id is unique across projects, so `rule` needs no project:

- `shep kelpie rule 14 yes`
- `shep kelpie rule 14 no rename the flag`
- `shep kelpie rule 15 use --dry-run`, for a worker's question. A `yes` there is the answer's text

Quotes are optional, but zsh still needs them around a note with `?`, `*`, `!` or an apostrophe. `-p` goes before the answer: after its first word, and after a `--`, a `-p` is part of the answer. `shep kelpie rule` alone lists the rulings waiting and asks which to answer and how.

`add` names the project after the repo, or `shep kelpie add <name>`. It makes `ready-for-agent`, `ready-for-human` and `review please` where the repo lacks them, and registers the runner, holding the project's settings as its `[app.dogs.kelpie]` table. `add` and `start` say how to bring the dog up when the adopted kelpie is not running with its channel. Worktrees, build folders and state go under `~/.kelpie`, never inside the checkout. Running `add` again changes nothing. `start` and `pause` find the project from the checkout, or take its name.

`doctor` changes nothing and prints one line per check, each missing piece with its fix, then exits non-zero if a project needs something it lacks. It checks that `claude` is installed and logged in, that `gh` is logged in and may push to each project's repo, the sandbox runtime every agent runs in, the shepherd's version, each project's labels, CodeRabbit where a project turns it on, the local review command or endpoint where one is set, the preview tools for a project that shows its UI, and that rulings have a webhook where they go to one. `shep kelpie doctor <project>` checks one project. `--test-alert` posts one test alert to the webhook, which is the only post it ever makes. A line marked `unsure` could not be settled, and does not fail the run: CodeRabbit is one, since a repo it has not yet reviewed looks the same as a repo without it.

Lookout edits a project's table in the runner's pane, and kelpie's own settings in its `[kelpie]` section of `dogs.toml`. Start that section from `kelpie-settings.example.toml`, and keep `dogs.toml` private: the webhook's URL is a credential. `ruling_channels` there, or in a project's table, picks the webhook, the relay or both. Both is the default, and only a project that posts to the webhook needs a `webhook` table. Its `[kelpie.reviewers]` defines the pull request reviewers, CodeRabbit, cubic and Codex, by their review windows, and a project's `pull_request_reviewers` lists the ones it uses in preference order: each round goes to the first whose window is free. Codex's table also takes `reviews_on_ready`, off when absent: turn it on only where Codex's automatic reviews are enabled. Then marking a draft ready is its summon and kelpie posts no comment on top, so one round spends one review. On an ntfy webhook, a ruling can be answered from the topic: run `shep kelpie totp` once and scan the QR code into an authenticator app, then reply with the line the alert ends on, such as `14 yes <code>`, with the app's code last. A reply takes the same answers as `shep kelpie rule`. Anyone who can read the topic can read rulings, but only a reply with the code of the moment answers one, and each code answers once. Five wrong codes turn answers off until `shep kelpie totp --unlock`, and `shep kelpie totp --rotate` replaces a secret that may have leaked. A change reaches a running runner at its next wake, within a minute when idle. `repo` and `forge` wait for its next start.

`shep describe <project>` labels each Claude session the runner starts with its issue and role, such as `#114 worker`.

Kelpie refuses a shepherd on another shep minor or major than the pinned one, naming both versions. The relay's `kelpie relay-*` commands talk to the shepherd's socket with the shep client kelpie is built with, never a `shep` on `PATH`.

### Moving an install from `~/.kelpie/shep`

An install from before `shep kelpie` runs its runners and dog from a Flockfile under `SHEP_HOME=~/.kelpie/shep`. It keeps working as it is. To move it into your own shepherd:

1. Take the runners and the dog out of the old shepherd by name, so no project runs twice, and stop it: `SHEP_HOME=~/.kelpie/shep shep delete <project>... kelpie`, then `SHEP_HOME=~/.kelpie/shep shep kill`. Name only kelpie's sheep, since that shepherd may run others. A `kill` alone leaves them in its saved roll, and a later `shep muster` there would start them beside the new ones. Their state files stay under `~/.kelpie/projects`
2. Adopt kelpie in your own shepherd, as above
3. For each project: `cd ~/.kelpie/repos/<project> && shep kelpie add <project> && shep kelpie start`. `add` makes the project's table from `~/.kelpie/projects/<project>/settings.toml`, and the runner keeps its state file
4. Once: `shep kelpie settings move <project>`, which moves `~/.kelpie/settings.toml` into the `[kelpie]` section

To adopt kelpie in `~/.kelpie/shep` itself instead, drop the dog's `kelpie` entry from the Flockfile and `SHEP_HOME=~/.kelpie/shep shep delete kelpie` first, since `shep adopt` refuses a name a sheep holds. The adopted dog reads the same book.

A runner's Flockfile entry, for a project set up by hand, is in `settings.example.toml`. It needs `SHEP_HOME` as an absolute path in `env`, since a sheep starts without it, and `kill_timeout = "10s"` or more, since a runner needs about 7s to stop cleanly.

### Skills

Every step kelpie drives an agent through runs a skill, by default from [mattpocock/skills](https://github.com/mattpocock/skills) (MIT). Kelpie vendors the ones it uses in `skills/`, pinned to one upstream commit with its licence, and writes them out as a Claude Code plugin when a runner starts. A project installs nothing.

| step | default skill | where it runs |
|---|---|---|
| `triage` | `triage` | not driven yet |
| `planning` | `to-tickets` | the planning call on each issue the board picks |
| `spec` | `to-spec` | not driven yet |
| `implement` | `implement` | the worker's first turn on an issue |
| `tests` | `tdd` | named in the worker's instructions |
| `review` | `code-review` | each Claude review round |
| `ci` | `diagnosing-bugs` | the worker's turn on a red CI run |
| `pr` | `pr` | named in the worker's instructions, unless the repo has a pull request template |
| `reset` | `handoff` | not driven yet |
| `retro` | `retro` | not driven yet (#104) |

A step that runs a skill starts its prompt with the skill's slash command, such as `/mattpocock:implement`, and kelpie's own prompt follows as its arguments. To override one, set it in the project's `[app.dogs.kelpie.skills]` table:

- `{ kind = "path", path = "..." }`: a skill folder with a `SKILL.md`, copied into a plugin of its own, `kelpie-<step>`
- `{ kind = "plugin", plugin = "...", skill = "..." }`: a skill in a Claude Code plugin's folder
- `{ kind = "none" }`: kelpie's own prompt, no skill

A skill that can't load runs kelpie's own prompt instead. The runner logs why, and `status` shows it under `skills`. `scripts/vendor-skills.sh <commit>` moves the pin.

### The review loop

Each pull request goes through a review loop before CI. A project lists its reviewers in `review.reviewers`, in the order the loop runs them, and kelpie's own settings define each one by name in `[kelpie.local_reviewers.<name>]`:

- `kind = "command"`: a command of your own that keeps the contract below
- `kind = "endpoint"`: kelpie's own reviewer, for any OpenAI-compatible server such as Ollama, LM Studio or llama.cpp's server
- `kind = "claude"`: a fresh Claude session on its own `model` and `effort`
- `kind = "session"`: a fresh session on the agent its `agent` names (see Agents below)

`claude` is always defined: the project's own Claude round on its reviewer's agent, `models.reviewer` unless the project names one. The loop ends once two rounds in a row, from two different reviewers, find nothing above a nit, and those nits get fixed with no further round. Where only one reviewer can run, one clean round ends it.

A local model alone:

```toml
# the project's [app.dogs.kelpie.review]
reviewers = ["qwen"]

# kelpie's [kelpie] section
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
paths = ["src/runner/merge/**", "src/guard/**"]
```

`paths` limits a reviewer to pull requests that change a file under one of its globs, and the loop skips it elsewhere. Every round's prompt carries the issue's acceptance criteria: the section under an "Acceptance criteria" heading, or the whole body without one.

A project that lists none runs `review.local` and then `claude`, and a table with neither runs `~/.claude/scripts/qwen-review.sh`, the maintainer's own command, then Claude. `review.local` is the older form: it takes the same keys as a definition, or `kind = "off"`, and the runner says so at start. `review.local_rounds` caps the rounds from local reviewers per work item. Once they are spent, only Claude reviewers run.

A missing command or an endpoint that doesn't answer stops the runner at start.

## Agents

An agent is a harness plus the model and effort it runs on. The harnesses are Claude Code, `claude-code`, and pi, `pi`, which runs a model on an OpenAI-compatible server such as Ollama. Kelpie's own settings define agents by name, and a project names one per role, over its `models` entry:

```toml
# kelpie's [kelpie] section
[kelpie.agents.opus-high]
harness = "claude-code"
model = "claude-opus-5-5"
effort = "high"

# the project's table
[app.dogs.kelpie.agents]
judge = "opus-high"
```

A role left out keeps its `models` entry, so a project that names none runs as before. A local reviewer of kind `session` names an agent from the same list. An agent nobody defines stops the runner at start, naming it.

`usage` says how an agent's usage is read, and so which account paces it: `claude` (the default on Claude Code) reads `/usage`, `codex` reads Codex's own 5-hour and weekly windows, and `none` is a local model that is never paced. It must be the harness's own reader, so leave it out: Claude Code reads `claude` and pi reads `none`. `codex` waits on a Codex harness (#126).

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

The sandbox lets a pi call reach the model's host on any port, and Ollama has no login, so a worker's commands can reach its whole API. Only point a pi agent at a server you're fine with that for. A forwarder that passes only the chat calls would close it, and isn't built yet (#235).

A local worker gets only the issues labelled `worker:local`. The rest run on `models.worker`, so you pick which issues it takes. Each account keeps its own daily allowance and 5-hour stop, shown under its name in `status.pacer`, and a new work item waits on every account its roles spend. `shep kelpie doctor` checks Codex answers for a project that spends it. A `none` agent holds `lease` (the GPU lock, `gpu`, by default) for the whole of each call instead, so a qwen round waits behind its turn, and `status.local_leases` shows who holds it.

`status` shows each role's tokens in `by_role`, with `cost_usd` only for calls whose harness reports dollars. `unpriced_calls` counts the rest.

An endpoint takes `url` (the base, up to and including `/v1`), `model`, and `context`, the context size in tokens the server gives that model. Kelpie diffs the pull request, cuts the diff to fit that context, and sends each piece with its own review prompt, `src/adapters/local/review-prompt.md`. Set `context` to what the server really uses: Ollama gives its OpenAI-compatible endpoint a small default context unless `OLLAMA_CONTEXT_LENGTH` says more, and drops whatever doesn't fit without saying so.

A command is run as `<command> --dir <worktree> --round <n> --diff <base>`, with:

- `QWEN_REVIEW_OUT`: the folder to write in
- `KELPIE_REVIEW_HEAD`: the commit under review
- `TMPDIR`: the folder the GPU lock lives under
- `KELPIE_REVIEW_CRITERIA`: a file holding the issue's acceptance criteria

It writes `round-<n>.txt` in that folder, one finding per line as `SEVERITY|path:line|what|why` with `HIGH`, `MEDIUM` or `LOW`, and then an empty `round-<n>.txt.done`. Kelpie reads nothing without the marker, and nothing from stdout. A nonzero exit fails the round. A command that writes `LOW|<path>:0|not reviewed: <n> lines exceeds the chunk limit|...` is run again with `--files <hunk file>` in place of `--diff`, on that file alone. If that run fails, the placeholder stays as the finding.

`lease` names the lock kelpie holds around each round of a command or an endpoint. `gpu` is this machine's GPU lock, the one the qwen scripts take. Any other name is a lock of its own, so a reviewer on another machine's GPU never waits on this one's. Leave it off for a command that takes the lock itself, as `qwen-review.sh` does. `gpu_lease = true` is the older spelling of `lease = "gpu"`.

With a lease, before a round against Ollama, kelpie reads the host's `/api/ps`. An endpoint's host is its `url` without the `/v1`. A command names its host with `ollama = "http://localhost:11434"`, which needs a lease, and its model with `ollama_model`, else every model the host has loaded is checked. A model partly or wholly on the CPU fails the round and raises a ruling, and a yes runs the round again once the model is back on the GPU. A host with no `/api/ps` is not checked, and `status` shows the model's name, its share on the GPU, its context length and when it unloads.

Issues labelled `ready-for-agent` are the board. On a pull request kelpie opened, `ready-for-agent` or a review requesting changes starts a rework of it, the same as `shep kelpie rework <pr>`. On any other open pull request of kelpie's account, `ready-for-agent` adopts it, the same as `shep kelpie adopt <pr>`. Kelpie puts `ready-for-human` on each pull request it hands back.

- `CONTEXT.md`: the vocabulary
- `docs/adr/`: decisions that are hard to reverse
- `docs/design-log.md`: every decision so far, the facts behind them, and the test plan
- `docs/specs/`: each test series and its results

## License

MIT or Apache-2.0, at your option.
