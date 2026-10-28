# Metrics: the dog's and the forge's series

The dog sends through the same `Recorder` as a runner (`crates/core/src/metrics/mod.rs`); the rest of the metrics design is in `metrics.md`. ADR 0012, #920.

## Built

- It sends on its own `Shepherd` (`crates/core/src/metrics/dog.rs`), labelled `dog` and `source` by the metrics dog.
- `kelpie_leases_held{kind}`, `kelpie_lease_waiting{kind}` gauges, `kelpie_lease_grants_total{kind}` and `kelpie_lease_wait_seconds_total{kind}`: the desk looks at every lease after each change (`Desk::look`, `dog/watch/`). A grant is a holder not there at the last look, so the book's, the maintainer's and `cargo-test`'s are counted by one rule; the wait is from when the dog first saw the waiter queued, and a waiter that leaves is not counted. `e2e` is one kind for every host, as a host name is not a bounded label. A command seated for a run under the last dog is held but not a grant.
- `kelpie_summons_total{bot,outcome}`: `accepted` or `refused`, counted once per bot and time (or quoted opening) by the watch, not the window, so the runners' repeated totals count once, also after the window forgot a summon or the dog restarted (the saved windows seed the watch). A summon older than its span, or a refusal whose opening has passed, is not news when heard. The maintainer's grants count as summons in the window, not here.
- `kelpie_dog_bus_missed_total`: events on the shepherd's bus the dog never read, from a `Lagged` item of its own stream or the shepherd's `Dropped` notice (`Bus::hear`, `dog/bus.rs`). No label. A rate that climbs means the dog reads too slowly: the stream holds 64 events (`EVENT_CHANNEL_CAPACITY` in shep-client). The dog listens to `channel.note` and `process.*` only, never `channel.metric`, since every runner resends every series each minute.
- `kelpie_forge_holds_total{login_kind}` and `kelpie_forge_held`: a hold begins when a runner reports one and none stands. `login_kind` is always `login`: the dog's hold is the login's, as the App's used-up answer holds only its own reads. The hold ends on its own, so the dog's tick (`Kept::tick`) sets the gauge and sends every series again each minute.

## Forge calls and the login's rate limit

These come from the runners, not the dog: the dog has no `gh`, and a runner already asks the forge.

- `kelpie_forge_calls_total{api,by,ended}`: each line `forge_ledger::Ledger::record_with` writes (`crates/core/src/forge_ledger/mod.rs`, `crates/core/src/metrics/forge.rs`), counted whether or not the file takes it. `api` and `by` are the line's, `ended` its `Ended` in kebab case. A read of `rate_limit` (`Kind::RateLimit`, `Kind::RateLimitReset`) is not counted, as GitHub counts it against no limit. No `kind` or `project` label: a series per kind would not fit the cap, and `sheep` names the project. The login's two APIs ended four ways and the App's two ended three (an App call is never held) are the 14 series
- `kelpie_forge_rate_used{bucket}` and `kelpie_forge_rate_reset_timestamp_seconds{bucket}`, `bucket` `core` or `graphql`: gauges from `Forge::rate_limit`, one `gh api rate_limit`. The runner reads it on its pass, at most once a minute (`Looks::rate_limit_due`), and a failed read keeps the gauges and stands once as a warning. The read is a `rate_limit` line in the forge ledger. Four series. The same read also gives the `search` limit, which `gh search prs` spends from; it has no gauge, but `rate_limit_reset` holds the forge for it as for `core` and `graphql`
- Every runner reports the same login's numbers, so a query takes `max without (sheep, id, fold)` across them (`docs/guide/metrics.md`).

## Model leases

The calls waiting for, and holding, a lease on a gateway's model, by `model`. These come from the runners too. The adapters take the lease and have no `Recorder`, so the runner counts where it hears the call say it waits (`Runner::told`) and begins (`Runner::began`), with the model from the watched call (`Watched.lease`). `crates/core/src/metrics/model_lease.rs`, `crates/runner/src/runner/flight/waiting/mod.rs`

- `kelpie_model_lease_waiting{model}`, `kelpie_model_lease_held{model}`: gauges set from the calls in flight at each told wait, each begin and each pass, so a call that ends sets its model's gauge down. A model once seen is set to 0 rather than left at its last count
- `kelpie_model_lease_wait_seconds_total{model}`, `kelpie_model_lease_grants_total{model}`: counted at each begin, the wait from when the call first said it waited. A model on a URL host and a round that runs a command have no `Watched.lease`, so are not counted
- A call carried through a handover keeps its gateway lease, so the new image counts it held from `Kept.model` (`crates/core/src/ports/agent.rs`) and `Kept.lease`; one carried by an image that did not note the model counts on none until it ends
- Four series a model, so 20 for five models, which the budget test counts against `SERIES_CAP`

## Availability

What the agent board (ADR 0018) will choose by, read by the dog for every agent file it loads. Only the dog's `status` trigger shows it so far, as `availability`. `crates/dog/src/dog/availability/`, `crates/adapters/src/adapters/paddock/mod.rs`

- Each model an agent calls behind a `paddock` gateway (`Runs::gateway`), every 30s (`LOAD_TICK`), from `GET /paddock/status`: `slots` (`turns.lease`, 0 when none is listed), `held` (leases neither revoked nor reclaimable) and `waiting` (waiters of kind `lease`), with `read_at`. A failed read, or a gateway whose key is unset, is `unknown`, never free. An `openai` gateway has no status and is not listed
- Each account an agent's `limit` names (`claude`, `codex`), at most every `RECHECK_SECS` (600) in the same pass: the `session` and `week` windows with `read_at`. A failed read keeps the last reading; one never read is `unknown`. A Codex home the settings cannot give leaves `codex` unknown
- The reads block, so each runs off the dog's loop; the root builds the ports (`dog_ports`, `src/main.rs`) and the dog crate names only core's
