# Work-split experiment: results

## Per-run table

| module | strategy | run | units | wall s | findings | real | false | unsure | other |
|---|---|---|---|---|---|---|---|---|---|
| config | crew_nohooks | 1 | 932,045 | 201 | 5 | 3 | 2 | 0 | 0 |
| config | crew_nohooks | 2 | 879,819 | 270 | 3 | 3 | 0 | 0 | 0 |
| config | crew_nohooks | 3 | 878,655 | 72 | 4 | 2 | 2 | 0 | 0 |
| config | inline | 1 | 105,694 | 120 | 2 | 1 | 1 | 0 | 0 |
| config | inline | 2 | 110,640 | 226 | 2 | 1 | 1 | 0 | 0 |
| config | inline | 3 | 135,468 | 54 | 1 | 1 | 0 | 0 | 0 |
| config | phased | 3 | 636,194 | 254 | 11 | 5 | 6 | 0 | 0 |
| config | phased | 4 | 530,867 | 267 | 17 | 8 | 9 | 0 | 0 |
| config | phased | 5 | 563,513 | 275 | 12 | 7 | 5 | 0 | 0 |
| lookout | crew_nohooks | 2 | 3,129,073 | 249 | 13 | 6 | 7 | 0 | 0 |
| lookout | crew_nohooks | 3 | 2,245,221 | 252 | 11 | 7 | 4 | 0 | 0 |
| lookout | crew_nohooks | 4 | 3,140,001 | 240 | 13 | 6 | 7 | 0 | 0 |
| lookout | inline | 2 | 343,260 | 232 | 15 | 5 | 9 | 1 | 0 |
| lookout | inline | 3 | 221,470 | 196 | 16 | 6 | 8 | 2 | 0 |
| lookout | inline | 4 | 265,992 | 102 | 12 | 6 | 5 | 1 | 0 |
| lookout | inline_cbm | 1 | 330,740 | 117 | 16 | 8 | 8 | 0 | 0 |
| lookout | inline_cbm | 2 | 373,370 | 129 | 13 | 6 | 7 | 0 | 0 |
| lookout | inline_cbm | 3 | 317,587 | 115 | 11 | 3 | 7 | 1 | 0 |
| lookout | phased | 2 | 666,312 | 256 | 18 | 6 | 11 | 1 | 0 |
| lookout | phased | 3 | 784,981 | 325 | 16 | 5 | 11 | 0 | 0 |
| lookout | phased | 4 | 586,975 | 232 | 17 | 6 | 11 | 0 | 0 |
| supervisor | crew_nohooks | 4 | 1,616,543 | 174 | 18 | 8 | 10 | 0 | 0 |
| supervisor | crew_nohooks | 5 | 1,482,290 | 114 | 11 | 7 | 3 | 0 | 1 |
| supervisor | crew_nohooks | 6 | 1,507,893 | 133 | 19 | 14 | 5 | 0 | 0 |
| supervisor | inline | 1 | 852,650 | 261 | 16 | 8 | 7 | 1 | 0 |
| supervisor | inline | 2 | 551,609 | 123 | 7 | 5 | 2 | 0 | 0 |
| supervisor | inline | 4 | 793,291 | 186 | 10 | 4 | 5 | 0 | 1 |
| supervisor | inline_cbm | 1 | 768,189 | 184 | 7 | 4 | 2 | 1 | 0 |
| supervisor | inline_cbm | 2 | 719,805 | 192 | 9 | 4 | 4 | 1 | 0 |
| supervisor | inline_cbm | 3 | 612,181 | 159 | 9 | 5 | 3 | 1 | 0 |
| supervisor | phased | 1 | 869,375 | 267 | 10 | 6 | 4 | 0 | 0 |
| supervisor | phased | 2 | 1,013,362 | 385 | 11 | 6 | 4 | 1 | 0 |
| supervisor | phased | 4 | 736,875 | 279 | 16 | 3 | 12 | 1 | 0 |

## Per-cell median table

| module | strategy | runs | median units | median wall s | median findings | precision (real/(real+false)) | real findings / 1M units |
|---|---|---|---|---|---|---|---|
| config | crew_nohooks | 3 | 879,819 | 201 | 4.0 | 0.67 | 2.97 |
| config | inline | 3 | 110,640 | 120 | 2.0 | 0.60 | 8.53 |
| config | phased | 3 | 563,513 | 267 | 12.0 | 0.50 | 11.56 |
| lookout | crew_nohooks | 3 | 3,129,073 | 249 | 13.0 | 0.51 | 2.23 |
| lookout | inline | 3 | 265,992 | 196 | 15.0 | 0.44 | 20.46 |
| lookout | inline_cbm | 3 | 330,740 | 117 | 13.0 | 0.44 | 16.64 |
| lookout | phased | 3 | 666,312 | 256 | 17.0 | 0.34 | 8.34 |
| supervisor | crew_nohooks | 3 | 1,507,893 | 133 | 18.0 | 0.62 | 6.30 |
| supervisor | inline | 3 | 793,291 | 186 | 10.0 | 0.55 | 7.74 |
| supervisor | inline_cbm | 3 | 719,805 | 184 | 9.0 | 0.59 | 6.19 |
| supervisor | phased | 3 | 869,375 | 279 | 11.0 | 0.43 | 5.73 |

Unique findings verified: 371 (real=176, false=183, unsure=12, error=2)

## Notes

- **Pre-existing spend-limit outage.** Every `ws_mini.jsonl` line timestamped
  around 2026-09-26T01:46-01:48Z (an earlier attempt at this same grid) failed
  with "You've hit your monthly spend limit" or a transient
  "Server is temporarily limiting requests" error. Both had cleared by the
  time this run started (a plain `claude -p` ping succeeded), and no
  spend-limit or rate-limit error recurred for the rest of the session. Those
  old lines are excluded from every table above; the grid was refilled with
  fresh run numbers rather than reusing them.
- **The crew-prompt fix worked.** Before the required change, `crew_nohooks`'s
  final message sometimes only referred back to a subagent's report
  ("The final count stands at `FINDINGS: 20`" without repeating the list),
  which made findings unrecoverable from the ledger's own `report` field. The
  new instruction ("your final message must repeat the full merged,
  deduplicated list... do not refer back to a subagent's report") produced a
  full restated list in every one of the 9 fresh `crew_nohooks` runs.
- **No retries were needed.** All 19 round-1 fill-in runs and all 6
  `inline_cbm` runs ended with a `FINDINGS:` line on the first attempt — the
  "retry once" path in the brief was never exercised.
- **One extraction bug, caught and fixed mid-run.** `lookout inline run=2`
  wrapped every finding line in a single pair of backticks
  (`` `path:line: claim` ``), which the original `path:line:` regex didn't
  match (it only tolerated a bullet/number prefix), so the run's 15 findings
  were briefly invisible to the verifier despite the run itself being
  complete. Fixed by stripping one outer backtick pair per line before
  matching; re-extracting recovered all 15 (plus 2 more from other runs with
  the same formatting) with no other finding-count mismatches anywhere in the
  33 complete runs. `results/findings.jsonl`'s `extracted_findings` now
  matches every run's own reported `FINDINGS: n` exactly.
- **Two findings never got a verdict.** Both are the same location
  (`crates/shep-daemon/src/supervisor/handover.rs:155`, from two different
  runs' independent wording of the same claim about a dropped `ParkedPumps`
  reply). The Opus verifier hit `--max-turns 8` chasing `report_fds`/`resume`
  callers across the handover module and returned no text both times it was
  tried; recorded as `verdict: "error"` rather than silently dropped, and
  excluded from the precision denominator (which only counts real/false).
- **`inline_cbm` cost and speed.** On `lookout` (58k lines) it was clearly
  faster than plain `inline` (median wall 117s vs 196s) at similar cost and
  precision. On `supervisor` (22k lines) it was slower than plain `inline`
  (median wall 184s vs 186s — roughly a wash) and no more precise. With n=3
  per cell this is not a strong signal either way; the graph tools seem to
  help more on the larger, harder-to-navigate module.
- **Cost.** The 33 complete runs feeding this report totalled about $49 in
  `total_cost_usd`; verifying the 373 unique findings (373 calls, since 2
  were retried once) cost about $87 more, for roughly $136 total this
  session. `crew_nohooks` is consistently the most expensive strategy per run
  (parallel subagents each re-read a quarter of the module against a mostly
  cold cache) but is not consistently the most precise.

