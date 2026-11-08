# Budget

Pacing, time, usage, settings. The dog, its leases and the forge limits are in `budget-forge.md`.

## Pacing

- A 5-hour window or a week at 100% holds every turn and dispatch until it resets; the longer hold when both apply. `pacer::spent`, `runner/pace.rs`. #15, #1596, #1785
- Nothing else holds: the 50% mark and unreadable usage hold nothing, and the day's allowance is gone from the reading, `status` and the gauges. Usage is still read. #67, #15, #1785, #1787
- `pacer::window` (50%) and `PARK_AT_PCT` stay, for the agent board's waits alone. `dog/board/pick.rs` #1785
- `add <issue>` skips no hold. #15, #1785
- Each account paces separately; new item paces on every account its agents spend, its own agent's included. #129, #1293
- An API agent (`login: api`, `Limit::Api`) spends credit, not a plan window: it is never paced, no plan hold stops it, and it adds no account to dispatch pacing. The dog reads no meter for it and hides `ANTHROPIC_API_KEY` from every process it starts, as it hides the Claude login. #1658, `runner/pace.rs`, `dog/availability/`, agent-files.md
- An API agent is held when the API refuses its key: spent credit ("credit balance is too low", no time, checked again after `CREDIT_RECHECK_SECS`, 6 hours) or a spent spend limit (held until the time its message gives when that is after now, else one `RECHECK_SECS`, and `CREDIT_RECHECK_SECS` when its message gives no readable time). The dog holds the API as a whole, since every `login: api` agent shares one key, and the board waits each such agent until the hold ends. The hold is in the dog's memory, so `shep restart kelpie` clears it. A refused worker turn tells the board, and its unpinned item moves to the next pick, `turn/refused/mod.rs`. agent-board-api.md, `api_refusal.rs`, `dog/availability/`
- `budget.per_item_usd` ($10) parks item `over-budget` before a turn or round, never mid-call; yes doubles it. The maintainer answers, on the PM's recommendation: `over-budget` starts `false`, and its wake shows phase, findings, CI, priority, files and spend (`pm/call.rs`); `true` keeps the PM answering. ADR 0020, #492, #1792, `core/work_item/spend/`
- A contribution's calls take a slot while they run, count against `budget.per_item_usd` through its `spend`, and go ahead of new issues; a yes or a no with a note to its `over-budget` ruling doubles the cap. Like the PM's and the issue-label calls, they are tied to no work item. #621, `runner/contributions/budget.rs`

## Settings

- Read through shep client: runner sheep's `[app.dogs.kelpie]` table + kelpie's `[kelpie]` in `dogs.toml` (start needs no dog); missing required key stops runner by name. #114, #332, `crates/core/src/settings/mod.rs`; `settings/x.rs` below is in `crates/core/src/settings/`
- Changes apply at next wake, or each 30s when busy; some wait for restart. Failing change keeps old settings, alerts once a reason, shows in `status`; hand-written table checked at start, reread. #809, `runner/reread/`
- Every setting is read by something, apart from the keys that load as `Gone` (any value, never refused): `[pacing]`, `agents.implementers`, `agents.implementer_choice`, `agents.fallback_after`, `agents.slack`, `agents.reviewers` and kelpie's own `[agent_board]`. `[pacing]` is out of the schema, `schemars(skip)`, and the rest stay in it marked deprecated. None is read; doctor names each of the project's own, and `settings migrate`, which `upgrade` runs, deletes each, `agents.reviewers` after moving its names into `agents.allow` and `contributions.reviewers`. `[pacing]` is read by nothing since #1787 and #1789 and written by nothing since #1790. A removed key is refused by name with its replacement. Pre-table files named. #332, #1790, `settings/removed.rs`, `settings/agents/`, `flock/set/migrate.rs`
- `git`: `checkout`, `remote` (`origin`'s GitHub repo when absent), `base` (`main` when absent; a change reaches only the work items opened after it), `maintainer` (the login kelpie mentions on a ruling it posts through its App; `gh`'s own login when absent). `merging` and `issues` are refused by name: `merging`'s place is `rulings.pm_decides.merge`, and `issues` went with the follow-up ruling that always raises. `settings/git.rs`
- `ci`: `block`, `fix_attempts` (-1); `concurrency`: `active_items` (1), `pending_rulings` (2). `settings/ci.rs`
- `budget.per_item_usd` ($10), `settings/budget.rs`; `[pacing]` is the `Gone` table in `settings/mod.rs`
- `agents`: see agents.md. `settings/agents/`
- `rulings`: `pm_decides`, the kinds the PM answers (see who-answers.md); `decide` and `full_panel_paths` refused, deleted by `settings migrate`. `settings/rulings.rs`
- `worker`: `allowed_domains`, `guard_hooks`, `instructions_file`, `turn_timeout` (60), `compact_window` (200000; pi a reserve, goose a threshold: compaction.md). `runner/turn/`
- `skills`: per-step overrides; `retro` off (the PM's batch retro when named); `triage`, `spec`, `reset` driven by no step yet. `crates/core/src/skills/mod.rs`
- `hooks`, `e2e`. ADR 0009, `settings/hooks.rs`, `settings/e2e.rs`
- `[kelpie]`: `webhook`, `gpu_metrics_url`, `codex_home`, `gateways`, `leases.cargo-test` (3), `api.monthly_credit_usd` (what the API credit gives each calendar month, which Anthropic gives kelpie no call to read; the dog reads it every 30s and carries it on its `credit` answer, and the PM's board paces on it). `agent_board` loads as `Gone`. `crates/core/src/webhook.rs`

## Not built

- Hour-of-day pacing + utilization logging, for a peak-hour effect. `docs/design-log.md` Decided, not built
- GPU busy vs idle waits; needs sampling, phases. #251

## Watching

- `shep kelpie watch [<issue> | pm]` streams runner steps + session transcripts; reads files only (no runner call). Sessions found by id from `session-started` step, never guessed folders. Starts at end; `--back <n>` shows history. #408
