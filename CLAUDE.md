# shep-kelpie

Kelpie is a shep dog that runs Claude Code workers from a planned work item to a merged pull request, holding gates, leases and pacing in code. MIT OR Apache-2.0, private until it is ready.

## Where things stand

The design is settled and nothing runs yet. The pillars get measured before any code, in this order: transport tests, model calibration, work-split tests, then the MVP (one project, one worker, merge on `ask`, no GUI).

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
