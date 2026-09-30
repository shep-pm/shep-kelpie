# shep-kelpie

Kelpie is a shep dog that runs Claude Code workers from a planned work item to a merged pull request, holding gates, leases and pacing in code. MIT OR Apache-2.0, private until it is ready.

## Where things stand

The design is settled and the pillars are measured. The MVP is being built from #5 (one project, one worker, merge on `ask`, no GUI), one ticket at a time. The code is one Rust crate, `shep-kelpie`, at the repo root, with `#![forbid(unsafe_code)]`.

- `docs/design-log.md`: status, every decision, the measured facts and the test plan. Read it before proposing anything a test has not settled.
- `docs/specs/`: one spec per test series, and its results. The experiment code and raw results live in shep-pm/kelpie-lab, and results here cite its commits as "the experiments repo".
- `docs/adr/`: decisions that are hard to reverse. A change that contradicts one names the ADR and argues for reopening it.

## Vocabulary

`CONTEXT.md` is the vocabulary. Use its terms exactly, in prose, code and issue titles: worker, crew, work item, phase, reset, handoff, lease, summon, ruling. In shep's terms, kelpie is a dog, a project is a sheep, and a worker is one of its lambs.

## Kelpie and shep

Kelpie builds on the published shep crates (`shep-client`, `shep-core`, `shep-channel`) and ships on its own release cycle. When kelpie needs something from shep that any dog could use, file it on shep-pm/shep as a general feature, the way shep-pm/shep#623 and #624 were.

Gotchas:

- Keep a shepherd's `SHEP_HOME` short. Its control socket is `$SHEP_HOME/run/shep.sock`, and macOS caps a socket path at 104 bytes. Kelpie uses `~/.kelpie/shep`.
- `shep trigger <sheep> <action> [params]` takes params positionally.
- Headless Claude Code has its own traps (stdin, cumulative cost, cache sharing). They are under Facts in the design log.

## Code style

Invoke the `rust-house-style` skill before writing or reviewing Rust. The rules are shep-pm/rust-house-style, IR-1 to IR-48.

CI (`.github/workflows/test.yml`) runs `cargo fmt --all --check`, `cargo clippy --all-targets --locked -- -D warnings`, `cargo test --locked`, rustdoc with `-D warnings`, and `cargo +1.88 check` for the MSRV. Run the same before handing a branch over. `.github/workflows/file-size.yml` also fails any `.rs` file over 1000 lines (IR-48).

## Writing and git

- Committed text calls the maintainer "the maintainer" and uses repo-relative paths, because the repo goes public one day.
- Commit subjects are conventional: `type(scope): summary`, with types `feat` `fix` `perf` `refactor` `docs` `test` `ci` `chore` `style`, and `!` on the commit that breaks something. Bodies carry the full reasoning.
- One commit per item. Work reaches `main` through pull requests.

## Agent skills

### Issue tracker

GitHub Issues on `shep-pm/shep-kelpie`, via `gh`. See `docs/agents/issue-tracker.md`.

### Triage labels

The five default roles, label string equal to role name. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: `CONTEXT.md` at the root, ADRs in `docs/adr/`, and `docs/design-log.md`. See `docs/agents/domain.md`.
