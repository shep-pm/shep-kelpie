//! `lease e2e`: a project's end-to-end tests, run by the dog on the project's host
//!
//! The ask carries the folder the command runs from and nothing else. The
//! dog works out the project from that folder, and takes the host from the
//! project's own settings, so a worker cannot name one.
//! The dog streams the command's output back, a line at a time, and ends
//! with its exit code.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::counted_run::NO_LEASE;
use crate::lease::door::{Answer, Ask, E2E, NO_E2E_HOST, Visit};

/// What the command exits with when the dog says the project has no e2e
/// host, as `EX_CONFIG`
pub(crate) const NOT_CONFIGURED: u8 = 78;

/// How long a dog has to answer; a live one answers at once
const ANSWER_WITHIN: Duration = Duration::from_secs(5);

/// How an ask for the e2e tests ended, which the exit code alone cannot say:
/// the command's own code can equal one of the dog's
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ended {
    /// The command ran, and exited with this code
    Exited(u8),
    /// The dog says the project has no e2e host, and why
    NoHost(String),
    /// The dog was not reached, or went away, and why. The run may not have
    /// happened, or its end is unknown.
    Unreached(String),
    /// The dog answered something other than a run, and what
    Refused(String),
}

impl Ended {
    /// The number to exit with: the command's own, [`NOT_CONFIGURED`] for no
    /// host, [`NO_LEASE`] when the dog was not reached or went away, and 1
    /// for a refusal
    pub(crate) fn code(&self) -> u8 {
        match self {
            Self::Exited(code) => *code,
            Self::NoHost(_) => NOT_CONFIGURED,
            Self::Unreached(_) => NO_LEASE,
            Self::Refused(_) => 1,
        }
    }
}

/// Asks the dog at `socket` to run the e2e tests for the project holding
/// `worktree`, prints the command's output as it arrives, and returns the
/// number to exit with
pub(crate) async fn run(socket: &Path, worktree: PathBuf) -> u8 {
    run_printing(socket, worktree, &mut std::io::stdout())
        .await
        .code()
}

/// `run`, printing the command's lines to `out`, and saying how the ask ended.
pub async fn run_printing(socket: &Path, worktree: PathBuf, out: &mut impl Write) -> Ended {
    let visit = match Visit::knock(socket, &ask(worktree)).await {
        Ok(visit) => visit,
        Err(why) => return Ended::Unreached(told(&unreached(&why))),
    };
    follow(visit, out).await
}

// What the dog is asked: to run the e2e tests for the project holding `worktree`.
fn ask(worktree: PathBuf) -> Ask {
    Ask {
        worktree: Some(worktree),
        ..Ask::new(E2E, std::process::id(), "shep-kelpie lease e2e")
    }
}

// The dog's answers to an ask, printed and turned into a code.
async fn follow(mut visit: Visit, out: &mut impl Write) -> Ended {
    let first = match tokio::time::timeout(ANSWER_WITHIN, visit.answer()).await {
        Ok(answer) => answer,
        Err(_) => return Ended::Unreached(told("the dog took the ask and never answered")),
    };
    let mut granted = false;
    let mut next = first;
    loop {
        match next {
            Err(why) if granted => return Ended::Unreached(told(&gone(&why))),
            Err(why) => return Ended::Unreached(told(&unreached(&why))),
            Ok(None) if granted => return Ended::Unreached(told(&gone("the dog closed the door"))),
            Ok(None) => {
                return Ended::Unreached(told("the dog closed the door without answering"));
            }
            Ok(Some(Answer::Queued(ahead))) => {
                eprintln!("shep kelpie lease: waiting for the e2e host, {ahead} ahead");
            }
            Ok(Some(Answer::Granted)) => granted = true,
            Ok(Some(Answer::Output(line))) => {
                // A reader that went away leaves the run going; its code still counts.
                let _ = writeln!(out, "{line}").and_then(|()| out.flush());
            }
            Ok(Some(Answer::Done(code))) => return Ended::Exited(u8::try_from(code).unwrap_or(1)),
            Ok(Some(Answer::Error(why))) if why.starts_with(NO_E2E_HOST) => {
                return Ended::NoHost(told(&why));
            }
            Ok(Some(Answer::Error(why))) => {
                return Ended::Refused(told(&format!("the dog refused: {why}")));
            }
            Ok(Some(
                other @ (Answer::Go
                | Answer::Wait(_)
                | Answer::Held(_)
                | Answer::Picks(_)
                | Answer::Fits(_)),
            )) => {
                return Ended::Refused(told(&format!("the dog answered {other:?} to the e2e ask")));
            }
        }
        next = visit.answer().await;
    }
}

// What a run says when the dog goes away after granting the host: the
// command's end is then unknown, and the dog ended the run with its worker.
fn gone(why: &str) -> String {
    format!(
        "{why} before the e2e run ended: the e2e tests did not finish, so say so in your final message"
    )
}

// A worker cannot start the dog, so it is told to go on without the run.
fn unreached(why: &str) -> String {
    format!("{why}: the e2e tests did not run, so say so in your final message")
}

// Says `message` on stderr, and hands it back.
fn told(message: &str) -> String {
    eprintln!("shep kelpie lease: {message}");
    message.to_owned()
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    use super::*;
    use crate::ipc::{connected_pair, split};

    // A visit to a dog on the other end of a connected pair of streams, which
    // reads one ask and answers it with `answer`, handing back the ask it read.
    async fn dog_answering(
        worktree: &Path,
        answer: &str,
    ) -> (Visit, tokio::task::JoinHandle<Option<Ask>>) {
        let (dog, visitor) = connected_pair().await.unwrap();
        let visit = Visit::over(visitor, &ask(worktree.to_owned()))
            .await
            .unwrap();
        let answer = answer.to_owned();
        let dog = tokio::spawn(async move {
            let (read, mut write) = split(dog);
            let line = BufReader::new(read).lines().next_line().await.ok()??;
            write
                .write_all(format!("{answer}\n").as_bytes())
                .await
                .ok()?;
            serde_json::from_str(&line).ok()
        });
        (visit, dog)
    }

    #[tokio::test]
    async fn lease_e2e_exits_78_when_the_dog_says_it_is_not_configured() {
        let worktree = Path::new("/work/tree");
        let (visit, dog) = dog_answering(
            worktree,
            r#"{"error":"this project has no e2e host: [app.dogs.kelpie.e2e]"}"#,
        )
        .await;

        let code = follow(visit, &mut Vec::new()).await.code();

        assert_eq!(code, 78);
        let heard = dog.await.unwrap().expect("the dog read an ask");
        assert_eq!(heard.take, "e2e");
        assert_eq!(heard.worktree, Some(worktree.to_owned()));
    }

    #[tokio::test]
    async fn lease_e2e_prints_the_output_and_exits_with_the_runs_code() {
        let (visit, _dog) = dog_answering(
            Path::new("/work/tree"),
            "{\"queued\":1}\n\"granted\"\n{\"output\":\"ok 1\"}\n{\"output\":\"not ok 2\"}\n{\"done\":3}",
        )
        .await;
        let mut printed = Vec::new();

        let code = follow(visit, &mut printed).await.code();

        assert_eq!(code, 3);
        assert_eq!(String::from_utf8(printed).unwrap(), "ok 1\nnot ok 2\n");
    }

    #[tokio::test]
    async fn lease_e2e_exits_75_when_the_dog_goes_away_mid_run() {
        let (visit, _dog) = dog_answering(
            Path::new("/work/tree"),
            "\"granted\"\n{\"output\":\"ok 1\"}",
        )
        .await;
        let mut printed = Vec::new();

        let code = follow(visit, &mut printed).await.code();

        assert_eq!(code, 75);
        assert_eq!(String::from_utf8(printed).unwrap(), "ok 1\n");
    }

    #[tokio::test]
    async fn lease_e2e_exits_1_when_the_dog_refuses_for_any_other_reason() {
        let (visit, _dog) = dog_answering(
            Path::new("/work/tree"),
            r#"{"error":"a results folder in the worktree is a link"}"#,
        )
        .await;

        assert_eq!(follow(visit, &mut Vec::new()).await.code(), 1);
    }

    #[tokio::test]
    async fn lease_e2e_exits_75_when_the_dog_is_not_reached() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("lease.sock");

        assert_eq!(run(&socket, dir.path().to_owned()).await, 75);
    }

    #[tokio::test]
    async fn a_command_that_exits_with_a_code_the_dog_uses_is_still_the_commands() {
        for code in [75, 78] {
            let (visit, _dog) = dog_answering(
                Path::new("/work/tree"),
                &format!("\"granted\"\n{{\"done\":{code}}}"),
            )
            .await;

            let ended = follow(visit, &mut Vec::new()).await;

            assert_eq!(ended, Ended::Exited(code));
        }
    }
}
