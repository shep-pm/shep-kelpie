# Review bots and the GitHub App

Measured 2026-10-12 on shep-pm/kelpie-scratch (public) for shep-pm/shep-kelpie#643, on the free plans of CodeRabbit and cubic and with Codex connected to the maintainer's account. The App is `kelpie-shep-pm[bot]`.

## A pull request the App opened (kelpie-scratch#17)

- CodeRabbit skipped it on open, with the status "Review skipped: bot user not eligible for review". The App's `@coderabbitai full review` comment did not start one either: the status stayed skipped.
- cubic refused the App's `@cubic-dev-ai review` within 4 seconds: "Couldn't start the review: please upgrade to a paid plan to review bot-authored PRs." It refused the maintainer's summon on the same pull request with the same words, so the limit follows the author, not the summoner.
- Codex answered the App's `@codex review` with "To use Codex here, create a Codex account and connect to github". The maintainer's `@codex review` on the same pull request drew a review 3.5 minutes later.

## A pull request the maintainer opened, summoned by the App (kelpie-scratch#18)

- cubic accepted the App's summon ("I have started the AI code review") and posted its review 39 seconds later.
- Codex refused the App the same way as on #17.
- The App adding CodeRabbit's `review please` label did not start a review. The status read "Review skipped: manual review required for this OSS repository".

## Check runs, reviews and the merge on a pull request the App opened (kelpie-scratch#28)

Measured 2026-11-03, after the App was given Checks (write). The ruleset on `main` asked for no approvals and six Actions checks.

- The workflows ran on the App's pull request and all six required checks passed. A branch in the same repo needs no approval to run them.
- The maintainer's Request changes read `reviewDecision: CHANGES_REQUESTED` and `mergeStateStatus: BLOCKED`, through the App's token as through the maintainer's. The maintainer's Approve then left `reviewDecision` empty, since the ruleset asks for no approvals, so the latest review is read from `reviews`, not from `reviewDecision`. The App read the maintainer as `MEMBER`.
- The App's token created check runs with `POST /repos/{owner}/{repo}/check-runs` and completed them with `PATCH .../check-runs/{id}`: `in_progress`, then `success`, and `skipped` as a conclusion. Each shows its `output.title` beside its name on the pull request. `fixtures/gh-check-run.json` is one, read back with `GET`.
- With a `kelpie` check from the App (`integration_id` 5264246) added to the ruleset's required checks, the pull request read `BLOCKED` while it ran and `CLEAN` once it passed. A new push left the new head with no kelpie check and `BLOCKED` until one was posted on it.
- `POST .../check-runs/{id}/rerequest`, what the Re-run button sends, answered 201 and changed nothing: it only sends the App a `check_run` webhook.
- The maintainer's login merged it with a merge commit under the ruleset.
- CodeRabbit skipped it with "Bot user detected", and answered "Review skipped" to the maintainer's `@coderabbitai review` and `@coderabbitai full review` alike. The App's `review please` label and the App's summon drew nothing. Its docs describe only `reviews.auto_review.ignore_usernames`, which a summon overrides, and not this skip.
- cubic has reviewed no pull request on kelpie-scratch, the maintainer's included, so this run says nothing about it.

## What follows

- On the free plans, a pull request authored by the App loses cubic and CodeRabbit outright, so kelpie opens pull requests as the maintainer. On 2026-11-03 CodeRabbit refused even the maintainer's summon on one.
- Only cubic answers a summon from the App, and Codex only answers a person linked to a Codex account. Summons stay on the maintainer's login.
- Check runs do not depend on who opened the pull request, so the App can post kelpie's gates as checks on the maintainer's. A check made required blocks every head until kelpie posts it, so only a check kelpie posts on every head may be required.
