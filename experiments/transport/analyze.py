"""Summarize the transport ledgers into markdown tables on stdout."""

import collections
import glob
import json
import re
import statistics

from common import RESULTS


def load(name):
    path = RESULTS / f"{name}.jsonl"
    if not path.exists():
        return []
    return [json.loads(l) for l in open(path)]


def med(xs):
    xs = [x for x in xs if x is not None]
    return round(statistics.median(xs)) if xs else None


def t1():
    rows = load("t1")
    print("## T1: one `claude -p` process per turn\n")
    by = collections.defaultdict(list)
    for r in rows:
        if r.get("turn") and r.get("units") is not None:
            by[(r["variant"], r["run"])].append(r)
    print("| variant | run | turn 1 units (write) | turn 2 units (write) | turn 10 units | total units, 10 turns | median s/turn |")
    print("|---|---|---|---|---|---|---|")
    for (v, run), rs in sorted(by.items()):
        rs.sort(key=lambda r: r["turn"])
        t = {r["turn"]: r for r in rs}
        def cell(n):
            r = t.get(n)
            return f"{r['units']:.0f} ({r['cache_write']})" if r else "-"
        print(f"| {v} | {run} | {cell(1)} | {cell(2)} | {t[10]['units']:.0f} | "
              f"{sum(r['units'] for r in rs):.0f} | {statistics.median(r['wall_s'] for r in rs):.1f} |")
    for r in rows:
        if r.get("phase") in ("killed", "kill_point"):
            print(f"\ncrash: {json.dumps({k: r.get(k) for k in ('phase','killed_on','after_s','alive_at_kill','transcript_lines_before','transcript_lines_after')})}")
        if r.get("phase", "").startswith("usage"):
            print(f"\n{r['phase']}: week {r.get('week_pct')}%, session {r.get('session_pct')}%")
    print()


def t2():
    print("## T2: one long-lived stream-json worker\n")
    print("| run | turn 1 | turn 10 | 10-turn total | median s/turn | after 10 min idle | bigline longest | /compact s | after compact | resume after kill |")
    print("|---|---|---|---|---|---|---|---|---|---|")
    types = collections.Counter()
    samples = []
    for path in sorted(glob.glob(str(RESULTS / "t2.run*.jsonl"))):
        rows = [json.loads(l) for l in open(path)]
        t = {r.get("turn"): r for r in rows if r.get("turn") is not None}
        nums = [t[i] for i in range(1, 11) if i in t]
        def u(k):
            r = t.get(k)
            return f"{r['units']:.0f} (w {r['cache_write']})" if r and r.get("units") is not None else "-"
        run = nums[0]["run"] if nums else "?"
        print(f"| {run} | {u(1)} | {u(10)} | {sum(r['units'] for r in nums):.0f} | "
              f"{statistics.median(r['wall_s'] for r in nums):.1f} | {u('after_idle')} | "
              f"{t.get('bigline', {}).get('longest_line')} | {t.get('compact', {}).get('wall_s')} | "
              f"{u('after_compact')} | {u('after_crash_resume')} |")
        for r in rows:
            for k in ("types", "types_after_resume"):
                types.update(r.get(k) or {})
            samples += r.get("limit_samples") or []
    print(f"\nEvent types seen (test R): {dict(types)}")
    print(f"Events mentioning rate limits: {len(samples)}")
    if samples:
        print("First sample shape: `" + json.dumps(samples[0])[:600] + "`")
    print()


def t3():
    print("## T3: a background session woken over its socket\n")
    for r in load("t3"):
        if r.get("step") == "inject":
            print(f"- mode {r['mode']}: woke={r['woke']} after {r['woke_after_s']} s, "
                  f"turn units {r.get('units')}, context {r.get('context')}, socket reply {r.get('socket_reply')!r}")
    print()


def t4():
    print("## T4: tools kelpie serves\n")
    rows = load("t4")
    for part in ("t4a", "t4b"):
        for step in ("floor_without", "floor_with", "trivial", "tool_call"):
            rs = [r for r in rows if r.get("part") == part and r.get("step") == step]
            if rs:
                print(f"- {part} {step}: median context {med(r.get('context') for r in rs)}, "
                      f"median units {med(r.get('units') for r in rs)}, median wall {med(r.get('wall_s') for r in rs)} s, "
                      f"answers {sorted(set(str(r.get('result_head')) for r in rs))}, "
                      f"turns {sorted(set(str(r.get('num_turns')) for r in rs))}")
    print()


def t5():
    print("## T5: MCP channel push\n")
    for r in load("t5"):
        print(f"- woke={r.get('woke')}, events while idle {r.get('events_while_idle')}, stderr {r.get('stderr_tail')!r}")
    print()


def l_lease():
    print("## L: lease round trip through shep\n")
    rows = load("l")
    g = [r for r in rows if r.get("step") == "koji_granted"]
    k = [r for r in rows if r.get("step") == "reclaimed_to_reactmap"]
    print(f"- want to grant, runner side: {[round(r['runner_want_to_grant_ms'], 1) for r in g if r.get('runner_want_to_grant_ms')]} ms")
    print(f"- want to grant, from the driver's CLI call: {[round(r['driver_to_grant_ms']) for r in g if r.get('driver_to_grant_ms')]} ms")
    print(f"- holder killed to next grant: {[round(r['kill_to_grant_ms'], 1) for r in k if r.get('kill_to_grant_ms')]} ms")
    dropped, last, lagged = 0, None, 0
    path = RESULTS / "l.dog.jsonl"
    if path.exists():
        for line in open(path):
            d = json.loads(line)
            if d.get("event") == "lagged":
                lagged += 1
                dropped += int(re.search(r"count: (\d+)", d["detail"]).group(1))
            if d.get("event") == "flood_seen":
                last = d["value"]
    print(f"- flood of 5000 metrics: {dropped} events dropped over {lagged} lag reports, last value seen {last}")
    for r in rows:
        if r.get("step") == "trigger_dog":
            print(f"- `shep trigger` to an adopted dog: `{(r.get('stdout') or '').strip().splitlines()[-1] if r.get('stdout') else r.get('stderr')}`")
    print()


if __name__ == "__main__":
    for f in (t1, t2, t3, t4, t5, l_lease):
        f()
