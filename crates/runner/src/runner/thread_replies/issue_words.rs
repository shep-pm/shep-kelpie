//! The maintainer's comment on an issue with no work item, which is words to
//! the project manager
//!
//! A comment on an issue nothing is watched on is held the way free text under
//! a ruling is: it goes to `pm_told` as a [`held_line`], gets a thumbs up, and
//! rides the next `Wake::Told`. Only the maintainer's own comment is, and only
//! on an open issue the triage poll listed, which lists issues alone: a pull
//! request's conversation is read apart, and its comments are a review round's.
//! One written before this runner first asked is read by a run that is over,
//! and one the poll had listed nothing for is kept for a later read.

use super::super::pm::held_line;
use super::plain_login;
use crate::ports::{ThreadComment, Timestamp};
use crate::runner::Runner;
use crate::runner::thread_replies::UNHELD_FOR;

impl Runner {
    // The `pm_told` line for `comment`, where it is the maintainer's own words
    // on an open issue with no work item and no triage wait, written since this
    // runner first asked for the comments. None for anyone else's comment, a
    // bot's, kelpie's own, and one on a pull request or a closed issue.
    pub(super) fn issue_words(&mut self, comment: &ThreadComment) -> Option<String> {
        // A comment written before this runner first asked is one a run that is
        // over read already, however far a saved watch reaches the read back.
        let first = self.threads.first?;
        if comment.bot || comment.created < first || self.threads.judged.contains_key(&comment.id) {
            return None;
        }
        let mine =
            self.state.posted.contains(&comment.id) || self.posted.borrow().contains(&comment.id);
        // An issue with a work item is read by its comment round, and a triage
        // wait is answered by its author's comments, not held here.
        let waiting = (self.triage_waits())
            .iter()
            .any(|(issue, _)| *issue == comment.thread);
        if mine || waiting || self.item_on(comment.thread) {
            return None;
        }
        // With no listing read yet, the issue is not known to be open, or even
        // an issue: the comment is kept for the read that can tell.
        let Some(listed) = self.listed_issue(comment.thread) else {
            self.keep_back(comment);
            return None;
        };
        if !listed {
            return None;
        }
        let app = self.app_login().map(|app| plain_login(&app));
        let who = plain_login(&comment.author);
        // The maintainer's own login, and never the App posting as it
        let maintainer = plain_login(&self.maintainer().ok()?);
        (who == maintainer && app.is_none_or(|app| who != app))
            .then(|| held_line(comment.thread, comment.id, None, &comment.body))
    }

    // Keeps `comment` for a read that can decide it, by reaching back to when it
    // was written. The earliest one reached covers the rest of them.
    fn keep_back(&mut self, comment: &ThreadComment) {
        self.threads.unheld = Some(match self.threads.unheld {
            Some((thread, at)) if at <= comment.created => (thread, at),
            _ => (comment.thread, comment.created),
        });
    }

    // How far the next read reaches back for a comment kept this way, or None.
    // One kept past `UNHELD_FOR` is left unheard, and said: a poll that keeps
    // failing to list the issues is no reason to read the same comments again.
    pub(in crate::runner) fn unheld_back(&mut self, now: Timestamp) -> Option<Timestamp> {
        match self.threads.unheld.take() {
            Some((_, at)) if now.0.saturating_sub(at.0) <= UNHELD_FOR => Some(at),
            Some((thread, _)) => {
                self.notes.push(format!(
                    "the maintainer's comment on #{thread} was read with no issue \
                     listing known, and is left unheard"
                ));
                None
            }
            None => None,
        }
    }
}
