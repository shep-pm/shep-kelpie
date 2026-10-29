# Rulings

What a runner asks the maintainer, and how an answer comes back. The pass they sit in is in review.md.

- Seven kinds: `merge`, `question`, `stuck` (reason), `agent-files`, `foreign-change`, `follow-up` (findings wait for a yes), `contribution` (with reason: `security`, `over-budget`, `question`, `waiting`, `no-edits`, `cannot-adopt`, `merge`; see `contribution-release.md`, `contribution-merge.md`). ADR 0006, #330, `runner/ruling/`
- A `contribution` ruling has no work item and posts nothing on the contributor's pull request (it would accuse them and tip off an attacker): webhook, `status` and `rule` only. Nor to the PM (`contribution-merge.md`, `github-app.md`). #620, #629, #630
- Ruling = log line + `status` entry + webhook post, one question; `no <note>` is worker's next turn. #89
- A ruling is named by its ticket id where the maintainer reads it: `#<pr>`, `#<issue>` before there is a pull request (a question waits for its draft, `workers.md`), or `k12` locally. The question, the webhook title, kelpie's GitHub comment and card and `status`'s `id` show it, and a reply starts with it. #508, #509, #1010
- `rule <ruling> <answer>` (`rule 427`, `rule k12`; one ruling per item within a project), refuses an answer older than the ruling. An answer file left under an old counter id is read once, then refused. #509. Each trigger is `shep kelpie <verb>`, project from folder unless `-p`. #119, #507
- `rule` reaches a stopped runner via `<kelpie home>/<project>/answers/<ruling>` files applied next pass. `runner/trigger/`
- One webhook (`discord` or `ntfy`) until shep carries questions; URL is a credential, passed to `curl` on stdin. ADR 0005, #333, `crates/core/src/webhook.rs`
- ntfy reply carries TOTP (RFC 6238): a code posted on the alert sat on the topic it guarded. #111
- `shep kelpie totp`: secret under `<kelpie home>/totp/` (0700, 0600); every code seen in any post spent first; used steps hard-linked claims (runners agree); 5 wrong codes disable ntfy answers until `--unlock` at terminal. RFC 4226, #111
- Topic read every 15s while alerted ruling waits, backing off to 5 min. Limits fail closed: topic writer can lock answers or fake alerts, not act without code. #111
- `trusted` webhook with `token` needs codes only for merge yes. #298
