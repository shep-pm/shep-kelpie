//! `shep kelpie pm`: the maintainer drives the project manager's session
//!
//! The runner holds the project manager for this process, so no wake
//! starts, and `pm` asks again while a wake in flight runs to its end. Then
//! its session runs in this terminal, in its settings and sandbox, as
//! `attach` runs a worker's. The runner is told the session's pid too, and
//! holds the project manager while either runs. Once the session exits the
//! runner lets it go, and its next wake resumes the same session. Should
//! both end without saying so, the runner lets it go when it next looks.

use std::time::Duration;

use shep_client::Client;

use super::control;
use crate::runner::{PmAttaching, ProjectName};
use crate::terminal;

/// How often `pm` asks again while a wake is in flight
const ASK_AGAIN: Duration = Duration::from_secs(5);

/// Holds `project`'s project manager, and runs its session here until it exits
///
/// # Errors
///
/// A message naming why its session cannot be held, run or let go.
pub(super) async fn pm(client: &Client, project: &ProjectName) -> Result<Vec<String>, String> {
    let me = std::process::id();
    let ask = async |action: &str, session: Option<u32>| {
        let params = match session {
            Some(session) => format!("{action} {me} {session}"),
            None => format!("{action} {me}"),
        };
        let answer = control::send(client, project, "pm", Some(&params)).await?;
        Ok(answer.concat())
    };
    let wait = async || {
        tokio::select! {
            () = tokio::time::sleep(ASK_AGAIN) => true,
            _ = tokio::signal::ctrl_c() => false,
        }
    };
    drive(ask, |line| println!("{line}"), wait).await
}

/// The hold itself: `ask` sends the runner `attach`, with the session's pid
/// once it runs, or `detach`, `say` tells the maintainer, and `wait` waits
/// to ask again and is false once the maintainer stops waiting
///
/// # Errors
///
/// As [`pm`].
pub(super) async fn drive(
    mut ask: impl AsyncFnMut(&str, Option<u32>) -> Result<String, String>,
    mut say: impl FnMut(&str),
    mut wait: impl AsyncFnMut() -> bool,
) -> Result<Vec<String>, String> {
    let mut told = false;
    let (session, folder, command) = loop {
        let answer = ask("attach", None).await?;
        let read = serde_json::from_str(&answer)
            .map_err(|e| format!("the runner's answer to `pm` is not one: {e}: {answer}"))?;
        match read {
            PmAttaching::Ready {
                session,
                folder,
                command,
            } => break (session, folder, command),
            PmAttaching::Running => {
                return Err(format!("the runner answered `pm` out of turn: {answer}"));
            }
            PmAttaching::Waiting => {
                if !told {
                    say(
                        "the project manager is answering a wake, and no other starts: \
                         waiting for it to end",
                    );
                    told = true;
                }
                if !wait().await {
                    let let_go = ask("detach", None).await.err();
                    let why = let_go.map_or(String::new(), |e| format!(" ({e})"));
                    return Err(format!(
                        "stopped waiting, so the project manager carries on{why}"
                    ));
                }
            }
        }
    };
    say(&format!(
        "holding the project manager: its session {} in {}, until you exit",
        session.0,
        folder.display()
    ));
    let mut began = false;
    let held = async |pid| {
        began = true;
        if let Err(e) = ask("attach", Some(pid)).await {
            say(&format!(
                "the runner was not told the session's pid, so the project manager is held \
                 only while this command runs: {e}"
            ));
        }
    };
    let ran = terminal::run(command.command(), held).await;
    // A session kelpie lost track of may still run, so its hold stays for
    // the runner to let go once it ends.
    if let (Err(e), true) = (&ran, began) {
        return Err(format!(
            "{e}. The project manager stays held while its session may run, and the runner \
             lets it go once it has ended"
        ));
    }
    let detached = ask("detach", None).await;
    match (ran, detached) {
        (Ok(status), Ok(_)) if status.success() => Ok(vec![format!(
            "let the project manager go: its next wake resumes session {}",
            session.0
        )]),
        (Ok(status), Ok(_)) => Ok(vec![format!(
            "let the project manager go (claude ended with {status}): its next wake resumes \
             session {}",
            session.0
        )]),
        (Err(e), Ok(_)) => Err(format!("{e}, so the project manager carries on as it was")),
        (_, Err(e)) => Err(format!(
            "cannot let the project manager go: {e}. The runner lets it go once this \
             command ends"
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, VecDeque};

    use serde_json::json;

    use super::*;
    use crate::terminal::Foreground;
    use crate::test::write_script;

    // The runner as `pm` reaches it: each answer to a plain `attach` in
    // turn, and every request it was sent.
    struct Runner {
        answers: VecDeque<String>,
        sent: Vec<String>,
    }

    impl Runner {
        fn ask(&mut self, action: &str, session: Option<u32>) -> Result<String, String> {
            self.sent.push(match session {
                Some(_) => format!("{action} <session>"),
                None => action.to_owned(),
            });
            Ok(match (action, session) {
                ("detach", _) => json!({ "project": "koji" }).to_string(),
                (_, Some(_)) => json!({ "attach": "running" }).to_string(),
                (_, None) => self
                    .answers
                    .pop_front()
                    .expect("an answer for every attach"),
            })
        }
    }

    #[tokio::test]
    async fn it_waits_out_a_wake_then_runs_the_session_and_lets_it_go() {
        let dir = tempfile::tempdir().unwrap();
        let claude = dir.path().join("claude");
        let said = dir.path().join("said");
        write_script(
            &claude,
            &format!("#!/bin/sh\necho \"$*\" > '{}'\n", said.display()),
        );
        let ready = PmAttaching::Ready {
            session: crate::ports::SessionId("s-pm".into()),
            folder: dir.path().to_owned(),
            command: Foreground {
                program: claude.display().to_string(),
                args: vec!["--resume".into(), "s-pm".into()],
                cwd: dir.path().to_owned(),
                env: BTreeMap::new(),
            },
        };
        let mut runner = Runner {
            answers: VecDeque::from([
                json!({ "attach": "waiting" }).to_string(),
                serde_json::to_string(&ready).unwrap(),
            ]),
            sent: Vec::new(),
        };
        let mut lines = Vec::new();
        let done = drive(
            async |action: &str, session| runner.ask(action, session),
            |line| lines.push(line.to_owned()),
            async || true,
        )
        .await
        .unwrap();
        assert_eq!(
            runner.sent,
            ["attach", "attach", "attach <session>", "detach"]
        );
        assert_eq!(std::fs::read_to_string(&said).unwrap(), "--resume s-pm\n");
        assert_eq!(
            done,
            ["let the project manager go: its next wake resumes session s-pm"]
        );
        assert!(lines[0].starts_with("the project manager is answering a wake"));
    }

    #[tokio::test]
    async fn giving_up_while_a_wake_runs_lets_it_go() {
        let mut runner = Runner {
            answers: VecDeque::from([json!({ "attach": "waiting" }).to_string()]),
            sent: Vec::new(),
        };
        let err = drive(
            async |action: &str, session| runner.ask(action, session),
            |_| {},
            async || false,
        )
        .await
        .unwrap_err();
        assert_eq!(err, "stopped waiting, so the project manager carries on");
        assert_eq!(runner.sent, ["attach", "detach"]);
    }
}
