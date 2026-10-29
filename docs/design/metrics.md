# Metrics

Kelpie's numbers, live, on shep's metrics endpoint. ADR 0012, #920.

## Built

- Runner and dog send channel samples (`shep_channel::Shepherd::metric_with`); shep's metrics dog serves them on `/metrics` (default `127.0.0.1:9615`) with the sender's labels first: `sheep` (the project), `id`, `fold` for a runner; `dog`, `source` for the dog. `crates/core/src/metrics/mod.rs`
- Through the `Sink` port (`crates/core/src/ports/metrics.rs`): the real one is the runner's `Shepherd`, a test holds `RecordedSink` and asserts the samples sent. Sending never blocks or fails; a full outbox drops a sample, and the next carries the total.
- Counters are kept whole in kelpie's memory (`Recorder`) and each change sends the new total. A runner start begins them at zero, as Prometheus expects of a restart.
- A runner sends at most `SERIES_CAP` (1023) series, one under the dog's 1024 a sender, which leaves the one `kelpie_series_dropped_total` is; a new series past it is not sent and counts there, with one log line per name. The cap follows shep-pm/shep#1222 and needs a shep release with it to hold in full: an older metrics dog holds 256 a sender, and a busy project loses the series past them. The budget test drives every role on every harness and kind of call, and five agents' finished items across the ten kinds, which is 755 series, 502 of them the finished lines'. The cap's history is in `design-log.md`.
- Names `kelpie_*`, snake case; counters end `_total`. Labels only from bounded sets: `role`, `kind`, `agent` and `effort` (named in settings), `harness`, `model`, `ended`, `account`, `window`, `phase`, `feature`, `outcome`, `type`, `tests`, `severity`, `le`, `after`, `server` (kelpie's own servers by name, any other `repo`). Never an issue, pull request, session or path.

## Series

Calls, counted as each ledger line is appended (`Ledger::append`, `crates/core/src/usage/mod.rs`), the file written or not. Lines a stopping runner writes, imports and `shep kelpie issue`'s are not counted:

- `kelpie_calls_total{role,kind,agent,harness,effort,ended}`: `agent` is the settings' name for it (`codex`, `coderabbit`, `qwen`), and `agent` and `effort` read `unknown` where the line has none (imported lines, a local round's effort)
- Forge calls (`kelpie_forge_calls_total`) and the login's rate limit gauges (`kelpie_forge_rate_used`, `kelpie_forge_rate_reset_timestamp_seconds`) are in `metrics-dog.md`; they are 18 series
- `kelpie_tokens_total{role,agent,harness,model,effort,type}`: `type` is `input`, `output`, `cache_read`, `cache_write`
- `kelpie_cost_usd_total{role,agent,harness,model,effort}`, priced calls only; `kelpie_unpriced_calls_total{role,harness}`
- `kelpie_compactions_total{role,harness}`: a compaction the harness did by itself partway through a call, from the ledger line's `compactions`. Claude Code: its compaction hook (`crates/adapters/src/adapters/claude/settings/mod.rs`) leaves a mark in the call's scratch folder, counted when the call ends; only a worker with a `compact_window` has the hook, and a call another image adopted counts none. pi: each `compaction_end` of a turn that made a summary. Kelpie's own `/compact` is `kind="compact"` in `kelpie_calls_total`, not here. Codex and goose: none yet
- `kelpie_call_seconds_total{role,kind}`, beside `kelpie_calls_total` for a mean

Plan limits: gauges the pacer sets whenever it reads an account's usage (`Runner::pace`, `runner/pace.rs`), not when the reading fails. `account` is `claude` or `codex` (`Account::as_str`). `crates/core/src/metrics/plan.rs`

- `kelpie_plan_used_ratio{account,window}`: `window` is `session` (5 hours) or `week`, 0 to 1
- `kelpie_plan_resets_at_seconds{account,window}`: Unix seconds
- `kelpie_plan_allowance_ratio{account}`, `kelpie_plan_spent_today_ratio{account}`: the share of the week today may spend, and the share spent since the day began, 0 to 1. They read the same with `pacing.enabled` off

- The runner's lock: `kelpie_runner_lock_held_seconds_total` and `kelpie_runner_lock_holds_total{le}` (`1`, `+Inf`), three series. `runner-lock.md`, `crates/core/src/metrics/lock.rs`

Work items (phases, how they ended, review rounds, findings, red CI heads) are in `metrics-work-items.md`.

Features: whether what a project turned on runs.

- `kelpie_feature_enabled{feature}` gauge (1 or 0) at open and each reread: `code_graph`, `browser`, `dev_server` (browser on and `main`'s launch file selects a configuration), `hooks`
- `kelpie_feature_runs_total{feature,outcome}`: `code_graph` index `ok`, `failed`, `skipped` (no binary), `ended` (call ended first). `runner/code_graph/`
- `browser`: the bridge opened for a turn `ok`, `failed`, `skipped` (tools not installed). `runner/browser.rs`
- `dev_server`: `ok` (answered within `READY_WITHIN`), `failed` (could not be planned or started, or silent), `skipped` (no launch file on `main`, or none configured). `runner/dev_server/`
- `hooks`: each hook the runner runs, `ok` (exit 0 or a deliberate exit 2) or `failed`; no label per hook or point. The hooks carry the recorder (`Hooks::counted_by`); a `kelpie guard` or `kelpie confine` process has none, so the worker's own `commit`, `push` and `command` hooks are not counted. `crates/core/src/hooks/mod.rs`
- `KelpieFeatureNeverWorks` only counts a feature while `kelpie_feature_enabled` is 1.

MCP servers, tool calls and requests are read from a call's transcript once it ends, and so are the reports of `shep kelpie usage`: `metrics-transcripts.md`.

The dog's series (leases, summons, the forge hold) and the runners' model leases are in `metrics-dog.md`.

## Alerts and dashboard

- `assets/prometheus/kelpie-rules.yml`: a feature failed or skipped since the runner started with no `ok` run (30m); a feature failing in over half its runs in 6h; an MCP server failing to connect in over half its calls in 6h; a plan's week over 0.9 used with over a day to its reset (15m); a role's calls `failed`, `timed-out` or `unreadable` in over a quarter of 6h's calls; one tool error kind, `other` left out, over 0.5 of a role's 6h calls with at least 4 of them (30m); a runner's series dropped.
- `assets/grafana/kelpie.json`: spend by role (with open work items as a line on a right axis) and by model, tokens by type, calls by how they ended, work items by phase, features, MCP servers and tool calls, mean call time, the dog's leases held and waiting, mean lease wait, summons and the forge hold, cost and tokens per merged work item, cache-read share, review rounds, fix turns and findings per finished item, work items by agent and kind, plan use by account and window, request context sizes, cache writes by what came before, and tool errors by kind, role and project over the dashboard's range.
- `docs/guide/metrics.md`: scrape config, the series, what each alert means, loading a backfill.
- `tests/metrics_assets.rs` reads both files as Prometheus and Grafana do: the tool error alert and panel are there, every alert is named in the guide, every series either reads is one the guide lists, every panel type is in `__requires`, and no two panels hold the same grid squares. `promtool` is not in CI, so the alert's expression is checked there by hand (`docs/facts/tool-errors.md`).

## Backfill

- `shep kelpie usage [<project>] --openmetrics` (`crates/core/src/usage/openmetrics.rs`) prints the ledger's call and finished lines as OpenMetrics counters, each total stamped with its line's `at`, for `promtool tsdb create-blocks-from openmetrics`. The lines are replayed through an uncapped `Recorder`, so names and labels are the live ones, with `sheep` the project and no `id` or `fold`. Lines are taken oldest first, imported ones included, and the totals run from the ledger's first line, as a runner started then would send them. Two lines in one second leave the later total.

## Not built

- MCP servers and tool calls for Codex (#1279) and goose (#784).
- Feature runs of the hooks `kelpie guard` and `kelpie confine` run inside a worker's sandbox: another process, with no recorder.
- Requests for goose (#784).
- Canary: a real issue a night through each harness with every feature on, checked from its transcript. #927
