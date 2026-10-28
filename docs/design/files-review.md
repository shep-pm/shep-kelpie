# Design: Review, merge and the forge

Part of the design index, `README.md`. Read the rows for the code you touch, then those files.

| file | covers | touch (`core/`: `crates/core/src/`, `dog/`: `crates/dog/src/dog/`, `adapters/`: `crates/adapters/src/`, `runner/`: `crates/runner/src/runner/`) |
|---|---|---|
| review.md | review, bots, rulings | `runner/{review,ruling}/`, `core/webhook.rs` |
| review-outcomes.md | fix turn outcomes | `runner/review/outcomes/` |
| local-review.md | local reviewers, kept rounds | `runner/review/keep.rs`, `core/settings/{local_rounds,local}.rs`, `adapters/local/` |
| merge.md | CI, catch-up, merge | `runner/{gate,merge}/`, `runner/close_merged.rs` |
| main-ci.md | red on `main` | `runner/main_ci/` |
| stuck-checks.md | stuck checks | `runner/gate/queued/` |
| github-app.md | App tokens, posting, answers | `runner/voice/`, `core/guard/gh.rs`, `worker_folder/` |
| who-answers.md | who answers | `runner/pm/rulings.rs` |
| contributions.md | others' pull requests | `core/{settings/{contributions.rs,reviewers/},state/contributions.rs}`, `runner/contributions/` |
| contribution-security.md | its security review | `runner/{contributions/,flight/security.rs}`, `process/worktree/` |
| contribution-pass.md | reviewers' pass, green head | `runner/contributions/pass.rs`, `process/worktree/` |
| contribution-talk.md | contributors' words, quiet clock | `runner/contributions/{heard,clock}.rs`, `runner/thread_replies/` |
| contribution-push.md | a fork worker's push; red-CI comment | `runner/contributions/{push,after}.rs` |
| contribution-release.md | asking for a release: its answers | `runner/contributions/{release,ruling}.rs` |
| contribution-merge.md | merge ruling: yes, thanks, no | `runner/contributions/{merge,ruling}.rs`, `core/settings/rulings.rs` |
| representative.md | agent file, `agents.representative`, post wording | `runner/representative/`, `core/{agents/front,settings/agents}/`, `agents/representative.md` |
| dependencies.md | bot bumps: rebase, proof, merge, red | `core/{settings/dependencies,state/bumps}.rs`, `runner/{dependencies,dev_server/bump,flight/proving.rs}`, `adapters/{e2e_runner.rs,pages.rs,bridge/client/}` |
