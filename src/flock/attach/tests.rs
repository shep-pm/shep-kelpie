use std::collections::{BTreeMap, VecDeque};
use std::path::Path;

use serde_json::json;

use super::*;
use crate::terminal::Foreground;
use crate::test::write_script;

// The runner as `attach` reaches it: each answer in turn, and every action
// it was sent.
struct Runner {
    answers: VecDeque<Result<String, String>>,
    sent: Vec<String>,
}

impl Runner {
    fn answering(answers: impl IntoIterator<Item = Result<String, String>>) -> Self {
        Self {
            answers: answers.into_iter().collect(),
            sent: Vec::new(),
        }
    }

    fn ask(&mut self, action: &str, session: Option<u32>) -> Result<String, String> {
        match session {
            Some(pid) => self.sent.push(format!("{action} {pid}")),
            None => self.sent.push(action.to_owned()),
        }
        match (action, session) {
            ("detach", _) => Ok(json!({ "project": "koji" }).to_string()),
            (_, Some(_)) => Ok(json!({ "attach": "running", "issue": 7 }).to_string()),
            (_, None) => self
                .answers
                .pop_front()
                .expect("an answer for every attach"),
        }
    }
}

fn waiting() -> Result<String, String> {
    Ok(json!({ "attach": "waiting", "issue": 7 }).to_string())
}

// The worker's session, as a stand-in `claude` in `worktree` that writes
// its pid, folder, variable and arguments to `said`
fn ready(claude: &Path, worktree: &Path) -> Result<String, String> {
    let command = Foreground {
        program: claude.display().to_string(),
        args: vec!["--resume".into(), "s-7".into()],
        cwd: worktree.to_owned(),
        env: BTreeMap::from([("CLAUDE_CODE_TMPDIR".to_owned(), Some("/k/tmp".to_owned()))]),
    };
    let answer = Attaching::Ready {
        issue: 7,
        session: crate::ports::SessionId("s-7".into()),
        worktree: worktree.to_owned(),
        command,
    };
    Ok(serde_json::to_string(&answer).unwrap())
}

fn stand_in(dir: &Path, exit: u8) -> std::path::PathBuf {
    let claude = dir.join("claude");
    let said = dir.join("said");
    write_script(
        &claude,
        &format!(
            "#!/bin/sh\nprintf '%s\\0' $$ \"$PWD\" \"$CLAUDE_CODE_TMPDIR\" \"$@\" > '{}'\nexit {exit}\n",
            said.display()
        ),
    );
    claude
}

#[tokio::test]
async fn attach_waits_out_the_call_in_flight_then_runs_the_session_here_and_lets_go() {
    let dir = tempfile::tempdir().unwrap();
    let dir = dir.path().canonicalize().unwrap();
    let worktree = dir.join("wt");
    std::fs::create_dir(&worktree).unwrap();
    let claude = stand_in(&dir, 0);
    let mut runner = Runner::answering([waiting(), waiting(), ready(&claude, &worktree)]);
    let (mut said, mut waits) = (Vec::new(), 0);

    let lines = drive(
        async |action: &str, session| runner.ask(action, session),
        |line| said.push(line.to_owned()),
        async || {
            waits += 1;
            true
        },
    )
    .await
    .unwrap();

    let ran = std::fs::read_to_string(dir.join("said")).unwrap();
    let words: Vec<&str> = ran.trim_end_matches('\0').split('\0').collect();
    let session = format!("attach {}", words[0]);
    assert_eq!(
        runner.sent,
        ["attach", "attach", "attach", session.as_str(), "detach"],
        "the runner holds the item while the session's own pid runs"
    );
    assert_eq!(waits, 2);
    let worktree_text = worktree.display().to_string();
    assert_eq!(
        said,
        [
            "a call for #7 is in flight, and no other starts for it: waiting for it to end"
                .to_owned(),
            format!("attached to #7: its worker's session s-7 in {worktree_text}, until you exit"),
        ]
    );
    assert_eq!(
        lines,
        ["detached from #7: its next turn carries on from session s-7"]
    );
    assert_eq!(
        words[1..],
        [worktree_text.as_str(), "/k/tmp", "--resume", "s-7"],
        "the session ran in the worktree with the runner's command"
    );
}

#[tokio::test]
async fn a_session_that_fails_still_lets_the_work_item_go() {
    let dir = tempfile::tempdir().unwrap();
    let dir = dir.path().canonicalize().unwrap();
    let claude = stand_in(&dir, 3);
    let mut runner = Runner::answering([ready(&claude, &dir)]);
    let lines = drive(
        async |action: &str, session| runner.ask(action, session),
        |_| {},
        async || true,
    )
    .await
    .unwrap();
    assert_eq!(runner.sent.len(), 3, "{:?}", runner.sent);
    assert_eq!(
        lines,
        [
            "detached from #7 (claude ended with exit status: 3): its next turn carries on \
             from session s-7"
        ]
    );
}

#[tokio::test]
async fn a_refusal_is_said_as_the_runner_says_it_and_nothing_runs() {
    let why = "the worker on #7 has no session yet: its first turn starts one";
    let mut runner = Runner::answering([Err(why.to_owned())]);
    let err = drive(
        async |action: &str, session| runner.ask(action, session),
        |_| {},
        async || true,
    )
    .await
    .unwrap_err();
    assert_eq!(err, why);
    assert_eq!(runner.sent, ["attach"], "nothing was held to let go");
}

#[tokio::test]
async fn stopping_the_wait_lets_the_work_item_go() {
    let mut runner = Runner::answering([waiting()]);
    let err = drive(
        async |action: &str, session| runner.ask(action, session),
        |_| {},
        async || false,
    )
    .await
    .unwrap_err();
    assert_eq!(err, "stopped waiting, so #7 carries on");
    assert_eq!(runner.sent, ["attach", "detach"]);
}
