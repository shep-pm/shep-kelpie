---
# CodeRabbit, the pull request review bot, as a reviewer: listed in a
# project's `agents.reviewers`, it reads the pull request once a pass, in
# its place in the list, and the worker gets one fix turn for the threads
# it leaves open. Its window is its account's: one review an hour until a
# review footer states more. Its free plan reviews public repos only, so a
# project whose repo GitHub marks private cannot list it. `rounds: 1` would
# let it read each work item's pull request once, whatever its passes. A bot
# writes its own prompt, so the body stays empty.
role: reviewer
harness: bot
bot: coderabbit
reviews: 1
hours: 1
---
