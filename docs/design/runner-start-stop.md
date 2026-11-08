# Starting, pausing and finishing a project

The verbs that move a project's runner sheep. `add` and the project's table are in work.md, and the verbs that ask a running runner over its socket are in runner-socket-client.md.

- Project runs exactly while its runner's sheep runs; no paused state. #268
- `finish`: open items end, nothing new picked, then stops the sheep; `finishing` saved so restart keeps finishing. `add`, `adopt`, `rework` refused meanwhile; flag cleared just before stop. #303
- `start`, `pause`, `finish` touch only sheep kelpie started as `runner <project>` (never start one the maintainer stopped); `start` waits 30s for JSON `status`. `src/flock/{verbs,control/}`
- `pause`: drain, then wait out calls and any merge (up to 1h), then stop sheep (stop mid-merge can leave a merged PR's item open); a wait that runs out or fails sends `undrain keep`, which leaves the runner running and the turns the pause ended unstarted for 10 min (`PAUSE_HOLD`), so a retried `pause` finds them stopped; a bare `undrain` starts them at once. A Claude Code or pi worker turn ends at its next tool call (workers.md); `pause` names those items and waits 10 min for it (a full test run is ~6 min), then stops the call as `shep stop` does. Other calls wait ceiling + 5 min, and a drain ends a call still queued for its lease at that ceiling, so a call that never reached its model cannot hold the wait. #268, #821, #1328, #1718
