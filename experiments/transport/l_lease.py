"""L: a GPU lease round trip between project runners and the dog, through shep.

Usage: python3 l_lease.py [ROUNDS]
Needs the pinned shep at ~/.kelpie/bin/shep and a built lease-dog.
"""

import json
import os
import subprocess
import sys
import time

from common import HERE, RESULTS, ledger

TEST = "l"
SHEP = os.path.expanduser("~/.kelpie/bin/shep")
ENV = {**os.environ, "SHEP_HOME": os.path.expanduser("~/.kelpie/shep")}
DOG = str(HERE / "lease-dog/target/debug/lease-dog")
FLOCKFILE = HERE / "l_flockfile.toml"


def shep(*args, check=False):
    proc = subprocess.run([SHEP, *args], capture_output=True, text=True, env=ENV, cwd=str(HERE), timeout=60)
    if check and proc.returncode != 0:
        raise RuntimeError(f"shep {' '.join(args)} failed: {proc.stderr.strip()}")
    return proc


def bodies(proc):
    """Every string under a `body` key in shep's JSON output."""
    found = []

    def walk(v):
        if isinstance(v, dict):
            for k, x in v.items():
                if k == "body" and isinstance(x, str):
                    found.append(x)
                else:
                    walk(x)
        elif isinstance(v, list):
            for x in v:
                walk(x)

    try:
        walk(json.loads(proc.stdout))
    except json.JSONDecodeError:
        pass
    return found


def status(name):
    for body in bodies(shep("trigger", name, "status", "--format", "json")):
        try:
            return json.loads(body)
        except json.JSONDecodeError:
            continue
    return None


def wait_holding(name, timeout=15.0):
    t0 = time.monotonic()
    while time.monotonic() - t0 < timeout:
        st = status(name)
        if st and st.get("holding"):
            return st, time.monotonic() - t0
        time.sleep(0.05)
    return status(name), None


def round_trip(n):
    shep("start", "koji")
    shep("start", "reactmap")
    time.sleep(1.5)

    t0 = time.time() * 1000
    shep("trigger", "koji", "want", "gpu")
    st, waited = wait_holding("koji")
    ledger(TEST, {"round": n, "step": "koji_granted", "driver_wait_s": waited,
                  "runner_want_to_grant_ms": (st["t_grant"] - st["t_want"]) if st and st.get("t_grant") else None,
                  "driver_to_grant_ms": (st["t_grant"] - t0) if st and st.get("t_grant") else None})

    shep("trigger", "reactmap", "want", "gpu")
    time.sleep(1.0)
    queued = status("reactmap")
    ledger(TEST, {"round": n, "step": "reactmap_queued", "holding": queued and queued.get("holding")})

    t_kill = time.time() * 1000
    shep("signal", "koji", "SIGKILL")
    st, waited = wait_holding("reactmap")
    ledger(TEST, {"round": n, "step": "reclaimed_to_reactmap", "driver_wait_s": waited,
                  "kill_to_grant_ms": (st["t_grant"] - t_kill) if st and st.get("t_grant") else None})

    shep("trigger", "reactmap", "release", "gpu")
    time.sleep(0.5)


def main():
    rounds = int(sys.argv[1]) if len(sys.argv) > 1 else 3
    start = shep("start", str(FLOCKFILE))
    ledger(TEST, {"step": "flock_started", "rc": start.returncode, "stderr": start.stderr[-300:]})
    time.sleep(2)
    dog_log = open(RESULTS / "l.dog.jsonl", "a")
    dog = subprocess.Popen([DOG], stdout=dog_log, stderr=subprocess.STDOUT, env=ENV)
    time.sleep(1.5)
    try:
        for n in range(1, rounds + 1):
            round_trip(n)
        flood = shep("trigger", "reactmap", "flood", "5000", "--format", "json")
        ledger(TEST, {"step": "flood_sent", "bodies": bodies(flood)})
        time.sleep(5)
    finally:
        dog.terminate()
        dog.wait(timeout=10)
        dog_log.close()

    # Can a trigger reach a dog? Adopt the dog-side program and try.
    adopt = shep("adopt", DOG)
    ledger(TEST, {"step": "adopt", "rc": adopt.returncode,
                  "stdout": adopt.stdout[-400:], "stderr": adopt.stderr[-400:]})
    time.sleep(2)
    trig = shep("trigger", "lease-dog", "status")
    ledger(TEST, {"step": "trigger_dog", "rc": trig.returncode,
                  "stdout": trig.stdout[-400:], "stderr": trig.stderr[-400:]})
    rehome = shep("rehome", "lease-dog")
    ledger(TEST, {"step": "rehome", "rc": rehome.returncode, "stderr": rehome.stderr[-200:]})
    shep("stop", "all")


if __name__ == "__main__":
    main()
