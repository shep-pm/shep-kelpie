# shep-kelpie

A [shep](https://github.com/shep-pm/shep) dog that runs Claude Code workers on your projects, from a planned work item to a merged pull request. It holds the merge gate, the review budgets and the pacing in code, and calls Claude only for the work and for judgement.

This is early. It runs the maintainer's own projects and changes without notice, and there is no release yet.

## What it needs

- shep 0.12
- Rust 1.88 or later, to build it
- Claude Code, signed in
- `git`, and `gh` signed in to the account that opens the pull requests
- a GitHub repo per project, with `ready-for-agent` and `ready-for-human` labels
- a local review command or an OpenAI-compatible endpoint, or a project that lists `claude` alone

## Merging

A project on `merge_authority = "auto"` merges its pull requests without asking once every gate passes, and posts a notice after. The example settings use `ask`, which raises a ruling before every merge.

## Planning

When the board picks an issue, a planning call on Opus reads the repo at `main` and decides whether it is one pull request or several. Most stay one. Several become sub-issues of the issue, each with its labels and blocked by the pieces it needs first, and the issue gets one comment with the plan.

- Under `auto` the split happens on its own. Under `ask` it's a ruling: `yes` opens the sub-issues, `no` works the issue whole, and `answer <note>` plans it again with your note
- An issue with sub-issues is never worked itself, and kelpie closes it once every sub-issue is closed
- A sub-issue is never planned again, and neither is an issue added with `add`
- Off by default until the sub-issue and blocked-by calls have run against a real repo: `[planning] enabled = true` turns it on, and `[models.planner]` picks the model
- A split or a parent close the forge refuses three times in a row waits on a ruling, and the board goes on

## Running a project

Kelpie needs shep 0.12 and runs in your own shepherd, beside your other sheep. Adopt it once and leave it enabled: the adopted kelpie is the dog that holds the leases every runner asks before a summon, and it asks shep for the channel the lease commands reach it on.

```sh
shep adopt /path/to/shep-kelpie --name kelpie
```

A kelpie adopted before it asked for the channel has none until it is adopted again: run the same `shep adopt`, then `shep disable kelpie` and `shep enable kelpie`. A `kelpie-dog` sheep left from before is removed when the adopted kelpie starts, and its book at `~/.kelpie/dog/book.json` is kept as it is. The adopted kelpie gets no `KELPIE_HOME` from shep, so its book is always under `~/.kelpie`.

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

`doctor` changes nothing and prints one line per check, each missing piece with its fix, then exits non-zero if a project needs something it lacks. It checks that `claude` is installed and logged in, that `gh` is logged in and may push to each project's repo, Claude Code's sandbox, the shepherd's version, each project's labels, CodeRabbit where a project turns it on, the local review command or endpoint where one is set, the preview tools for a project that shows its UI, and that rulings have a webhook where they go to one. `shep kelpie doctor <project>` checks one project. `--test-alert` posts one test alert to the webhook, which is the only post it ever makes. A line marked `unsure` could not be settled, and does not fail the run: CodeRabbit is one, since a repo it has not yet reviewed looks the same as a repo without it.

Lookout edits a project's table in the runner's pane, and kelpie's own settings in its `[kelpie]` section of `dogs.toml`. Start that section from `kelpie-settings.example.toml`, and keep `dogs.toml` private: the webhook's URL is a credential. `ruling_channels` there, or in a project's table, picks the webhook, the relay or both. Both is the default, and only a project that posts to the webhook needs a `webhook` table. Its `[kelpie.reviewers]` defines the pull request reviewers, CodeRabbit and cubic, by their review windows, and a project's `pull_request_reviewers` lists the ones it uses in preference order: each round goes to the first whose window is free. On an ntfy webhook, a ruling can be answered from the topic: run `shep kelpie totp` once and scan the QR code into an authenticator app, then reply with the line the alert ends on, such as `14 yes <code>`, with the app's code last. A reply takes the same answers as `shep kelpie rule`. Anyone who can read the topic can read rulings, but only a reply with the code of the moment answers one, and each code answers once. Five wrong codes turn answers off until `shep kelpie totp --unlock`, and `shep kelpie totp --rotate` replaces a secret that may have leaked. A change reaches a running runner at its next wake, within a minute when idle. `repo` and `forge` wait for its next start.

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

## Skills

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

## The review loop

Each pull request goes through a review loop before CI. A project lists its reviewers in `review.reviewers`, in the order the loop runs them, and kelpie's own settings define each one by name in `[kelpie.local_reviewers.<name>]`:

- `kind = "command"`: a command of your own that keeps the contract below
- `kind = "endpoint"`: kelpie's own reviewer, for any OpenAI-compatible server such as Ollama, LM Studio or llama.cpp's server
- `kind = "claude"`: a fresh Claude session on its own `model` and `effort`

`claude` is always defined: the project's own Claude round on `models.reviewer`. The loop ends once two rounds in a row, from two different reviewers, find nothing above a nit, and those nits get fixed with no further round. Where only one reviewer can run, one clean round ends it.

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
