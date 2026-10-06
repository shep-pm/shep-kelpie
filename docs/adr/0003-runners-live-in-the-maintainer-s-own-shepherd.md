# Runners live in the maintainer's own shepherd

Kelpie's runners and its dog are sheep of the maintainer's own shepherd, beside their other sheep, rather than of a second shepherd under `~/.kelpie/shep` (decided by the maintainer on shep-pm/shep-kelpie#110). Kelpie is adopted as a dog, so `shep kelpie add` in a checkout sets a project up and `shep kelpie start` runs it. This reopens the "own pinned shepherd" half of ADR 0001: a restart of the shepherd now stops kelpie's runners too, which the stop ladder and the state file already survive.

The dog that holds the leases is a sheep named `kelpie-dog`, not the adopted dog. On shep 0.10.1 and 0.11.0 an adopted dog starts with no shepherd channel, so it cannot answer the `status`, `take` and `return` triggers the lease commands send, and shep refuses a change to a dog's config. The adopted `kelpie` stays disabled: it is what `shep kelpie <args>` runs, and how lookout finds kelpie's settings schema. The dog's sheep has a name of its own because `shep adopt` refuses a name a sheep holds, and `shep disable kelpie` deletes a sheep named `kelpie`.

## Considered options

- **Keep kelpie's own shepherd.** A tester would run two shepherds and keep a second `SHEP_HOME` in every command, and kelpie's runners would be missing from the flock they already watch.
- **The adopted dog holds the leases.** It has no channel, so the maintainer's lease commands would need a private socket or a file mailbox beside shep. Worth taking up if shep gives adopted dogs a channel.
- **The dog's sheep named `kelpie`, beside the disabled adopted dog.** Measured to work, but a later `shep disable kelpie` deletes it.

## Consequences

- `shep kelpie add` registers the runner and `kelpie-dog`, and replaces a Flockfile dog under the old name `kelpie`, which holds the name the adoption needs.
- `kelpie lease` asks `kelpie-dog`, then `kelpie`, so an install from before this keeps its lease commands.
- Kelpie takes any shepherd on its pinned minor line and refuses another, as before.

ADR 0004 reopens the dog's half: the adopted `kelpie` is the lease dog and `kelpie-dog` is gone. ADR 0005 reopens the stop: shep, not kelpie's own ladder, is to stop every lamb.
