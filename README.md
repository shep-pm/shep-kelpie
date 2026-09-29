# shep-kelpie

A [shep](https://github.com/shep-pm/shep) dog that runs Claude Code workers on your projects, from a planned work item to a merged pull request. It holds the merge gate, the review budgets and the pacing in code, and calls Claude only for the work and for judgement.

First build under way. A project runner reads its settings, keeps its state, and answers `status`, `start` and `pause`.

## Running a project

Settings live at `~/.kelpie/projects/<project>/settings.toml`. Start from `crates/kelpie/settings.example.toml`, which holds the defaults for shep.

Every runner also reads `~/.kelpie/settings.toml`, which names the webhook rulings are posted to. Start from `crates/kelpie/kelpie-settings.example.toml`, and keep the file private: the URL is a credential. `ruling_channels` there, or in a project's own settings, picks the webhook, the relay or both. Both is the default, and only a project that posts to the webhook needs a `[webhook]` table. On an ntfy webhook, a ruling can be answered from the topic: tap Merge, Send back or Leave on its alert, or reply with the line the alert ends on, one-time code included.

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

Kelpie never creates labels in a project's repo. Create `ready-for-agent` and `ready-for-human` there before its first run. Issues labelled `ready-for-agent` are the board. On a pull request kelpie opened, `ready-for-agent` or a review requesting changes starts a rework of it, the same as `shep trigger <project> rework <pr>`. On any other open pull request of kelpie's account, `ready-for-agent` adopts it, the same as `shep trigger <project> adopt <pr>`. Kelpie puts `ready-for-human` on each pull request it hands back.

- `CONTEXT.md`: the vocabulary
- `docs/adr/`: decisions that are hard to reverse
- `docs/design-log.md`: every decision so far, the facts behind them, and the test plan
- `docs/specs/`: each test series and its results

## License

MIT or Apache-2.0, at your option.
