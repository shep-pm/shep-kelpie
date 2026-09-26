# shep-kelpie

A [shep](https://github.com/shep-pm/shep) dog that runs Claude Code workers on your projects, from a planned work item to a merged pull request. It holds the merge gate, the review budgets and the pacing in code, and calls Claude only for the work and for judgement.

First build under way. A project runner reads its settings, keeps its state, and answers `status`, `start` and `pause`.

## Running a project

Settings live at `~/.kelpie/projects/<project>/settings.toml`. Start from `crates/kelpie/settings.example.toml`, which holds the defaults for shep.

The runner is a sheep under kelpie's own shepherd, with a flock entry like this:

```toml
[[app]]
name = "shep"
script = "/path/to/kelpie"
args = ["runner", "shep"]
channel = true
shutdown_with_message = true
```

Then `SHEP_HOME=~/.kelpie/shep shep trigger shep status`.

- `CONTEXT.md`: the vocabulary
- `docs/adr/`: decisions that are hard to reverse
- `docs/design-log.md`: every decision so far, the facts behind them, and the test plan
- `docs/specs/`: each test series and its results

## License

MIT or Apache-2.0, at your option.
