//! What a forge call was, and which of GitHub's APIs it spends from

use serde::{Deserialize, Serialize};

/// What a forge call was, named as the port method that made it; the App's
/// calls through the voice carry their own kinds
// wire format: changing this is a breaking change to the forge ledger
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Who can see the repo
    Visibility,
    /// The repo's default branch
    DefaultBranch,
    /// Whether the repo requires a pull request's branch to be up to date with its base
    RequiresUpToDate,
    /// Whether the repo has a branch
    BranchExists,
    /// A folder of the repo's default branch, listed
    Folder,
    /// A file of the repo's default branch, read
    FileText,
    /// The repo's label names
    RepoLabels,
    /// A label made on the repo
    CreateLabel,
    /// Whether the login may push
    CanPush,
    /// Whether a review bot has commented on the repo
    ReviewBotSeen,
    /// One issue, read
    Issue,
    /// One issue's sub-issues, listed
    SubIssues,
    /// The open issues whose sub-issues are all closed
    FinishedParents,
    /// The board's ready issues
    ReadyIssues,
    /// The board's open pull requests
    OpenPullRequests,
    /// One pull request, read with its checks
    PullRequest,
    /// Whether the maintainer has reviewed a pull request
    Reviewed,
    /// What people wrote on a pull request
    Talk,
    /// The login gh acts as
    Viewer,
    /// When an account was created
    AccountCreated,
    /// The comments made since a time
    CommentsSince,
    /// The comments in a pull request's review threads since a time
    ThreadReplies,
    /// Every comment on one issue
    IssueComments,
    /// Whether a login may write to the repo
    CanWrite,
    /// The reactions on a comment
    CommentMarks,
    /// A comment posted as the login
    Comment,
    /// The repo's open issues
    OpenIssues,
    /// An issue opened as the login
    CreateIssue,
    /// An issue closed
    CloseIssue,
    /// An issue closed for a reason the forge records
    CloseIssueAs,
    /// The repo's open pull requests, each with its title and what its body closes
    OpenPulls,
    /// A pull request closed unmerged
    ClosePullRequest,
    /// A pull request opened
    CreatePullRequest,
    /// A pull request's title and body rewritten
    EditPullRequest,
    /// The logs of a head's failed CI runs
    FailedRunLogs,
    /// The latest completed CI run of each workflow on a branch
    BranchRuns,
    /// The repo's workflows, listed
    Workflows,
    /// The end of one CI run's failed log
    RunLogTail,
    /// The failed jobs of a head's CI runs rerun
    RerunFailed,
    /// The failed jobs of one CI run rerun
    RerunRun,
    /// The held CI runs of a head approved
    ApproveWorkflowRuns,
    /// A pull request marked ready
    MarkReady,
    /// A label put on a pull request or taken off
    SetLabel,
    /// A label put on an issue or taken off
    SetIssueLabel,
    /// A blocked-by link added to an issue
    AddBlockedBy,
    /// A review bot's activity on a pull request
    ReviewBot,
    /// A review thread resolved as the login
    ResolveThread,
    /// A reply to a review thread as the login
    ReplyToThread,
    /// A pull request merged
    Merge,
    /// A pull request's place in the merge queue
    MergeQueue,
    /// Auto-merge turned off
    DisableAutoMerge,
    /// When the used-up rate limit resets, asked once a call finds it used up
    RateLimitReset,
    /// The login's rate limits as GitHub counts them, read once a minute
    RateLimit,
    /// The App's token for a worker
    AppWorkerToken,
    /// The App's comment, or its post of one
    AppComment,
    /// A comment-only review the App posted on a pull request
    AppReview,
    /// A comment the App posted, rewritten
    AppEditComment,
    /// A thumbs up from the App
    AppThumbsUp,
    /// An issue opened by the App
    AppCreateIssue,
    /// An issue's body rewritten by the App, its title as it stands
    AppEditIssue,
    /// A review thread resolved by the App
    AppResolveThread,
    /// A reply to a review thread by the App
    AppReplyToThread,
    /// A label made by the App
    AppCreateLabel,
    /// A label put on an issue or taken off by the App
    AppSetIssueLabel,
}

impl Kind {
    /// The name the ledger gives it, such as `pull_request`
    pub fn name(self) -> String {
        super::wire_name(&self)
    }

    /// Whether the call changes what the forge shows, which a read right
    /// after it can see
    pub fn writes(self) -> bool {
        !matches!(
            self,
            Self::Visibility
                | Self::DefaultBranch
                | Self::RequiresUpToDate
                | Self::BranchExists
                | Self::Folder
                | Self::FileText
                | Self::RepoLabels
                | Self::CanPush
                | Self::ReviewBotSeen
                | Self::Issue
                | Self::SubIssues
                | Self::FinishedParents
                | Self::ReadyIssues
                | Self::OpenPullRequests
                | Self::PullRequest
                | Self::Reviewed
                | Self::Talk
                | Self::Viewer
                | Self::AccountCreated
                | Self::CommentsSince
                | Self::ThreadReplies
                | Self::IssueComments
                | Self::CanWrite
                | Self::CommentMarks
                | Self::OpenIssues
                | Self::OpenPulls
                | Self::FailedRunLogs
                | Self::BranchRuns
                | Self::Workflows
                | Self::RunLogTail
                | Self::ReviewBot
                | Self::MergeQueue
                | Self::RateLimitReset
                | Self::RateLimit
                | Self::AppWorkerToken
        )
    }

    /// Whether GitHub counts the call against a rate limit, which a read of
    /// the rate limits themselves is not
    pub fn counts(self) -> bool {
        !matches!(self, Self::RateLimit | Self::RateLimitReset)
    }

    /// Which of GitHub's APIs the call spends from; a call that asks both
    /// counts as GraphQL, the one with the smaller allowance
    ///
    /// gh's own list commands go through its GraphQL list query, so `gh issue
    /// list` and `gh pr list` count as GraphQL whatever flags they carry.
    pub fn api(self) -> Api {
        match self {
            Self::Visibility
            | Self::DefaultBranch
            | Self::RepoLabels
            | Self::CanPush
            | Self::Issue
            | Self::FinishedParents
            | Self::ReadyIssues
            | Self::OpenPullRequests
            | Self::OpenPulls
            | Self::PullRequest
            | Self::Reviewed
            | Self::Talk
            | Self::ThreadReplies
            | Self::CommentMarks
            | Self::CreateIssue
            | Self::CloseIssue
            | Self::ClosePullRequest
            | Self::MarkReady
            | Self::SetLabel
            | Self::SetIssueLabel
            | Self::ReviewBot
            | Self::ResolveThread
            | Self::ReplyToThread
            | Self::Merge
            | Self::MergeQueue
            | Self::DisableAutoMerge
            | Self::AppResolveThread
            | Self::AppReplyToThread => Api::GraphQl,
            Self::BranchExists
            | Self::RequiresUpToDate
            | Self::Folder
            | Self::FileText
            | Self::SubIssues
            | Self::CloseIssueAs
            | Self::AddBlockedBy
            | Self::CreateLabel
            | Self::RateLimitReset
            | Self::RateLimit
            | Self::ReviewBotSeen
            | Self::Viewer
            | Self::AccountCreated
            | Self::OpenIssues
            | Self::CommentsSince
            | Self::IssueComments
            | Self::CanWrite
            | Self::Comment
            | Self::CreatePullRequest
            | Self::EditPullRequest
            | Self::FailedRunLogs
            | Self::BranchRuns
            | Self::Workflows
            | Self::RunLogTail
            | Self::RerunFailed
            | Self::RerunRun
            | Self::ApproveWorkflowRuns
            | Self::AppWorkerToken
            | Self::AppComment
            | Self::AppReview
            | Self::AppEditComment
            | Self::AppThumbsUp
            | Self::AppCreateIssue
            | Self::AppEditIssue
            | Self::AppCreateLabel
            | Self::AppSetIssueLabel => Api::Rest,
        }
    }
}

impl Api {
    /// The name the ledger gives it: `rest` or `graphql`
    pub fn name(self) -> String {
        super::wire_name(&self)
    }
}

/// Which of GitHub's APIs a call spends from
// wire format: changing this is a breaking change to the forge ledger
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Api {
    /// The REST API
    Rest,
    /// The GraphQL API
    #[serde(rename = "graphql")]
    GraphQl,
}

impl Identity {
    /// The name the ledger gives it: `login` or `app`
    pub fn name(self) -> String {
        super::wire_name(&self)
    }
}

/// Whose rate budget a forge call spent
///
/// The maintainer's login and the App's installation token each have a
/// budget of their own, so the ledger tells them apart.
// wire format: changing this is a breaking change to the forge ledger
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Identity {
    /// The maintainer's `gh` login
    #[default]
    Login,
    /// Kelpie's GitHub App, on an installation token
    App,
}
