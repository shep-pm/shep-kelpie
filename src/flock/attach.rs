//! `shep kelpie attach <issue>`: the maintainer drives a work item's session
//!
//! The runner holds the work item for this process, so no call starts for
//! it, and `attach` asks again while a call in flight runs to its end. Then
//! the worker's session runs in this terminal, in the worker's settings and
//! sandbox. The runner is told the session's pid too, and holds the work
//! item while either runs. Once the session exits, the runner lets the work
//! item go, and its next turn carries on from the same session. Should both
//! end without saying so, the runner lets it go at its next pass.

use std::time::Duration;

use shep_client::Client;

use super::control;
use crate::runner::{Attaching, ProjectName};
use crate::terminal;

/// How often `attach` asks again while a call for the work item is in flight
const ASK_AGAIN: Duration = Duration::from_secs(5);

/// Attaches to the work item for `issue` in `project`, and runs its
/// session here until it exits
///
/// # Errors
///
/// A message naming why the work item cannot be attached, with what to do
/// instead, or why its session could not run or be let go.
pub(super) async fn attach(
    client: &Client,
    project: &ProjectName,
    issue: u64,
) -> Result<Vec<String>, String> {
    let me = std::process::id();
    let ask = async |action: &str, session: Option<u32>| {
        let params = match session {
            Some(session) => format!("{issue} {me} {session}"),
            None => format!("{issue} {me}"),
        };
        let answer = control::send(client, project, action, Some(&params)).await?;
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

/// The attach itself: `ask` sends the runner `attach`, with the session's
/// pid once it runs, or `detach`, `say` tells the maintainer, and `wait`
/// waits to ask again and is false once the maintainer stops waiting
///
/// # Errors
///
/// As [`attach`].
pub(super) async fn drive(
    mut ask: impl AsyncFnMut(&str, Option<u32>) -> Result<String, String>,
    mut say: impl FnMut(&str),
    mut wait: impl AsyncFnMut() -> bool,
) -> Result<Vec<String>, String> {
    let mut told = false;
    let (issue, session, worktree, command) = loop {
        let answer = ask("attach", None).await?;
        let read = serde_json::from_str(&answer)
            .map_err(|e| format!("the runner's answer to `attach` is not one: {e}: {answer}"))?;
        match read {
            Attaching::Ready {
                issue,
                session,
                worktree,
                command,
            } => break (issue, session, worktree, command),
            Attaching::Running { .. } => {
                return Err(format!(
                    "the runner answered `attach` out of turn: {answer}"
                ));
            }
            Attaching::Waiting { issue } => {
                if !told {
                    say(&format!(
                        "a call for #{issue} is in flight, and no other starts for it: \
                         waiting for it to end"
                    ));
                    told = true;
                }
                if !wait().await {
                    let let_go = ask("detach", None).await.err();
                    let why = let_go.map_or(String::new(), |e| format!(" ({e})"));
                    return Err(format!("stopped waiting, so #{issue} carries on{why}"));
                }
            }
        }
    };
    say(&format!(
        "attached to #{issue}: its worker's session {} in {}, until you exit",
        session.0,
        worktree.display()
    ));
    let mut began = false;
    let held = async |pid| {
        began = true;
        if let Err(e) = ask("attach", Some(pid)).await {
            say(&format!(
                "the runner was not told the session's pid, so the work item is held only \
                 while this command runs: {e}"
            ));
        }
    };
    let ran = terminal::run(command.command(), held).await;
    // A session kelpie lost track of may still run, so its hold stays for
    // the runner to let go once it ends.
    if let (Err(e), true) = (&ran, began) {
        return Err(format!(
            "{e}. #{issue} stays held while its session may run, and the runner lets it go \
             once it has ended"
        ));
    }
    let detached = ask("detach", None).await;
    let ended = match &ran {
        Ok(status) if status.success() => String::new(),
        Ok(status) => format!(" (claude ended with {status})"),
        Err(_) => String::new(),
    };
    match (ran, detached) {
        (Ok(_), Ok(_)) => Ok(vec![format!(
            "detached from #{issue}{ended}: its next turn carries on from session {}",
            session.0
        )]),
        (Err(e), Ok(_)) => Err(format!("{e}, so #{issue} carries on as it was")),
        (_, Err(e)) => Err(format!(
            "cannot let #{issue} go: {e}. The runner lets it go at its next pass once this \
             command ends"
        )),
    }
}

#[cfg(test)]
mod tests;
