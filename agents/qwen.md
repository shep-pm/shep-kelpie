---
# The maintainer's qwen-review script, which runs the local model itself, so
# kelpie holds nothing around it. `shep kelpie add`
# writes this file only where the script is installed. A command keeps the
# README's contract and writes its own prompt, so the body stays empty.
role: reviewer
harness: command
command: ~/.claude/scripts/qwen-review.sh
description: Reads a diff for defects with the maintainer's local qwen-review script, as a first pass.
tier: light
---
