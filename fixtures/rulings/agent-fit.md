### `sonnet-high` does worse at `fix` than `opus-high`. Change its `kinds`?

`sonnet-high` does clearly worse at `fix` than `opus-high`, on their median hours.

| agent | median hours |
|---|---|
| `sonnet-high` | 3h30m |
| `opus-high` | 1h35m |

Suggested `kinds` for `agents/sonnet-high.md`: `kinds = ["docs", "mechanical"]`

| Answer | What it does |
|---|---|
| 👍 or `71 yes` | Changes no file: the `kinds` line above is yours to write into the agent file |
| 👎 or `71 no <note>` | Changes no file and sends no note: the work item has finished |

@octocat
