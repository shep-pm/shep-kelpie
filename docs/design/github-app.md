# The GitHub App

Per-user App: tokens, posting, answering rulings on GitHub. Who answers a ruling is in `who-answers.md`, and the gate check runs a work item shows on its pull request are in `check-runs.md`.

## App and tokens

- Rulings, notices, rounds move to GitHub via an App each user registers (GitHub never notifies of your own comments); ntfy stays optional. One App per owner via manifest flow (shared App would need key shipped or hosted). #395
- App holds contents, workflows, issues, pull requests write + checks, statuses, metadata read; merging stays on user's `gh`, App never merges. Kelpie's own token: issues + PRs only, can't push. #395, #396
- Covered worker call gets a token for one repo, `contents` (and `workflows`) only; refused if GitHub grants more (guard parsing can't be complete; credential is the boundary). #420, #462
- Token reaches worker as a `gh` config file, not a variable (every process shows those); git credential helpers emptied. #420, #429
- Covered worker writes `pull-request.md`; kelpie opens and edits the PR, refusing a non-conventional title. Files read/written without following links. #420, #439, #898, `crates/worker_folder/`
- Kelpie joins a body's soft-wrapped lines before it opens or edits (GitHub breaks a body at every newline, so one wrapped at 80 columns shows as a narrow column). A line starting a list item, heading, quote, table row or thematic break, one after a blank line, and one after a heading, table row or hard break each start a line of their own; fenced and indented code is kept as written. The `wording` skill is unchanged: a rule there is no gate. `runner/paragraphs/`. #1640
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
- A 5xx is the forge failing and not the ask, and `ForgeError::passes` says so from the status `gh` printed (`ports/forge_error.rs`). Only the issue read that opens a worker's turn is asked again on it: held back and retried at the next look, with no `turn-failed` ruling (`runner/turn/mod.rs`). Every other read and write fails as before, since a write that answered 5xx may have landed. A used-up limit still wins, read as `Limited` before any status. #1715

## Posting

- Kelpie's posts go through `Voice` port as the App on a covered repo, never falling back to maintainer's login (notifies no one). Failed post never fails its step; told once in log. #397
- Summons, PR labels, merges, ready marks stay on maintainer's `gh` (bots answer a person; nothing shows they answer an App). #397
- Ruling comment mentions `git.maintainer` (else `gh` login) and says how to answer. Own review rounds post on the PR (bots post theirs). #397, #376
- Ruling comments share one card format so reactions and ticks read same options. #464, `runner/voice/card/`
- A yes-or-no ruling's card names each option once, under how it is answered: its table's first column is the thumbs and the reply, so the comment carries no separate list of answers, and its pick reads by name. A merge ruling's and a question's table stays lettered, since the project manager votes by key and a merge's options are ticked boxes. `brief` keeps the lettered table for every kind. #1750, `runner/voice/card/`
- Ruling settled elsewhere has its comment edited. #472
- Worker posts no comment. Guard reads `gh` as gh does, refuses shell expansion. #418, `crates/core/src/guard/gh.rs`
- Findings post as one comment-only review (`Voice::review`) with a comment per line, once per head; `publish` hooks see body and every comment. A bot's findings never post. #627, `contribution-pass.md`

## Answering on GitHub

- Answered by reply in thread, read once per board poll without runner lock. #398
- Only the mentioned login with write access answers; merge answer names its id (no conversational yes merges; strangers can't make kelpie speak). #398
- Kelpie's own `gh` comments recorded when posted, never read as answers (same login). #398
- Reading starts from GitHub's timestamp on the ruling comment, never local clock. #398
- First answer from any channel wins; ruling watched 1h more to say it's answered. #398
- Thumbs up/down from mentioned login answers yes/no ruling; tick counts only if that login or the App made every edit (GitHub doesn't say who ticked). #464

## Notifications

- `shep kelpie notifications clear` marks a repo's notifications done as the maintainer's login, never the App: notifications belong to a user, and an App token has none. Its own port, `Notifications`, so `Forge` and its wrappers stay as they are. `adapters/gh/notifications.rs`, `flock/notifications.rs`. #1747
- List with `all=true` (read ones stay in the inbox until done), then one `DELETE notifications/threads/<id>` each, in order. A call GitHub rate-limits, in the listing or a mark done, waits `cooldown.until(clock.now())` (`FALLBACK` when the answer names no time) and is asked again, saying each wait as it starts. Five limits in a row with no call succeeding between them stop the loop like any other failed call: a count of what was cleared of how many, and a non-zero exit. Running it again picks up what is left. `docs/facts/github-notifications.md`
