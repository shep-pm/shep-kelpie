# The GitHub App

The command that registers the App is step 6 of [Getting started](../../README.md#6-register-kelpies-github-app), and the one that installs the tools is step 3.

## Registering the App

App names are shared by all of GitHub, so if yours is taken, stop with Ctrl-C and run again with `--name <another name>`. Run it once for each owner: an App you register yourself installs only on the account that owns it. Running it again for an owner that has one says so and changes nothing, unless you give `--replace`. Before replacing, delete the old App on GitHub (its owner's settings, Developer settings, GitHub Apps), since its name is still taken while it exists, or give the new one `--name`. `shep kelpie github setup --help` lists the flags.

A new App is registered with these repository permissions, so none needs setting by hand afterwards:

| Permission | Access |
|---|---|
| Actions | Read and write |
| Checks | Read and write |
| Contents | Read and write |
| Deployments | Read-only |
| Environments | Read-only |
| Issues | Read and write |
| Merge queues | Read and write |
| Metadata | Read-only |
| Packages | Read-only |
| Pages | Read-only |
| Pull requests | Read and write |
| Repository webhooks | Read-only |
| Commit statuses | Read and write |
| Dependabot alerts | Read and write |
| Workflows | Read and write |

This is what the App is allowed. Each token kelpie mints still asks for only what its call needs.

GitHub's manifest has no logo field and no API uploads one, so the App starts with the default avatar. After the registration `setup` prints the App's settings page, `https://github.com/organizations/<owner>/settings/apps/<slug>` for an organization or `https://github.com/settings/apps/<slug>` for a user. Open it and, under Display information, upload [`assets/kelpie-logo.png`](https://raw.githubusercontent.com/shep-pm/shep-kelpie/main/assets/kelpie-logo.png) (download it first, since GitHub takes a file, not a link).

shep-kelpie keeps the App's private key in its home, `$SHEP_HOME/kelpie` or the folder `KELPIE_HOME` names, under `github/<owner>`, readable by you alone, and no worker can read it. Kelpie's own token holds issues and pull requests alone, so it cannot push, change code or merge: merging stays on your own `gh` login, and so do a review bot's summons and the labels on a pull request, since a bot answers a person.

On a repo the App covers, a worker never holds your `gh` login. Each of its calls gets a token minted for that repo alone with the permission to push, contents, and where the App holds it the permission to change workflow files, and nothing that can comment, review, approve or merge. Its `gh` and git use that token alone, never your `~/.config/gh` or the login `gh` keeps in the macOS keychain, and it may not read those or run `security`. Kelpie opens and edits the worker's pull request for it, from a file the worker writes. The token can push any branch of the repo, so protect `main` on GitHub with a branch protection rule or a ruleset: that is what stops a push to it. An App you registered before this needs the permission added: in its settings on GitHub, set Repository permissions, Contents, to Read and write, then accept the change on the installation; `doctor` names it as missing until you do. The `workflows` permission also lets a worker push a change to `.github/workflows`, so it can change CI, which runs with the repo's secrets: review a workflow change as you would any change to what runs with your secrets. An App without `workflows` gives workers a token that cannot push a workflow file, and `doctor` says which your workers have; to add it to an App you registered earlier, set Repository permissions, Workflows, to Read and write, then accept the change on the installation. On a repo no App covers, workers hold your `gh` login as before, and `doctor` says so.

On a repo the App covers, kelpie's reads of the board, the pull request, its review and its checks go through the App too, with a rate budget of its own, so they leave your login's. The App needs the read permissions Issues, Pull requests, Checks and Commit statuses; a new App is registered with them, and an App registered before this lacks Checks and Commit statuses, which `doctor` names. Until you add them (Repository permissions, set to Read-only, then accept the change on the installation), the reads that need them, a pull request's checks and a review bot's statuses among them, go through your `gh` login, and the runner says which permission was missing, once. A read the App is refused, or that finds the App's rate limit used up, goes through your login too, and the runner says so once. Everything else, writes included, stays as it was: the viewer, access checks, comments read since a time, failed run logs, and the merge. A repo no App covers is read by your login as before.

A ruling mentions the login in `git.maintainer`. Left out, it mentions the login `gh` acts as (`gh api user`), asked once a run, and `doctor` shows it as `(from gh)`. Set `git.maintainer` when `gh` acts as an account that is not you, such as a bot. If `gh` cannot say who it acts as, a ruling is posted without a mention, which notifies no one: `doctor` marks that `MISSING`, and the runner says so once a run in its log. An App newly installed on a repo is noticed within ten minutes.

## Tools and the login token

It puts the browser and the code graph server under shep-kelpie's home, `$SHEP_HOME/kelpie` (see [Code graph](building.md#code-graph)), the server a release pinned by version and SHA-256 and fetched with `curl`. A machine the release has no build for gets no server, and the install says so. The sandbox is crabpot, linked into kelpie, on every platform, so the install makes no copy of it anywhere; Windows fences with the WFP filter engine and needs a one-time `crabpot windows-install` (see [Windows](https://github.com/shep-pm/shep-kelpie/blob/main/docs/design/windows.md)). A call on a repo kelpie's GitHub App covers denies the Mach lookup of the two keychain services (`com.apple.securityd.xpc` and `com.apple.SecurityServer`) in its sandbox settings, so nothing a worker runs can ask the keychain for a login the `security` command stored, without a prompt. A repo the App does not cover keeps the services: its worker pushes and runs `gh` with the login the keychain holds.

Each `claude` call keeps a config folder of its own under the project's kelpie home, which holds no login, so it logs in with a token, whether or not the App covers the repo. Make it once, and keep it with `shep secret`:

```sh
claude setup-token
shep secret set CLAUDE_CODE_OAUTH_TOKEN
```

Then give the runner's sheep `CLAUDE_CODE_OAUTH_TOKEN = "{{secret:CLAUDE_CODE_OAUTH_TOKEN}}"` in its `env`. The runner hands the token to `claude` alone, in every role: the worker, the reviewers and the project manager. No other process it starts has the variable, and a Codex or pi call never gets it. It is one token the worker can read, where the keychain holds the full login. A call whose runner has no token does not start: it fails with the two commands, not with Claude Code's own "not logged in". `doctor` says MISSING for a project on Claude Code when the variable is set in neither the runner's `env` nor the shell you run it from. It reads from shep only which names in the runner's `env` are secret references, never the token, so a plain or empty value there does not count.
