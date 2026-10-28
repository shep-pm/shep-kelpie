# Writing issues

An issue meant for kelpie is written with kelpie's `issue` skill, wherever it is written: in your own Claude Code, pi, Codex or goose session, by a worker filing a follow-up, or by the project manager. The skill holds the format an implementer builds from: one pull request's worth, a `Touches:` line naming the files it changes, what to build, acceptance criteria of named tests, harness reach and blockers where they apply, one priority label and a kind. A decision the repo's docs leave open goes in the body as a stated assumption, "Assumed: X. Say otherwise.", never as a silent guess.

The dog's start, which an upgrade's restart runs, links the skill as `issue` into the skills folder of each harness installed here (`~/.claude/skills`, `~/.pi/agent/skills`, `~/.codex/skills`, `~/.config/goose/skills`), and every worker call and the project manager's get it beside kelpie's other skills. An `issue` already there that is not kelpie's link is left alone and named.

The skill checks its draft before filing it, and you can check any issue the same way:

```sh
shep kelpie issue check --draft issue.md --label "priority: P4"
shep kelpie issue check 7
```

The check reads structure, not quality: each part is there and well formed. It prints each part an issue lacks or holds malformed and exits 1, or exits 0 when it passes. An issue that passes is ready for an implementer. [Commands](commands.md) lists every part it reads.

The agent board picks the implementer an issue is built by ([Agents](agents.md)). Label it `agent:<name>!` to pin it to that implementer, listed or not, with no fallback: the board tries no other. A plain `agent:<name>` label does not choose. `shep kelpie issue "<request>"` and `issue --interactive` are gone, with the issue writer that ran them: the command says so and names the skill.
