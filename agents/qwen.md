---
# The maintainer's qwen-review script, which runs the local model and takes
# the GPU lock itself, so kelpie holds no lease around it. `shep kelpie add`
# writes this file only where the script is installed. A command keeps the
# README's contract and writes its own prompt, so the body stays empty.
role: reviewer
harness: command
command: ~/.claude/scripts/qwen-review.sh
---
