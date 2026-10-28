# Design

Kelpie: the maintainer's dog; one runner per project builds ready issues. Asks only via rulings.

## Always

- Gates, locks, rate windows live in code; Claude only for work, judgement. ADR 0001
- Kelpie controls each agent's context growth. `runner/turn/`
- Naming: dog = adopted `kelpie` (lease dog); project = sheep; agent calls = lambs. One kelpie for all projects. ADR 0001 to 0004
- Binary is the dog with no args + `SHEP_DOG_NAME` and `SHEP_NAME` set; needs the `shep adopt` channel; names itself in the handshake, never `silent`. `src/sheep/mod.rs`
- Needs shep 0.17.0+ (ADR 0016); `upgrade` refuses builds it would. shep-pm/shep#688, #319, #656, #914
- Home: `$SHEP_HOME/kelpie` (`KELPIE_HOME`), shep plugin, folder per project; the dog's files stay under `$SHEP_HOME`. #253
- The shepherd is reached through the `Dial` port (`core/shepherd/connection.rs`): `Socket` is the real one; tests dial `FakeShepherd`. #587
- Socket path over 103 bytes: no start. #1147
- Home never moved from `~/.kelpie`: no start, names each old file, exit 78 (`stop_exit_codes`). #311, #345
- State file version 91 (`VERSION` in `core/state/mod.rs`); each change one way: rollback = finish or drop open items. `core/state/removed/mod.rs`
- Business code is Rust; Tauri GUI after MVP. #5
- A runner serves the typed protocol on `run/<project>.sock`; verbs fall back to `shep trigger`. ADR 0017. runner-socket.md
- Crates under `crates/` use nothing above them; the root holds the binary, verbs (`src/{flock,upgrade,sheep,doctor,issues}/`, `src/schema.rs`). ADR 0015
- No budget numbers in agent prompts; no worker merges/posts as the maintainer (fence.md, github-app.md).

## Files

| file | covers | touch (`core/`: `crates/core/src/`, `dog/`: `crates/dog/src/dog/`, `adapters/`: `crates/adapters/src/`, `runner/`: `crates/runner/src/runner/`) |
|---|---|---|
| work.md | projects, slots, issues | `src/issues/check.rs`, `runner/slots.rs`, `src/flock/verbs.rs`, `core/board/check.rs` |
| board.md | board, items, adoption | `core/board/`, `runner/{dispatch,adopt}/` |
| pm.md | the project manager | `runner/pm/` |
| pm-board.md | its board | `core/board/briefing/`, `runner/briefing/` |
| workers.md | runner loop, turns | `runner/{turn,flight}/` |
| runner-lock.md | lock, hold metric | `runner/{lock_hold,off_lock}.rs` |
| runner-lock-passes.md | queued verbs, threads | `runner/{queued,board_read,trigger}` |
| runner-socket.md | socket, client, fallback | `runner/socket/`, `crates/client/`, `src/flock/control/socket.rs` |
| runner-lock-worktrees.md | worktree jobs | `runner/worktree_jobs.rs` |
| fence.md | sandbox, guard, hooks | `core/{profile,guard,hooks}/` |
| fence-reads.md | reads | `core/dep_sources.rs`, `adapters/harness/`, `runner/turn/` |
| fence-reach.md | hosts | `core/package_managers/` |
| browser.md | a worker's browser: bridge, filter, tools | `core/{bridge,browse,settings/browser}.rs`, `adapters/bridge/`, `runner/browser.rs`, `process/tools/` |
| browser-proxy.md | the browser's proxy | `adapters/bridge/proxy/`, `adapters/forwarder/http.rs` |
| browser-dev-server.md | a worker's dev server | `runner/dev_server/`, `core/ports/dev_server.rs`, `adapters/dev_server/` |
| compaction.md | when a session compacts, `COMPACT` | `runner/compact/`, `adapters/{pi/compact,goose/compact,codex/context}.rs` |
| skills.md | step skills, copies, wording skill | `core/skills/*.rs` |
| plugins.md | repo plugins: sources, copies, loading, MCP | `core/skills/repo/`, `adapters/{skills,claude/mcp,pi/skills,codex/skills,goose/skills}.rs` |
| agent-files.md | agent file, `extends`, baselines | `core/agents/`, `src/flock/add/` |
| agent-migration.md | `agents migrate` | `core/agents/legacy.rs` |
| agent-allow.md | allow list | `core/settings/agents/`, `runner/add.rs` |
| agents.md | harnesses, gateways | `adapters/`, `core/settings/agents/`, `process/spawn.rs` |
| cli-adapter.md | a new CLI | `adapters/cli/` |
| handover.md | in-flight turns handed on | `src/sheep/handover/`, `runner/flight/carry/`, `adapters/process/adopt/`, `src/upgrade/handover.rs` |
| windows.md | `sh`, `env`, test fakes | `process/spawn.rs`, `test_support/` |
| windows-crates.md | crates on Windows | `owner_only/` |
| waits-and-spend.md | waits, held-up turns, failing steps, spend | `runner/{turn/unreachable,looks}/`, `adapters/{unreachable,codex}/`, `core/usage/` |
| review.md | review, bots, rulings | `runner/{review,ruling}/`, `core/webhook.rs` |
| review-outcomes.md | fix turn outcomes | `runner/review/outcomes/` |
| local-review.md | local reviewers, kept rounds | `runner/review/keep.rs`, `core/settings/{local_rounds,local}.rs`, `adapters/local/` |
| merge.md | CI, catch-up, merge | `runner/{gate,merge}/`, `runner/close_merged.rs` |
| main-ci.md | red on `main` | `runner/main_ci/` |
| stuck-checks.md | stuck checks | `runner/gate/queued/` |
| local-tickets.md | local ticket store, `ticket` verbs | `core/local_tickets/`, `src/flock/ticket.rs`, `runner/close_merged.rs` |
| github-app.md | App tokens, posting, answers | `runner/voice/`, `core/guard/gh.rs`, `worker_folder/` |
| who-answers.md | who answers | `runner/pm/rulings.rs` |
| contributions.md | others' pull requests | `core/{settings/{contributions.rs,reviewers/},state/contributions.rs}`, `runner/contributions/` |
| contribution-security.md | its security review | `runner/contributions/`, `runner/flight/security.rs`, `process/worktree/` |
| contribution-pass.md | reviewers' pass, green head | `runner/contributions/pass.rs`, `process/worktree/` |
| contribution-talk.md | contributors' words, quiet clock | `runner/contributions/{heard,clock}.rs`, `runner/thread_replies/` |
| contribution-push.md | a fork worker's push; red-CI comment | `runner/contributions/{push,after}.rs` |
| contribution-release.md | asking for a release: its answers | `runner/contributions/{release,ruling}.rs` |
| contribution-merge.md | merge ruling: yes, thanks, no | `runner/contributions/{merge,ruling}.rs`, `core/settings/rulings.rs` |
| representative.md | agent file, `agents.representative`, post wording | `runner/representative/`, `core/{agents/front,settings/agents}/`, `agents/representative.md` |
| dependencies.md | bot bumps: rebase, proof, merge, red | `core/{settings/dependencies,state/bumps}.rs`, `runner/{dependencies,dev_server/bump,flight/proving.rs}`, `adapters/{e2e_runner.rs,pages.rs,bridge/client/}` |
| triage.md | settings, triager, pick rule, `since`, the verb, `status` | `core/settings/triage.rs`, `runner/{triage,open_checks.rs}`, `core/ports/issue.rs` |
| triage-limits.md | limits | `runner/triage/limits.rs` |
| triage-pass.md | a pass: post, labels, duplicate, agent-ready | `runner/{triage,flight}/`, `core/ports/voice.rs` |
| triage-images.md | an issue's images | `runner/triage/images.rs`, `adapters/gh/attachment.rs` |
| triage-author.md | author's round: ask, answers, silence | `runner/triage/{ask,heard}.rs` |
| triage-local.md | local session | `runner/triage/local.rs` |
| triage-spam.md | the spam check | `runner/triage/spam.rs` |
| triage-reader.md | who a comment is for | `runner/triage/facts.rs` |
| triage-hand.md | `hand_ready` take-back | `runner/triage/hand.rs` |
| triage-check.md | the check on every issue | `runner/triage/checks.rs` |
| log-lines.md | log lines | `runner/{drain,skip_log,claim}`, `runner/review_bot/lease_wait.rs`, `dog/{desk.rs,door/}` |
| metrics.md | kelpie's series, alerts, dashboard | `core/metrics/`, `core/ports/metrics.rs`, `assets/{prometheus,grafana}/` |
| metrics-work-items.md | work-item series, `finished` line | `core/usage/finished.rs`, `runner/ci_failure/` |
| metrics-dog.md | leases, summons, forge | `core/metrics/{dog,forge,model_lease}.rs`, `dog/watch/` |
| metrics-transcripts.md | a call's transcript: MCP, tools, `usage` | `crates/facts/`, `runner/transcript_count/`, `core/usage/` |
| budget.md | pacing, dog + leases, time, usage, settings | `runner/pace.rs`, `dog/`, `core/usage/`, `core/settings/`, `src/sheep/mod.rs` |

Each file is under 8 KB; over it, split, add a row. A changed decision updates its bullet.

Elsewhere: `docs/design-log.md` (history, old formats), `docs/facts/`, `docs/adr/`, `GLOSSARY.md`.
