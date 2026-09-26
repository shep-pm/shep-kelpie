"""A stand-in project runner for test L: a sheep that speaks the shepherd channel.

Actions (via `shep trigger <runner> <action> [params]`):
  want <res>     raise the running total wants.<res> and ask for a lease
  status         JSON: totals, whether it holds a lease, want and grant times
  grant <res>    the dog grants the lease
  release <res>  give the lease back; raises releases.<res>
  flood <n>      emit n flood.count metrics as fast as possible
"""

import json
import os
import sys
import threading
import time

FD = int(os.environ.get("SHEP_CHANNEL_FD", "3"))
out = os.fdopen(FD, "w", buffering=1)
inp = os.fdopen(os.dup(FD), "r")
lock = threading.Lock()
state = {"wants": {}, "releases": {}, "holding": None, "t_want": None, "t_grant": None}


def send(msg):
    with lock:
        out.write(json.dumps(msg) + "\n")
        out.flush()


def metric(name, value):
    send({"kind": "metric", "name": name, "value": value})


def reply(action, body, msg_id):
    send({"kind": "action-reply", "action": action, "body": body, "id": msg_id})


def handle(msg):
    name, params, msg_id = msg.get("name"), (msg.get("params") or "").strip(), msg.get("id")
    now_ms = time.time() * 1000
    if name == "want":
        res = params or "gpu"
        state["wants"][res] = state["wants"].get(res, 0) + 1
        state["t_want"] = now_ms
        reply(name, "ok", msg_id)
        metric(f"wants.{res}", state["wants"][res])
    elif name == "status":
        reply(name, json.dumps(state), msg_id)
    elif name == "grant":
        state["holding"] = params or "gpu"
        state["t_grant"] = now_ms
        reply(name, "ok", msg_id)
        print(f"granted {state['holding']} after {now_ms - (state['t_want'] or now_ms):.0f} ms", flush=True)
    elif name == "release":
        res = params or "gpu"
        state["holding"] = None
        state["releases"][res] = state["releases"].get(res, 0) + 1
        reply(name, "ok", msg_id)
        metric(f"releases.{res}", state["releases"][res])
    elif name == "flood":
        n = int(params or "1000")
        reply(name, f"sending {n}", msg_id)
        for i in range(1, n + 1):
            metric("flood.count", i)
    else:
        reply(name, f"unknown action {name}", msg_id)


def main():
    send({"kind": "ready"})
    print(f"runner up, channel fd {FD}", flush=True)
    for line in inp:
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            continue
        if msg.get("kind") == "action":
            handle(msg)
        elif msg.get("kind") == "shutdown":
            break
    sys.exit(0)


if __name__ == "__main__":
    main()
