# Coding standards

Read when reviewing a change. Mechanical rules live in CI and tests; these are the judgement calls they cannot make.

- Field lifecycle. For every field a change adds to the state file or a work item, list each path that sets it and each path that clears it, and name the scenario where it is left set when it should not be (a round that ends with no push, a withdrawn ruling, a new pass, a restart, main moving). A field with a stale scenario is a defect.
- A wrong ruling is a defect. A ruling raised without cause, or an `auto` merge held without cause, costs the maintainer's attention. It is never a safe over-flag.
- Restart. Each new phase or step transition is checked against a runner restarting between its steps, including anything kept only in memory.
