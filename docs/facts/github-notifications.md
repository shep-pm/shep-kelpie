# GitHub notifications

- Measured on the maintainer's login: `gh api` on a repo's notifications answers with `gh`'s default scopes. The answer's `X-Accepted-OAuth-Scopes` is `notifications, repo`, and the token has `repo`. A repo can hold hundreds: 702 unread and 124 read on this repo when #1747 was written.
- A row's keys are `id`, `last_read_at`, `reason`, `repository`, `subject`, `subscription_url`, `unread`, `updated_at` and `url`. The `id` is a JSON string of digits.
- `all=true` lists the read notifications too, which stay in the inbox until they are done. `PUT repos/<owner>/<repo>/notifications` only marks them read and is not used.
- `gh api --paginate` prints one JSON array for each page, one after another, so the adapter reads a stream of arrays. This is `gh`'s documented behaviour, not recorded from a live answer.
- Not measured: `DELETE notifications/threads/<id>` was never run, since it would clear a real notification. GitHub's docs say it matches Done in the inbox. Whether a secondary limit stops a run of hundreds of them, and after how many, is unknown: the loop waits out each limit and stops only at five in a row.
- `fixtures/gh-api-notifications.json` is not a live recording: the worker's sandbox blocks `gh api`. It is built from the keys above, with invented ids, titles and repo, in the shape `--paginate` prints.
