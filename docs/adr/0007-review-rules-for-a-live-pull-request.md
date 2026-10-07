# Review rules for a live pull request

This amends ADR 0006. Its readers stand: each listed reviewer reads once, in order, with no loop and no judge. What it left open is what happens over time on a live pull request: bots that review late, windows, rulings, parked items. The experiments repo measured reviewers reading fixed snapshots, so none of that was measured, and shep-pm/shep#707 hit four of those cases on 2026-10-06:

- Three of cubic's P3 threads stayed open on a pull request the merge ruling called clear, because the nit rule (#314) holds nits back from the worker.
- cubic was passed over for its window, then reviewed the head on its own while CI ran, and nothing read its threads before the ruling.
- The maintainer's `no` asking for those three nits restarted the pass: four reads and 30 minutes for a nit fix.
- With `max_items = 1`, the item parked on the merge ruling held the only slot, and the project did nothing while it waited.

The maintainer decided on 2026-10-06, from the measured numbers already in hand:

- **Nits get a fix turn.** A round of nits goes to the worker like any round. A nit-only fix starts nothing new: no reviewer reads again because of it, and a bot's nits on that fix's head are not sent again (#354).
- **A fix for anything above a nit is checked by its test, not by a model.** Where a reviewer leaves a failing test, kelpie runs it before and after the fix, as ADR 0006 decided and did not build (#356). Otherwise the next reviewer's read stands, as before.
- **A merge ruling's `no <note>` is a fix turn.** Its fix goes to CI and back to the ruling. `rework <note>` restarts the pass, for a change that needs the full review (#354).
- **A bot that reviews late is read.** Its threads go to the worker, during CI or after the merge ruling (#351).
- **A parked item frees its slot.** Items parked on rulings do not count against `max_items`. `max_parked` caps them, so an unanswered alert cannot open pull request after pull request (#355).

## Considered options

- **A model re-checks each fix.** A closed question ("is finding X fixed?") cannot produce new nits, so it cannot loop. ADR 0006 removed the deep round's re-check session for this reason: it asks a model what kelpie can learn by running the test, and a test-backed finding was real 97% of the time.
- **Keep holding nits.** Nits held back stay as open threads on a pull request that reads as clear, and the maintainer reads that as a review that missed them. A nit fix costs about $0.20, and the rule that it starts nothing new is what keeps it from becoming the loop ADR 0006 removed.
- **Raise `max_items` instead.** It works today, but one number then sets both how many workers run and how many pull requests can wait, and a project with a low `max_items` for its usage still stalls on one ruling.

## Consequences

- The nit rule's entry in the design log (#314) and #103's slot rule are rewritten, not left beside the new rules.
- A merge ruling has three answers: `yes`, `no <note>` and `rework <note>`.
- A session resumed more than an hour after its last turn has lost its prompt cache and pays the cache write again, roughly $0.20 to $0.50 on #707's numbers. Freeing parked slots makes that more common, and it is accepted.
- Reviewers stay read-only until #356's design read decides how one leaves a test.
