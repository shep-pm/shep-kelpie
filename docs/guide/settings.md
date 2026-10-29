# Settings

A project's settings are its `[app.dogs.kelpie]` table. `shep kelpie get [<key>]` prints it, or one dotted key, as TOML, and `shep kelpie set <key> <value>` changes one key, such as `shep kelpie set git.merging auto` or `shep kelpie set agents.reviewers '["qwen"]'`. The value is a TOML value, and text that is not one is a string. `set` writes only a table the runner would accept, and refuses to move `git.checkout` or `git.remote` onto a checkout or repo another project runs, as `add` does. It says when a running runner takes the change: at its next wake, or for `git.checkout`, `git.remote` and kelpie's gateways, when it next starts. Lookout edits the table too, in the runner's pane. Every setting sits in a table of its own.

Two example files hold what every setting means, in their comments, and nothing else says it:

- [`settings.example.toml`](https://github.com/shep-pm/shep-kelpie/blob/main/settings.example.toml) has every project key, and shows a runner's Flockfile entry for a project set up by hand
- [`kelpie-settings.example.toml`](https://github.com/shep-pm/shep-kelpie/blob/main/kelpie-settings.example.toml) has shep-kelpie's own settings, shared by every project: the `[kelpie]` section of `dogs.toml`, which lookout edits in the dog's pane. Keep `dogs.toml` private, since the webhook's URL is a credential

A change reaches a running runner at its next wake, within a minute when idle. `git.checkout`, `git.remote` and kelpie's gateways wait for its next start, and the log says so. A new `git.base` reaches the work items opened after the change; each one in flight finishes on the base it was cut from. The dog reads `[kelpie.leases]`, and each review bot's window from its agent file, only when it starts, so after a change to either run `shep restart kelpie`.

A key kelpie no longer reads, such as `review.reviewers`, `coderabbit` or `planning`, stops the runner, naming the key and what replaces it. So does each top-level key from before the tables: `repo`, `forge`, `merge_authority`, `ci`, `max_items`, `max_parked` and `skills.ci` name the key that took each over, and `private_names` says to check text for your own words with a hook in `hooks` instead, which kelpie runs for every harness and role.

## What `add` writes

`add` writes the project's settings with these values. What each setting does is in [`settings.example.toml`](https://github.com/shep-pm/shep-kelpie/blob/main/settings.example.toml).

- `git.checkout` and `git.remote`: the checkout and the GitHub repo its `origin` names
- `git.base`: the branch `--base` named, or the repo's default branch. A work item records it when it opens. Work items branch from `origin/<base>`, and their pull requests open against and merge into it. Adoption and contributions accept a pull request into the base and refuse one into another branch
- `git.merging = "ask"` and `git.issues = "ask"`
- `ci.block` is on when the checkout has `.github/workflows`, and `ci.fix_attempts = -1`. `ci.queued_minutes` is not written and reads 60: how long a check may stay `queued` or `in progress` in the wait for CI before kelpie reruns its workflow run once, and past that asks you with a `check-queued` ruling. The same number is how long a head with no check at all waits: with a workflow on `origin/main` that runs on a pull request it then asks you with a `no-checks` ruling, and with none it goes on as skipped at once
- `concurrency.active_items = 1` and `concurrency.pending_rulings = 2`
- `budget.per_item_usd = 10`
- no list of implementers: the agent board picks the implementer a new work item opens on, out of every agent file whose role is `implementer` that the project allows. `agents.md` says the picking. An older table's `agents.implementers`, `agents.implementer_choice` and `agents.fallback_after` are read by nothing: `doctor` names them, and `shep kelpie upgrade` deletes them
- no `agents.allow`, which allows every agent, and no `agents.slack`, which reads 60 minutes: how far past the soonest finish the agent board lets a work item's agent finish
- `agents.reviewers = ["defect-hunter-lite"]`, with `iq3-xxs-review` first when kelpie's settings hold a gateway named `paddock`, and no review bot
- `contributions.pick = "off"`, `contributions.security_reviewer = "security-reviewer"`, `contributions.quiet_days = 7` and `contributions.nudge_days = 3`
- `triage.pick = "off"`, `triage.hand_ready = false`, `triage.wait_days = 7`, `triage.max_questions = 10` and `triage.agent = "triager"`
- `triage.per_day = 10`, `triage.per_author_per_day = 2`, `triage.min_account_days = 30` and `triage.max_body_kb = 20`: the limits checked before an issue's triage call, each `0` for off. The two daily caps are capped at 200, and a higher one is refused at startup. `triage.md` says what each one does
- `triage.authors = {}`: no login is given a role of its own, so the forge's association sets what the triager may ask. `triage.md` says the roles
- no `[app.dogs.kelpie.triage.spam]` table, which means no spam check. `triage.spam.gateway`, `triage.spam.path` and `triage.spam.threshold = 0.8` turn it on, and `triage.md` says what it does
- `pacing.enabled = true`
- no `[app.dogs.kelpie.tickets]` table, which means `tickets.source = "github"`. `local` takes the project's tickets from files, see [Local tickets](tickets.md)
- `worker.allowed_domains = []`, `worker.turn_timeout = 60` and `worker.compact_window = 200000`
- `worker.package_registries = true`: a call that works an item also reaches the registries of the package managers its worktree keeps a lockfile or manifest for, in its root or a folder directly under it, so `allowed_domains` only needs hosts beyond them. The hosts are `registry.npmjs.org` (npm, pnpm, bun), `registry.yarnpkg.com` and `registry.npmjs.org` (yarn), `crates.io`, `index.crates.io` and `static.crates.io` (cargo), `proxy.golang.org` and `sum.golang.org` (go), and `pypi.org` and `files.pythonhosted.org` (uv, pip). `false` gives only `allowed_domains`. `doctor` prints a `package registries` line for each project that keeps any, naming each manager and whether `package_registries` or `allowed_domains` reaches it, and `MISSING` with the hosts to add where neither does

The checkout is the project's repo, and shep-kelpie runs `git fetch`, `git worktree` and `git branch` against its `.git`. Its worktrees, build folders and state go under `$SHEP_HOME/kelpie/<project>`, never inside it. Add from a clone you don't work in if you'd rather keep your own checkout out of it.

## Browser

A project can let its worker look at the UI it builds. Kelpie runs a browser outside the worker's sandbox, where Chromium cannot start, and the worker reaches it over a socket kelpie filters. It is off unless the project turns it on:

```toml
[app.dogs.kelpie.browser]
enabled = true
headed = false
domains = ["pokemon-go-api.github.io", "*.leekduck.com"]
configuration = "web"   # optional: the launch configuration to start
routes = ["/", "/settings"]   # optional: the pages a web bump is proved on, ["/"] when absent
```

- `shep kelpie tools install` installs the Playwright MCP server and Chromium under `<kelpie home>/tools`, and a project that turns the browser on without them gets a MISSING `browser` line in `doctor`.
- The worker's working turns have it, on every harness, as the `playwright` server. A reviewer, the project manager and `attach` do not.
- Chromium resolves only `domains`. Every other host and every IP address fails to resolve, and the filter refuses a URL that is not `http` or `https` to one of them. An entry with a port, an IP address or `localhost` is refused when the settings load: Chromium's resolver rules name hosts and cannot hold one to a port. A tool call that names a `filename` is refused, and a screenshot comes back in the tool's reply.
- Kelpie starts the project's dev server for each such turn from `.claude/launch.json` as `main` has it, never the worktree's copy, and stops it when the turn ends. `configuration` names the launch configuration, else the first one. The server runs in a sandbox of its own that reaches `domains`, on the configured `port` when it is free or a picked one under `autoPort`. The browser opens it at `http://localhost:<port>` and no other local address. A project with no launch file on `main` gets no server, and a server that does not answer in 60 seconds leaves the turn running, with its log in the work item's build folder.
- `routes` are the pages a web bump is proved on (see `dependencies.web`). Each starts with `/`, and the settings refuse one that does not, or an empty list. Where the project has the browser, the tools and a launch file on `main`, kelpie starts the dev server in the bump's checkout, in a sandbox that writes only that checkout and reaches only `domains`, and opens each route in the browser, headless whatever `headed` says. A route passes when the page loads, no request to the dev server answers 5xx and the console logs no error. A failed route turns the bump red like a failed e2e run, and the comment names the route and the status, never a URL or a path. A project with both the browser and an e2e host needs both to pass. A project with neither leaves a web bump open, and `status` says why. A dev server that does not answer leaves the bump open too.
- `headed = true` opens a window you can watch.
- macOS only. On Linux an enabled table stops the runner when it starts, naming shep-pm/shep-kelpie#135.
- A handover waits while a worker's turn holds a browser, since the browser lives in the running image.
