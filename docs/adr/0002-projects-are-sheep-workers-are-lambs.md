# Projects are sheep, workers are lambs

Each project runs as a sheep, kelpie's project runner, which holds the project manager's code, the board and the project's settings. Its workers are that sheep's lambs. Sheep are long-lived and kept alive while lambs come and go, and stopping a project should stop its workers, which shep's stop ladder already does.

## Considered options

- **Workers as sheep, one fold per project.** The shepherd would hold each worker's pipes across a kelpie restart, but flock entries would churn per work item, and every worker would be a row in `shep flock`.

## Consequences

- The runner, not the shepherd, holds each worker's pipes. A runner crash costs every worker one turn, which it resumes from its transcript.
- Worker names in shep's UI need lamb labels (shep-pm/shep#624). Until then, kelpie's own UI names them.
- Per-project settings ride on the sheep entry through an opaque per-dog table (shep-pm/shep#623).
- Kelpie and its runners talk through shep: triggers down, channel metrics up on the bus. Runners report running totals, because metrics drop under backpressure.
