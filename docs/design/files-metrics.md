# Design: Pacing, spend and metrics

Part of the design index, `README.md`. Read the rows for the code you touch, then those files.

| file | covers | touch (`core/`: `crates/core/src/`, `dog/`: `crates/dog/src/dog/`, `adapters/`: `crates/adapters/src/`, `runner/`: `crates/runner/src/runner/`) |
|---|---|---|
| budget.md | pacing, dog + leases, time, usage, settings | `runner/pace.rs`, `dog/`, `core/{usage,settings}/`, `src/sheep/mod.rs` |
| waits-and-spend.md | waits, held-up turns, failing steps, spend | `runner/{turn/unreachable,looks}/`, `adapters/{unreachable,codex}/`, `core/usage/` |
| log-lines.md | log lines | `runner/{drain,skip_log,claim}`, `runner/review_bot/lease_wait.rs`, `dog/{desk.rs,door/}` |
| metrics.md | kelpie's series, alerts, dashboard | `core/metrics/`, `core/ports/metrics.rs`, `assets/{prometheus,grafana}/` |
| metrics-work-items.md | work-item series, `finished` line | `core/usage/finished.rs`, `runner/ci_failure/` |
| metrics-dog.md | leases, summons, forge | `core/metrics/{dog,forge,model_lease}.rs`, `dog/watch/` |
| metrics-transcripts.md | a call's transcript: MCP, tools, `usage` | `crates/facts/`, `runner/transcript_count/`, `core/usage/` |
