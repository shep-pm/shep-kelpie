# Building

A worker builds its work item inline, in a worktree of its own cut from `origin/main`, on the implementer the agent board picked, or the one its issue's `agent:<name>!` label pins. It commits, pushes, opens a draft pull request that ends `Resolves #<issue>`, and ends its turn. When the repo has a pull request template, or its owner's public `.github` repository has one for repos that have none, it fills that. A project's `worker.instructions_file` adds rules the repo's own docs don't carry.

A worker's first prompt is the issue's title and body, followed by the comments the maintainer wrote on the issue, oldest first. Comments from anyone else, bots, kelpie's App and kelpie's own comments under your login are left out. The comments are capped at 16 KiB: past that the oldest are left out and the prompt says how many. Comments posted after the first turn started are not sent.

Every turn after the first that changes the pull request's diff ends its prompt by asking the worker to update the pull request's body to match the head it pushes: what changed, the files touched, the test evidence and the decisions. That covers a review fix, a CI fix, a merge queue removal, a no or a rework from you, and a rework or adoption of a pull request. A worker on your login runs `gh pr edit`, and a worker whose credential cannot edit the pull request rewrites the file kelpie sets the body from. A turn that only merges `main` in does not ask, since it changes no part of the pull request's own change.

A worker whose first turn ends with no pull request and no question is sent back once, and asks you for a ruling if it stops short again; once you answer a question it asks, the next turn that stops short is sent back once again. A worker that needs a decision only you can make ends its turn on a question, which reaches you as a ruling, and your answer is its next turn.

A work item with no pull request ends when its issue is closed and it holds no work: no commit on its branch or in its worktree that `main` lacks, and no file left uncommitted. The worker found nothing to change, or you closed the issue. It ends as soon as the worker's turn does, with no ruling. Parked on a ruling, its issue is read at most once every five minutes, and it ends once a read finds the issue closed, its ruling withdrawn. The log, the webhook (when one is set), `status`, the board and `usage` say the issue was closed with no change, and its history entry has `closed` set, `merged` not. shep-kelpie cannot tell who closed the issue, so a worker that closes it on its own ends its work item the same way. Work on the branch or in the worktree, or an open pull request that closes the issue, leaves the work item to go on as before. A turn still running at `worker.turn_timeout` minutes is ended and asks you whether to go on.

Each worker runs inside the sandbox with its fence: it writes only its worktree and build folder, reaches only GitHub (including the hosts its log and artifact downloads redirect to), the model's API, the registries of the package managers its worktree keeps files for (`worker.package_registries`) and the project's `allowed_domains`, and never merges, marks a pull request ready or summons a review bot. A worker that cannot read an external API's docs from there asks before it guesses; `tools` on a Claude Code agent file adds `WebFetch` (see [Agents](agents.md)). `kelpie confine` checks every file write and `kelpie guard` every command. Workers run their test suites under the `cargo-test` lease (see [Leases](leases-and-e2e.md#leases)).

Kelpie keeps what is private off the forge. It refuses, and never sends, any commit message, issue, comment, reply or pull request text that names:

- a path that names you: your home folder, kelpie's home and the project's checkout, `/home/<name>`, `C:\Users\<name>`, and the scratch folders `/private/tmp` and `/var/folders`
- an address on a local network, such as `192.168.1.20` or `host.local`
- in a post kelpie makes to a public repo, a project of your flock whose repo GitHub marks private, as `owner/name` or by its bare name. The `gh` calls and commits of a worker are not held to this rule

A `~/` path goes through, since it is the same folder on every machine that runs shep. A post to a repo GitHub marks private may name another private project. A refusal says which rule fired and never repeats the text. The flock's repos are read when the runner starts, so a project added later is covered after the runner restarts.

## Skills

Every step shep-kelpie drives an agent through runs a skill, by default from [mattpocock/skills](https://github.com/mattpocock/skills) (MIT). shep-kelpie vendors the ones it uses in `skills/`, pinned to one upstream commit with its licence, and writes them out as a Claude Code plugin when a runner starts. A project installs nothing.

| step | default skill | where it runs |
|---|---|---|
| `triage` | `triage` | not driven yet |
| `spec` | `to-spec` | not driven yet |
| `implement` | `implement` | the worker's first turn on an issue |
| `tests` | `tdd` | named in the worker's instructions |
| `ci_fix` | `diagnosing-bugs` | the worker's turn on a red CI run |
| `pr` | `pr` | named in the worker's instructions, unless the repo or its owner's `.github` repository has a pull request template |
| `reset` | `handoff` | not driven yet |
| `retro` | off, `retro` with `{ kind = "kelpie" }` | the project manager's batch retro, one call over the work items finished since the last (see Retros) |
| none, not a `[skills]` step | `code-review` | the `standards` and `spec` reviewers, when a project lists them (see [Agents](agents.md#agents)) |

Some vendored skills call others, and those are vendored too: `codebase-design` (from `tdd`), `grilling` and `domain-modeling` (from `triage` and `grill-with-docs`) and `writing-for-agents` (from `retro`). No step names them, and a test fails when a vendored skill or its docs call one that is not vendored, except `setup-matt-pocock-skills`, which a project runs once to set itself up and a worker never runs.

A runner writes only what a worker needs, because every harness lists each skill a plugin holds on every request. With the defaults a worker's copy holds six: `implement`, `tdd`, `diagnosing-bugs` and `pr`, plus `code-review` and `codebase-design`, which `implement` and `tdd` call. The other nine are written only where a step needs them, so `retro = { kind = "kelpie" }` writes `retro` and the `writing-for-agents` it calls. A step outside the worker's four runs kelpie's own prompt until you name it: with the defaults the `skills` field of `shep kelpie status --json` names no skill for `triage`, `spec` or `reset`, and naming one `{ kind = "kelpie" }` gives it its command, its folder, and what its text calls.

A step that runs a skill starts its prompt with the skill's slash command, such as `/mattpocock:implement`, and shep-kelpie's own prompt follows as its arguments. To override one, set it in the project's `[app.dogs.kelpie.skills]` table:

- `{ kind = "path", path = "..." }`: a skill folder with a `SKILL.md`, copied into a plugin of its own, `kelpie-<step>`
- `{ kind = "plugin", plugin = "...", skill = "..." }`: a skill in a Claude Code plugin's folder
- `{ kind = "none" }`: shep-kelpie's own prompt, no skill

A skill that can't load runs shep-kelpie's own prompt instead. The runner logs why, and `status` shows it under `skills`.

## Repo plugins

A plugin the repo turns on in its `.claude/settings.json` reaches every agent shep-kelpie runs there, which a headless Claude Code run does not do by itself: it loads only installed plugins, and workers run with `--setting-sources project`. So `rust-house-style:rust-house-style`, named in a repo's docs, is a skill its workers can call.

- As a runner starts, when it rereads its settings, and when the default branch it last fetched has moved, it reads `.claude/settings.json` on the checkout's default branch, never a worker's branch, so a pull request cannot turn a plugin on for its own review.
- Each `enabledPlugins` entry set to `true` is looked up in `extraKnownMarketplaces`. A `github` or `git` source is cloned, or fetched if already there, with the runner's own `git` into `<kelpie home>/marketplaces/<name>`. A `directory` source is a relative path to a folder inside the checkout, read as the default branch holds it (the checkout itself is never moved) and copied to `<kelpie home>/marketplaces/<name>`: an absolute path and a `..` are refused, and a link in the folder is left out.
- The plugin's folder is found in the marketplace's `.claude-plugin/marketplace.json`. Its parts, every other folder of it, and a trimmed manifest are copied into a plugin of the same name under `<project folder>/skills/repo/<name>`, rewritten at every start, so its skills keep their names.
- Every call gets it as one more `--plugin-dir`: the worker's turns, the reviewers and the project manager. `[app.dogs.kelpie.repo_setup]` narrows that.
- A plugin's `skills/`, `agents/`, `commands/` and `hooks/` are copied as files and folders, and `repo_setup.parts` narrows which: a link inside a plugin is left out. Every other top-level folder of the plugin and the files it keeps at its own root are copied too, since a skill names a file under the plugin's root (`${CLAUDE_PLUGIN_ROOT}/docs/rules.md`, or `RULES.md` at the root) and the name reaches nothing where the copy leaves it out; a change to such a file alone is copied over too. `.mcp.json` is the one file left out of the copy's root: its servers reach the call through kelpie, and it sits in `mcp-root` for a server that runs a file of it. In a copied `SKILL.md` that variable is the copy's own folder, so the path is one the worker can open: a worker reads the project's skills folder. A `SKILL.md` that is no text at all is copied as it stands and filled in with nothing, so one unreadable skill file never skips the plugin. Hooks are read from `hooks/hooks.json`, so a script a hook runs must sit in `hooks/` too, and a hook command reaches it through `${CLAUDE_PLUGIN_ROOT}`, which Claude Code fills in for a hook. A plugin's hooks run beside the hooks in the call's settings file, inside the call's fence, and cannot replace or turn off kelpie's own. A plugin's MCP servers load too, as [MCP servers](#mcp-servers) says. A second plugin of a name an earlier one supplies, in the order the repo lists them, is skipped. A `github` or `git` marketplace is fetched again only when one of those happens, not on a timer. A plugin with none of the parts `parts` loads is skipped. A plugin whose marketplace entry points outside the marketplace is skipped too.

A plugin that cannot load is skipped, the runner logs why and the run goes on: no `extraKnownMarketplaces` entry, a failed fetch with no copy yet, no such plugin in the marketplace, none of the parts that load, or a name shep-kelpie keeps for itself (`mattpocock`, `kelpie-<step>`). A failed fetch keeps the last good copy and `status` marks the plugin stale. A `SKILL.md` whose frontmatter is not valid YAML is told the same way, naming the file and the parser's message: pi refuses one and its warning reaches no log shep-kelpie keeps, while Claude Code reads it. The plugin loads and its other skills with it, and `status` lists the file under `unreadable`. `status` lists each plugin under `plugins` with its skills, why it was skipped, or the skill files it has that are not valid YAML, and `doctor` reports the same under `repo plugins` from the marketplaces already fetched, without fetching any.

### `repo_setup`

A project narrows what reaches each agent, and adds a plugin the repo does not name, in its `[app.dogs.kelpie.repo_setup]` table. Every key is optional, an unknown key stops the runner naming it, and a change is read as the runner rereads its settings.

- `plugins`: `true` by default. `false` loads none of the plugins the repo turns on, and `extra` still loads.
- `own`: `true` by default. The repo's own `.claude/skills/`, `.claude/agents/`, `.claude/commands/`, the `hooks` of `.claude/settings.json` and the servers of `.mcp.json`, read from the default branch and written as one more plugin, `kelpie-repo`. `false` leaves them out and keeps the plugins, `extra` ones included. See [The repo's own files](#the-repos-own-files).
- `roles`: the calls that get plugins and the repo's own files, from `worker`, `reviewer` and `pm`. All of them by default. `roles = ["worker"]` keeps plugins and the repo's own files from the reviewers and the project manager. `advisor` and `researcher` went with the ruling panel and are refused: `shep kelpie settings migrate` drops them from the list. It narrows `extra` as well.
- `parts`: which parts of a plugin, and of the repo's own files, load, from `skills`, `agents`, `commands`, `hooks` and `mcp`. All of them by default. `parts = ["skills"]` copies only a plugin's skills, and leaves its agents, commands and hooks out, and the same for the repo's own files. `mcp` picks the MCP servers of a plugin and of the repo's `.mcp.json`, see [MCP servers](#mcp-servers). `parts` without `mcp` leaves both out.
- `skip`: plugins, as `name@marketplace`, left out though the repo turns them on. A skipped plugin is not listed in `status`.
- `extra`: plugins the repo does not turn on, each as `name@marketplace` with its marketplace's source as one string: `github:owner/name`, `git:<url>` or `directory:<folder inside the checkout>`, and `#<branch or tag>` after a `github` or `git` address. The kinds are those of the `source` in an `extraKnownMarketplaces` entry. A marketplace given here takes the place of the repo's of the same name, and two plugins of one marketplace must give the same source. An extra plugin that cannot load is skipped with its reason, as the repo's are.

```toml
[app.dogs.kelpie.repo_setup]
roles = ["worker", "reviewer"]
parts = ["skills", "agents"]
skip = ["noisy@house"]
extra = { "style@lab" = "github:acme/lab#v2" }
```

### The repo's own files

A repo sets Claude Code up for everyone who works in it without a plugin too: skills in `.claude/skills/`, sub-agents in `.claude/agents/`, commands in `.claude/commands/` and hooks in `.claude/settings.json`. A worker loads them because its turn runs in the worktree, but the project manager runs in a folder of its own, so shep-kelpie hands them over.

- It reads the four from the checkout's default branch, never a worker's branch, and writes them as one more plugin, `kelpie-repo`, under `<project folder>/skills/repo/kelpie-repo`, rewritten at every start and whenever the default branch moves. A plugin's name carries a namespace, so its skills are invoked as `kelpie-repo:<skill>`. A repo's plugin cannot take that name.
- Every call gets it as one more `--plugin-dir`, as it gets a plugin, for the roles `repo_setup.roles` names. A Claude Code call that runs in a worktree whose agent files are exactly the default branch's already loads them there, and does not get the plugin as well: a worker's turns and the reviewers get none, so no skill or hook shows up twice.
- The `hooks` of `.claude/settings.json` go to the plugin's `hooks/hooks.json`. They run beside the hooks in the call's settings file, inside the call's fence, and cannot replace or turn off shep-kelpie's own. A hook that runs a script in the repo reaches it through the call's folder, which for the project manager is not the repo, so the script is not found there. Only the `hooks` are read from `settings.json`.
- `repo_setup.own = false` leaves all of this out and keeps the repo's plugins. `parts` narrows it as it narrows a plugin. A link in these folders is left out, as in a plugin.
- `status` lists it under `plugins` as `.claude`, with the skills it brought, and `doctor` reports it under `repo plugins`. A repo with none of these files has no entry.

### MCP servers

A plugin can bring MCP servers, and so can the repo's own `.mcp.json`. shep-kelpie runs most calls with `--strict-mcp-config` and an allow-list in `--tools`, which drops every server a plugin brings even when the plugin loads. So it reads them itself, and while `mcp` is among `repo_setup.parts` (the default) adds them to the call's MCP config and their tools to its allow-list.

- A plugin's servers come from its `.mcp.json`, or the `mcpServers` of its manifest, inline or as the path of a file inside the plugin. The repo's own come from `.mcp.json` on the checkout's default branch, never a worker's branch and never the worktree's copy, so a pull request cannot add a server to its own review. `own = false` leaves the repo's `.mcp.json` out and keeps the plugins' servers, and `plugins = false` the reverse.
- A server is named as Claude Code names a plugin's: `plugin_<plugin>_<server>`, with any character outside letters, digits, `_` and `-` as `_`. The repo's keep their names. Its tools are allowed as `mcp__<name>`, every tool of the server. The code graph keeps its own name, which a repo's server cannot take.
- A plugin's server may run one of the plugin's files through `${CLAUDE_PLUGIN_ROOT}`. The copy holds every folder of the plugin and the files at its root, but not the parts `repo_setup.parts` leaves out, so the whole plugin is copied to `mcp-root` inside it as well, which Claude Code does not read, and the variable in a server names that folder. In a copied `SKILL.md` the variable names the plugin's copy itself, and in a skill of the `kelpie-repo` plugin it names that plugin's copy.
- The calls that get them are those `repo_setup.roles` names, on Claude Code, pi, Codex and goose. pi, Codex and goose write a server in their own shape, and leave out one they cannot load (an SSE server, a `${VAR}` outside pi's `env` and `headers`, any `${VAR}` for Codex or goose, and for goose also an HTTP server with `headers` and a stdio value holding a double quote), with a note in the runner's log. goose keeps a server whose env holds a key it never passes (`PATH`, `NODE_OPTIONS`), without that key, and says so. goose is given its servers when a session starts, so a server added later is not seen by a session that began before. A call that only reads, a batch retro, gets none, so it stays unable to write. The same calls get the plugin's other parts, so a call that loads no MCP server today still loads none unless the table names its role.
- A server runs inside the call's fence, as any command the call runs does. The hosts a server reaches still need the call's allowed domains: `worker.allowed_domains` for the worker and the reviewers. The project manager reaches the model and GitHub only, so a server that needs another host cannot connect there.
- `doctor` reports `repo mcp servers`: each server with a `url` and its host, and which of them `worker.allowed_domains` lacks. A server that starts a command names no host, since what the command reaches cannot be told from how it starts, so those are not checked.

## Code graph

A project can give its reviewers, and with `worker` its workers, a graph of the code in the worktree, served by [codebase-memory-mcp](https://github.com/DeusData/codebase-memory-mcp). One call answers where a symbol is, what calls it and what it touches, where a worker would run a series of greps. It is off unless the project turns it on:

```toml
[app.dogs.kelpie.code_graph]
enabled = true
worker = false
```

- `shep kelpie tools install` fetches the pinned release into `<kelpie home>/tools/bin`. It prints a line as each of its five steps starts and ends, naming each tool with its pinned version, and closes with a table of every tool and its result. A download shows a progress bar only when stderr is a terminal. Kelpie writes each call's MCP config itself and never runs the tool's own `install`, which writes to your agent config files.
- Before each reviewer, and before each worker turn with `worker`, kelpie indexes the work item's worktree into `<kelpie home>/<project>/code-graphs/<issue>`. Indexing is local CPU and spends no tokens. It runs on the call's own thread, not under the runner's lock, so a stop ends it, and it stops after five minutes. The folder goes when the work item's worktree does.
- Claude Code gets the server through `--mcp-config` beside `--strict-mcp-config`, and `mcp__codebase-memory-mcp` in its allow-list. pi gets it in the `mcp.json` of the call's own home, Codex in a `-c mcp_servers.codebase-memory-mcp=...` override, and goose in a `--with-extension` flag. No other MCP server loads but those of the repo's plugins and `.mcp.json`, see [MCP servers](#mcp-servers). The server runs inside the call's sandbox, may read its binary and index, writes only its index, and reaches no host.
- Only the reviewers and the triager have it, and only on Claude Code, pi, Codex and goose. `worker` adds the worker's own working turns, and is off: the server's 17 tool definitions ride in every request of every turn (16,470 bytes on Claude Code, about 4.3k tokens), and no worker session recorded so far, on Claude Code or pi, has answered one of them (`docs/facts/mcp-servers.md`). A retro and the project manager do not have it at all.
- A worker with no graph is indexed for nothing: kelpie builds the index only for a call that is given the server.
- A project that turns it on while the server is not installed gets a MISSING `code graph` line in `doctor`. Its calls run without a graph and the runner logs why, so a missing binary or a failed index never stops a work item.
- The setting is read as the runner rereads its settings.

## Retros

shep-kelpie runs a retro over a batch of finished work items, with the project manager's agent. It runs after every 10 work items finish, merged or closed, and when a drain starts with any finished since the last retro. A batch is at most the latest 10, so a history that built up while the retro was off is not read all at once. A project with no project manager runs none. The call is a one-off with a new session, not a turn of the project manager's standing session, so the report does not grow that session's context.

The retro reads a digest that shep-kelpie builds in code from the records it keeps, so the call spends its tokens reading, not collecting. It has one section for each work item: its title and type, the project manager's wakes and actions that named it, each ruling with its kind, how long it waited and who answered, its review rounds and their findings, its fix turns after a failed check, its merges of main and conflicts, its wall time, its line from the usage ledger and the `Friction` section of its pull request's body. Wakes, review rounds, fix turns and merges of main come from `<kelpie home>/<project>/journal.jsonl`, which the runner adds to as each happens, because the state file forgets a work item once it finishes. The digest ends with the batch's totals: spend by role and kind, the wakes that changed nothing, and the rulings with how long each waited. The prompt asks for patterns across the batch first (repeated wakes, rulings that waited, findings that were stale or wrong, steps that cost more than the work), then problems in a single work item, and for a sentence and nothing else when nothing is worth your time.

What the digest counts:

- A work item's section holds only its own run. Its entries start when the item started and end at its finish, so an issue that ran twice in the batch shows as two numbered runs, each with its own wakes, rounds and rulings.
- A wake counts for a work item when it woke for it, picked it or acted on it. One that only names the item in its list of what it holds back counts for none of it.
- A wake whose answer was `starts nothing now` counts as one that changed nothing, in the item's section and in the batch's totals.
- Review rounds are counted as rounds that read the pull request. A round that read it and found nothing is listed with 0 findings, and a reviewer the pass went without is listed too, numbered, with its reason.
- A project's first retro has no earlier retro to start from, so its totals cover the batch's own span, from its first work item's start.
- A call whose harness reported no cost shows as `local, unpriced`, and a turn ended at its ceiling as `timed out, usage not read`. Neither shows as `$0.00`.
- A `Friction` section stops at a harness's `🤖 Generated with` trailer. One past 600 characters is cut at a sentence, or at a word where it holds no sentence, and ends with `(cut)`.

A worker keeps a voice at no extra call. When something slowed it down, it adds a `## Friction` section to its pull request's body, a line or two written while its cache is warm. shep-kelpie reads it from the body when the work item ends, keeping at most 600 characters. Workers are told of the section only while the retro is on.

The call can read and search and nothing else: it edits no file, runs no command and pushes nothing, on every harness the project manager runs on. On Claude Code that is an allow-list, `--tools Read,Glob,Grep,Skill` with no MCP server loaded, so nothing a project's settings allow adds to it. Its reply is the report, and the runner writes it to `<kelpie home>/retro-reports/unverified/<project>-batch-<time>.md`, headed with the project and the batch's issue numbers, then the agent and the session.

shep-kelpie never acts on a report, so nothing a retro suggests is applied. You pull the reports every so often, consolidate them and fix what is worth fixing; consolidating them stays out of shep-kelpie, because a retro that agents act on by themselves can loop on nonsense. A batch asks once. A retro that fails, times out (after 15 minutes), comes back empty or is cut short by a stop is logged and skipped, never retried, and it holds no slot, so no work item waits on it. It starts only when the project manager is free and not held back by the pacer or a terminal; a drain asks once and then skips it. A retro already running when a drain starts is waited on, up to its 15 minutes, and `status` counts that in the drain's ceiling. The retro's usage goes in the usage ledger as a `retro` call of the `pm` role, and `usage` totals it on a line of its own, so its cost can be read apart from the project manager's wakes. The retro is off unless the `[app.dogs.kelpie.skills]` table names it: `retro = { kind = "kelpie" }` runs the vendored `retro`, and a `path` or `plugin` runs a skill of your own. The worker used to run the retro, resuming its session once per work item. That cost about 14% of a night's spend, mostly in writing the worker's whole session back to the cache, and the worker never saw the project manager's holds, the rulings or the merges, which the batch retro reads.
