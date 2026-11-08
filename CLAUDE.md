# shep-kelpie

Kelpie is a shep dog that runs coding agents (Claude Code, pi, Codex, goose) from a planned work item to a merged pull request. MIT OR Apache-2.0.

## Layout

- Cargo workspace (ADR 0015). Crates live in `crates/`, named `shep-kelpie-<part>`. The root package holds `main.rs` and the verbs, and is the composition root (`src/sheep/mod.rs`). Nothing depends on it.
- Bottom up: the leaves (`process`, `facts`, `tree`, `owner_only`, `disk`, `worker_folder`, `trim`), then `core` (vocabulary, settings, ports), `protocol`, `client`, `dog`, `adapters`, `runner`.
- A crate uses only crates below it. A dev-dependency never points at a crate that depends on it.
- Every crate is `#![forbid(unsafe_code)]`, except `tree`, `owner_only` and `disk`, which allow it in one Windows module.
- Test rigs sit in the lowest crate that can hold them, behind its `test-support` feature. `src/test.rs` gathers them for the verbs' tests. Core's tests that drive a `Runner` live in `crates/runner/src/runner/core_tests/`.
- The root re-exports each module under its own name, so `cargo test --lib <module path>` works from the root. Bare `cargo fmt --all`, `clippy` and `test` cover every member, and `-p <package>` names one.

## Docs

- `docs/design/`: every current decision. Start at its README, then the area index (`files-*.md`).
- `docs/design-log.md`: history and the test plan. Search it.
- `docs/facts/`: measured facts. `docs/specs/`: test series and results.
- `docs/adr/`: hard-to-reverse decisions. Reopening one needs an argument. Its README lists rejected options.
- `GLOSSARY.md`: use its terms exactly. Kelpie is a dog, a project a sheep, a worker a lamb.

## Shep

- Built on the published `shep-client`, `shep-core` and `shep-channel`. Anything any dog could use goes to shep-pm/shep.
- `SHEP_HOME` is `~/.kelpie/shep`: macOS caps the socket path at 104 bytes.
- `shep trigger <sheep> <action> [params]` takes params positionally.
- Headless Claude Code traps: `docs/facts/headless-claude-code.md`.

## Code

- Load the `rust-house-style` skill before writing or reviewing Rust. Read `CODING_STANDARDS.md` when reviewing.
- Before handing over: `cargo fmt --all`, `cargo clippy --all-targets --locked -- -D warnings`, and the tests for what changed (`cargo test --lib <module path>` or `--test <name>`). CI runs the rest.
- One cargo command shape per session, one cargo command at a time.
- Before pushing and after merging `main`, run:
  - `scripts/file-size.sh`: no `.rs` file over 1000 lines
  - `python3 scripts/doc-length.py`: comment blocks within IR-47 (`--update` when one shrinks)
  - `python3 scripts/settings-count.py`: a new settings key needs a reason the project manager cannot decide it
  - `scripts/ticket-port.sh`: tickets go through `ports.tickets`, never `ports.forge` (ADR 0014)
- A test that reads Markdown outside the code folders lists it in `scripts/ci-changes.sh`.
- macOS and Windows CI run only on `main`, so a pull request never waits on them.

## Working an issue

- Read the issue, spec #5, `GLOSSARY.md`, `docs/adr/` and `docs/design/` first. Ask only for a decision they leave open.
- Tests drive the runner through its stand-ins, the dog's lease book with a fake clock, and adapters against recorded output. Assert only what the outside world sees.
- A worker's sandbox binds no TCP port, and on macOS binds Unix sockets only in its scratch folder. `docs/agents/sandbox-red-tests.md` lists the tests that fail there. Say in the pull request which tests only CI ran.
- A change to one adapter (`crates/adapters/src/adapters/{claude,pi,codex,goose}*`) or to code that branches on `AgentHarness` reaches pi in the same pull request, or names the open pi issue. Codex and goose may follow.
- New measured facts go in `docs/facts/`. A changed decision updates `docs/design/`.
- Done when every acceptance criterion is met, with the evidence in the pull request body, which ends `Resolves #<n>`.

## Git

- Public repo: say "the maintainer" and use repo-relative paths.
- Commit subjects and pull request titles: `type(scope): summary`, types `feat` `fix` `perf` `refactor` `docs` `test` `ci` `chore` `style`, `!` when breaking. Bodies carry the reasoning. `.githooks/commit-msg` checks them.
- One commit per item. Merge commits, never squash. Everything reaches `main` through pull requests.
- release-plz writes `CHANGELOG.md`. Never edit it by hand.
- Issues: `gh` on `shep-pm/shep-kelpie` (`docs/agents/issue-tracker.md`). Labels: `docs/agents/triage-labels.md`.
