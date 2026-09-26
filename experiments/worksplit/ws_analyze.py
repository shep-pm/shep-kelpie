"""Summarize the compact work-split runs into a markdown table on stdout."""

import collections
import json
import os
import re
import statistics

LEDGER = os.path.join(os.path.dirname(os.path.abspath(__file__)), "../transport/results/ws_mini.jsonl")
DEFAULT_MODULE = "crates/shep-daemon/src/supervisor"


PROJECT_DIR = os.path.expanduser("~/.claude/projects/-Users-rin--kelpie-repos-shep")
MARKERS = {"inline": "Do all the reading yourself", "crew": "Split the files into four groups",
           "crew_nohooks": "Split the files into four groups", "phased": "A previous session already reviewed"}


def transcript_findings(module, strategy, finished_ts, final_text):
    """The last FINDINGS report in the transcript of the matching run, found by
    its prompt and a finish time within two minutes of the ledger line."""
    import datetime
    import glob
    target = datetime.datetime.fromisoformat(finished_ts).timestamp()
    best = None
    for path in glob.glob(os.path.join(PROJECT_DIR, "*.jsonl")):
        mtime = os.path.getmtime(path)
        if abs(mtime - target) > 120:
            continue
        with open(path) as f:
            text = f.read()
        if MARKERS[strategy] not in text or module not in text:
            continue
        # The run's own transcript: its final message is the ledger's result.
        tail = json.dumps(final_text[-120:])[1:-1] if final_text else None
        if not tail or tail not in text:
            continue
        reports = [m for m in re.findall(r'"text":"((?:[^"\\]|\\.)*FINDINGS:\s*\d+(?:[^"\\]|\\.)*)"', text)]
        if reports and (best is None or abs(mtime - target) < best[0]):
            best = (abs(mtime - target), json.loads('"' + reports[-1] + '"'))
    return best[1] if best else ""


def main():
    runs = collections.defaultdict(lambda: {"units": 0, "wall": 0.0, "turns": 0, "steps": [], "report": "",
                                            "findings": None, "subagents": 0, "by_model": collections.Counter()})
    for line in open(LEDGER):
        r = json.loads(line)
        key = (r.get("module") or DEFAULT_MODULE, r["strategy"], r["run"])
        agg = runs[key]
        agg["units"] += r.get("units_all_models") or 0
        agg["wall"] += r.get("wall_s") or 0
        agg["turns"] += r.get("num_turns") or 0
        agg["steps"].append(r["step"])
        agg["subagents"] += ((r.get("subagent_stats") or {}).get("spawned") or 0)
        agg["by_model"].update(r.get("units_by_model") or {})
        if r["step"] in ("all", "phase_b"):
            agg["report"] = r.get("report") or ""
            if "FINDINGS" not in agg["report"]:
                agg["report"] = transcript_findings(key[0], r["strategy"], r["ts"], r.get("report") or "") or agg["report"]
            m = re.search(r"FINDINGS:\s*(\d+)", agg["report"])
            agg["findings"] = int(m.group(1)) if m else None

    print("| module | strategy | run | units (all models) | wall s | turns | subagents | findings | files cited |")
    print("|---|---|---|---|---|---|---|---|---|")
    medians = collections.defaultdict(list)
    for (module, strategy, run), a in sorted(runs.items()):
        files = len(set(re.findall(r"([\w/]+\.rs):\d+", a["report"])))
        complete = a["findings"] is not None and (strategy != "phased" or "phase_b" in a["steps"])
        mark = "" if complete else " (incomplete)"
        print(f"| {module.split('/')[-1]} | {strategy}{mark} | {run} | {a['units']:,} | {a['wall']:.0f} | "
              f"{a['turns']} | {a['subagents']} | {a['findings']} | {files} |")
        if complete:
            medians[(module, strategy)].append((a["units"], a["wall"], a["findings"], files))
    print("\n| module | strategy | complete runs | median units | median wall s | median findings |")
    print("|---|---|---|---|---|---|")
    for (module, strategy), xs in sorted(medians.items()):
        print(f"| {module.split('/')[-1]} | {strategy} | {len(xs)} | {statistics.median(x[0] for x in xs):,.0f} | "
              f"{statistics.median(x[1] for x in xs):.0f} | {statistics.median(x[2] for x in xs)} |")


if __name__ == "__main__":
    main()
