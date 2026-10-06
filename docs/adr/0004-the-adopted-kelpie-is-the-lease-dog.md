# The adopted kelpie is the lease dog

This reopens the dog's half of ADR 0003. That ADR ran the lease dog as a sheep named `kelpie-dog` and left the adopted `kelpie` disabled, because shep 0.10.1 and 0.11.0 gave an adopted dog no shepherd channel. It named the adopted dog holding the leases as the option to take up if shep ever gave adopted dogs a channel. shep 0.12 does (shep-pm/shep#656): a dog asks with `shep-channel: true` in its `--version` answer, `shep adopt` records the ask, and every start of the dog sets `channel` and `shutdown_with_message` (shep-pm/shep-kelpie#180).

So kelpie asks for the channel, and the adopted `kelpie`, started with no arguments, is the lease dog. Under ADR 0003 that start exited 2, so an enabled `kelpie` restart-looped and had to be left disabled.

## Considered options

- **Keep `kelpie-dog`.** Two things named for one dog, and the adopted one still restart-loops unless disabled by hand.
- **The adopted dog holds the leases.** Taken.

## Consequences

- `shep kelpie add` registers the runner alone. It and `start` say how to bring the dog up when the adopted kelpie is not enabled, is not running, or was adopted before it asked for the channel, and `start` then starts nothing.
- The dog runs only when shep starts it, with `SHEP_DOG_NAME` and `SHEP_NAME` both set. `shep kelpie` with no verb sets only `SHEP_DOG_NAME`, and prints the usage.
- A kelpie adopted before the ask starts with no channel and exits naming the fix, so it restart-loops until shep gives up and marks it errored, each start logging the same fix.
- The adopted dog removes a left-over `kelpie-dog` sheep of kelpie's before it opens the book, and never writes the book to do it. shep answers the delete once the sheep has exited, so the book on disk is the one that sheep last saved. A left-over whose entry sets `KELPIE_HOME` or `HOME` is refused and kept, since shep hands an adopted dog no `KELPIE_HOME` and its own `HOME`, so that book may be elsewhere.
- `shep kelpie lease` and the lease triggers name `kelpie`. Support for a Flockfile dog under that name, and `add`'s move from it to `kelpie-dog`, are removed: a sheep named `kelpie` stops `shep enable kelpie` anyway.
- Kelpie takes only a 0.12.x shepherd.

ADRs 0005 and 0006 leave this decision as it is.
