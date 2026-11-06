### CI is still red. Look again?

CI failed at 6b0606e: test, clippy. The worker has had 2 fix turns on red runs.

| Answer | What it does |
|---|---|
| 👍 or `71 yes` | Kelpie looks at CI again |
| 👎 or `71 no <note>` | Sends the worker back with a note, and the review reads its change again |

**Project manager**: The failure is a flaky network test, so one rerun should clear it.

@octocat
