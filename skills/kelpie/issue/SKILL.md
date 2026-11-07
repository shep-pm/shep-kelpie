---
name: issue
description: The format of an issue a kelpie implementer can build from, and the check it must pass. Use when writing, filing or rewriting an issue for kelpie.
---

# How to write an issue for kelpie

An issue meant for kelpie is written the same way wherever it comes from. One issue is one pull request that works, tests and ships on its own. Use the repo's own words, from its glossary and docs, and write every path from the repo's root.

## The format

- **Title**: what will be true, in the repo's words, after its kind as a conventional type, as `fix(board): a held issue says why`.
- **`Touches:`**: a line naming the files the change edits, each path in backticks, new ones marked `(new)`. List every file a changed type, field or signature forces an edit in, found by searching for it. A piece whose `Touches:` passes 13 files is split. A path a worker's fence refuses, a harness's own file such as `.claude/`, `.codex/`, `.agents/`, `.pi/`, `.goose/`, `.config/goose/` or `.mcp.json` at the root, fails the check, since a worker cannot write it. Split it out: the part a worker can build stays one issue, and the fenced change becomes its own, labelled `ready-for-human` for the maintainer to make by hand. The worker's issue says `Blocked by:` that one where it needs the file first.
- **`## What to build`**: one line of why, then bullets naming the files, types, functions and shapes to add or change, and what stays unchanged where a builder might wander.
- **`## Acceptance criteria`**: `- [ ]` items the pull request is checked against. Named tests by their full module path, each pinning one behaviour and its failure cases, then the repo's own check commands in its documented shape. Say which tests only CI can run, and require nothing the builder cannot get, such as a live model or a CI result.
- **`Harness reach:`**: when `Touches:` names one harness's adapter (a `claude`, `pi`, `codex` or `goose` file or folder under `adapters/`), a line saying which harnesses the change reaches. A change to one adapter reaches pi in the same issue, or names the open pi issue that does. Codex may follow in its own issue.
- **`Blocked by:`**: when another issue must merge first, or edits the same function, a line naming it as `#<n>`, with why in the body. Link it as a blocker on the tracker too. Add none without a real dependency or collision.
- **Labels**: one priority label, `priority: P0` to `priority: P4`, by the repo's `docs/agents/triage-labels.md` where it has one.
- **Kind**: the title's type, one of `feat`, `fix`, `docs`, `refactor`, `perf`, `test`, `ci`, `chore` or `style`. A repo whose issue titles carry no type keeps the title a plain sentence and puts the kind in one `kind:<type>` label instead. Follow what the repo's own issues do, and never add two `kind:` labels. `bug` and `enhancement` are not kinds.

A piece of a split starts with `Slice <n> of #<parent>.` and is linked to the parent as a sub-issue on the tracker (`gh issue create --parent <parent>`), since that link is what closes the parent. A decision the docs leave open goes in the body as a stated assumption, "Assumed: X. Say otherwise.", never as a silent guess.

```markdown
Touches: `crates/adapters/src/adapters/pi/skills.rs`, `crates/adapters/src/adapters/claude/mod.rs`, `docs/design/plugins.md`

Harness reach: Claude Code and pi in this issue. Codex follows in its own issue, blocked by this one.

Blocked by: #41

## What to build

A repo plugin's skills load on pi as they do on Claude Code, so a worker on either harness lists the same skills.

- `skill_args` in `pi/skills.rs` passes each plugin's `skills/` folder with `--skill`
- `claude/mod.rs` keeps `--plugin-dir` as it is

## Acceptance criteria

- [ ] `adapters::pi::tests::a_repo_plugins_skills_reach_pi` passes
- [ ] `docs/design/plugins.md` says pi loads them
- [ ] `cargo fmt --all --check` and `cargo clippy --all-targets --locked -- -D warnings` pass
```

It is titled `feat(pi): a repo plugin's skills load on pi as they do on Claude Code` and filed with the label `priority: P4`.

In a repo whose titles carry no type, it is titled `A repo plugin's skills load on pi as they do on Claude Code` and filed with the labels `kind:feat` and `priority: P4`.

## Check it

Before filing, write the issue to a file, its title on the first line and its body below, and check the draft with the labels you will file it with:

```sh
shep kelpie issue check --draft <file> --label <label> --label <label>
```

It needs no shepherd, settings or network, so it runs in a kelpie worker's sandbox too; there, run the kelpie binary your instructions name for `lease run` in place of `shep kelpie`. It checks structure, not quality: each part above is there and well formed. It prints each part the draft lacks or holds malformed and exits 1, or exits 0 when it passes, and exits 2 when it could not read the file. Fix each one and check again until it passes, then file it.

Once it is filed, run `shep kelpie issue check <n>` on it where you can, which reads the issue as the project's tickets hold it. A worker's sandbox cannot reach them, so a worker checks only its draft. An issue that passes is ready for an implementer.

A `ready-for-human` issue for a fenced change is filed with that one fence failure left standing, since the maintainer makes it and no worker reads it. Its other parts still pass.
