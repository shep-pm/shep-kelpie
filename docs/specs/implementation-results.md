# Implementation test results

Run 2026-09-26 on Claude Code 2.1.283, following `docs/specs/implementation-tests.md`. Raw results are in the experiments repo named in CLAUDE.md, at commit b8e02b9 under `implementation/`: a record and a diff per run, the cases with their validation evidence, and `SCORES.md`, whose tables `run_series.py score` rebuilds from the records. Units weigh cache reads 0.1, cache writes 2, output 5 and uncached input 1. Scored runs cost $45.02.

## Verdict

- **The harder cases still hit the ceiling.** 65 of 66 scored runs passed their hidden tests. Cases of 2 to 5 source files and up to 270 lines, with specs that name no file, did not separate the configs or the strategies on pass/fail. Dollars and wall time did.
- **Implementer default: Sonnet 5 at medium effort**, by the proposed rule. It passed 6 of 6 for $2.24, the cheapest config within one case of the best.
- **Work split for implementation: inline**, by the proposed rule. Every strategy passed 12 of 12, and inline was cheapest per pass at $0.36.
- **Haiku is not the cheap option on multi-file work.** It took about twice Sonnet's turns and 2.5 times its units, cost more ($2.74) and missed one case. In the first implementer set it cost about half what Sonnet did.
- **Whether codebase-memory-mcp helps an implementer is still open.** No inline_cbm run called it.

## Cases

All six fail at the parent with only the tests applied, pass at the commit, and split into test and source halves that recompose the commit exactly.

| case | crates | source files, lines | hidden tests | red at the parent by |
|---|---|---|---|---|
| channel-evict-metric-not-ready | channel | 4, 122 | 7 | build error |
| core-depends-on-field | core | 3, 62 | 5 | build error |
| client-bounded-link-waits | client | 2, 160 | 7 | build error |
| core-daemon-kill-signal-grammar | core, daemon | 5, 196 | 4 | build error in core, assertions in the daemon |
| core-daemon-env-arg-templates | core, daemon | 4, 270 | 3 | assertions |
| core-declared-keys | core | 3, 146 | 3 | build error |

A seventh case validated but was held back: its tests call a crate-private function whose signature the commit changes, so its spec would have had to name it.

## Calibration grid

Inline, one run per case.

| config | passed | USD | units | median wall s | median turns |
|---|---|---|---|---|---|
| Opus 5.5 low | 6/6 | 3.70 | 1.04M | 67 | 16 |
| Sonnet 5 medium | 6/6 | 2.24 | 1.12M | 64 | 20 |
| Haiku 4.5 low | 5/6 | 2.74 | 2.74M | 142 | 36 |
| Fable 5.1 medium | 6/6 | 14.94 | 2.04M | 157 | 30 |

- Haiku's miss: on core-depends-on-field it never registered the new field as taking effect at the next spawn, which the spec said.
- Units and dollars disagree across models. Fable used fewer units than Haiku and cost five times as much, so a comparison between models has to be priced per model, not in units.

## Work-split grid

Sonnet 5 medium, two runs per case. Inline's first run is the calibration grid's Sonnet run.

| strategy | passed | USD | USD per pass | units per pass | median wall s |
|---|---|---|---|---|---|
| inline | 12/12 | 4.29 | 0.36 | 179k | 65 |
| phased | 12/12 | 7.80 | 0.65 | 325k | 113 |
| crew | 12/12 | 7.07 | 0.59 | 333k | 73 |
| inline_cbm | 12/12 | 4.47 | 0.37 | 186k | 63 |

- **inline_cbm measured inline, not the graph.** The server connected and indexed each run's worktree in about 7 seconds, but Claude Code lists MCP tools as deferred behind a tool search, and no run looked them up. A retest needs the tools loaded up front or a prompt that requires a first query.
- **Crew mostly ran as inline with a helper.** 3 of 12 runs dispatched no subagent and 4 dispatched one. Where subagents ran, they took a median 35% of the run's units. The costs include them.
- **Phased pays for its plan.** The plan session took 42 to 62% of a run's cost, and the plans ran 4k to 10k characters.

## Caveats

- One sample per calibration cell and two per work-split cell.
- The client case's first spec left `PartialEq` off the new error type's derives, which the hidden tests need. Haiku followed the list literally and its hidden tests did not compile. The spec was fixed and all four client calibration runs were rerun. The first-spec runs are kept apart and not scored, and no work-split run used that spec.
- An API stall at about 09:29 hung three runs for about 15 minutes. The wall medians subtract it, and pass/fail was unaffected.
- The core-depends-on-field commit itself fails two wire-snapshot tests, so every run on that case does too. Those count as the baseline.
- One run touched test code: Haiku added its own test module on the templates case. Its hidden tests still passed.
- 95 tool calls were denied, none fatal. Most were edit scripts in python, perl or sed, and `cd <worktree> && ...` chains.
- Review findings on the diffs were not scored. Hidden tests were the only quality signal.
