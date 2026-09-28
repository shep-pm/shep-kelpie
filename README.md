# shep-kelpie

A [shep](https://github.com/shep-pm/shep) dog that runs Claude Code workers on your projects, from a planned work item to a merged pull request. It holds the merge gate, the review budgets and the pacing in code, and calls Claude only for the work and for judgement.

First build under way. A project runner reads its settings, keeps its state, and answers `status`, `start` and `pause`.

## Running a project

Settings live at `~/.kelpie/projects/<project>/settings.toml`. Start from `crates/kelpie/settings.example.toml`, which holds the defaults for shep.

Every runner also reads `~/.kelpie/settings.toml`, which names the webhook every ruling is posted to. Start from `crates/kelpie/kelpie-settings.example.toml`, and keep the file private: the URL is a credential.

The runner is a sheep under kelpie's own shepherd, with a flock entry like this:

```toml
[[app]]
name = "shep"
script = "/path/to/kelpie"
args = ["runner", "shep"]
channel = true
shutdown_with_message = true
kill_timeout = "10s"
```

`kill_timeout` matters. `shep stop` and `restart` give a runner only that long after the shutdown message, 1.6s by default, then SIGKILL. A runner needs about 7s to stop cleanly, so set it to `10s` or more.

Then `SHEP_HOME=~/.kelpie/shep shep trigger shep status`.

Kelpie never creates labels in a project's repo. Create `ready-for-agent` and `ready-for-human` there before its first run. Issues labelled `ready-for-agent` are the board. On a pull request kelpie opened, `ready-for-agent` or a review requesting changes starts a rework of it, the same as `shep trigger <project> rework <pr>`. Kelpie puts `ready-for-human` on each pull request it hands back.

- `CONTEXT.md`: the vocabulary
- `docs/adr/`: decisions that are hard to reverse
- `docs/design-log.md`: every decision so far, the facts behind them, and the test plan
- `docs/specs/`: each test series and its results

## License

MIT or Apache-2.0, at your option.
