//! The heads a work item's merge may take: ones a review round read, ones
//! kelpie sends to CI unread on purpose, and an adopted pull request's head
//! as it arrived

use super::WorkItem;

impl WorkItem {
    /// Records that a review round read `head`
    pub fn reviewed(&mut self, head: String) {
        if !self.reviewed_heads.contains(&head) {
            self.reviewed_heads.push(head);
        }
    }

    /// Records `head` as one kelpie sends to CI unread on purpose: a fix
    /// turn's push, a pass the project's own list gives nobody to read, or
    /// kelpie's own catch-up of a head it vouches for
    pub fn send_unread(&mut self, head: String) {
        if !self.sent_unread.contains(&head) {
            self.sent_unread.push(head);
        }
    }

    /// Whether the merge gate may take `head`: a review round read it, kelpie
    /// sent it to CI unread on purpose, or the pull request arrived at it
    /// when adopted
    pub fn vouches_for(&self, head: &str) -> bool {
        let is = |known: &String| known == head;
        self.reviewed_heads.iter().any(is)
            || self.sent_unread.iter().any(is)
            || self.arrived.as_ref().is_some_and(is)
    }
}
