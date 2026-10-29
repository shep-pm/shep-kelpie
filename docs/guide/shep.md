# What shep does for it

shep-kelpie is a dog of your own shepherd, and leans on it for everything a process manager does:

- **Supervision.** The adopted `kelpie` is the dog, and each project's runner is a sheep. shep starts, restarts and stops them, and keeps them beside your other sheep. `shep stop <project>` stops a runner, and `shep delete <project>` removes it
- **Lambs.** Every agent call a runner starts, a worker's turn, a reviewer's session or the project manager's wake, is a lamb of that runner. `shep describe <project>` labels each with its issue and role, such as `#114 worker`
- **Stopping the calls.** A runner asks for `shutdown_with_message`, so a stop reaches it as a message, and it sends each call's process group SIGTERM and exits at once, without waiting for them. shep's stop then ends every lamb the runner left, whatever process group or session it is in (shep-pm/shep#688, ADR 0005), so a runner's entry takes shep's default `kill_timeout`. A runner still ends a single call itself, at its turn ceiling, through the process group each call leads
- **Triggers and status.** Every `shep kelpie` verb is a trigger on the shepherd channel, which `shep trigger <project> <action>` sends too, and the runner answers while its calls run. Runners ask the dog for leases with notes on shep's bus, which shep's metrics dog never reads
- **Settings.** A project's settings are its runner's `[app.dogs.kelpie]` table, and shep-kelpie's own are the `[kelpie]` section of `dogs.toml`. shep keeps both, and lookout edits both from shep-kelpie's settings schema
- **Logs.** `shep bleats <project>` is a runner's log, and `shep bleats kelpie` the dog's

One thing shep-kelpie still does itself, until shep can:

- **Reaching you.** Rulings go to kelpie's own webhook, ntfy or Discord, and ntfy replies are checked with kelpie's own TOTP, until shep can carry a question itself (shep-pm/shep#689)

shep-kelpie refuses a shepherd on another shep minor or major than the pinned one, or on an earlier patch, naming both versions. It needs 0.19.0 or later, the shep it is built with. shep-kelpie's commands talk to the shepherd's socket with the shep client it is built with, never a `shep` on `PATH`. Keep a shepherd's `SHEP_HOME` short: see [shep-kelpie's home](home-and-logs.md#shep-kelpies-home).
