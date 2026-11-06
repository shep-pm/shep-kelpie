//! The author's own session: the prompt in the first comment, and its write-back
//!
//! The first triage comment holds, in a collapsed `<details>` section, a block
//! with the prompt an author pastes into their own agent session: the issue, the
//! files named and the questions asked. Their session writes what it settles
//! back into the issue body; the record keeps `local { at, body }`, `body` a
//! hash of the body kelpie last saw, and kelpie's own edits set it. A body whose
//! hash differs is triaged again by a [`Purpose::LocalRead`] call, whose comment
//! ends the round. Everything the block names is put on one line with backticks
//! gone, so none of it closes the fence. Silence ends the wait as `none` does.

use std::collections::BTreeMap;

use super::super::Runner;
use super::super::contributions::post::forge_is_busy;
use super::super::report::Begin;
use super::ask::{Purpose, REPLY_HINT, closed_note, questions_block};
use super::marker::MARKER;
use super::research::{Found, Question};
use crate::ports::{ForgeError, Issue};
use crate::state::StateError;

/// The skill the prompt starts with
const SKILL: &str = "grill-with-docs";

/// Where an author without the skill can get it, said beside the prompt
pub(super) const SKILL_WHERE: &str = "The `grill-with-docs` skill is from mattpocock/skills. Kelpie's own \
    copy of it is in `shep-pm/shep-kelpie` at `skills/skills/engineering/grill-with-docs`.";

/// What kind of post the representative is asked for on the pass that reads the
/// issue again after its author's own session rewrote its body
pub(in crate::runner) const LOCAL_READ_KIND: &str = "Write the triage comment on this issue, which kelpie read again \
    after its author's own session rewrote its body from kelpie's prompt. Greet the author, say \
    what kelpie now found, and ask only the questions still open. This is the last round: what \
    is still open goes to the maintainer. It is a comment, not a review: no verdict, and nothing \
    promised.";

impl Runner {
    // The first comment that asks the author: the representative's words, the
    // questions code lists with each recommended answer, and the prompt in a
    // collapsed section, the triage marker last. The body hash comes from a read
    // made just now, as the wording call can run for minutes.
    pub(super) fn ask_comment(
        &mut self,
        issue: u64,
        words: &str,
        found: &Found,
        cc: Option<&str>,
    ) -> Option<(String, u64)> {
        let read =
            super::ticket(issue).and_then(|ticket| self.ports.tickets.issue(&self.remote, ticket));
        let read = match read {
            Ok(read) => read,
            Err(e) => {
                let why = format!("cannot read its body: {e}");
                self.triage_failed(issue, &why, !forge_is_busy(&e));
                return None;
            }
        };
        let questions = questions_block(&found.open);
        let prompt = local_block(
            self.remote.as_str(),
            issue,
            self.scrub_triage_title(issue, &read.title),
            &found.files,
            &found.open,
        );
        // The questions go to someone other than the author only on an issue the
        // App opened, and the mention under the words is what notifies them.
        let cc = cc.map_or_else(String::new, |login| format!("cc @{login}\n\n"));
        let body = format!(
            "{words}\n\n{cc}{questions}\n\n{REPLY_HINT}\n\n<details>\n<summary>Settle these in \
             your own terminal</summary>\n\n{SKILL_WHERE}\n\n```text\n{prompt}\n```\n\n</details>\n\n{MARKER}"
        );
        Some((body, body_hash(&read.body)))
    }

    // Reads each issue whose triage waits on a local session. A body whose hash
    // differs is the round's answer: the triager reads the issue again on that
    // body. Silence `triage.wait_days` after the prompt ends as `none` does, here
    // and not in [`Runner::hear_triage`], so a rewrite is read before it counts.
    pub(super) fn watch_local_waits(
        &mut self,
        reads: &BTreeMap<u64, Result<Issue, ForgeError>>,
    ) -> Result<(), StateError> {
        for record in self.triage_waiting() {
            let (Some(local), issue) = (record.local, record.issue) else {
                continue;
            };
            // No read under a call in flight: its comment is what ends the wait,
            // and a body read between the call and its comment is the body that
            // call was made on.
            if self.flights.triaging() == Some(issue) || self.triage_given_up(issue) {
                continue;
            }
            // An issue the poll did not read, as one that began to wait while it
            // read, is read at the next.
            let Some(read) = reads.get(&issue) else {
                continue;
            };
            let body = match read {
                // Closed while the poll read: the listing it came with shows it open.
                Ok(found) if !found.open => {
                    self.triage_wait_stood(issue, closed_note(issue))?;
                    continue;
                }
                Ok(found) => found.body.clone(),
                Err(e) => {
                    let why = format!("cannot read its body: {e}");
                    self.triage_failed(issue, &why, !forge_is_busy(e));
                    continue;
                }
            };
            if body_hash(&body) != local.body {
                let seen = body_hash(&body);
                let told = self.triage_tries.get(&issue).is_some_and(|t| t.local_edit);
                let tries = self.triage_tries.entry(issue).or_default();
                tries.local_edit = true;
                tries.local_read = Some(seen);
                if !told {
                    self.notes.push(format!(
                        "triage #{issue}: its body changed since kelpie's prompt went up, so the \
                         triager reads it again"
                    ));
                }
                continue;
            }
            let heard = self
                .triage_tries
                .get(&issue)
                .is_some_and(|t| t.answers.is_some());
            if heard || self.ports.clock.now().0 < local.at.0.saturating_add(self.wait_seconds()) {
                continue;
            }
            self.notes.push(format!(
                "triage #{issue}: its author wrote no comment and no new body within \
                 `triage.wait_days` of the prompt, which counts as `none`: the maintainer \
                 triages it as it stands"
            ));
            self.triage_none(issue)?;
        }
        Ok(())
    }

    // Whether the pass that reads a body the author's session rewrote is out of
    // date: the body changed again while its call ran, so its comment, its
    // labels and a ready write from it would describe a body the issue no longer
    // holds. The pass is owed again on the newer body.
    pub(super) fn local_read_stale(&mut self, issue: u64) -> Result<bool, StateError> {
        let owed = self
            .triage_tries
            .get(&issue)
            .filter(|t| t.purpose == Purpose::LocalRead)
            .and_then(|t| t.local_read);
        let Some(owed) = owed else {
            return Ok(false);
        };
        let read =
            super::ticket(issue).and_then(|ticket| self.ports.tickets.issue(&self.remote, ticket));
        let found = match read {
            Ok(found) => found,
            Err(e) => {
                let why = format!("cannot read its body: {e}");
                self.triage_failed(issue, &why, !forge_is_busy(&e));
                return Ok(true);
            }
        };
        let seen = body_hash(&found.body);
        if seen == owed {
            return Ok(false);
        }
        self.notes.push(format!(
            "triage #{issue}: its body changed again while the triager read it, so that read is \
             not posted and it is taken again on the newer body"
        ));
        if let Some(tries) = self.triage_tries.get_mut(&issue) {
            tries.local_edit = true;
            tries.local_read = Some(seen);
            tries.waiting = None;
            tries.body = None;
        }
        Ok(true)
    }

    // Keeps the hash a `local` wait is measured against at the body kelpie just
    // wrote, so a write of kelpie's own is never read as the author's session's
    // edit. A record with no such wait needs nothing: its next wait takes the
    // body the forge holds then.
    pub(in crate::runner) fn triage_body_written(
        &mut self,
        issue: u64,
        body: &str,
    ) -> Result<(), StateError> {
        if self.triage_record(issue).and_then(|t| t.local).is_none() {
            return Ok(());
        }
        let mut next = self.state.clone();
        if let Some(record) = next
            .triage
            .as_mut()
            .and_then(|t| t.issues.iter_mut().find(|t| t.issue == issue))
            && let Some(local) = record.local.as_mut()
        {
            local.body = body_hash(body);
        }
        self.save(next)
    }

    // The issues whose author answered, or whose body the author's session
    // rewrote, each owed a call that takes a slot as the first one does.
    pub(super) fn triage_round_due(&mut self) -> Result<Option<Begin>, StateError> {
        for record in self.triage_waiting() {
            let issue = record.issue;
            if self.flights.triage_tried(issue) || self.triage_given_up(issue) {
                continue;
            }
            let tries = self.triage_tries.get(&issue);
            let answers = tries.is_some_and(|t| t.answers.is_some());
            let read = record.local.is_some() && tries.is_some_and(|t| t.local_edit);
            let purpose = match (answers, read) {
                (true, _) => Purpose::Answers,
                (false, true) => Purpose::LocalRead,
                _ => continue,
            };
            // The day's cap reaches the round as it reaches the first pass; the record
            // gives its author, filled in from the listing when it names none. A round
            // call waits for a free place rather than ending an issue already triaged. The
            // author's share and the account's age are left out: this issue is taken on.
            let limited = self.triage_limited(&record.author, false);
            if limited && self.triage_round_held() {
                continue;
            }
            self.flights.triage_try(issue);
            if let Some(job) = self.triage_job(issue, purpose, limited) {
                return Ok(Some(Begin::Triage(Box::new(job))));
            }
        }
        Ok(None)
    }
}

// The prompt an author pastes into their own session: the issue, how to read it
// and the files named, the open questions with what the research recommends,
// and the write-back. Every word from an agent's reply is put on one line with
// its backticks gone, so none can close the fence or become a prompt line.
pub(super) fn local_block(
    repo: &str,
    issue: u64,
    title: &str,
    files: &[String],
    questions: &[Question],
) -> String {
    let title = match title.is_empty() {
        true => String::new(),
        false => format!(": {}", one_line(title)),
    };
    let mut block = format!("/{SKILL} Issue {repo}#{issue}{title}.\n");
    block.push_str(&format!(
        "\nRead it with `gh issue view {issue} --comments`.\n"
    ));
    if !files.is_empty() {
        block.push_str("\nThen read the files kelpie's triage comment named:\n");
        for file in files {
            block.push_str(&format!("- `{}`\n", one_line(file)));
        }
    }
    block.push_str(
        "\nSettle these open questions, each with the answer kelpie's research recommends:\n",
    );
    for (i, q) in questions.iter().enumerate() {
        let asked = format!("{}. {}", i + 1, one_line(&q.question));
        match one_line(&q.recommended).as_str() {
            "" => block.push_str(&format!("{asked}\n")),
            recommended => block.push_str(&format!("{asked} (recommended: {recommended})\n")),
        }
    }
    block.push_str(
        "\nWhen we agree, rewrite this issue's body with acceptance criteria and a `Touches:` \
         list of the files the work touches, and comment on the issue with the decisions we \
         made.",
    );
    block
}

// The title, file, question or answer as one line that cannot close the fence
// the prompt is posted in, or stand as a line of the prompt itself
fn one_line(text: &str) -> String {
    (text.chars())
        .map(|c| match c {
            '`' => ' ',
            c if c.is_whitespace() => ' ',
            c => c,
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

// FNV-1a over the body's bytes, trimmed
pub(super) fn body_hash(body: &str) -> u64 {
    crate::board::check::fnv1a(body.trim().as_bytes())
}
