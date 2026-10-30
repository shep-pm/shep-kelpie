# shep-kelpie

A [shep](https://github.com/shep-pm/shep) dog that runs coding agents (Claude Code, pi or Codex) on your projects, from a planned work item to a merged pull request. It holds the merge gate, the review budgets and the pacing in code, and calls a model only for the work and for judgement.

This is early. It changes without notice, and there is no release yet.

## How it works

```
 issue skill ──> issue: acceptance criteria, agent:<name>, ready-for-agent
                                 │
                                 v
 pick:   the project manager, or the board's rule (priority, then age)
                                 │
                                 v
 build:  one worker builds it inline and opens a draft pull request
                                 │
                                 v
 review: reviews.need reviewers, each the agent board's pick ──> one fix turn per reviewer
                                 │                          with any finding
                                 v
 CI:     red goes back to the worker (one rerun first if the log names no changed file);
         red again on the same head is a ruling
                                 │ green, on the latest main
                                 v
 merge:  a ruling under `ask`, kelpie's code under `auto`, as a merge commit
```

1. **Issue.** You, or any agent session, write an issue with kelpie's `issue` skill: acceptance criteria, one pull request's worth, labelled with the agent that should build it where one is wanted, and checked by `shep kelpie issue check` (see [Writing issues](docs/guide/issues.md#writing-issues)).
2. **Pick.** A `ready-for-agent` issue joins the board. The project manager's agent picks the next one, or, without one, the board takes the first in its order, which starts as priority, then age, and opens a work item for it (see [The board](docs/guide/board.md#the-board)).
3. **Build.** A worker, one of the project's implementers, builds it inline, opens a draft pull request and ends its turn (see [Building](docs/guide/building.md#building)).
4. **Review.** A pass runs `reviews.need` reviewers (2 by default), each the agent board's pick from the reviewer files `agents.allow` holds, apart from `contributions.security_reviewer`. Each reads the pull request once. One that finds anything, nits included, sends the worker all its findings for one fix turn, and the next reviewer reads the fix. No loop, no judge (see [The review](docs/guide/review.md#the-review)).
5. **CI.** A red run goes back to the worker. A second red run on a head the worker left alone asks you.
6. **Merge.** Under `ask` you get a ruling; under `auto` kelpie merges once every gate passes. Either way it is a merge commit of the head the gates passed (see [CI and the merge](docs/guide/ci-and-merge.md#ci-and-the-merge)).

Pull requests from other people are followed too, when `contributions.pick` allows it. Kelpie reads each head for security, answers the contributor's comments, and, once the contributor has been quiet for `nudge_days`, asks them in one comment whether it may take the pull request from here. A yes, a thumbs up on that comment, or silence until `quiet_days` is a release, recorded as `released`; a no restarts the wait, and a second no followed by quiet is a `waiting` ruling for you: keep waiting, adopt anyway, or close with thanks (see [Contributions](docs/guide/contributions.md)). You rule every merge of such a pull request, `git.merging = "auto"` included, and a `no <note>` goes to the contributor in the representative's words. A release hands the pull request to kelpie: it opens a work item on its branch, files an issue from the pull request's title and body when it closes none, has the project's reviewers read it and a worker finish it. Your merge ruling answers `yes`, `no <note>` or `rework <note>`, and a `rework` takes it over at once without waiting for the contributor.

Anything kelpie cannot settle itself is a [ruling](docs/guide/rulings.md#rulings) for you. Each project runs as a sheep of your own shepherd, and every agent call it starts is one of that sheep's lambs (see [What shep does for it](docs/guide/shep.md#what-shep-does-for-it)).

## Getting started

From nothing to a worker on your repo. The examples use a project called `scratch`, on the repo `shep-pm/shep`.

### Before you start

You need these on your machine:

- macOS, Linux or native Windows. Native Windows runs a worker with the shepherd in an interactive desktop session, Git for Windows and a one-time `crabpot windows-install`. [`docs/design/windows.md`](https://github.com/shep-pm/shep-kelpie/blob/main/docs/design/windows.md) has the install steps; [#241](https://github.com/shep-pm/shep-kelpie/issues/241) tracks what is left of the port
- [shep](https://github.com/shep-pm/shep) 0.13, from 0.13.0, and Rust 1.88 or later, to build shep-kelpie
- Claude Code, signed in
- `node` and `npm`: `tools install` runs them
- on Linux, `bwrap`, which the sandbox needs. There the sandbox fences a nested `.claude` or `.codex` folder only if it exists when a call starts (a new one is caught after the call), and a worker cannot reach the dog's lease door, so its `cargo-test` runs unleased. `shep kelpie doctor` says so
- `git`, and `gh` signed in to the account that opens the pull requests
- `curl` and `openssl`, which reach GitHub as kelpie's App and sign its tokens
- `pi`, `codex` or `goose`, only to run an agent on that harness (see [Agents](docs/guide/agents.md#agents))
  - For pi, `ripgrep` and `fd` on the `PATH` as well: pi's own `grep` and `find` tools run `rg` and `fd`, and neither is in the pi folder kelpie gives each call. `shep kelpie doctor` names a project that lacks them
  - On Codex and goose, a reviewer and a retro read the repo through a read-only shell (`kelpie guard --read-only`: `cat`, `head`, `tail`, `ls`, `tree`, `grep`, `rg`, `find`, `wc`, `file`, `stat`, `nl`, no git, no redirects). goose's `tree` is allowed, and its `edit` is refused. Its `write` is refused too, unless the reviewer is asked for a failing test (`ci.test_command`), when it writes the reviewer's tests folder and nothing else
- a GitHub repo and a checkout of it. Work goes into the repo's default branch, or the branch `add --base <branch>` names

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

Every project needs them. The sandbox every agent runs in is crabpot, linked into kelpie's own binary, so it needs no install on macOS or Linux; the install brings the browser, the code graph server and the packages the harnesses load.

```sh
shep kelpie tools install
```

```
installed kelpie's tools in /path/to/.shep/kelpie/tools
```

What the install puts where, the macOS keychain block and the login token a repo the App covers needs are in [Tools and the login token](docs/guide/github-app.md#tools-and-the-login-token).

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
agent files: wrote sonnet-high, opus-high, defect-hunter, defect-hunter-lite, iq3-xxs-review, standards, spec, coderabbit, cubic, codex, pm in /path/to/.shep/kelpie/agents
runner `scratch`: added with its settings, stopped
`shep kelpie start scratch` runs it
```

On a repo without those four labels, `add` makes them. The runner puts `in-progress` on an issue while a work item has it. It writes kelpie's own [agent files](docs/guide/agents.md#agents) where they are missing, and never over one you edited, and registers the runner with the project's settings, stopped. The project is named after the repo, or `shep kelpie add <name>`, as `scratch` was here. Running `add` again changes nothing.

The settings `add` writes, with their defaults, are in [What `add` writes](docs/guide/settings.md#what-add-writes).

### 5. Register kelpie's GitHub App

shep-kelpie posts its rulings, notices and review rounds on GitHub as an App of its own that you register for yourself, so they reach you through GitHub's notifications: GitHub never notifies you of what your own login posts. Each ruling is a comment on the work item's pull request, or on its issue while it has none, that mentions you and says how to answer. A notice that a pull request merged is a comment on it, and each round of a reviewer of kelpie's own is posted on the pull request as it ends, with its findings or that it found none. The issues and labels shep-kelpie makes come from the App too. Reply to the comment to answer it (see [Rulings](docs/guide/rulings.md#rulings)), or use `shep kelpie rule`. Without an App, shep-kelpie posts as it always did, from your own login. For the account that owns the repo:

```sh
shep kelpie github setup
```

For a repo an organization owns, `shep kelpie github setup --org <org>`. It opens GitHub's page for a new App, filled in: the name `kelpie-<owner>`, no webhook, and only these permissions: contents, workflows, issues and pull requests to write, metadata to read. Click to create it, and GitHub sends you back to shep-kelpie, which keeps the App and opens the page that installs it. Install it on the repos shep-kelpie works. If the browser does not open, the command prints the address to open by hand.

What the App posts, what a worker's token can do and which permissions it needs are in [The GitHub App](docs/guide/github-app.md#the-github-app).

### 6. Check the machine

```sh
shep kelpie doctor
```

```
ok       claude: installed and logged in
ok       gh: logged in as <you>
ok       sandbox: the sandbox runtime can run
ok       shepherd: shep 0.19.0 at /path/to/.shep
ok       dog: kelpie's dog is running and has named itself
ok       scratch: checkout: /path/to/checkout is a git checkout with an origin
ok       scratch: implementers: the agent board picks among opus-high, sonnet-high
ok       scratch: push access: may push to shep-pm/shep
ok       scratch: labels: shep-pm/shep has `ready-for-agent`, `ready-for-human`, `in-progress`
ok       scratch: github app: kelpie's App is installed on shep-pm/shep and mints a token
ok       scratch: workers: a worker pushes with a token for shep-pm/shep alone, which cannot comment, review, approve or merge; it can push any branch, so protect `main` on GitHub
ok       scratch: reviewers: each pull request is read by defect-hunter-lite
ok       scratch: rulings: no webhook, so rulings reach you only in the log, `status` and `shep kelpie rule`
nothing a project needs is missing
```

Each `MISSING` line names its fix, and `doctor` exits non-zero until they are done. [Commands](docs/guide/commands.md#add-and-doctor) lists what it checks.

With no webhook, a ruling shows only in the runner's log, `shep kelpie status` and `shep kelpie rule`, so you have to look. To be told, set one: [The webhook](docs/guide/rulings.md#the-webhook) says how. With a webhook set, `doctor` ends:

```
ok       scratch: rulings: rulings post to the ntfy webhook
nothing a project needs is missing
```

### 7. Open the sandbox to your registries

A worker's sandbox reaches `github.com`, `api.github.com` and the model's API, and nothing else. `add` leaves `allowed_domains` empty, so a first issue on a repo with a cold cache can't fetch crates or npm packages. Set it with `set`, which writes the project's table on its runner sheep. A list is a TOML value, so quote it for the shell:

```sh
shep kelpie set worker.allowed_domains '["crates.io", "index.crates.io", "static.crates.io"]'
```

A value that does not fit the settings is refused with the reason, and nothing is written. `shep kelpie get worker.allowed_domains` reads it back.

It ends up holding:

```toml
[app.dogs.kelpie.worker]
allowed_domains = ["crates.io", "index.crates.io", "static.crates.io"]
```

For npm, that is `registry.npmjs.org`. What an entry may be is in [`settings.example.toml`](https://github.com/shep-pm/shep-kelpie/blob/main/settings.example.toml). A change reaches a running runner at its next wake.

`-p` takes a glob of project names for `set` and `get`, to change or read a setting in many projects at once. Quote it, or the shell expands `*` against files:

```sh
shep kelpie set -p 'shep*' agents.allow '["sonnet-high", "defect-hunter"]'
shep kelpie get -p '*' agents.allow
```

`set` checks every matched project first: if one refuses the value, none is written and the error names each that refused. `get` prints `project <name>: <value>` for each, or `not set`.

`agents.allow` limits both implementers and reviewers. Absent or empty, it allows every agent. The agent board picks each round's reviewer from the candidates that can run, until `reviews.need` have read or none remain. Set the number in the project's table:

```toml
[app.dogs.kelpie.reviews]
need = 2
```

### 8. Start it, and label a first issue

```sh
shep kelpie start
```

It starts the project's runner, and the project runs while it does: `shep start scratch` and lookout's start do the same, and `shep ls` shows it `online`. The runner reads the board from its first pass.

Put `ready-for-agent` on an issue that says what done looks like, with acceptance criteria, or have an agent write it with kelpie's `issue` skill (see [Writing issues](docs/guide/issues.md#writing-issues)). The runner gives it to a worker, which opens a draft pull request. Then the review pass runs `reviews.need` reviewers, each the agent board's pick, once each, and each reads a head only after CI has passed on it. CI runs again on the last fix. Last, you get a ruling before the merge. `shep kelpie status` shows every project. [The board](docs/guide/board.md#the-board) says what decides whether, and when, an issue starts.

### 9. Answer a ruling

A ruling waits in `status` and `shep kelpie rule`, and reaches the webhook if you set one in step 6. Answer it from the terminal:

```sh
shep kelpie rule          # lists the rulings waiting
shep kelpie rule 427 yes  # the number of the ruling's pull request, or its issue before there is one
```

[Rulings](docs/guide/rulings.md#rulings) has the seven kinds and every way to answer one, including a reply on GitHub or in an ntfy topic.

### 10. Pause, and find the log

```sh
shep kelpie pause
```

The runner stops once its calls end, and `shep kelpie start` runs it again (see [Pausing and finishing](docs/guide/commands.md#pausing-and-finishing)). A runner's log is `shep bleats <project>`, and the dog's is `shep bleats kelpie`.

To wind a project down without watching it, finish it instead:

```sh
shep kelpie finish
```

`finish` runs every open work item to its end before it stops the runner. `shep kelpie finish --all` does that for every project. The commands page sets it against `pause` and `drain`.

## Guides

One page for each topic, indexed in [docs/guide/README.md](docs/guide/README.md):

- [Writing issues](docs/guide/issues.md): the `issue` skill, `shep kelpie issue check` and the `agent:` label
- [The board and the project manager](docs/guide/board.md): what decides whether and when an issue starts, the briefing and the project manager
- [Contributions](docs/guide/contributions.md): pull requests from other people, their security review and the answers to what they ask
- [Building](docs/guide/building.md): the worker, skills, repo plugins, `repo_setup`, the repo's own files, MCP servers, the code graph and retros
- [The review](docs/guide/review.md): the reviewers, review bots, comments from people and local reviewers
- [CI and the merge](docs/guide/ci-and-merge.md): red runs, the merge gate and what `auto` does
- [Agents](docs/guide/agents.md): agent files, gateways, pi and Codex
- [Attaching to a worker](docs/guide/attach.md): steering a work item's worker by hand
- [Rulings](docs/guide/rulings.md): the seven kinds, how to answer them, and answers from GitHub and ntfy
- [The GitHub App](docs/guide/github-app.md): registering kelpie's App, what a worker's token can do, and the tools and login token
- [Upgrading](docs/guide/upgrading.md): moving the dog and the runners onto a new build
- [What shep does for it](docs/guide/shep.md): supervision, lambs, triggers, settings and logs
- [Metrics](docs/guide/metrics.md): the series on shep's metrics endpoint, the alerts and the dashboard
- [Commands](docs/guide/commands.md): `status`, `usage`, `watch`, `doctor` and the rest, and pausing and finishing
- [Leases and end-to-end tests](docs/guide/leases-and-e2e.md): the `cargo-test` lease and running e2e tests on another host
- [Settings](docs/guide/settings.md): every key, what `add` writes, and shep-kelpie's own settings
- [shep-kelpie's home](docs/guide/home-and-logs.md): where it keeps its files, and its logs

## Design

For how it works and why:

- `GLOSSARY.md`: the vocabulary
- `docs/adr/`: decisions that are hard to reverse
- `docs/design/`: every current decision, its reason and where it lives, split by area
- `docs/design-log.md`: the full record behind them, and the test plan
- `docs/facts/`: the measured facts, one file per topic
- `docs/specs/`: each test series and its results

## License

MIT or Apache-2.0, at your option.
