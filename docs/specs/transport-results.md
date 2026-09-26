# Transport test results

Run 2026-09-25 on Claude Code 2.1.282 with workers on Sonnet, following `docs/specs/transport-tests.md`. Raw ledgers are in the experiments repo named in CLAUDE.md, at commit d93f73b under `transport/results/`, and its `transport/analyze.py` rebuilds every table below from them. Units weigh cache reads 0.1, one-hour cache writes 2, output 5 and uncached input 1.

## Verdict

- **T1 stays the worker transport.** T2 costs the same per turn, within 3% on every turn, so it clears neither cost threshold. It is 2.2 times faster per turn, a win the thresholds did not price.
- **An outside program can wake an idle session.** T3 woke a background session over its socket three times out of three, about 5 seconds each. This is the takeover and wake-up route if kelpie ever needs one, on an undocumented protocol.
- **Tools: the stdio relay.** It adds 14 tokens to the floor, because MCP tools are deferred. The in-process server works on the plain CLI, but only under stream-json, and it saves nothing.
- **Channels do not fire under `-p`.** The CLI never registered the push. A background-session attempt exited without a transcript, so channels stay unproven.
- **Lease traffic stays on shep.** A round trip takes 1 to 3 ms and reclaim takes 17 ms, far inside the one-second threshold.
- **Utilization is readable for free.** Headless `/usage` costs no tokens, and stream-json emits `rate_limit_event` with the weekly utilization.

## T1: one `claude -p` process per turn

Each turn reads one file of about 3k tokens, so a turn is two model calls. "Write" is cache write.

| variant | run | turn 1 units (write) | turn 2 units (write) | turn 10 units | total, 10 turns | median s/turn |
|---|---|---|---|---|---|---|
| baseline | 1 | 61,418 (27,833) | 19,067 (5,323) | 28,867 | 273,685 | 3.9 |
| baseline | 2 | 8,615 (0) | 19,101 (5,339) | 28,870 | 220,941 | 3.8 |
| baseline | 3 | 8,515 (0) | 19,059 (5,319) | 28,866 | 220,767 | 3.8 |
| hooks off | 1 | 55,961 (25,344) | 18,473 (5,277) | 28,360 | 263,577 | 3.6 |
| system prompt every call | 1 | 76,103 (34,998) | 20,156 (5,317) | 29,976 | 298,572 | 3.9 |
| system prompt first call only | 1 | 9,537 (0) | 20,117 (5,303) | 29,954 | 231,734 | 3.7 |
| crash at turn 6 | 1 | 8,480 (0) | 19,044 (5,312) | 28,885 | 220,809 | 3.8 |

- The first `--resume` wrote only the new turn (about 5.3k). The 29k rewrite seen in the earlier probe did not recur in seven runs.
- `--append-system-prompt-file` persists across `--resume`. Turns 2 to 10 read the same context whether the flag was passed again or not (1,247,080 against 1,246,596 tokens). Re-passing it costs nothing, and skipping it saves nothing.
- The maintainer's hooks add about 2.5k tokens to the floor and about 250 tokens a turn.
- Killing a worker on its first tool call left 4 transcript lines of the half-finished turn. Re-issuing the turn cost the same as a normal turn.
- A second run with identical content read turn 1 entirely from cache. Identical conversations share cache across sessions, so run 1 is the fair cold case.

## T2: one long-lived stream-json worker

| run | turn 1 | turn 10 | 10-turn total | median s/turn | after 10 min idle | longest line | `/compact` s | turn after compact | resume after kill |
|---|---|---|---|---|---|---|---|---|---|
| 1 | 61,616 (w 28,343) | 28,962 | 275,102 | 1.8 | 9,074 (w 62) | 32,840 | 23 | 62,205 (w 30,496) | 95,399 (w 47,093) |
| 2 | 7,764 (w 0) | 28,963 | 190,739 | 1.8 | 9,074 (w 62) | 32,840 | 62 | 62,519 (w 30,653) | 95,713 (w 47,250) |
| 3 | 7,764 (w 0) | 28,967 | 190,918 | 1.9 | 9,076 (w 62) | 32,840 | 45 | 62,735 (w 30,761) | 96,503 (w 47,645) |

- Per turn, T2 costs what T1 costs: the difference on turns 1 to 10 of the cold runs ranges from -0.9% to +2.6%.
- The cache survives 10 idle minutes: the next turn wrote 62 tokens.
- Bash output is cut at about 30k characters before it reaches the stream, so the longest line was 32,840 characters. The runner read it whole.
- `/compact` works as a stream-json turn and takes 23 to 62 seconds. The next turn rewrote about 30k of cache.
- Resuming a compacted session in a fresh process rewrote about 47k of cache: 95k units for a trivial turn. Compaction plus a restart is the expensive combination.
- Runs 2 and 3 ran beside run 1 on identical content and read most of it from cache, so their totals are not transport savings.

## R: rate-limit visibility

- `claude -p "/usage" --output-format json` returns the session and weekly utilization as text, with `total_cost_usd` 0 and no model turn.
- Stream-json output carries `rate_limit_event` messages: `{"type": "rate_limit_event", "rate_limit_info": {"status": "allowed_warning", "resetsAt": ..., "rateLimitType": "seven_day", "utilization": 0.93, "surpassedThreshold": 0.75, "unifiedWindows": {"five_hour": ..., "seven_day": ...}}}`. `-p --output-format json` does not.

## T3: a background session steered over its socket

- `claude --bg` refuses a folder whose trust prompt was never accepted, so T3 ran in the already-trusted home folder.
- An injected user message woke an idle background session 3 times out of 3, twice with `crossSessionInbound` unset and once with `accept`. The reply landed about 5.1 seconds after injection, including 1-second polling. A woken turn cost about 5k units.
- The session sees the message framed as one from another Claude session, not as a raw user turn.
- `claude attach` was not exercised; it needs a terminal.

## T4: tools kelpie serves to a worker

| route | floor context | tool call | answer |
|---|---|---|---|
| stdio relay (`--mcp-config`) | 34,507 against 34,493 without it | 3 turns, 5 s, 11,175 units | correct |
| in-process, stream-json control protocol | 32,836 | 3 turns, 2 s, 13,925 units | correct |

The in-process route needs no SDK. The host sends `control_request` `initialize` with `sdkMcpServers`, and then answers each `mcp_message` control request with `{"mcp_response": ...}`.

## T5: MCP channel push

The server declared `experimental["claude/channel"]` and sent `notifications/claude/channel` 25 seconds after initialization, to a stream-json worker launched with `--dangerously-load-development-channels server:kelpie`. Nothing arrived, stderr stayed empty, and the debug log shows the server connecting but never a channel registration or a gate decision. A background session launched with the same flags exited without a transcript.

## L: a lease round trip through shep

| measure | rounds 1, 2, 3 |
|---|---|
| want to grant, runner side | 2.5, 1.2, 1.5 ms |
| want to grant, from the driver's CLI call | 28, 24, 30 ms |
| holder killed to next grant | 16.8, 17.1, 17.0 ms |

- A flood of 5,000 metrics made the dog's subscription lag, dropping 3,653 events, and the last value, 5,000, still arrived. Running totals converge.
- A runner that restarts reports its totals from zero again. The dog has to reset its bookkeeping on the runner's `restart` or `online` event, or totals need an epoch.
- An operator's SIGKILL reports as a `stop` event, not `exit`. A restart reports as `restart` then `online`, with no `start`.
- `shep trigger` reaches an adopted dog by exact name and answers `no_channel`. A dog answers triggers only if it has a shepherd channel.

## Also learned

- Workers inherit the maintainer's interactive hooks. In the work-split run, `agent-delegation-guard` blocked a worker's subagent dispatches, and the worker stopped to ask. Kelpie needs a worker settings profile of its own.
- Moving the week from 93% to 94% took all of T1, T2, T3, T4 and L together.
