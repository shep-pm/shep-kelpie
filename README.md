# shep-kelpie

A [shep](https://github.com/shep-pm/shep) dog that runs Claude Code workers on your projects, from a planned work item to a merged pull request. It holds the merge gate, the review budgets and the pacing in code, and calls Claude only for the work and for judgement.

First build under way. A project runner reads its settings, keeps its state, and answers `status`, `start` and `pause`.

## Running a project

Settings live at `~/.kelpie/projects/<project>/settings.toml`. Start from `crates/kelpie/settings.example.toml`, which holds the defaults for shep.

Every runner also reads `~/.kelpie/settings.toml`, which names the webhook every ruling is posted to. Start from `crates/kelpie/kelpie-settings.example.toml`, and keep the file private: the URL is a credential.

Kelpie runs under its own shepherd, with `SHEP_HOME=~/.kelpie/shep`. Each runner is a sheep in its flock, and so is the dog, which holds the leases every runner asks before a summon:

```toml
[[app]]
name = "shep"
script = "/path/to/kelpie"
args = ["runner", "shep"]
env = { SHEP_HOME = "/path/to/home/.kelpie/shep" }
channel = true
shutdown_with_message = true
kill_timeout = "10s"

[[app]]
name = "kelpie"
script = "/path/to/kelpie"
args = ["dog"]
env = { SHEP_HOME = "/path/to/home/.kelpie/shep" }
channel = true
shutdown_with_message = true
kill_timeout = "10s"
```

`env` matters. A sheep starts with only `HOME`, `LANG`, `PATH`, `USER` and its `SHEP_*` variables, so `SHEP_HOME` has to be set in its entry. The value must be an absolute path, since shep does not expand `~` in `env`. A runner and the dog refuse to start without it, or with a relative one. The runner passes it on to the relay, whose `kelpie relay-*` commands send its answers to the shepherd at that home. They talk to its socket with the shep client kelpie is built with, never a `shep` on `PATH`, and refuse a shepherd on another shep minor or major than the pinned one.

`kill_timeout` matters too. `shep stop` and `restart` give a runner only that long after the shutdown message, 1.6s by default, then SIGKILL. A runner needs about 7s to stop cleanly, so set it to `10s` or more.

Then `SHEP_HOME=~/.kelpie/shep shep trigger shep status`.

## The local round

Each pull request goes through a review loop before CI. Rounds alternate between a local model and a Claude session, local first, and the loop ends once one of each in a row finds nothing above a nit. `[review.local]` in a project's settings picks the local round:

- `kind = "off"`: every round is the Claude round, and one that finds nothing above a nit ends the loop
- `kind = "endpoint"`: kelpie's own reviewer, for any OpenAI-compatible server such as Ollama, LM Studio or llama.cpp's server
- `kind = "command"`: a command of your own that keeps the contract below

A file without the table runs `~/.claude/scripts/qwen-review.sh`, the maintainer's own command. A missing command or an endpoint that doesn't answer stops the runner at start.

An endpoint takes `url` (the base, up to and including `/v1`), `model`, and `context`, the context size in tokens the server gives that model. Kelpie diffs the pull request, cuts the diff to fit that context, and sends each piece with its own review prompt, `crates/kelpie/src/adapters/local/review-prompt.md`. Set `context` to what the server really uses: Ollama gives its OpenAI-compatible endpoint a small default context unless `OLLAMA_CONTEXT_LENGTH` says more, and drops whatever doesn't fit without saying so.

A command is run as `<command> --dir <worktree> --round <n> --diff <base>`, with:

- `QWEN_REVIEW_OUT`: the folder to write in
- `KELPIE_REVIEW_HEAD`: the commit under review
- `TMPDIR`: the folder the GPU lock lives under

It writes `round-<n>.txt` in that folder, one finding per line as `SEVERITY|path:line|what|why` with `HIGH`, `MEDIUM` or `LOW`, and then an empty `round-<n>.txt.done`. Kelpie reads nothing without the marker, and nothing from stdout. A nonzero exit fails the round. A command that writes `LOW|<path>:0|not reviewed: <n> lines exceeds the chunk limit|...` is run again with `--files <hunk file>` in place of `--diff`, on that file alone.

`gpu_lease = true`, on either kind, has kelpie hold the GPU lock around each round. Leave it off for a command that takes the lock itself, as `qwen-review.sh` does.

Kelpie never creates labels in a project's repo. Create `ready-for-agent` and `ready-for-human` there before its first run. Issues labelled `ready-for-agent` are the board. On a pull request kelpie opened, `ready-for-agent` or a review requesting changes starts a rework of it, the same as `shep trigger <project> rework <pr>`. On any other open pull request of kelpie's account, `ready-for-agent` adopts it, the same as `shep trigger <project> adopt <pr>`. Kelpie puts `ready-for-human` on each pull request it hands back.

- `CONTEXT.md`: the vocabulary
- `docs/adr/`: decisions that are hard to reverse
- `docs/design-log.md`: every decision so far, the facts behind them, and the test plan
- `docs/specs/`: each test series and its results

## License

MIT or Apache-2.0, at your option.
