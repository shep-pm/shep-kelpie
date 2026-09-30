# The adopted kelpie is the lease dog

This reopens the dog's half of ADR 0003. That ADR ran the lease dog as a sheep named `kelpie-dog` and left the adopted `kelpie` disabled, because shep 0.10.1 and 0.11.0 gave an adopted dog no shepherd channel. It named the adopted dog holding the leases as the option to take up if shep ever gave adopted dogs a channel. shep 0.12 does (shep-pm/shep#656): a dog asks with `shep-channel: true` in its `--version` answer, `shep adopt` records the ask, and every start of the dog sets `channel` and `shutdown_with_message` (shep-pm/shep-kelpie#180).

So kelpie asks for the channel, and the adopted `kelpie`, started with no arguments, is the lease dog. Left disabled, it was the thing that exited 2 on every start and restart-looped when enabled.

## Considered options

- **Keep `kelpie-dog`.** Two things named for one dog, and the adopted one still restart-loops unless disabled by hand.
- **The adopted dog holds the leases.** Taken.

## Consequences

- `shep kelpie add` registers the runner alone. It and `start` say how to bring the dog up when the adopted kelpie is not enabled, or was adopted before it asked for the channel, and `start` then starts nothing.
- The adopted dog removes a left-over `kelpie-dog` sheep of kelpie's before it opens the book, and never writes the book to do it. shep answers the delete once the sheep has exited, so the book on disk is its last. One whose entry sets `KELPIE_HOME` is refused and kept, since shep hands an adopted dog no `KELPIE_HOME` and that book may be elsewhere.
- `shep kelpie lease` and the lease triggers name `kelpie`. The Flockfile dog under that name, and the move from it to `kelpie-dog`, go: a sheep named `kelpie` stops `shep enable kelpie` anyway.
- Kelpie takes only a 0.12.x shepherd.
