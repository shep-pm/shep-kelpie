# Pushing to a contributor's branch

- Can the App push to a fork's pull request branch? Yes while "Allow edits by maintainers" is on, and no once it is off: `[remote rejected] ... (permission denied)`, exit 1.
- Can the maintainer's `gh` login push? The same: yes while it is on, `permission denied` once it is off.
- Can the maintainer's login push to a fork an organization owns that has "Allow edits by maintainers" on? No: seen on #1241, `maintainerCanModify: true` and still `Permission to Nitjsefnie-OSC/shep-kelpie.git denied to TurtIeSocks` (403).
- Can the App approve a fork's pending run? No: `403 Resource not accessible by integration`. The App holds no `actions` permission.
- Can the maintainer's login approve one? Yes: `201 Created` with an empty body.

Measured on 2026-10-21 (#636), with `gh` 2.96.0 and git 2.50.1, against `shep-pm/kelpie-scratch` and a fork of it by a second account that had never contributed there. The repo asks first-time contributors to have their runs approved. The App's installation token was minted for that repo alone and held `contents`, `issues`, `pull_requests`, `workflows`, `merge_queues` and `vulnerability_alerts` written and `metadata` read.

## Pushes

The second account opened two pull requests from its fork, one with "Allow edits by maintainers" on and one off. Each login then pushed one commit to each branch over HTTPS: the App with its token as `x-access-token`, the maintainer through `gh auth git-credential`. A push with no credential fails to ask for a username, so each push that went through was its login's.

| Branch | App | Maintainer |
|---|---|---|
| edits on | exit 0 | exit 0 |
| edits off | exit 1, permission denied | exit 1, permission denied |
| edits on, then turned off by the contributor | exit 1, permission denied | exit 1, permission denied |

- `GET /repos/{owner}/{repo}/pulls/{n}` gives both tokens the same `maintainer_can_modify`, and `head.repo.full_name` names the fork. It follows the contributor turning the box off at once. `fixtures/gh-api-fork-pull-edits-on.json` and `fixtures/gh-api-fork-pull-edits-off.json` are the App's reads.
- A push by the maintainer to the fork's branch starts its runs as `action_required` too, with the maintainer as `triggering_actor`. The runs wait on the contributor, not on who pushed.

## Pending runs

- Opening the pull request started one run per workflow (`test`, `commits`, `file size`), each `status: completed`, `conclusion: action_required`, `event: pull_request`. `GET /repos/{owner}/{repo}/actions/runs?status=action_required&head_sha={sha}` lists them (`fixtures/gh-api-fork-pull-pending-runs.json`).
- `POST /repos/{owner}/{repo}/actions/runs/{id}/approve` approves that one run. The `test` run approved by the maintainer ran and passed, and the other two runs for the same head still waited.
- Approving a run for an old head did not help the newer one: after the maintainer approved the first head's `test` run, all three runs of the pull request's newest head still waited.
- With the App's token the call answered 403 (`fixtures/gh-api-approve-fork-run-as-the-app.json`). GitHub's documentation for the endpoint asks for the `actions` repository permission, written, which the App does not hold. Not measured with it granted.

## A fork an organization owns

- Observed on shep-pm/shep-kelpie#1241, not a measured run: the pull request's `maintainerCanModify` was `true`, its head repository's owner `Nitjsefnie-OSC` was an `Organization`, and the maintainer's login `TurtIeSocks` was refused the push with `Permission to Nitjsefnie-OSC/shep-kelpie.git denied to TurtIeSocks` (403).
- Kelpie reads such a fork as not pushable whatever `maintainerCanModify` says: the query asks `headRepository { owner { __typename } }` and an `Organization` owner counts. `fixtures/gh-pr-reviewed-org-fork.json` is that reply's shape, written by hand from the schema, not recorded from GitHub.
- Not measured: a fork inside the same organization as the base repository. Kelpie counts it as an organization's fork too.
- Not re-run with the 2026-10-21 tooling (`gh` 2.96.0, git 2.50.1, the App's token): the refusal is the maintainer's login on the day of #1241 only.
