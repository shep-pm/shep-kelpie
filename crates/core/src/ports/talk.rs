//! What people wrote on a pull request: its comments and its review threads

/// What the forge says an author may do on the repo
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    /// The repo's owner, a member of its organisation or a collaborator, who
    /// can write to it
    Writer,
    /// A bot or an App, whose comments are no one's request
    Bot,
    /// Anyone else, such as a stranger commenting on a public repo
    Other,
}

/// One comment, on the pull request's conversation or in a review thread
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remark {
    /// The forge's id for it, which rises with each comment of its kind
    pub id: u64,
    /// Its author's login
    pub author: String,
    /// What its author may do on the repo
    pub standing: Standing,
    /// What it says, as written
    pub body: String,
}

/// A review thread on the pull request's diff
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TalkThread {
    /// The forge's id for it, which resolving and replying take
    pub id: String,
    /// The file it is on
    pub path: String,
    /// The line, when the forge still places it
    pub line: Option<u32>,
    /// Whether it is resolved
    pub resolved: bool,
    /// Whether the lines it is on have changed since, so the forge no longer
    /// shows it against the head
    pub outdated: bool,
    /// Its comments, oldest first
    pub remarks: Vec<Remark>,
}

/// Everything people wrote on one pull request
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Talk {
    /// The comments on its conversation, oldest first
    pub comments: Vec<Remark>,
    /// Its review threads, resolved ones included
    pub threads: Vec<TalkThread>,
}
