# Architecture decision records

Each ADR below is one decision that is hard to reverse. A later ADR can reopen part of an earlier one, and says so in its own text.

## Decisions

- 0001 [Kelpie stands on shep, not in it](0001-kelpie-stands-on-shep-not-in-it.md): kelpie is its own repo built on the published shep crates, and a generic gap becomes a shep issue.
- 0002 [Projects are sheep, workers are lambs](0002-projects-are-sheep-workers-are-lambs.md): each project runs as one long-lived sheep and its workers are that sheep's lambs.
- 0003 [Runners live in the maintainer's own shepherd](0003-runners-live-in-the-maintainer-s-own-shepherd.md): runners and the dog are sheep of the maintainer's own shepherd, and kelpie is adopted as a dog.
- 0004 [The adopted kelpie is the lease dog](0004-the-adopted-kelpie-is-the-lease-dog.md): the adopted `kelpie` holds the leases through the shepherd channel shep 0.12 gives it, and `kelpie-dog` is gone.
- 0005 [shep stops every lamb](0005-shep-stops-every-lamb.md): shep's stop reaches every lamb in the tree, and kelpie drops its own stop ladder, orphan sweep and pid files.
- 0006 [One agent builds, reviewers run once in series](0006-one-agent-builds-reviewers-run-once.md): the issue says what done means, one agent builds it, and each listed reviewer reads once with one fix turn, with no loop and no judge.
- 0007 [Review rules for a live pull request](0007-review-rules-for-a-live-pull-request.md): nits and a merge ruling's `no` get a fix turn, a late bot is read, and a parked item frees its slot.

## Decided against

Check this list before proposing an option. Where a later ADR reopened the decision, the line says so.

- adopt Paperclip (0001)
- build on Vibe Kanban (0001)
- a third flock category, or per-project "puppies", inside shep (0001)
- standalone with no shepherd (0001)
- workers as sheep, one fold per project (0002)
- keep kelpie's own shepherd (0003)
- the adopted dog holds the leases (0003, taken later by 0004)
- the dog's sheep named `kelpie`, beside the disabled adopted dog (0003)
- keep `kelpie-dog` (0004)
- every agent session a sheep (0005)
- drop kelpie's process groups (0005)
- keep kelpie's stop ladder and make it complete (0005)
- keep the deep round (0006)
- keep the review loop with a better prompt (0006)
- keep the planning call, off by default (0006)
- a model re-checks each fix (0007, and the deep round's re-check session in 0006)
- keep holding nits back from the worker (0007)
- raise `max_items` instead of freeing a parked item's slot (0007)
