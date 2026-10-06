---
# Codex, the ChatGPT Codex app's pull request review, as a reviewer: listed
# in a project's `agents.reviewers`, it reads the pull request once a pass,
# in its place in the list, and the worker gets one fix turn for the threads
# it leaves open. Its window is the weekly allowance the plan gives code
# reviews. `reviews_on_ready: true` says it reviews a pull request when it
# leaves draft, as the repo's Codex settings may have it: then marking ready
# is its summon, with no comment, and it is listed before any other bot.
# Leave it off unless Codex's automatic reviews are on. A bot writes its own
# prompt, so the body stays empty.
role: reviewer
harness: bot
bot: codex
reviews: 10
hours: 168
reviews_on_ready: false
---
