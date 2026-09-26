"""Shared plumbing for the scripts that drive the local Ollama box.

Imported by model-calibrate/calibrate.py and by hunt-screen.py. Both need the
same four things and there is one right answer for each, so they live here
rather than in whichever script grew them first:

* where the box is, read from a file that is never printed
* the GPU lock, which is the same mkdir lock qwen-review.sh takes
* a structured-output chat request
* reading JSON back out of an answer that may be wrapped in prose

The lock in particular has to be one implementation. Two processes agreeing
about a lock's path but disagreeing about its stale rule is a lock that lets
both of them through.
"""
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

NUM_CTX = int(os.environ.get("LOCAL_LLM_NUM_CTX", "65536"))
LOCK = Path(os.environ.get("TMPDIR", "/tmp")) / "qwen-review" / "gpu.lock"
LOCK_WAIT = int(os.environ.get("QWEN_LOCK_WAIT", "3600"))


def ollama_base():
    """The Ollama endpoint, from $LOCAL_LLM_HOST or ~/.claude/.local-llm-host.

    # Errors

    Exits with a message naming the file when neither is set. The host itself
    is never printed: it is a LAN address and these scripts run in logs and
    transcripts that get shared.
    """
    host = os.environ.get("LOCAL_LLM_HOST", "")
    hostfile = Path.home() / ".claude" / ".local-llm-host"
    if not host and hostfile.exists():
        host = hostfile.read_text().strip()
    if not host:
        print(f"no host: set $LOCAL_LLM_HOST or write it to {hostfile}", file=sys.stderr)
        sys.exit(1)
    return os.environ.get("LOCAL_LLM_OLLAMA", f"http://{host}:11434")


class GpuLock:
    """The mkdir lock qwen-review.sh takes, same path and same stale rule."""

    def __init__(self, what):
        self.what = what

    def __enter__(self):
        LOCK.parent.mkdir(parents=True, exist_ok=True)
        waited, told = 0, False
        while True:
            try:
                LOCK.mkdir()
                break
            except FileExistsError:
                holder = (LOCK / "pid").read_text().strip() if (LOCK / "pid").exists() else ""
                alive = holder.isdigit() and subprocess.run(
                    ["kill", "-0", holder], capture_output=True).returncode == 0
                if not alive and waited >= 15:
                    subprocess.run(["rm", "-rf", str(LOCK)])
                    continue
                if waited >= LOCK_WAIT:
                    print(f"gave up after {waited}s waiting for the GPU lock", file=sys.stderr)
                    sys.exit(1)
                if not told:
                    what = (LOCK / "what").read_text().strip() if (LOCK / "what").exists() else "?"
                    print(f"waiting for the GPU, held by pid {holder or '?'} running {what}",
                          file=sys.stderr)
                    told = True
                time.sleep(15)
                waited += 15
        (LOCK / "pid").write_text(f"{os.getpid()}\n")
        (LOCK / "what").write_text(f"{self.what}\n")
        return self

    def __exit__(self, *exc):
        subprocess.run(["rm", "-rf", str(LOCK)])


def chat(base, model, prompt, schema, think, max_tokens):
    """One structured-output request. `schema` is passed as Ollama's `format`."""
    body = {
        "model": model, "stream": False,
        "messages": [{"role": "user", "content": prompt}],
        "format": schema,
        "options": {"num_ctx": NUM_CTX, "num_predict": max_tokens},
    }
    if think is not None:
        body["think"] = think
    request = urllib.request.Request(f"{base}/api/chat", data=json.dumps(body).encode(),
                                     headers={"content-type": "application/json"})
    with urllib.request.urlopen(request, timeout=1800) as response:
        return json.loads(response.read())


def unload(base, model):
    """Evict one model, so the VRAM goes back when a run ends."""
    request = urllib.request.Request(f"{base}/api/generate",
                                     data=json.dumps({"model": model, "keep_alive": 0}).encode(),
                                     headers={"content-type": "application/json"})
    urllib.request.urlopen(request, timeout=60).read()


def unload_others(base, model):
    """Evict every other loaded model, so the one in use gets the whole card.

    Left to itself Ollama may split a newcomer across GPU and CPU rather than
    evict an idle model, and then the speed measures the neighbour. Call it
    under the GPU lock, so no other round is mid-request on what gets unloaded.
    """
    try:
        with urllib.request.urlopen(f"{base}/api/ps", timeout=10) as response:
            running = json.loads(response.read()).get("models", [])
        for entry in running:
            if entry.get("name") != model:
                unload(base, entry["name"])
    except (urllib.error.URLError, OSError, ValueError, KeyError):
        pass


def parse_json(text):
    """The answer as an object, or None. Falls back to the first {...} in prose."""
    import re
    try:
        return json.loads(text)
    except (json.JSONDecodeError, TypeError):
        pass
    match = re.search(r"\{.*\}", text or "", re.S)
    if match:
        try:
            return json.loads(match.group(0))
        except json.JSONDecodeError:
            return None
    return None
