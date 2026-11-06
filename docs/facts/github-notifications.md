# GitHub notifications

- Measured on the maintainer's login: `gh api` on a repo's notifications answers with `gh`'s default scopes. The answer's `X-Accepted-OAuth-Scopes` is `notifications, repo`, and the token has `repo`. A repo can hold hundreds: 702 unread and 124 read on this repo when #1747 was written.
- A row's keys are `id`, `last_read_at`, `reason`, `repository`, `subject`, `subscription_url`, `unread`, `updated_at` and `url`. The `id` is a JSON string of digits.
- `all=true` lists the read notifications too, which stay in the inbox until they are done. `PUT repos/<owner>/<repo>/notifications` only marks them read and is not used.
- Measured with `gh` 2.103.3: `gh api --paginate` printed one JSON array holding every row of every page, with no break between pages. The adapter reads a stream of arrays, which takes that and one array for each page.
- Not measured: `DELETE notifications/threads/<id>` was never run, since it would clear a real notification. GitHub's docs say it matches Done in the inbox. Whether a secondary limit stops a run of hundreds of them, and after how many, is unknown: the loop waits out each limit and stops only at five in a row.
- `fixtures/gh-api-notifications.json` is three rows of a live `gh api --paginate` answer on this repo, a public one, so its titles are public. Another project's rows stay out of it.
