# Kelpie stands on shep, not in it

Kelpie is its own private repo, built on the published shep crates and released on its own cycle. It runs as a third-party dog under its own pinned shepherd, apart from the shepherd used to develop shep, so a dev restart never kills a worker. Nothing kelpie-specific lands in shep: a generic gap becomes an ordinary shep issue on its own merits, as shep-pm/shep#623 and #624 did.

## Considered options

- **Adopt Paperclip.** Its gates live in agents (a merge is an agent running `gh pr merge`), its plugin spec bars plugins from approval, checkout and budget logic, it has no rationing primitive for an hourly review window or a shared GPU, and its own issues would be a second tracker. Parts of it are worth lifting; see the design log.
- **Build on Vibe Kanban.** Its sunset was announced 2026-04-10 and it is community maintained since. Worth reading, not worth building on.
- **A third flock category, or per-project "puppies", inside shep.** Every kelpie change would become a shep release, run through shep's docs trigger and review budget, and put agent policy inside a general process manager.
- **Standalone with no shepherd.** Kelpie would supervise itself, and a kelpie crash would take its workers with it.

ADR 0003 moves the runners and the dog into the maintainer's own shepherd. The rest of this decision stands.
