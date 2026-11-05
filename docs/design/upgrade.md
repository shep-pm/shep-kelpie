# Upgrade restarts

`upgrade` restarts the dog, one canary runner, then every other running runner together. `src/upgrade/restart.rs`, `src/upgrade/canary.rs`. The handover and drain each runner goes through are in handover.md and agents.md.

## Order

- The dog first: it waits until no runner has a merge in flight, and is never drained. Every runner leases from it. #1671
- Then one canary, the first runner to show no merge in flight. The others are neither drained nor handed over and keep working.
- Then every other running runner, together, once the canary has passed. Each runs the per-runner steps unchanged: handover else drain, merge wait, restart, and a stopped sheep stays stopped.
- Why the serial order went: a runner can wait 30 minutes or more on a merge or a worker turn, and every runner behind it waited with it. No ADR asked for the order.

## The pick

- `pick` asks every running runner for `status` in parallel every `patience.poll` and takes the first that shows none in flight, the first by name when two do at one look.
- In flight is `merging(&body)` as for the merge wait. A `working` ticket does not count: its turn crosses the handover.
- A runner that answers no `status` is not quiet. None quiet in `patience.merge` stops the upgrade, names each runner and what it waits on, and says no runner was restarted.
- The pick's `status` body is kept as the "before" read, replaced by the last `status` the merge wait reads just before the restart, so a step that began failing on the old build while the canary drained is not new. A pick found stopped when its turn comes stays stopped, and picking goes on.

## The check

After the canary's restart, `status` is asked every `patience.poll` and `judge(before, now, installed)` reads each body. It adds no timer and no `Patience` field.

- Passes: `passes` is at least 1 (a first full pass of the loop, #1670), `build` is the installed stamp, and `commit` is the installed commit when the installed build names one.
- Fails at once: `settings_refused` present, a step in `work_items` newly `failing`, a ticket in the "before" `working` list whose `turn.state` is now `failed`, or another `build` or `commit`.
- A current build sends no `passes` until its first pass ends, so a body with none waits like one with 0. A build from before #1670 never sends one: a canary on the installed build that still counts none after `patience.start` is taken for it and passes, so a rollback onto such a build is not stuck. Every other check applies the whole time.
- Fails on silence: no `status` for `patience.start` after the restart, or the runner not running.
- Otherwise it keeps asking, and Ctrl-C ends it. A step failing before and after the restart is not new.

## A failed canary

- No other runner is touched. `restart_all` returns `Stopped::Canary`, which `run` words apart from `Stopped::Other`: the dog and the canary run the new build, the rest the old one, and a second restart onto the same build is no help.
- Nothing is rolled back, and `builds/installed.seen` is not written.
- After an install the message names `shep kelpie upgrade --rollback` as what puts the previous build back. After a rollback it says the same command swaps the builds forward again. After a bare upgrade it names none: cargo kept no copy of the build it replaced.

## The rest

- One future per runner, joined on one task (`canary::join_all`, no new crate). `say` is shared behind a `RefCell`, and each line names its runner.
- A runner that stays silent after its restart is collected and asked again once the others are done, then named.
- One runner's error lets the others run to their ends. The errors are joined in name order.
- The interrupt is awaited once for the group. Each runner still draining is sent `undrain`.
- `--now` still goes canary first with its checks, with no handover and no drain.
