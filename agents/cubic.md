---
# cubic, the pull request review bot, as a reviewer: a candidate for every
# review pass whose `agents.allow` holds it. The agent board picks each
# round's reviewer toward `reviews.need`. If picked, it reads once a pass,
# and the worker gets one fix turn for the threads it leaves open.
# Its window is its account's: the free plan gives a repo GitHub marks
# private 20 reviews a month, and a public repo draws on a fair-use pool of
# reviewed lines. Kelpie passes over a closed window. If it has never
# reviewed on the repo and does not answer, kelpie leaves it out of picks
# for 7 days, bringing it back sooner if it reviews. A bot writes its own
# prompt, so the body stays empty.
role: reviewer
harness: bot
bot: cubic
reviews: 20
hours: 720
description: Reviews the pull request on GitHub as the cubic bot.
tier: standard
---
