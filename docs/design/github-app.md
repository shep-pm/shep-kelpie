# The GitHub App

Per-user App: tokens, posting, answering rulings on GitHub. Who answers a ruling is in `who-answers.md`.

## App and tokens

- Rulings, notices, rounds move to GitHub via an App each user registers (GitHub never notifies of your own comments); ntfy stays optional. One App per owner via manifest flow (shared App would need key shipped or hosted). #395
- App holds contents, workflows, issues, pull requests write + checks, statuses, metadata read; merging stays on user's `gh`, App never merges. Kelpie's own token: issues + PRs only, can't push. #395, #396
- Covered worker call gets a token for one repo, `contents` (and `workflows`) only; refused if GitHub grants more (guard parsing can't be complete; credential is the boundary). #420, #462
- Token reaches worker as a `gh` config file, not a variable (every process shows those); git credential helpers emptied. #420, #429
- Covered worker writes `pull-request.md`; kelpie opens and edits the PR, refusing a non-conventional title. Files read/written without following links. #420, #439, #898, `crates/worker_folder/`
- A fork worker only commits; kelpie pushes. #633, `contribution-push.md`
- Workers push any branch: `doctor` says protect `main`. Kelpie reads covered workers' failed CI logs (3 runs, 6000 bytes each, noise dropped, a log over that led by its failure lines and their next 15 lines) rather than widen token. #420
- Worker token renewed every 30 min (1h life). `runner/login_refresh.rs`
- `shep kelpie github setup` registers an App from 127.0.0.1 with random `state`, refusing other hosts. #396
- App under `<kelpie home>/github/<owner>/`, key 0600 (an owner-only ACL on Windows, `docs/design/windows.md`), never in argument or variable (like TOTP secret). #396
- JWTs signed with `openssl`, sent by `curl` config on stdin (not in process list, no RSA crate). #396
- Tokens cached per repo until 5 min before expiry; one mint flight per repo. #397
- `doctor` checks each repo's token; no App = `unsure`. #396

## Reading

- Board, PR, review and check reads (`issue`, `ready_issues`, `open_pull_requests`, `pull_request`, `reviewed`, `review_bot`) go through the App's reading token where it covers the repo: the App's own rate budget takes them off the login's. Viewer, access checks, `sub_issues`, `comments_since`, `thread_replies_since`, failed run logs and every write stay on the login. `sub_issues` stays: a parent's sub-issues can sit in another repo, and a short list would close the parent early. #582
- Same `gh` commands as the login's, run with `GH_CONFIG_DIR` on a folder of kelpie's own (`<kelpie home>/reads/<owner>--<repo>/gh`) holding the token, and `GH_TOKEN` and its kin unset (an environment token would win). Not curl/GraphQL: custom queries can't be checked live from a worker. `adapters/gh/app.rs`. #582
- Reading token is its own grant (`Grant::Reader`: issues, pull requests, checks, statuses read), kept apart from the posting token; an App without checks/statuses 422s and falls back to `ReaderCore`. The token's returned `permissions` say what it holds. `github/tokens.rs`. #582
- `RateHeld` routes (`crates/core/src/ports/rate_limit/app_reads/mod.rs`): a read the token lacks a permission for, or that answers "Resource not accessible by integration", goes to the login and says the permission, once per repo and read. A refused (repo, read) is remembered and not asked of the App again until the repo's token is minted again or an hour passes; a timeout or network failure is not remembered. App reads take no turn in the login's pace; the App's used-up answer sends its reads to the login until it resets and holds nothing else. #582
- Forge ledger `by` (`login`|`app`) records whose budget a call spent; `usage --forge` splits by identity. `doctor` has a `reads` line. #582

## Posting

- Kelpie's posts go through `Voice` port as the App on a covered repo, never falling back to maintainer's login (notifies no one). Failed post never fails its step; told once in log. #397
- Summons, PR labels, merges, ready marks stay on maintainer's `gh` (bots answer a person; nothing shows they answer an App). #397
- Ruling comment mentions `git.maintainer` (else `gh` login) and says how to answer. Own review rounds post on the PR (bots post theirs). #397, #376
- Ruling comments share one card format so reactions and ticks read same options. #464, `runner/voice/card/`
- Ruling settled elsewhere has its comment edited. #472
- Worker posts no comment. Guard reads `gh` as gh does, refuses shell expansion. #418, `crates/core/src/guard/gh.rs`
- Findings post as one comment-only review (`Voice::review`) with a comment per line, once per head; `publish` hooks see body and every comment. A bot's findings never post. #627, `contribution-pass.md`

## Check runs

- The App shows a work item's gates on its pull request's head as check runs (`Voice::check_run`, `update_check_run`), written with `Grant::Checks` (`checks: write` alone), so an installation without it still posts comments. `runner/checks/`. #1637
- `kelpie / review` runs through `Phase::Review` and succeeds when the head leaves it. `kelpie / ruling` runs while a ruling waits on the head, titled with its question; it succeeds on a yes and fails on a no. A gate a head never reaches has no check.
- A head the item leaves (sent back to the worker, superseded by a push, ended or dropped) has its running checks closed: `skipped`, except `kelpie / ruling` and `kelpie` fail only where the answer to the ruling was a no or a rework. A comment that withdraws a ruling is not a no. A restart forgets what was posted, so it closes nothing posted before it.
- `kelpie` runs until `Phase::Merge` on the head, then succeeds, and fails where the ruling failed. Every gated head has one, so it is the only check a ruleset may require. A gate check is a view, not a gate.
- Checks are a view kelpie writes and never reads back; the state file is the record. Ids and last posted state are in memory per work item and head, so a restart posts each check once more. Re-run sends a webhook kelpie never gets, so each summary says `shep kelpie rework <pr>`.
- A refusal for a missing permission turns check runs off for the repo until the runner restarts, logged once; any other error is tried again on the next change. `doctor` has a `check runs` line.

## Answering on GitHub

- Answered by reply in thread, read once per board poll without runner lock. #398
- Only the mentioned login with write access answers; merge answer names its id (no conversational yes merges; strangers can't make kelpie speak). #398
- Kelpie's own `gh` comments recorded when posted, never read as answers (same login). #398
- Reading starts from GitHub's timestamp on the ruling comment, never local clock. #398
- First answer from any channel wins; ruling watched 1h more to say it's answered. #398
- Thumbs up/down from mentioned login answers yes/no ruling; tick counts only if that login or the App made every edit (GitHub doesn't say who ticked). #464
