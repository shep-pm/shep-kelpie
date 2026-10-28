# Design

Kelpie: the maintainer's dog; one runner per project builds ready issues. Maintainer asked only via rulings.

## Always

- Gates, locks, rate windows live in code; Claude only for work and judgement. ADR 0001
- Kelpie controls each agent's context growth. `runner/turn/`
- Naming: dog = adopted `kelpie` (lease dog); project = sheep; agent calls = lambs. One kelpie for all projects. ADR 0001 to 0004
- Binary is the dog with no args + `SHEP_DOG_NAME` and `SHEP_NAME` set; needs the `shep adopt` channel; names itself in the handshake, so never `silent`. `src/sheep/mod.rs`
- Needs shep 0.17.0+ (ADR 0016); `upgrade` refuses builds it would. shep-pm/shep#688, #319, #656, #914
- Home: `$SHEP_HOME/kelpie` (`KELPIE_HOME`), shep plugin, folder per project; the dog's files stay under `$SHEP_HOME`. #253
- The shepherd is reached through the `Dial` port (`core/shepherd/connection.rs`): `Socket` is the real one; tests dial `FakeShepherd`. #587
- Socket path over 103 bytes: runner won't start. #1147
- Home never moved from `~/.kelpie`: won't start, naming each old file, exit 78 (`stop_exit_codes`). #311, #345
- State file version 89 (`VERSION` in `core/state/mod.rs`); each change one way: rollback = finish or drop open items. `core/state/removed/mod.rs`
- Business code is Rust; Tauri GUI after MVP. #5
- Crates under `crates/` use nothing above them; the root holds the binary, verbs (`src/{flock,upgrade,sheep,doctor,issues}/`, `src/schema.rs`). ADR 0015
- No budget numbers in agent prompts; no worker merges/posts as the maintainer (fence.md, github-app.md).

## Files

| file | covers | touch (`core/` is `crates/core/src/`, `dog/` is `crates/dog/src/dog/`, `adapters/` is `crates/adapters/src/`, `runner/` is `crates/runner/src/runner/`) |
|---|---|---|
| work.md | projects, slots, issues | `src/issues/check.rs`, `runner/slots.rs`, `src/flock/verbs.rs`, `core/board/check.rs` |
| board.md | board, items, adoption | `core/board/`, `runner/{dispatch,adopt}/` |
| pm.md | the project manager, its board | `runner/{pm,briefing}/` |
| workers.md | runner loop, turns | `runner/{turn,flight}/` |
| runner-lock.md | lock, hold metric | `runner/{lock_hold,off_lock}.rs` |
| runner-lock-passes.md | queued verbs, board, threads | `runner/{queued,board_read,trigger}` |
| runner-lock-worktrees.md | worktree jobs | `runner/worktree_jobs.rs` |
| fence.md | sandbox, guard, hooks | `core/{profile,guard,hooks}/` |
| fence-reads.md | reads | `core/dep_sources.rs`, `adapters/harness/`, `runner/turn/` |
| fence-reach.md | hosts | `core/package_managers/` |
| browser.md | a worker's browser: bridge, filter, tools | `core/{bridge,browse}.rs`, `adapters/bridge/`, `runner/browser.rs`, `core/settings/browser.rs`, `process/tools/` |
| browser-proxy.md | the browser's proxy: origins, addresses, limits | `adapters/bridge/proxy/`, `adapters/forwarder/http.rs` |
| browser-dev-server.md | a worker's dev server: launch, port, readiness | `runner/dev_server/`, `core/ports/dev_server.rs`, `adapters/dev_server/` |
| compaction.md | when a session compacts, `compact_window`, `COMPACT` | `runner/compact/`, `adapters/{pi/compact,goose/compact,codex/context}.rs` |
| skills.md | step skills, the skill copy, the wording skill | `core/skills/*.rs` |
| plugins.md | repo plugins: sources, copies, harness loading, MCP | `core/skills/repo/`, `adapters/{skills,claude/mcp,pi/skills,codex/skills,goose/skills}.rs` |
| agent-files.md | agent file, `extends`, baselines | `core/agents/`, `src/flock/add/` |
| agent-migration.md | `agents migrate` | `core/agents/legacy.rs` |
| agents.md | harnesses, gateways | `adapters/`, `core/settings/agents/`, `process/spawn.rs` |
| cli-adapter.md | a new CLI | `adapters/cli/` |
| handover.md | in-flight turns handed to a new build, `upgrade` | `src/sheep/handover/`, `runner/flight/carry/`, `adapters/process/adopt/`, `src/upgrade/handover.rs` |
| windows.md | `sh`, `env`, test fakes on Windows | `process/spawn.rs`, `test_support/` |
| waits-and-spend.md | waits, held-up turns, failing steps, spend | `runner/{turn/unreachable,looks}/`, `adapters/{unreachable,codex}/`, `core/usage/`, `core/settings/agents/` |
| review.md | review, bots, rulings | `runner/{review,ruling}/`, `core/webhook.rs` |
| review-outcomes.md | fix turn findings' outcomes | `runner/review/outcomes/` |
| local-review.md | local reviewers, lock, kept rounds | `runner/review/keep.rs`, `core/settings/{local_rounds,local}.rs`, `adapters/local/` |
| merge.md | CI, catching up, merge, after it | `runner/{gate,merge}/`, `runner/{close_merged,main_ci}.rs` |
| stuck-checks.md | checks stuck on a finished run | `runner/gate/queued/` |
| local-tickets.md | the local ticket store, `ticket` verbs, closing on merge | `core/local_tickets/`, `src/flock/ticket.rs`, `runner/close_merged.rs` |
| github-app.md | App tokens, posting, answers | `runner/voice/`, `core/guard/gh.rs`, `worker_folder/` |
| who-answers.md | who answers | `runner/pm/rulings.rs` |
| contributions.md | others' pull requests: settings, detection | `core/settings/{contributions.rs,reviewers/}`, `runner/contributions/`, `core/state/contributions.rs` |
| contribution-security.md | a contribution's security review | `runner/contributions/`, `runner/flight/security.rs`, `process/worktree/` |
| contribution-pass.md | reviewers' pass over a green head | `runner/contributions/pass.rs`, `process/worktree/` |
| contribution-talk.md | what contributors say, answers, quiet clock | `runner/contributions/{heard,clock}.rs`, `runner/thread_replies/` |
| contribution-push.md | a fork worker's push; the red-CI comment | `runner/contributions/{push,after}.rs` |
| contribution-release.md | asking for a release: yes, thumbs up, silence, ruling | `runner/contributions/{release,ruling}.rs` |
| contribution-merge.md | a contribution's merge ruling: yes, thanks, no | `runner/contributions/{merge,ruling}.rs`, `core/settings/rulings.rs` |
| representative.md | agent file, `agents.representative`, post wording | `runner/representative/`, `core/agents/front/`, `core/settings/agents/`, `agents/representative.md` |
| dependencies.md | bot bumps: rebase, proof, merge, red | `core/settings/dependencies.rs`, `runner/{dependencies,dev_server/bump}/`, `runner/flight/proving.rs`, `core/state/bumps.rs`, `adapters/{e2e_runner,pages}.rs`, `adapters/bridge/client/` |
| triage.md | settings, triager, pick rule, `since`, the verb, `status` | `core/settings/triage.rs`, `runner/triage/`, `runner/open_checks.rs`, `core/ports/issue.rs` |
| triage-limits.md | limits | `runner/triage/limits.rs` |
| triage-pass.md | a pass: post, labels, duplicate, agent-ready | `runner/{triage,flight}/`, `core/ports/voice.rs` |
| triage-images.md | an issue's images | `runner/triage/images.rs`, `adapters/gh/attachment.rs` |
| triage-author.md | author's round: ask, answers, silence | `runner/triage/{ask,heard}.rs` |
| triage-local.md | local session | `runner/triage/local.rs` |
| triage-spam.md | the spam check | `runner/triage/spam.rs` |
| triage-reader.md | who a comment is for | `runner/triage/facts.rs` |
| triage-hand.md | `hand_ready` take-back | `runner/triage/hand.rs` |
| triage-check.md | the check on every issue | `runner/triage/checks.rs` |
| log-lines.md | runner and dog log lines | `runner/drain/`, `runner/{skip_log,claim}.rs`, `runner/review_bot/lease_wait.rs`, `dog/desk.rs`, `dog/door/` |
| metrics.md | kelpie's series, alerts, dashboard | `core/metrics/`, `core/ports/metrics.rs`, `assets/{prometheus,grafana}/` |
| metrics-work-items.md | work-item series, `finished` line | `core/usage/finished.rs`, `runner/ci_failure.rs` |
| metrics-dog.md | leases, summons, forge | `core/metrics/{dog,forge,model_lease}.rs`, `dog/watch/` |
| metrics-transcripts.md | a call's transcript: MCP, tools, `usage` | `crates/facts/`, `runner/transcript_count/`, `core/usage/` |
| budget.md | pacing, dog + leases, time, usage, settings | `runner/pace.rs`, `dog/`, `core/usage/`, `core/settings/`, `src/sheep/mod.rs` |

Each file stays under 8 KB; over it, split in two and add a row here. A changed decision updates its bullet.

Elsewhere: `docs/design-log.md` (history, old file formats), `docs/facts/`, `docs/adr/`, `GLOSSARY.md`.
