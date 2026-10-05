# shep stops every lamb

This reopens the process half of ADR 0002. That ADR said stopping a project stops its workers "which shep's stop ladder already does". It does not. Kelpie starts every child in a process group of its own (`.process_group(0)` in `src/adapters/process.rs` and `src/adapters/bridge.rs`), and shep's ladder reaches only the sheep's own group. The runner asks for `shutdown_with_message`, so a stop sends it no signal at all, and once the runner exits shep skips its `kill_tree` rung. A worker, its tools, an MCP server or a dev server survives any stop the runner does not finish itself: a SIGKILL, a panic, a stop while a call runs past the budget. A survivor that is a `claude -p` keeps working and may push.

To cover that, kelpie grew a supervisor inside a supervisee: its own stop ladder (SIGTERM, 3 seconds, SIGKILL), a 7-second stop budget that needs `kill_timeout = "10s"` on the runner's entry, an orphan sweep at start, and pid files for dev servers. These are left over from building kelpie as a standalone app before it was a dog.

So the stop is shep's. shep gets one general feature, shep-pm/shep#688: on any stop, snapshot the sheep's lamb tree before the leader goes, then SIGTERM and later SIGKILL every lamb still alive, whatever group or session it is in. Once that ships, kelpie drops its own ladder, the orphan sweep, the pid files and the `kill_timeout` override, and the runner's entry takes shep's default. Every process kelpie starts is a lamb of a sheep: an agent session is a lamb of its project's runner, and the runner never starts one any other way.

## Considered options

- **Every agent session a sheep.** The shepherd would own each one directly, but sheep are long-lived and flock entries would churn per call, which ADR 0002 already turned down. The maintainer turned it down again on 2026-10-05.
- **Drop kelpie's process groups now.** Children in the runner's group get shep's signal, but anything a session starts in a group or session of its own still escapes, and kelpie loses the group it uses to end one worker at its turn ceiling.
- **Keep kelpie's ladder and make it complete.** A sweep at every start and a pid file for every child. That is a second process supervisor in a dog, the thing shep is.

## Consequences

- Until shep's sweep ships, kelpie's ladder stays as it is.
- Kelpie keeps a process group per call only to end that one call (a turn ceiling, a pause). Ending a single lamb from the CLI or lookout is a shep feature too, asked on shep-pm/shep#354.
- The runner never blocks a thread on a lamb, so it answers a stop at once and has nothing left to wind down past shep's default `kill_timeout`.
- The relay goes. It was a `claude --bg` session that Claude Code's own supervisor owned, so no stop of kelpie's reached it. Rulings reach the maintainer through ntfy or the webhook until shep can carry a question itself (shep-pm/shep#689).
- The sessions the maintainer opens by hand (`--interactive`, `attach`, the project manager's) run in the maintainer's own terminal and are not lambs. git and gh calls stay short children of the runner.
