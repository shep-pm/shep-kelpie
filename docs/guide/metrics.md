# Metrics

Each project's runner sends its numbers to shep's metrics dog, which serves them on its Prometheus endpoint beside the flock's own series. You can watch spend, calls, work items and whether the features you turned on actually work, live, and alert on them.

## Turning it on

```sh
shep enable metrics          # serves http://127.0.0.1:9615/metrics
curl -s 127.0.0.1:9615/metrics | grep '^kelpie_'
```

The runners send to it already: there is nothing to set in kelpie. A runner sends every series again once a minute, so a metrics dog enabled or restarted after the runners has them all within a minute.

Point Prometheus at it, and load kelpie's alert rules from `assets/prometheus/kelpie-rules.yml`:

```yaml
global:
  scrape_interval: 15s
rule_files:
  - kelpie-rules.yml
scrape_configs:
  - job_name: shep
    static_configs:
      - targets: ["127.0.0.1:9615"]
```

Import `assets/grafana/kelpie.json` into Grafana for the dashboard. Shep's own dashboard, for the flock and the host, is in shep's `assets/grafana/`.

## The series

`agent` is the name the project's settings give the agent (`codex`, `coderabbit`, `qwen`), and `effort` how hard the model was set to think. Either reads `unknown` where the ledger has none, as on an imported call. The plan gauges are set each time the pacer reads usage, which it does before a call on the account starts, so they hold the last reading until the next.

Every runner series carries `sheep`, the project's name, as well as `id` and `fold`, which shep's metrics dog adds. The dog's series, last in the table, carry `dog` and `source` instead, and no `sheep`.

| Series | Labels | What it counts |
|---|---|---|
| `kelpie_calls_total` | `role`, `kind`, `agent`, `harness`, `effort`, `ended` | Model calls that ended, and how: `answered`, `unreadable`, `failed`, `timed-out`, `stopped`, `paused` |
| `kelpie_tokens_total` | `role`, `agent`, `harness`, `model`, `effort`, `type` | Tokens as the harness reported them: `input`, `output`, `cache_read`, `cache_write` |
| `kelpie_cost_usd_total` | `role`, `agent`, `harness`, `model`, `effort` | US dollars, from calls whose harness reported a cost |
| `kelpie_compactions_total` | `role`, `harness` | Compactions the harness did by itself partway through a call. Kelpie's own `/compact` is `kind="compact"` in `kelpie_calls_total`. Claude Code workers with a compact window, and pi; a call adopted by a new runner image counts none |
| `kelpie_unpriced_calls_total` | `role`, `harness` | Calls whose harness reported no cost |
| `kelpie_call_seconds_total` | `role`, `kind` | Seconds the calls ran; over `kelpie_calls_total` for a mean |
| `kelpie_plan_used_ratio` | `account`, `window` | How much of a plan window is used, 0 to 1, as the pacer last read it. `account` is `claude` or `codex`, `window` is `session` (5 hours) or `week` |
| `kelpie_plan_resets_at_seconds` | `account`, `window` | When that window resets, in Unix seconds |
| `kelpie_plan_allowance_ratio`, `kelpie_plan_spent_today_ratio` | `account` | The share of the week today may spend, and the share spent since the day began by anyone on the account, 0 to 1 |
| `kelpie_work_items` | `phase` | Open work items: `implement`, `review`, `ci`, `ruling`, `merge`, `done` |
| `kelpie_work_items_finished_total` | `outcome` | Work items finished: `merged`, `closed` (no change), `dropped` |
| `kelpie_work_item_seconds_total` | `outcome` | Wall seconds of the finished work items |
| `kelpie_ci_failures_total` | `tests` | Each head of a work item's pull request that CI read red, counted once per head, by whether the worker could have caught it. `sandbox-only`: every test that failed is in a module `docs/agents/sandbox-red-tests.md` lists whole, so no worker could have run it. `runnable`: at least one is a test the worker could have run, or is in a module that doc lists only in part, or is a test failure in a runner kelpie cannot read, or the log kelpie read was cut before its failing names, or more runs failed than kelpie reads logs of. `none`: no test failed, as on lint, docs, fmt, or a build that never got as far as running one, which is what a compile failure inside a test job is. `timed-out`: a job's `timeout-minutes` ended a run kelpie read, and no log names a failing test, so a test is slow and none failed; it needs every failed run read, and a log that names a failing test keeps that test's class. That list of modules is of kelpie's own repo, so on any other project every failing test is `runnable`. A head is counted once even when a still-red ruling answered `yes` sends the same head back to CI. A red head whose logs kelpie could not read counts nothing |
| `kelpie_review_rounds_total`, `kelpie_fix_turns_total`, `kelpie_rulings_total` | | Review rounds, fix turns and rulings of the finished work items |
| `kelpie_findings_total` | `severity` | Findings above a nit sent to the worker of a finished work item, each counted once across rounds: `high`, `medium` |
| `kelpie_feature_enabled` | `feature` | 1 when the project's settings turn the feature on: `code_graph`, `browser`, `dev_server` (the browser is on and `main`'s launch file selects a configuration), `hooks` |
| `kelpie_feature_runs_total` | `feature`, `outcome` | Times a feature ran, by `feature`: `code_graph`, `browser`, `dev_server` or `hooks`. The code graph's index: `ok`, `failed`, `skipped` (no server installed), `ended` (the call ended first). The browser's bridge opened for a turn: `ok`, `failed`, `skipped` (its tools are not installed). The dev server started for a turn's browser: `ok` (it answered in time), `failed` (it could not be planned or started, or did not answer within 60 seconds), `skipped` (`main` has no launch file, or one that selects no configuration). Each hook the runner runs: `ok` (it exited 0, or refused with 2 on purpose) or `failed` (any other exit, a timeout), one series per outcome and not per hook. Hooks run inside a worker's sandbox by `kelpie guard` are not counted |
| `kelpie_mcp_servers_total` | `server`, `outcome` | MCP servers given to calls, read from each call's transcript once it ends: `connected` or `failed`. `server` is `codebase-memory-mcp` or `playwright` for kelpie's own, and `repo` for any your project names. Claude Code names the servers that failed. pi names the tools it adds and drops and the calls it answers, so a pi server reads `failed` where its session leaves none of its tools as it ends and answered none of its calls, and kelpie gives no reason beyond that |
| `kelpie_mcp_tool_calls_total` | `server` | Tool calls a call made to each server, from Claude Code's and pi's transcripts. A server given and never called reads 0 |
| `kelpie_request_context_tokens` | `role`, `harness`, `le` | Requests to the model by the tokens they sent (input, cache read and cache write), read from each call's transcript once it ends. Buckets `le` 150000, 200000 and `+Inf`, each counting the requests no larger, so `histogram_quantile(0.9, sum by (le) (rate(kelpie_request_context_tokens[1h])))` is the context size nine requests in ten stay under. Claude Code and pi calls of the worker, reviewer and project manager only |
| `kelpie_cache_write_tokens_total` | `role`, `harness`, `after` | Tokens requests wrote to the cache, by what came before: `start` (a session's first request), `gap` (longer since the request before than the cache keeps a write, which is 1 hour when Claude Code wrote the 1-hour cache and 5 minutes otherwise, so the cache had dropped what it held) or `growth`. Claude Code and pi calls of the worker, reviewer and project manager only |
| `kelpie_tool_errors_total` | `role`, `kind` | Tool calls whose result the harness wrote as a failure, read from each call's transcript once it ends, by what made them fail: `guard-block` (kelpie's own `guard` or `confine`), `hook-block` (a hook that is not kelpie's), `tool-not-given`, `unknown-skill`, `permission-refused`, `git-ref-lock`, `tmp-refused`, `path-refused`, `command-not-found`, `edit-miss`, `timeout`, `mcp`, `other` (a command that ran and failed). A failed result counts once, however many lines of output it holds. Claude Code and pi calls of the worker, reviewer and project manager only. A hook that is not kelpie's is named in the runner's log |
| `kelpie_forge_calls_total` | `api`, `by`, `ended` | Forge calls through the ports, counted as the forge ledger (`<kelpie home>/forge.jsonl`) records them: `api` is `rest` or `graphql`, `by` is `login` (the maintainer's `gh`) or `app` (kelpie's GitHub App), `ended` is `ok`, `failed`, `held` (never made, because an earlier answer had the limit used up) or `used-up`. A call can ask GitHub more than once, so this is a lower bound on requests. The reads of `rate_limit` are left out, since GitHub counts them against no limit. There is no `kind` label: `shep kelpie usage --forge` has the breakdown by kind |
| `kelpie_forge_rate_used` | `bucket` | Calls GitHub has counted against the login's limit since it last reset, as `gh api rate_limit` says. `bucket` is `core` (REST) or `graphql`; GraphQL counts points, not calls. A runner reads it at most once a minute, and keeps the last reading while the forge cannot be asked, saying so once in `shep kelpie status` |
| `kelpie_forge_rate_reset_timestamp_seconds` | `bucket` | When that limit next resets, in Unix seconds |
| `kelpie_runner_lock_held_seconds_total` | | Seconds the runner's lock was held, summed over every hold. A pass holds it through its forge calls, so a `rate` near 1 means it is held all the time, and a trigger other than `status`, `drain` and `undrain` waits |
| `kelpie_runner_lock_holds_total` | `le` | Holds of the runner's lock by length: `le="1"` counts those of at most a second, `+Inf` all of them. `status` shows the longest hold of each source line under `lock` |
| `kelpie_model_lease_waiting` | `model` | Calls of this runner waiting for a lease on a gateway's model, from asking for it until the grant. A model whose calls have all ended stays at 0 |
| `kelpie_model_lease_held` | `model` | Calls of this runner holding a lease on a gateway's model |
| `kelpie_model_lease_wait_seconds_total`, `kelpie_model_lease_grants_total` | `model` | Seconds this runner's calls waited for a lease on a gateway's model, and the leases granted to them. Divide the rates for a mean wait. A model on a URL host, or a round that runs a command, takes no lease and is not counted |
| `kelpie_leases_held` | `kind` | The dog's leases in use: 0 or 1 for a book lease (the review bot windows), up to the capacity for `cargo-test`, and the busy hosts for `e2e` |
| `kelpie_lease_waiting` | `kind` | Runners and commands waiting for the lease |
| `kelpie_lease_grants_total`, `kelpie_lease_wait_seconds_total` | `kind` | Times the lease was granted, and the seconds those grants waited in the queue. Divide the rates for a mean wait. A grant to a command already running under the last dog is not counted |
| `kelpie_summons_total` | `bot`, `outcome` | Review bot summons runners reported: `accepted`, or `refused` for each new quote of when the window opens. The maintainer's own summons through `shep kelpie lease take` are not counted |
| `kelpie_dog_bus_missed_total` | | Events on the shepherd's bus the dog did not read because it fell behind or the shepherd dropped them. Each miss makes the dog read the flock again, so a `rate` that stays above 0 is worth a look |
| `kelpie_forge_holds_total` | `login_kind` | Times the forge's rate limit began holding every runner's calls. `login_kind` is `login`, the maintainer's `gh` login, which the dog's hold is for |
| `kelpie_forge_held` | | 1 while that hold stands, 0 when not |
| `kelpie_series_dropped_total` | | Samples of a series the runner held back, past the 1023 it sends |

The dog's series are sent from the dog's own tick, so a hold that has ended reads 0 within ten seconds. Its counters start at zero when the dog starts, and every `e2e` host counts as the one `e2e` kind.

A runner's counters start at zero when it starts, as Prometheus expects of a restart: read them with `rate` or `increase`. When a runner stops, its series go from the endpoint until it starts again.

## Where the 5,000 an hour goes

GitHub counts one limit per login, and every runner on the login reports the same `kelpie_forge_rate_*` numbers, so a query takes `max without (sheep, id, fold)` across them. `used` drops to near zero at each reset, and `increase` over it treats a drop as a counter reset, so it adds the calls since the reset and not a negative number. The core calls GitHub counted in the last hour that kelpie did not send, from the login's REST calls in `forge.jsonl` (held calls were never made, and the App has a budget of its own):

```promql
increase((max without (sheep, id, fold) (kelpie_forge_rate_used{bucket="core"}))[1h:1m])
  - on() sum(increase(kelpie_forge_calls_total{by="login", api="rest", ended!="held"}[1h]))
```

What is left is spent outside kelpie: your own `gh`, scripts and tools on the same login, or requests inside a port call that the ledger's one line does not count. Kelpie's REST calls matched GitHub's count one to one over a 2 minute test. GraphQL does not: it counts points by the query's cost, so 39 queries came to 12 points, and only `core` says what is outside.

## The dashboard

**Tool errors by kind and role** counts each kind over the dashboard's own time range, per role and project, sorted by the count, so the row at the top is the friction to fix. `other` is usually the largest, and is the worker's own command failing rather than anything around it.

## The alerts

- **KelpieFeatureNeverWorks**: a feature that is on has failed or been skipped since the runner started and has never worked once. Its calls run without it. The runner's log says why, in a line with `runs without`.
- **KelpieFeatureFailing**: a feature failed in over half its runs in the last 6 hours.
- **KelpieMcpServerFailing**: an MCP server failed to connect in over half the calls given it in the last 6 hours, so those calls ran without its tools. The runner's log names the server, the call and the error code, in a line with `failed to connect`, and `shep kelpie status` lists it under `standing` until a call finds it connected.
- **KelpiePlanNearlyUsed**: an account's weekly window is over 90% used with more than a day to its reset.
- **KelpieCallsFailing**: over a quarter of a role's calls in 6 hours failed, timed out or came back unreadable.
- **KelpieToolErrorTakesOver**: one kind of tool error, `other` left out, reached over half the calls a role made in the last 6 hours, over at least 4 calls. The kind in the alert is what made them fail, and the panel above is the same series read over a longer stretch.
- **KelpieSeriesDropped**: a runner holds back series past its cap, so some panels miss data. Its log names the series.

## Loading history

The endpoint starts at zero when a runner does, so what ran before you enabled metrics is not on it. `shep kelpie usage --openmetrics` prints the usage ledger as the same counters, each total at the time of its call or finished work item, for Prometheus to load as blocks:

```sh
shep kelpie usage koji --openmetrics > koji.om        # one project; leave it out for every project
promtool tsdb create-blocks-from openmetrics koji.om ./blocks
```

Then stop Prometheus, move the block folders `promtool` made into its data folder, and start it again. Load only the time before Prometheus's first scrape. The series carry `sheep` but not the `id` and `fold` the live ones have, so a query that sums them with `sum by (sheep, ...)` reads both. Counters run from the ledger's first line, as a runner started then would have sent them; read them with `rate` or `increase`. Imported ledger lines are included, with `agent` and `effort` as `unknown`.

## By work item

The endpoint has no work item, pull request or session in its labels, so its series stay few. To see one work item's calls and cost, use `shep kelpie usage`, which reads the project's usage ledger (see [Commands](commands.md)). Both are written from the same call lines, so they agree.
