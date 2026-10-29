# Rulings

A ruling is a decision only you make. The worker waiting on one is parked, and the rest of the project goes on: a parked work item gives its slot up, so the board can open another (see [The board](board.md#the-board)). It is one of seven kinds:

- `merge`: CI is green, and a yes merges the pull request. A `no <note>` sends the worker your note, and its fix goes to CI and back to you with no new pass of the review; a `rework <note>` sends your note for a change the whole review reads again. On a merge ruling that warns of open review bot threads, a pass no reviewer read or a head nothing vouches for, `no` starts a new pass too, since only a pass clears that, and the question says so. The merge ruling after a `no` fix, after the fix for a review bot's late review (see [Review bots](review.md#review-bots)), or after the fix for a person's comment (see [Comments from people](review.md#comments-from-people)), says its head is that fix and that no reviewer read it, and comes back to you even under `auto`. Open review bot nits are named on their own and hold nothing. On someone else's pull request the merge ruling is always yours, `auto` included, offers yes and no only, and a `no <note>` goes to the contributor (see [Contributions](contributions.md))
- `question`: the worker asks, and your answer is its next turn. A worker that has made commits pushes the branch and opens its draft pull request before it asks, so the question is on the pull request next to the work it is about. A worker with nothing to push asks on the issue
- `stuck`: the work item cannot go on by itself, and its reason says why: `rebase`, `still-red`, `check-queued`, `no-checks`, `merge-refused`, `closed`, `fix-not-pushed`, `test-still-fails`, `unpushed`, `push-refused`, `turn-timeout`, `turn-failed`, `adopt-failed` or `over-budget`. The question says what a yes does. For `adopt-failed` (an adopted pull request cannot start, usually because a worktree of yours holds its branch) no work item is parked: remove that worktree or switch it off the branch, and kelpie starts the pull request on its next poll. A yes only clears the ruling, and it comes back only if the error changes. A turn that passes its ceiling while still working raises no `turn-timeout` ruling: if its worktree holds a commit that `main` lacks, made in the last 10 minutes, or an uncommitted file written in them, kelpie resumes the turn with a prompt to commit what builds and carry on, up to 3 times for a work item, and the fourth ceiling raises the ruling. For `check-queued` (a check stayed queued after its run was rerun) the question names the jobs and links the run: a yes reruns the run once more and kelpie waits again for `ci.queued_minutes`, and a no reruns nothing and leaves the item waiting for you to act on the forge, with the wait started afresh. For `no-checks` (no check appeared on the head within `ci.queued_minutes`, though `origin/main` has a workflow that runs on a pull request) a yes passes that one head as skipped and kelpie goes on, back to the review round it interrupted or on to the merge ruling, and a no waits another `ci.queued_minutes`. A new push starts the wait over. With no such workflow on `origin/main` no ruling is raised: the head goes on as skipped at once. For `test-still-fails` (a reviewer's test still fails after the worker's two fixes) a yes sends one more fix turn on the same tests, and a no sends the worker your note, ends the check and lets the review go on. The comment on the pull request says only that a test fails; the output tails are in the terminal question
- `agent-files`: the pull request changes agents' own files, and a yes accepts them
- `foreign-change`: someone else changed the pull request, and a yes accepts the change
- `follow-up`: a merged pull request left findings unfixed, and a yes files them as issues
- `agent-fit`: the agent board reads the agent that finished an item as clearly worse at its kind of work than another agent that takes that kind. It is raised on that item's pull request once the item has finished, so no work item is parked on it. Its card shows both records side by side and the `kinds` line the board suggests. A yes and a no both change no file: kelpie never writes an agent's `kinds`, so that line is yours to write into `agents/<name>.md` if you want it. The board reads the pair again once the worse agent finishes 10 more items of that kind

A ruling shows in the runner's log, in `shep kelpie status` and in `shep kelpie rule`, and posts to the webhook when kelpie's `[kelpie.webhook]` is set (see [The webhook](#the-webhook)). The pull request gets a comment for every ruling but a merge. Answer it from the terminal:

- `shep kelpie rule 14 yes`
- `shep kelpie rule 14 no rename the flag`
- `shep kelpie rule 14 rework split the parser out`, for a merge ruling only
- `shep kelpie rule 15 use --dry-run`, for a worker's question. A `yes` there is the answer's text
- `shep kelpie rule 14 rerun`, for a `still-red` ruling that offers it: kelpie reruns the head's failed jobs once more and looks at CI again
- `shep kelpie rule 14 restore`, for a `turn-failed` ruling that offers it: kelpie puts the uncommitted agent files back as the head has them and resumes the turn

A ruling is named by its pull request's number, or by its ticket's id while it has no pull request: `rule 427 yes` answers the ruling on 427, and `rule k12 yes` one on the local ticket `k12`, which kelpie files none of yet (see [Commands](commands.md)). A work item waits on one ruling at a time, and GitHub numbers issues and pull requests from one sequence, so within a project a number names one ruling. `rule #427 yes` and `rule shep-kelpie#427 yes` read the same number, and a local id takes the `#` the same way: `rule shep-kelpie#k12 yes`. From a clone, the clone's project is looked at first. Elsewhere a number that one project has a ruling on works, and one that two projects have is refused with their names: `rule shep-kelpie#427 yes` picks one. Quotes are optional, but zsh still needs them around a note with `?`, `*`, `!` or an apostrophe. `-p` goes before the answer: after its first word, and after a `--`, a `-p` is part of the answer. `shep kelpie rule` alone lists the rulings waiting and asks which to answer and how. Each ruling's question says what `yes` and `no`, and on a merge ruling `rework`, do. A `no` or `rework` with a note is the worker's next turn.

Where an App covers the repo, a ruling's comment is one short format, whatever its kind: the problem where there is one, a title, a table of what picking each option leads to, the project manager's pick and why where it gave them (`**Project manager: A**. CI is green and every reviewer read it.`, see [Rulings the project manager answers](#rulings-the-project-manager-answers)), and the ways to answer. Here is a merge ruling. Kelpie writes the effect of each option from what it knows: the items held behind the pull request, CI, the reviewers that read it, and roughly what another review round costs from the ledger:

```markdown
### Merge this pull request at 6b0606e into main?

| | Option | If you pick it |
|---|---|---|
| **A** | Merge | Merges it into main and closes #567: the worker retro is off unless a project names it. CI passed. 2 review rounds read it. |
| **B** | Send back | Sends the worker back for a fix that goes to CI and back to you, with no new review round. |
| **C** | Rework | Sends the worker back for a change the whole review reads again. Another review round costs about $0.80. |

@octocat
- 👍 or 👎 answers, or tick one:
- [ ] A
- [ ] B
- [ ] C
- Or reply `14 no <note>` or `14 rework <note>`
```

A worker's question opens with the problem, and each option carries its effect. The worker writes both in its `<kelpie-question>` block: a first line `Problem: ` of one or two plain sentences, then each option as `- label => what picking it leads to`. The project manager rewrites the problem when it passes the question on, and adds a `board` note of what the board shows (items held behind this one, a slot freed or kept), which the card shows under the table. An option the worker gave no effect keeps the phrase its kind always had ("Sends the worker this answer"). The effect is no part of the answer the option gives.

```markdown
The push hook rejects two commits, and nothing says why.

### Which way should the push go?

| | Option | If you pick it |
|---|---|---|
| **A** | Wait | #495 stays parked until the hook is fixed. |
| **B** | Push anyway | Publishes both commits to main, and the hook stays broken. |
| **C** | Other | Reply with your own answer |

**On the board**
Nine ready issues are held behind #495.
```

You answer it in the thread, from the comment itself or a reply:

- A ruling that takes a yes or a no (`stuck`, `agent-files`, `foreign-change`, `follow-up`, `agent-fit`) takes your 👍 or 👎 on its comment. A merge ruling takes them too, as A and B.
- A merge ruling, and a worker's question that lists options, list them as task-list boxes. Tick one. A worker's question becomes the table from the options it lists, plus an `Other` row for a reply.
- A 👎, or a ticked `Send back` or `Rework`, carries no note, so the worker is told you gave none. Reply `14 no <note>` to send one.
- A tick counts only when every edit of the comment was by you or by the App that wrote it, which GraphQL's `userContentEdits` says. Anyone with write access can edit a comment, so one edit by someone else leaves the boxes dead and your 👍 or 👎 or a reply answers instead. A 👍 with a 👎 from you, or two ticked boxes, answers nothing.
- A reply works as it did. Only the login the comment mentions (`git.maintainer`, else the login `gh` acts as) answers, and it needs write access to the repo. Start the reply with the ruling's id, as `shep kelpie rule` does: `14 yes`, `14 no <note>`, `14 rework <note>` for a merge ruling. The number of the ruling's pull request or issue works in place of its id, such as `427 yes`. A bare `yes` answers nothing, so a conversational one never merges. A worker's question takes free text, with its id (not a pull request or issue number, which is part of the text) first only when several rulings wait on the thread.

Every answer, however given, is applied the same way, and the project manager reads that the ruling was answered in the board's events at its next wake.

A ruling that mentions no one, because `git.maintainer` is unset and `gh` cannot say who it acts as, is answered at the terminal only, and its comment says so. Anyone else's reply, reaction or tick, a bot's and a comment kelpie posted itself are left alone. Kelpie reacts with a thumbs up to the reply it took as the answer, and replies once to one it could not read, saying what to type. It reads the thread and each waiting ruling's comment about once a minute. The first answer wins, whether it comes from the thread, the terminal or ntfy, and the other is told the ruling is already answered. A worker's own `gh` may not comment: `gh pr comment`, `gh issue comment`, and any `gh pr` or `gh issue` verb but `create` and `edit` that carries a body or `--comment` (a `close` with a comment, a `review` with a body) are refused, as is any flag but `-R` before the verb and any `gh` word a shell expands (`$`, a backtick, `{`, `~`, a glob character) outside a `--body` or `--title`, since its `gh` is your login.

On an ntfy webhook, a ruling can also be answered in the topic (see [Answering in the topic](#answering-in-the-topic)).

With a project manager, it may answer a `stuck` ruling for you with a retry, or add its words to the ruling and post it again (see [The project manager](board.md#the-project-manager)).

## Rulings the project manager answers

`rulings.pm_decides` maps each kind of ruling to who answers it: `true` for the project manager, or `false` for you. Operational kinds, whose answer turns on facts it reads from the board, start `true`; `question`, `agent-files`, `agent-fit` and `foreign-change` start `false`. A kind the map leaves out is yours:

```toml
[app.dogs.kelpie.rulings.pm_decides]
question = false
agent-files = false
foreign-change = false
still-red = true
turn-timeout = true
# ... and every other kind, `true`
```

A ruling of a `true` kind that lists two options or more, on a project that names a project manager (`agents.pm`), wakes it, and its comment waits for that one wake. The project manager answers with one of the ruling's options, a `why` of at most 350 characters and a `design` flag, true when the question changes an ADR, the glossary, a public interface or the project's direction. An option it picks is applied at once and posted on the ruling's thread headed `Answered by the project manager: A, Merge`. A reply there of `<id> no <note>` reopens it: the ruling comes back for its work item and your note is the answer, so the worker takes it as it takes any `no`. It needs the work item to be open and its worker not running, and says why when it is not; an answer is watched for 24 hours. `status` counts the rulings it answered, by kind (`pm_answers`).

The ruling comes to you instead, with the project manager's pick and why on its card, when it flags `design`, when it is a merge on a project under `git.merging = "ask"`, and when its pick is one only you can carry out (`Ask the maintainer`, or an option that begins `You`). A wake that gives no answer, or fails, posts the ruling to you as it stands. Every other ruling comes to you at once, with no pick unless you ask for one (see [Asking the project manager](#asking-the-project-manager)), and so do these, whatever their kind: a question and a follow-up, which wake the project manager on their own (see [The project manager](board.md#the-project-manager)); a contribution's ruling; a turn kelpie could not set up, whose fix is on this machine; a ruling raised while the runner drains; and a turn that fails again the way one the project manager already answered on the same work item did, since its answer did not fix it.

The kinds are `question`, `agent-files`, `agent-fit`, `foreign-change`, `follow-up`, `rebase`, `still-red`, `check-queued`, `no-checks`, `merge-refused`, `closed`, `fix-not-pushed`, `test-still-fails`, `unpushed`, `push-refused`, `turn-timeout`, `turn-failed`, `adopt-failed` and `over-budget`. `merge`, `contribution-merge`, `security` and `local-model-spilled` set `true` are refused: the runner does not start, naming `rulings.pm_decides` (or `rulings.pm_alone` for the old list), and a change that sets one while the runner runs is refused too, keeping the settings it has.

`shep kelpie add` writes every kind into a new project's map, and `shep kelpie upgrade` runs `shep kelpie settings migrate`, which writes each kind a project's map is missing at its default and never changes one it has. That is every kind, not only those a list named: the operational kinds go in `true`, so the project manager answers them unless you set them `false`. A kind you delete from the map is written again at its default on the next run; set it `false` instead. `doctor` names a project whose map is missing kinds. The old `rulings.pm_alone` list still loads for one release, as `true` for each kind it names, and the migration turns it into the map. Setting both is refused. The migration also deletes `rulings.decide`, `rulings.full_panel_paths`, `agents.advisor`, `agents.researcher`, the `researcher` table and `advisor` and `researcher` in `repo_setup.roles`, the settings of the ruling panel that voted on rulings before the project manager answered them, which the runner refuses by name.

### Asking the project manager

A ruling that comes to you carries no pick from the project manager, and no wake runs for it. On a project that names one (`agents.pm`), you can ask in the ruling's thread, starting with its id as any reply does:

- `14 pm recommend`: the project manager reads the ruling and posts its pick and why in the thread, as `**Project manager: A**. <why>`, with the reply that gives it, such as `14 yes`. The ruling stays yours, and you answer it with the replies above. Asking again replaces the earlier pick.
- `14 pm options`: for when none of the options is right. The project manager posts up to 5 other approaches, numbered, and `14 pm 2` takes the second: on a ruling with fixed options it is your `14 no <note>` with the approach as the note, and on a worker's question it is your answer. The approaches stay on the ruling through a restart, and `pm <n>` with none, past the last, or on a contribution's ruling, which a `no` would only clear, is refused, saying which.
- `14 pm decide`: hands this one ruling to the project manager, as if its kind were `true` in `rulings.pm_decides`. It is refused, saying why, on the kinds set `true` is refused for (`merge`, `security`, `contribution-merge`, `local-model-spilled`) and on the rulings that come to you whatever the map says. What the project manager would have to hand back to you comes back with its pick in the thread.

Kelpie reacts with a thumbs up to a request it took. With no project manager named, each is refused with a reply saying so. A second request while the project manager is on the ruling is answered once and starts nothing. A wake that fails, times out or gives no answer says in the thread that the project manager did not answer, and the ruling stays yours. What it posts is plain text, so it cannot mention anyone or link anywhere.

On a worker's question, an answer that is exactly `14 pm recommend`, `14 pm options`, `14 pm decide` or `14 pm <n>` is that request, not your answer; any other answer that opens with `pm` is your answer as written.

## The webhook

A webhook posts each ruling where you will see it away from the terminal. Without one, a ruling shows only in the runner's log, `status` and `shep kelpie rule`, so you have to look.

It is the `[kelpie.webhook]` table of `dogs.toml` in the shepherd's home, for ntfy or Discord. Copy that table alone from [`kelpie-settings.example.toml`](https://github.com/shep-pm/shep-kelpie/blob/main/kelpie-settings.example.toml) and put your own URL in it: the file's `url` is a public ntfy.sh topic anyone can read. Its comments say what each key does, including a self-hosted ntfy's `ca_cert` and `token` and a `trusted` topic. `shep kelpie doctor` names the webhook rulings post to, and `shep kelpie doctor --test-alert` posts a test through the same settings.

### Answering in the topic

On an ntfy webhook, a ruling can be answered from the topic: run `shep kelpie totp` once and scan the QR code into an authenticator app, then reply with the line the alert ends on, such as `14 yes <code>`, with the app's code last. A reply takes the same answers as `shep kelpie rule`, and names the ruling the same ways: `427 yes <code>`, `#427 yes <code>` or `koji#427 yes <code>`. A reply by number written before the open ruling was raised is refused, saying the ruling changed and what it asks now, since a new ruling on the same pull request may ask something else. A reply by id, and an answer at the terminal, are taken as given. Anyone who can read the topic can read rulings, but only a reply with the code of the moment answers one, and each code answers once. A `trusted` topic is the exception: most replies there need no code (see [`kelpie-settings.example.toml`](https://github.com/shep-pm/shep-kelpie/blob/main/kelpie-settings.example.toml)). Five wrong codes turn answers off until `shep kelpie totp --unlock`, and `shep kelpie totp --rotate` replaces a secret that may have leaked.
