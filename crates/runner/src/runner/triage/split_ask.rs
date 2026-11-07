//! A split of someone else's issue, held until the maintainer says yes
//!
//! Only the maintainer's word starts git work, and the gate is code:
//! [`Runner::label_split`] files nothing while the plan's `asked` is set. Once
//! the pass ends, [`Runner::ask_split`] posts a new comment naming the
//! maintainer, and that comment's id and time are what `asked` holds from then
//! on. Before it goes up `asked` holds the triage comment, which no reply can
//! be to. The maintainer's `yes` or `ok` ([`super::heard`]) clears `asked` and
//! steps the record back to `body-written`, where the next poll files the
//! split. `no` drops the plan, and so does silence `triage.wait_days`.

use super::super::Runner;
use super::super::contributions::post::forge_is_busy;
use super::ask::says_yes;
use crate::board::{AGENT_LABEL, PIN, TRIAGE};
use crate::ports::Timestamp;
use crate::state::{SplitDraft, StateError, TriageQuestions, TriageStep};

// The comment that asks `maintainer` to say yes to `drafts`
fn ask_text(maintainer: &str, drafts: &[SplitDraft], days: u32) -> String {
    let mut text = format!(
        "@{maintainer} Kelpie judged this issue too big for one work item. With your yes it \
         files these sub-issues of it, in this order, each labelled `ready-for-agent`:\n\n"
    );
    for (n, draft) in drafts.iter().enumerate() {
        text.push_str(&format!("{}. {}", n + 1, draft.title.trim()));
        if !draft.after.is_empty() {
            let after: Vec<String> = draft.after.iter().map(|a| (a + 1).to_string()).collect();
            text.push_str(&format!(" (after {})", after.join(", ")));
        }
        let agent =
            (draft.labels.iter()).find_map(|l| l.strip_prefix(AGENT_LABEL)?.strip_suffix(PIN));
        if let Some(agent) = agent {
            text.push_str(&format!(" (built by {agent})"));
        }
        text.push('\n');
    }
    text.push_str(&format!(
        "\nReply `yes` to file them, or `no` to drop the plan. Only your reply counts. With no \
         reply in {days} days the plan is dropped and the issue stays as it is."
    ));
    text
}

impl Runner {
    // The issues whose split waits on the maintainer's word, each with when
    // the ask went up. A plan whose ask is not posted yet has no wait.
    pub(in crate::runner) fn split_waits(&self) -> Vec<(u64, Timestamp)> {
        let Some(triage) = self.state.triage.as_ref() else {
            return Vec::new();
        };
        (triage.splits.iter())
            .filter_map(|split| Some((split.issue, split.asked?)))
            .filter(|(issue, held)| {
                self.triage_record(*issue)
                    .is_none_or(|record| record.comment != held.comment)
            })
            .map(|(issue, held)| (issue, held.at))
            .collect()
    }

    // Posts the comment that asks the maintainer to say yes to the split of
    // `issue`, once, and saves it as the wait. False when it could not be
    // posted, which is tried at the next poll. A plan with no maintainer to ask
    // is dropped.
    pub(super) fn ask_split(&mut self, issue: u64) -> Result<bool, StateError> {
        let Some(split) = self.split_of(issue).cloned() else {
            return Ok(true);
        };
        let comment = self.triage_record(issue).map(|t| t.comment);
        if split.asked.is_none_or(|held| Some(held.comment) != comment) {
            return Ok(true);
        }
        let maintainer = match self.maintainer() {
            Ok(login) => login,
            Err(why) => {
                let note = format!(
                    "triage #{issue}: its split asks the maintainer and none is named, so it is \
                     dropped: {why}"
                );
                self.drop_split(issue, note)?;
                return Ok(true);
            }
        };
        let days = self.settings.triage.wait_days.get();
        let text = ask_text(&maintainer, &split.drafts, days);
        match self.ports.voice.post(&self.remote, issue, &text) {
            Ok(posted) => {
                let mut next = self.state.clone();
                let held = (next.triage.as_mut())
                    .and_then(|t| t.splits.iter_mut().find(|s| s.issue == issue));
                if let Some(held) = held {
                    held.asked = Some(TriageQuestions {
                        comment: posted.id,
                        at: posted.created,
                    });
                }
                self.save(next)?;
                Ok(true)
            }
            Err(e) => {
                let why = format!("cannot post the ask for its split: {e}");
                self.triage_failed(issue, &why, !forge_is_busy(&e));
                Ok(false)
            }
        }
    }

    // Takes the plan saved for `issue` back, with `note` in the log
    pub(super) fn drop_split(&mut self, issue: u64, note: String) -> Result<(), StateError> {
        let mut next = self.state.clone();
        if let Some(triage) = next.triage.as_mut() {
            triage.splits.retain(|s| s.issue != issue);
        }
        self.save(next)?;
        self.notes.push(note);
        Ok(())
    }

    // Hears what the maintainer wrote under the ask of `issue`, oldest first:
    // the first `yes` releases the split and the first `no` drops it. Any
    // other text leaves the wait open.
    pub(super) fn heard_split(&mut self, issue: u64, replies: &[&str]) -> Result<(), StateError> {
        for reply in replies {
            if says_yes(reply) {
                return self.allow_split(issue);
            }
            if super::ask::says_no(reply) {
                let note =
                    format!("triage #{issue}: the maintainer said no, so its split is dropped");
                return self.drop_split(issue, note);
            }
        }
        Ok(())
    }

    // Clears the hold on the split of `issue` and steps its record back to
    // `body-written`, which the next poll files from. The `needs-triage` the
    // pass put on is kelpie's own, so it joins the labels the pass began with.
    fn allow_split(&mut self, issue: u64) -> Result<(), StateError> {
        let mut next = self.state.clone();
        let Some(triage) = next.triage.as_mut() else {
            return Ok(());
        };
        let Some(record) = triage.issues.iter_mut().find(|t| t.issue == issue) else {
            let note = format!(
                "triage #{issue}: the maintainer said yes, but its triage record is gone, so \
                 its split is dropped"
            );
            return self.drop_split(issue, note);
        };
        record.step = TriageStep::BodyWritten;
        if !record.seen.iter().any(|l| l == TRIAGE) {
            record.seen.push(TRIAGE.to_owned());
        }
        if let Some(split) = triage.splits.iter_mut().find(|s| s.issue == issue) {
            split.asked = None;
        }
        self.save(next)?;
        self.notes.push(format!(
            "triage #{issue}: the maintainer said yes, so its split is filed at the next poll"
        ));
        Ok(())
    }

    // Drops each plan whose ask has gone `triage.wait_days` with no yes. It
    // needs no read of the thread, so with no App the plan still drops.
    pub(super) fn drop_silent_splits(&mut self) -> Result<(), StateError> {
        let now = self.ports.clock.now().0;
        for (issue, at) in self.split_waits() {
            if self.flights.triaging() == Some(issue)
                || now < at.0.saturating_add(self.wait_seconds())
            {
                continue;
            }
            let note = format!(
                "triage #{issue}: the maintainer gave no answer within `triage.wait_days` of the \
                 ask, so its split is dropped"
            );
            self.drop_split(issue, note)?;
        }
        Ok(())
    }
}
