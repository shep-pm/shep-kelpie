---
# cubic, the pull request review bot, as a reviewer: listed in a project's
# `agents.reviewers`, it reads the pull request once a pass, in its place in
# the list, and the worker gets one fix turn for the threads it leaves open.
# Its window is its account's: the free plan gives a repo GitHub marks
# private 20 reviews a month, and a public repo draws on a fair-use pool of
# reviewed lines. A bot writes its own prompt, so the body stays empty.
role: reviewer
harness: bot
bot: cubic
reviews: 20
hours: 720
---
