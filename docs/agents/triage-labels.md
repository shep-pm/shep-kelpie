# Triage Labels

The skills speak in terms of five canonical triage roles. This file maps those roles to the actual label strings used in this repo's issue tracker.

| Label in mattpocock/skills | Label in our tracker | Meaning                                  |
| -------------------------- | -------------------- | ---------------------------------------- |
| `needs-triage`             | `needs-approval`     | Researched by kelpie, waits on the maintainer |
| `needs-info`               | `needs-info`         | Waiting on reporter for more information |
| `ready-for-agent`          | `ready-for-agent`    | Fully specified, ready for an AFK agent  |
| `ready-for-human`          | `ready-for-human`    | Requires human implementation            |
| `wontfix`                  | `wontfix`            | Will not be actioned                     |

When a skill mentions a role (e.g. "apply the AFK-ready triage label"), use the corresponding label string from this table.

Edit the right-hand column to match whatever vocabulary you actually use.

Kelpie also puts `possible-spam` on an issue its **spam check** read as spam. It is no role: it is the label left for the maintainer to decide, and no worker acts on it. `shep kelpie triage <n>` takes it off when kelpie's own record says it put it on.

## Priority

Every issue gets one `priority:` label. The board takes `P0` first and `P4` last, then an issue with none.

| Label          | Meaning                                                                                                                                    |
| -------------- | ------------------------------------------------------------------------------------------------------------------------------------------ |
| `priority: P0` | An emergency that jumps the line: a runner is down, or kelpie is doing damage. Normally empty                                              |
| `priority: P1` | Keeps kelpie running without the maintainer stepping in: stalls, a turn or wake that never comes, rate-limit stalls, parents left open     |
| `priority: P2` | Performance: the same result for fewer tokens, less wall-clock, fewer GitHub requests or fewer CI rounds                                   |
| `priority: P3` | Bugs and docs                                                                                                                              |
| `priority: P4` | Features                                                                                                                                   |

A parent takes the priority of its most urgent open slice. When an issue fits two rows, it takes the higher one.
