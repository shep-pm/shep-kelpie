# Budget

Pacing, leases, forge limits, time, usage, settings.

## Pacing

- Daily allowance = account's weekly remainder over whole days to reset, fixed at day start; once spent nothing new dispatched (window and board screening already pace). #15, `runner/pace.rs`
- At 50% of 5-hour window, or a week at 100%, no turn starts until it resets; a spent week holds every turn, pacing on or off. #15, #1596
- `add <issue>` skips daily allowance, not 5-hour window. #15
- `pacing.enabled` (on): off lifts the allowance and 5-hour window, not a spent week; usage still read. #67
- Unreadable usage holds (a guess would spend the account it protects). #15
- Each account paces separately; new item paces on every account its agents spend, its own agent's included. #129, #1293
- An API agent (`login: api`, `Limit::Api`) spends credit, not a plan window: it is never paced, no plan hold stops it, and it adds no account to dispatch pacing. A `usage` key beside `login: api` is refused with its line. The dog reads no meter for it and hides `ANTHROPIC_API_KEY` from every process it starts, as it hides the Claude login. #1658, `runner/pace.rs`, `dog/availability/`
- `budget.per_item_usd` ($10) parks item `over-budget` before a turn or round, never mid-call; yes doubles it. #492, `core/work_item/spend/`
- A contribution's calls take a slot while they run, count against `budget.per_item_usd` through its `spend`, and go ahead of new issues; a yes or a no with a note to its `over-budget` ruling doubles the cap. Like the PM's and the issue-label calls, they are tied to no work item. #621, `runner/contributions/budget.rs`

## Dog, leases, forge limits

- Runners talk to kelpie through shep: triggers down, notes (`channel.note`) up as running totals (notes drop under backpressure). ADR 0013
- Runner asks dog's book for lease via `lease.<kind>.want.<epoch>`, epoch = its pid (dog reclaims a dead run's leases without racing the new one); bot window = `reviews` in `hours` from its file. `dog/mod.rs`
- Dog saves book to `<kelpie home>/dog/book.json` on every change (a restart grants no second summon); unreadable book starts CodeRabbit closed 1h. #19
- `cargo-test`: the one worker lease, 3 holders default, held while a connection to dog's `lease.sock` is open. #218, `dog/mod.rs`
- Workers build outside the lease, run `shep kelpie lease run cargo-test -- <command>` (exit 75 no lease, 127 no command, 124 past `RUN_LIMIT`), kelpie named by path. #218
- No GPU lease: paddock queues the GPU, so the `gpu` lock, `lease` in agent files (refused, pointing at `gateway`) and the `/api/ps` spill check are gone; `lease take|run gpu` are refused, pointing at `shep paddock run --model <m>`. `status.local_leases` shows a gateway model's paddock lease holder by model name. #390, #501, #502
- Triggers that only ask (`status`, `timings`, `drain`) never wake loop; board poll starts at `BOARD_POLL` (60s), doubles to `IDLE_POLL_MAX` (5 min) while no item is open and the board repeats; a moved board, an item opening or ending, `add`/`tell`/`rule`/`start` and a write of kelpie's own reset it; rate-limit read stays at 60s; `status.board_poll_secs`. #394, #1329
- Failing step backs off 15s-10 min; rate-limit answer holds every forge call until reset (10 min for secondary limit, longer if hit again). #394
- One forge hold and pace per login, kept by the dog (`forge` ask on `lease.sock`, `dog/forge.rs`): any runner's used-up answer holds all; a runner that cannot reach the dog uses its own (`DoorPace`, `core/lease/door_pace.rs`). Dog's copy is in memory, a restart clears it. #580
- `pick` and `fit` asks on `lease.sock`: the agent board's ranking and its fit reading, agent-board.md and agent-board-fit.md. `dog/board/`
- API credit (`[kelpie.api_credit]`, `usd` and `day` 1 to 31): a grant resetting 00:00 UTC on `day`, the month's last when shorter. The dog sums `login: api` agents' priced calls in the window over every ledger, picks such an agent first while the sum is under `usd` and drops it, pinned too, at or above; absent, no cap and no preference. Read at start (`shep restart kelpie`); `status.api_credit`. agent-board.md. #1659
- Pace: 60 calls/minute and 5/second per login (`PER_MINUTE`, `AT_ONCE`, `crates/core/src/ports/pace.rs`), under GitHub's secondary limits; each project gets cap / active projects. Queued calls wait (`Clock::wait`), never fail. #580
- Hold ends at the answer's `retry-after`, else its `x-ratelimit-reset`, else the 10-min fallback; same for `gh` and App calls (App limits are `ForgeError::Limited`; `PacedVoice` holds and paces them; worker-token mint waits the hold). #580
- REST reads that repeat (ruling-thread comments and the comment's node id, the review bot's comments, reviews and statuses) keep each answer's `ETag` per login and path, in memory, and send it as `If-None-Match`; a 304 is the kept answer, which GitHub does not count against the limit. Lists are read a page at a time, each its own conditional read. GraphQL (board, `gh pr view`, `gh issue view`, threads) has no conditional requests. A forge-ledger line carries `not_modified` when a call had 304s; `shep kelpie usage --forge` lists them by kind. `gh api`'s 304 is unmeasured: a tagged read that fails naming a 304, when the same read untagged works, sends no tag again; a list past 50 pages of 100 is an error. #592, `crates/adapters/src/adapters/gh/conditional/mod.rs`
- A write of kelpie's own (forge ledger counts the ones that went through) makes the next look due in `DUE_FLOOR`, not at the next poll; a push is not counted. #592, `runner/looks/`
- A step that retries gets one attempt per step (close of merged issue; ruling-comment edit backs off like a failing step). #580

## Settings

- Read through shep client: runner sheep's `[app.dogs.kelpie]` table + kelpie's `[kelpie]` in `dogs.toml` (start needs no dog); missing required key stops runner by name. #114, #332, `crates/core/src/settings/mod.rs`; `settings/x.rs` below is in `crates/core/src/settings/`
- Changes apply at next wake, or each 30s when busy; some wait for restart. Failing change keeps old settings, alerts once a reason, shows in `status`; hand-written table checked at start, reread. #809, `runner/reread/`
- Every setting is read by something; removed key refused by name with replacement. Pre-table files named. #332, `settings/removed.rs`
- `git`: `checkout`, `remote` (`origin`'s GitHub repo when absent), `merging`, `issues`. `settings/git.rs`
- `ci`: `block`, `fix_attempts` (-1); `concurrency`: `active_items` (1), `pending_rulings` (2). `settings/ci.rs`
- `budget.per_item_usd` ($10); `pacing.enabled` (on), `pacing.kickoff_hours` (8, display only). `settings/budget.rs`
- `agents`: see agents.md. `settings/agents/`
- `rulings`: `pm_decides`, the kinds the PM answers (see who-answers.md); `decide` and `full_panel_paths` refused, deleted by `settings migrate`. `settings/rulings.rs`
- `worker`: `allowed_domains`, `guard_hooks`, `instructions_file`, `turn_timeout` (60), `compact_window` (200000; pi a reserve, goose a threshold: compaction.md). `runner/turn/`
- `skills`: per-step overrides; `retro` off (the PM's batch retro when named); `triage`, `spec`, `reset` driven by no step yet. `crates/core/src/skills/mod.rs`
- `hooks`, `e2e`. ADR 0009, `settings/hooks.rs`, `settings/e2e.rs`
- `[kelpie]`: `webhook`, `gpu_metrics_url`, `codex_home`, `gateways`, `leases.cargo-test` (3), `api_credit` (`usd`, `day`). `crates/core/src/webhook.rs`

## Not built

- Hour-of-day pacing + utilization logging, for a peak-hour effect. `docs/design-log.md` Decided, not built
- GPU busy vs idle waits; needs sampling, phases. #251

## Watching

- `shep kelpie watch [<issue> | pm]` streams runner steps + session transcripts; reads files only (no runner call). Sessions found by id from `session-started` step, never guessed folders. Starts at end; `--back <n>` shows history. #408
