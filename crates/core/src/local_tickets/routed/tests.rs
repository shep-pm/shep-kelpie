use std::sync::Mutex;

use super::*;
use crate::local_tickets::Links;
use crate::test::short_tempdir;

fn id(text: &str) -> TicketId {
    text.parse().unwrap()
}

fn repo() -> ForgeSlug {
    "shep-pm/shep".to_owned().try_into().unwrap()
}

// GitHub as the tests see it: it says which calls reached it, and files issue 900
#[derive(Default)]
struct Github {
    calls: Mutex<Vec<String>>,
}

impl Github {
    fn called(&self, what: String) {
        self.calls.lock().unwrap().push(what);
    }
}

fn issue(title: &str) -> Issue {
    Issue {
        title: title.to_owned(),
        body: String::new(),
        labels: Vec::new(),
        open: true,
        parent: None,
        blocked_by: Vec::new(),
    }
}

impl Tickets for Github {
    fn issue(&self, _repo: &ForgeSlug, ticket: TicketId) -> Result<Issue, ForgeError> {
        self.called(format!("issue {ticket}"));
        Ok(issue("on GitHub"))
    }

    fn sub_issues(&self, _: &ForgeSlug, parent: TicketId) -> Result<Vec<SubIssue>, ForgeError> {
        self.called(format!("sub_issues {parent}"));
        Ok(Vec::new())
    }

    fn finished_parents(&self, _: &ForgeSlug) -> Result<Vec<FinishedParent>, ForgeError> {
        self.called("finished_parents".into());
        Ok(Vec::new())
    }

    fn ready_issues(&self, _: &ForgeSlug) -> Result<Vec<ReadyIssue>, ForgeError> {
        self.called("ready_issues".into());
        Ok(Vec::new())
    }

    fn open_issues(&self, _: &ForgeSlug) -> Result<Vec<OpenIssue>, ForgeError> {
        self.called("open_issues".into());
        Ok(Vec::new())
    }

    fn issue_comments(
        &self,
        _: &ForgeSlug,
        ticket: TicketId,
    ) -> Result<Vec<ThreadComment>, ForgeError> {
        self.called(format!("issue_comments {ticket}"));
        Ok(Vec::new())
    }

    fn create_issue(
        &self,
        _: &ForgeSlug,
        title: &str,
        _: &str,
        _: &[&str],
    ) -> Result<TicketId, ForgeError> {
        self.called(format!("create_issue {title}"));
        Ok(id("900"))
    }

    fn add_blocked_by(
        &self,
        _: &ForgeSlug,
        ticket: TicketId,
        blocker: TicketId,
    ) -> Result<bool, ForgeError> {
        self.called(format!("add_blocked_by {ticket} {blocker}"));
        Ok(true)
    }

    fn close_issue(&self, _: &ForgeSlug, ticket: TicketId) -> Result<(), ForgeError> {
        self.called(format!("close_issue {ticket}"));
        Ok(())
    }

    fn close_issue_as(
        &self,
        _: &ForgeSlug,
        ticket: TicketId,
        _: IssueCloseReason,
    ) -> Result<(), ForgeError> {
        self.called(format!("close_issue_as {ticket}"));
        Ok(())
    }

    fn set_issue_label(
        &self,
        _: &ForgeSlug,
        ticket: TicketId,
        label: &str,
        _: bool,
    ) -> Result<(), ForgeError> {
        self.called(format!("set_issue_label {ticket} {label}"));
        Ok(())
    }
}

fn stand_up(source: TicketSource) -> (tempfile::TempDir, Arc<Github>, Routed, LocalTickets) {
    let home = short_tempdir();
    let local = LocalTickets::new(home.path().join("tickets"));
    let github = Arc::new(Github::default());
    let routed = Routed::new(github.clone(), local.clone(), source);
    (home, github, routed, local)
}

#[test]
fn each_ticket_is_read_from_the_source_its_id_names() {
    for source in [TicketSource::Local, TicketSource::Github] {
        let (_home, github, routed, local) = stand_up(source);
        local
            .create("In the store", "", &[], Links::default())
            .unwrap();
        assert_eq!(routed.issue(&repo(), id("12")).unwrap().title, "on GitHub");
        assert_eq!(
            routed.issue(&repo(), id("k1")).unwrap().title,
            "In the store"
        );
        assert_eq!(*github.calls.lock().unwrap(), ["issue 12"]);
    }
}

#[test]
fn a_ticket_is_closed_and_labelled_in_the_source_its_id_names() {
    let (_home, github, routed, local) = stand_up(TicketSource::Local);
    local
        .create("In the store", "", &[], Links::default())
        .unwrap();
    routed.close_issue(&repo(), id("k1")).unwrap();
    routed.close_issue(&repo(), id("12")).unwrap();
    routed
        .set_issue_label(&repo(), id("12"), "wip", true)
        .unwrap();
    assert!(!routed.issue(&repo(), id("k1")).unwrap().open);
    assert_eq!(
        *github.calls.lock().unwrap(),
        ["close_issue 12", "set_issue_label 12 wip"]
    );
}

#[test]
fn a_link_goes_to_the_source_of_the_ticket_it_blocks() {
    let (_home, github, routed, local) = stand_up(TicketSource::Local);
    local.create("Blocker", "", &[], Links::default()).unwrap();
    local.create("Blocked", "", &[], Links::default()).unwrap();
    routed.add_blocked_by(&repo(), id("k2"), id("k1")).unwrap();
    routed.add_blocked_by(&repo(), id("12"), id("7")).unwrap();
    assert_eq!(
        routed.issue(&repo(), id("k2")).unwrap().blocked_by,
        [id("k1")]
    );
    assert_eq!(*github.calls.lock().unwrap(), ["add_blocked_by 12 7"],);
}

#[test]
fn the_board_reads_the_projects_source() {
    let (_home, github, routed, local) = stand_up(TicketSource::Local);
    local
        .create("Ready", "", &[crate::board::READY], Links::default())
        .unwrap();
    assert_eq!(routed.ready_issues(&repo()).unwrap().len(), 1);
    assert!(github.calls.lock().unwrap().is_empty());

    let (_home, github, on_github, _local) = stand_up(TicketSource::Github);
    on_github.ready_issues(&repo()).unwrap();
    assert_eq!(*github.calls.lock().unwrap(), ["ready_issues"]);
}

#[test]
fn a_follow_up_is_filed_on_github_even_for_a_local_project() {
    let (_home, github, routed, local) = stand_up(TicketSource::Local);
    let filed = routed.create_issue(&repo(), "Follow up", "", &[]).unwrap();
    routed.open_issues(&repo()).unwrap();
    assert_eq!(filed, id("900"));
    assert_eq!(
        *github.calls.lock().unwrap(),
        ["create_issue Follow up", "open_issues"]
    );
    assert!(local.all().unwrap().is_empty());
}

#[test]
fn a_ticket_for_the_project_is_filed_listed_and_commented_in_its_own_source() {
    let (_home, github, routed, local) = stand_up(TicketSource::Local);
    let filed = routed
        .file_ticket(&repo(), "Follow up", "Seen twice.", &["needs-triage"])
        .unwrap();
    assert_eq!(filed, id("k1"));
    let open = routed.open_tickets(&repo()).unwrap();
    let held: Vec<_> = open.iter().map(|t| (t.ticket, t.title.as_str())).collect();
    assert_eq!(held, [(id("k1"), "Follow up")]);
    routed.comment_ticket(&repo(), filed, "Again.").unwrap();
    assert_eq!(local.get(filed).unwrap().comments.len(), 1);
    assert!(github.calls.lock().unwrap().is_empty());

    let (_home, github, on_github, local) = stand_up(TicketSource::Github);
    let filed = on_github
        .file_ticket(&repo(), "Follow up", "", &[])
        .unwrap();
    on_github.open_tickets(&repo()).unwrap();
    assert_eq!(filed, id("900"));
    assert_eq!(
        *github.calls.lock().unwrap(),
        ["create_issue Follow up", "open_issues"]
    );
    assert!(local.all().unwrap().is_empty());
}

#[test]
fn a_source_changed_to_local_through_the_cell_moves_the_board_and_leaves_a_github_ticket() {
    let (_home, github, routed, local) = stand_up(TicketSource::Github);
    local
        .create("Ready", "", &[crate::board::READY], Links::default())
        .unwrap();
    routed.source_cell().set(TicketSource::Local);

    assert_eq!(routed.ready_issues(&repo()).unwrap().len(), 1);
    routed.finished_parents(&repo()).unwrap();
    assert_eq!(routed.open_tickets(&repo()).unwrap().len(), 1);
    let filed = routed.file_ticket(&repo(), "Filed", "", &[]).unwrap();
    assert_eq!(filed, id("k2"));
    assert!(github.calls.lock().unwrap().is_empty());

    // A ticket opened on GitHub before the change is still there.
    assert_eq!(routed.issue(&repo(), id("12")).unwrap().title, "on GitHub");
    routed.close_issue(&repo(), id("12")).unwrap();
    assert_eq!(
        *github.calls.lock().unwrap(),
        ["issue 12", "close_issue 12"]
    );
}

#[test]
fn a_source_changed_to_github_through_the_cell_moves_the_board_and_leaves_a_local_ticket() {
    let (_home, github, routed, local) = stand_up(TicketSource::Local);
    local
        .create("Ready", "", &[crate::board::READY], Links::default())
        .unwrap();
    routed.source_cell().set(TicketSource::Github);

    assert!(routed.ready_issues(&repo()).unwrap().is_empty());
    routed.finished_parents(&repo()).unwrap();
    routed.open_tickets(&repo()).unwrap();
    let filed = routed.file_ticket(&repo(), "Filed", "", &[]).unwrap();
    assert_eq!(filed, id("900"));
    assert_eq!(
        *github.calls.lock().unwrap(),
        [
            "ready_issues",
            "finished_parents",
            "open_issues",
            "create_issue Filed"
        ]
    );

    // A ticket opened in the store before the change is still there.
    assert_eq!(routed.issue(&repo(), id("k1")).unwrap().title, "Ready");
    routed.close_issue(&repo(), id("k1")).unwrap();
    assert!(!routed.issue(&repo(), id("k1")).unwrap().open);
    assert_eq!(github.calls.lock().unwrap().len(), 4);
}
