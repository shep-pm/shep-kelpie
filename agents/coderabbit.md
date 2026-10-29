---
# CodeRabbit, the pull request review bot, as a reviewer: a candidate for
# every review pass whose `agents.allow` holds it. The agent board picks
# each round's reviewer toward `reviews.need`. If picked, it reads once
# a pass, and the worker gets one fix turn for the threads
# it leaves open. Its window is its account's: one review an hour until a
# review footer states more. Its free plan reviews public repos only, so a
# project whose repo GitHub marks private cannot name it in `agents.allow`.
# With no agents named, kelpie passes it over there. A closed window is
# passed over too. If it has never reviewed on the repo and does not answer,
# or the repo is not public, kelpie leaves it out of picks for 7 days,
# bringing it back sooner if it reviews. `rounds: 1` would
# let it read each work item's pull request once, whatever its passes. A bot
# writes its own prompt, so the body stays empty.
role: reviewer
harness: bot
bot: coderabbit
reviews: 1
hours: 1
description: Reviews the pull request on GitHub as the CodeRabbit bot, for a public repo.
tier: standard
---
