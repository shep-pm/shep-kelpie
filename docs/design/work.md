# Work

Projects, slots, the issue check. The board and work items are in `board.md`, the project manager in `pm.md` and its board in `pm-board.md`.

## Projects

- `shep kelpie add`: registers stopped runner + `[app.dogs.kelpie]` table; takes `--base`, writes `git.base`; reads all, then makes only what's missing. `src/flock/verbs.rs`
- `add` refuses a checkout or repo another project's table names, even a broken one (two runners, same issues). `src/flock/verbs.rs`
- Project runs exactly while its runner's sheep runs; no paused state. #268
- `finish`: open items end, nothing new picked, then stops the sheep; `finishing` saved so restart keeps finishing. `add`, `adopt`, `rework` refused meanwhile; flag cleared just before stop. #303
- `get [<key>]` prints the project's table or one dotted key; `set <key> <value>` reads the value as TOML (else a string), checks the whole result as a runner start and reread do (settings, agents, reviewers, rulings, gateway shapes with no key read, skills; and for a moved `git.checkout` or `git.remote`, the checkout and `add`'s one-runner-per-checkout-and-repo rule), and only then writes the table back; refusals write nothing. A table written another way (lookout, `shep copy`) gets the same checks at runner start (refuses, exit 78) and reread, and a refusal alerts. Says whether a running runner takes the change at its next wake or start. Not `[kelpie]`. #273, #965, `src/flock/set/mod.rs`
- `start`, `pause`, `finish` touch only sheep kelpie started as `runner <project>` (never start one the maintainer stopped); `start` waits 30s for JSON `status`. `src/flock/verbs.rs`
- `pause`: drain, then wait out calls and any merge (up to 1h), then stop sheep (stop mid-merge can leave a merged PR's item open); a wait that runs out or fails sends `undrain keep`, which leaves the runner running and the turns the pause ended unstarted for 10 min (`PAUSE_HOLD`), so a retried `pause` finds them stopped; a bare `undrain` starts them at once. A Claude Code or pi worker turn ends at its next tool call (workers.md); `pause` names those items and waits 10 min for it (a full test run is ~6 min), then stops the call as `shep stop` does. Other calls wait ceiling + 5 min. #268, #821, #1328

## Slots

- `concurrency.active_items` (1): a project's slots, each item own branch + worktree (a parked PR must not hold the project). #103, `runner/slots.rs`
- Slot bounds model calls only: parked or `Done` item frees it, model-free phases slotless; one needing a model again goes before new work. A call on a followed contribution takes one too, before new work (contribution-security.md). #355, #621, #1162, ADR 0007
- `concurrency.pending_rulings` (2): no new work while that many items parked (alerts can't open PR after PR); `add` ignores it. #355
- Issue naming a file a parked branch changes skipped as `overlap` (ruling may still change it). #355
- Resuming a parked session after 1h repays cache write; accepted. shep-pm/shep#707
- Bot window grant serves one item, never inherited (would summon twice in a window the dog counted once). #103

## Issues

- The first prompt of a work item names the paths its issue names that `main` lacks as "may have moved", with the closest file that ends the same way. One `git ls-tree`, read in place and only when the body names a path. A folder on `main` resolves, and a name with no `/` counts only with an extension files on `main` have. `runner/turn/mod.rs`, #1322
- The issue check reads structure, not quality: `board::check::check` over an issue's title, body and labels returns every part it lacks or holds malformed, each naming the part. The parts: a title; a `Touches:` line (any case, in a list, in bold or as `**Touches**:`, as every labelled line is read) naming a path in backticks, on it or in the list under it, or a `Touches` heading with such paths under it; `What to build` with text under it and `Acceptance criteria` with a `- [ ]` item, as headings at any level and case, closing `#`s and bold dropped, a section ending at the next heading of its level or above; a line in a code fence (three or more backticks or tildes, closed only by as many or more of the same) is in no section and is no labelled line; a `Harness reach:` line with text when a `Touches:` path has a part after `adapters/` named for `claude`, `pi`, `codex` or `goose`; a `Blocked by:` line, when there is one, naming `#<n>` or `k<n>`; one priority label (`board::is_priority_label`, so a bare `P2` too); a kind, its one `kind:<type>` label, else its title's conventional prefix, being one of #1379's nine types (`board::check::kinds`: the gh guard's list and parser, `guard::conventional_type`, less `build` and `revert`), two `kind:` labels refused and `bug`/`enhancement` not read. Sources: the triager's body shape, `CLAUDE.md`'s pi rule, `docs/agents/issue-tracker.md`'s `Blocked by:` line, `docs/agents/triage-labels.md` and ADR 0018. ADR 0018, `core/board/check.rs`
- `shep kelpie issue check <ticket>` reads the ticket through `ports.tickets` (`Routed`), prints that it passes or each failure, and exits 0 when it passes, 1 when it fails and 2 when it could not be checked. `check` as the first word after `issue`, wherever `-p` sits, is always the subcommand, and `verbs::main` runs it before asking for `SHEP_HOME`. `--draft <file> [--label <label>]...` checks a file, its first line the title, with no shepherd, settings or network, so it runs in a worker's sandbox, whose profile denies the shepherd's and kelpie's homes and reaches only the dog's door. `src/issues/check.rs`, `src/flock/verbs/check.rs`
- The issue skill (`skills.md`) holds the same format; its example passing the check, and its naming every part the check reads, are tested. `core/skills/issue/mod.rs`
- The board runs the check on each new or edited open issue, and the triager runs only on one that fails it: `triage-check.md`. ADR 0018

## Not built

- Overlapping items bundled when small, chained otherwise; today overlap shows as a conflict turn. `docs/design-log.md` Decided, not built
- Oversized work split into chained items by the worker; today the issue skill scopes each issue to one pull request first. ADR 0006
