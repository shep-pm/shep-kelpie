//! The maintainer's comment on an issue with no work item, which is words to
//! the project manager
//!
//! A comment on an issue nothing is watched on is held the way free text under
//! a ruling is: it goes to `pm_told` as a [`held_line`], gets a thumbs up, and
//! rides the next `Wake::Told`. Only the maintainer's own comment is, and only
//! on an open issue the triage poll listed, which lists issues alone: a pull
//! request's conversation is read apart, and its comments are a review round's.
//! A comment written before this runner first asked for the comments is one a
//! run that is over already read, and is not held again.

use super::super::pm::held_line;
use super::plain_login;
use crate::ports::ThreadComment;
use crate::runner::Runner;

impl Runner {
    // The `pm_told` line for `comment`, where it is the maintainer's own words
    // on an open issue with no work item and no triage wait, written since this
    // runner first asked for the comments. None for anyone else's comment, a
    // bot's, kelpie's own, and one on a pull request or a closed issue.
    pub(super) fn issue_words(&self, comment: &ThreadComment) -> Option<String> {
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
        if mine || waiting || !self.listed_issue(comment.thread) || self.item_on(comment.thread) {
            return None;
        }
        let app = self.app_login().map(|app| plain_login(&app));
        let who = plain_login(&comment.author);
        // The maintainer's own login, and never the App posting as it
        let maintainer = plain_login(&self.maintainer().ok()?);
        (who == maintainer && app.is_none_or(|app| who != app))
            .then(|| held_line(comment.thread, comment.id, None, &comment.body))
    }
}
