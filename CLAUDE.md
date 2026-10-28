# shep-kelpie

Kelpie is a shep dog that runs Claude Code workers from a planned work item to a merged pull request, holding gates, leases and pacing in code. MIT OR Apache-2.0.

- A Cargo workspace (ADR 0015): the root package `shep-kelpie`, the binary, and the crates under `crates/`, each `#![forbid(unsafe_code)]` (`tree`, `owner_only` and `disk` deny it and allow it in their one Windows module) and using only what is below it. So far `process` (`spawn`, `terminal`, `tools`, `worktree`), `facts` (`mcp_use`, `request_use`, `tool_error`), `tree` (`tree`, `interrupt`: a process tree stopped as one, a process group on Unix and a job object on Windows, and the signals that end kelpie), `owner_only` (`owner_only`: a folder or file only its owner can use and the check that one is, mode bits on Unix and a protected DACL on Windows), `disk` (`disk`: free space and a folder's allocated bytes on Unix and Windows, counting hard links once), `worker_folder`, `trim`, `core` (the vocabulary, settings and ports, with the stand-ins that name only them behind its `test-support` feature), `protocol` (`frame`, `request`, `reply`, `status`: the runner's wire types, using only `core`, serde and serde_json), `client` (the runner's socket from the asking side, using `protocol` and `core`), `dog` (the lease dog, using only `core`), `adapters` (the real ports and the forge, `adapters` and `github`, using only `core` and the leaves), `runner` (the work-item loop and the rig, `Rig`, behind its `test-support` feature, using `core`, the leaves and `protocol`) and the dev-only `test_support`. The root package holds `main.rs` and the verbs (`flock`, `upgrade`, `sheep`, `doctor`, `issues`, `schema`), depends on every crate, and is the composition root: `src/sheep/mod.rs` builds the real adapters and hands them to the runner. Nothing depends on the root. A package is the repo's name plus `-<part>`. Core's tests that drive a `Runner` or name an adapter live in `crates/runner/src/runner/core_tests/`.
- A module keeps its name in its crate and the root re-exports it, so `crate::spawn` and `cargo test --lib <module path>` hold from the root. `default-members` names every member, so a bare `cargo fmt --all`, `clippy` or `test` covers them. To name one member, `-p <package>`.
- The build enforces three rules. A crate may use what its line above names. A dev-dependency never points at a crate that depends on the crate, since that builds two copies of it and its own types stop matching. The rig lives in the lowest crate that can hold each part, behind that crate's `test-support` feature, and the root's `src/test.rs` gathers it for the verbs' tests.
- The design is settled and the first build (#5) is done. Kelpie runs the maintainer's projects, this one included, one issue per worker at a time.

## Docs

- `docs/design/`: every current decision, its reason and where it lives. Read its README, then the area files for the code you touch.
- `docs/design-log.md`: the history and mechanics behind those decisions, and the test plan. Search it.
- `docs/facts/`: measured facts, one file per topic.
- `docs/specs/`: one spec per test series, with results. Experiment code and raw results live in shep-pm/kelpie-lab, cited as "the experiments repo".
- `docs/adr/`: hard-to-reverse decisions. Contradicting one means naming it and arguing to reopen it. Its README lists every rejected option: check it before proposing one.
- `GLOSSARY.md`: use its terms exactly in prose, code and issue titles (worker, crew, work item, phase, reset, handoff, lease, summon, ruling). In shep's terms kelpie is a dog, a project is a sheep, a worker is a lamb.

## Shep

- Kelpie builds on the published `shep-client`, `shep-core` and `shep-channel`, on its own release cycle.
- Anything any dog could use goes to shep-pm/shep as a general feature (like shep-pm/shep#623 and #624).
- Keep `SHEP_HOME` short: macOS caps the `$SHEP_HOME/run/shep.sock` path at 104 bytes. Kelpie uses `~/.kelpie/shep`.
- `shep trigger <sheep> <action> [params]` takes params positionally.
- Headless Claude Code traps (stdin, cumulative cost, cache sharing): `docs/facts/headless-claude-code.md`.

## Code and checks

- Invoke the `rust-house-style` skill before writing or reviewing Rust (shep-pm/rust-house-style, IR-1 to IR-48). Read `CODING_STANDARDS.md` when reviewing.
- CI (`.github/workflows/test.yml`): `cargo fmt --all --check`, `cargo clippy --all-targets --locked -- -D warnings`, `cargo test --locked` in three parts (`scripts/test-part.sh`), rustdoc with `-D warnings`, `cargo +1.88 check` (MSRV), and a non-blocking `windows` job that runs `cargo check --workspace --all-targets --locked --keep-going` while #241 ports kelpie. A pull request that changes only Markdown skips the Rust jobs. A test that reads a Markdown file outside the code folders lists it in `scripts/ci-changes.sh`.
- Before handing a branch over: fmt, clippy and the tests for what changed (`cargo test --lib <module path>`, or `--test <name>`). Leave the full suite (about six minutes), rustdoc and the MSRV check to CI.
- One cargo command shape per session, one cargo command at a time.
- No `.rs` file over 1000 lines (IR-48, `.github/workflows/file-size.yml`). Run `scripts/file-size.sh` before pushing and after merging `main`.
- No comment block over IR-47's line limits: `///` 6 lines after the summary (12 with `# Errors`), `//!` 10, `//` 4 in a row (`.github/workflows/doc-length.yml`). Blocks over today are listed in `.github/doc-length-baseline.txt`: one may shrink, none may be added, and one that now fits comes off the list (`python3 scripts/doc-length.py --update` rewrites it). Run `python3 scripts/doc-length.py` before pushing and after merging `main`.
- A ticket is read and written through `ports.tickets`, never `ports.forge` (ADR 0014). Run `scripts/ticket-port.sh` before pushing.

## Working an issue

These hold for a kelpie worker too.

- First read the issue, its parent spec #5, `GLOSSARY.md`, `docs/adr/` and `docs/design/`. Ask only for a decision they leave open.
- Tests follow #5: the runner's work-item loop through its stand-ins, the dog's lease book with a fake clock, adapter parsing against real recorded output. Assert only what the outside world sees.
- A worker's sandbox binds no TCP port, and on macOS binds Unix sockets only in its own scratch folder. `docs/agents/sandbox-red-tests.md` lists the tests that fail there and pass in CI. Say in the pull request which tests only CI ran.
- Workers run on Claude Code, pi and Codex. A change to one adapter (`crates/adapters/src/adapters/claude*`, `pi*`, `codex*`) or to code that branches on `AgentHarness` reaches pi in the same pull request, or names the open pi issue that does. Codex may follow in its own issue. See `CODING_STANDARDS.md`.
- New measured facts go in their `docs/facts/` file (or a new one listed in its README) in the same pull request. A changed decision updates `docs/design/` too.
- Done when every acceptance criterion is met, with the evidence in the pull request body, which ends `Resolves #<n>`.

## Writing and git

- The repo is public: say "the maintainer" and use repo-relative paths.
- Conventional commit subjects, `type(scope): summary`, types `feat` `fix` `perf` `refactor` `docs` `test` `ci` `chore` `style`, `!` on a breaking commit. Bodies carry the full reasoning.
- `.githooks/commit-msg` checks locally (`git config core.hooksPath .githooks` once per clone), `.github/workflows/commits.yml` checks every pull request.
- Pull request titles are conventional too, and become the merge commit's subject.
- Merge commits, never squash. Release pull requests squash themselves.
- One commit per item. Everything reaches `main` through pull requests.
- release-plz writes `CHANGELOG.md` from commit subjects, so never hand-write an entry. Off until the maintainer sets `RELEASES` to `on`.

## Agent skills

- Issue tracker: GitHub Issues on `shep-pm/shep-kelpie` via `gh`. See `docs/agents/issue-tracker.md`.
- Triage labels: the five default roles, label string equal to role name. See `docs/agents/triage-labels.md`.
- Domain docs: single-context, the files under Docs above. See `docs/agents/domain.md`.
