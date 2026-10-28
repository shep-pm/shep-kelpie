//! A pull request as a rework or an adoption reads it

use super::PullRequestState;

/// A pull request as a rework or an adoption reads it
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reviewed {
    /// Open, merged or closed
    pub state: PullRequestState,
    /// Its title
    pub title: String,
    /// Its body, as written
    pub body: String,
    /// The issues on the same repo that it closes when it merges
    pub closes: Vec<u64>,
    /// The branch it merges into
    pub base: String,
    /// The branch it merges from
    pub branch: String,
    /// Whether that branch is on a fork rather than the repo itself
    pub from_fork: bool,
    /// Whether the author lets the repo's maintainers push to the branch
    /// ("allow edits by maintainers"). The forge says so for a fork only.
    pub maintainer_can_edit: bool,
    /// Where the branch's repository is cloned from, `None` once a fork was
    /// deleted
    pub head_clone_url: Option<String>,
    /// Whether an organization owns the branch's repository. GitHub refuses a
    /// maintainer's push to such a fork whatever `maintainer_can_edit` says.
    pub head_owner_is_organization: bool,
    /// Its author's login, empty for a deleted account
    pub author: String,
    /// Whether it is still a draft
    pub draft: bool,
    /// Its labels' names
    pub labels: Vec<String>,
    /// The maintainer's latest review: the latest one a person left, since
    /// kelpie's own reviewers post none and bots are left out
    pub review: Option<MaintainerReview>,
}

impl Reviewed {
    /// Whether kelpie can push to the branch: it is on the repo itself, or on
    /// a fork that still exists, lets the maintainers edit and no
    /// organization owns
    pub fn pushable(&self) -> bool {
        !self.from_fork
            || (self.maintainer_can_edit
                && self.head_clone_url.is_some()
                && !self.head_owner_is_organization)
    }
}

/// One review the maintainer left, as written
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaintainerReview {
    /// The forge's id for it
    pub id: String,
    /// Whether it requests changes, which only a reviewer other than the
    /// pull request's author can do
    pub changes_requested: bool,
    /// Its body, empty when it has none
    pub body: String,
    /// Its comments whose threads are still unresolved, in the forge's order
    pub comments: Vec<ReviewComment>,
}

/// One comment on a line of a pull request's diff
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewComment {
    /// The file it is on
    pub file: String,
    /// The line, when the forge still places it
    pub line: Option<u32>,
    /// What it says
    pub body: String,
}
