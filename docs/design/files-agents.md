# Design: Agents and their sandbox

Part of the design index, `README.md`. Read the rows for the code you touch, then those files.

| file | covers | touch (`core/`: `crates/core/src/`, `dog/`: `crates/dog/src/dog/`, `adapters/`: `crates/adapters/src/`, `runner/`: `crates/runner/src/runner/`) |
|---|---|---|
| agents.md | harnesses, gateways | `adapters/`, `core/settings/agents/`, `process/spawn.rs` |
| agent-files.md | agent file, `extends`, baselines | `core/agents/`, `src/flock/add/` |
| agent-migration.md | `agents migrate` | `core/agents/legacy.rs` |
| agent-allow.md | allow list | `core/settings/agents/`, `runner/add.rs` |
| agent-board.md | pick and fit estimates, the dog's board, `pick` and `fit` asks | `core/agent_board/`, `dog/{board,door}/`, `core/lease/door_pick.rs` |
| agent-board-runner.md | dispatch's ask, rework, handoff | `runner/add.rs`, `runner/dispatch/`, `runner/turn/handoff/` |
| cli-adapter.md | a new CLI | `adapters/cli/` |
| skills.md | step skills, copies, wording skill | `core/skills/*.rs` |
| plugins.md | repo plugins: sources, copies, loading, MCP | `core/skills/repo/`, `adapters/{skills,claude/mcp,pi/skills,codex/skills,goose/skills}.rs` |
| fence.md | sandbox, guard, hooks | `core/{profile,guard,hooks}/` |
| fence-git.md | a worker's git: worktree, clone | `core/profile/`, `adapters/harness/policy.rs` |
| fence-reads.md | reads | `core/dep_sources.rs`, `adapters/harness/`, `runner/turn/` |
| fence-reach.md | hosts | `core/package_managers/` |
| browser.md | a worker's browser: bridge, filter, tools | `core/{bridge,browse,settings/browser}.rs`, `adapters/bridge/`, `runner/browser.rs`, `process/tools/` |
| browser-proxy.md | the browser's proxy | `adapters/{bridge/proxy/,forwarder/http.rs}` |
| browser-dev-server.md | a worker's dev server | `runner/dev_server/`, `core/ports/dev_server.rs`, `adapters/dev_server/` |
| windows.md | `sh`, `env`, test fakes | `process/spawn.rs`, `test_support/` |
| windows-crates.md | crates on Windows | `owner_only/` |
