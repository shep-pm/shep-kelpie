//! One run: the worktree goes to the host, its command runs there, and the
//! results folder comes back
//!
//! The dog starts two programs here, `ssh` and `rsync`, each with an argument
//! list and no shell between. Everything that names a place comes from the
//! [`Plan`], which the project's settings made. Neither sees a hidden
//! variable. Every program is killed if the run is dropped, which is how a
//! worker hanging up, or the ceiling, ends it.

use std::ffi::OsString;

use std::path::Path;
use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc::Sender;

use super::{Plan, Remote};
use crate::lease::door::LONGEST_LINE;
use crate::spawn::{self, shell_quote};
use crate::status::shell_number;

/// The longest message kept of what rsync said when it failed
const LONGEST_COMPLAINT: usize = 1000;

/// Copies the worktree to the host, runs the project's command there, copies
/// the results folder back, and returns the command's exit code
///
/// Each line the command writes is sent to `lines` as it arrives. Results are
/// copied back whatever the command's code. When they cannot be, a line says
/// so and the command's code stands.
///
/// # Errors
///
/// A message when the copy to the host fails or a program cannot start, so
/// the command did not run.
pub(super) async fn run(
    remote: &Remote,
    plan: &Plan,
    lines: Sender<String>,
) -> Result<i32, String> {
    copy_out(remote, plan).await?;
    let code = command(remote, plan, &lines).await?;
    match copy_back(remote, plan).await {
        Ok(()) => Ok(code),
        Err(why) => {
            let _ = lines
                .send(format!("e2e: the results did not come back: {why}"))
                .await;
            Ok(code)
        }
    }
}

// The worktree to `kelpie-e2e/<project>` on the host, ignored files and `.git`
// left out. The host's own copies of ignored files stay, so a build cache
// kept in one survives a run.
async fn copy_out(remote: &Remote, plan: &Plan) -> Result<(), String> {
    let mut from = OsString::from(&plan.worktree);
    from.push("/");
    let to = format!("{}:{}/", plan.host, plan.remote_dir());
    let mut args = vec!["-a".into(), "--delete".into(), "-e".into()];
    args.push(remote.ssh.clone().into_os_string());
    args.extend(
        [
            // rsync makes the last folder only, so the first is made here.
            "--rsync-path=mkdir -p kelpie-e2e && rsync",
            "--exclude=.git",
            "--filter=:- .gitignore",
        ]
        .map(OsString::from),
    );
    args.extend([from, to.into()]);
    rsync(remote, &args)
        .await
        .map_err(|why| format!("the copy to {} failed: {why}", plan.host))
}

// `./<command>` in the copy, through `ssh -tt`: the pty is what makes the
// host hang the command up when ssh is killed. Its output, including ssh's
// own, goes to `lines`.
async fn command(remote: &Remote, plan: &Plan, lines: &Sender<String>) -> Result<i32, String> {
    let words = format!(
        "cd {} && {}",
        shell_quote(&plan.remote_dir()),
        shell_quote(&format!("./{}", plan.command.display()))
    );
    let mut child = Command::from(spawn::command(&remote.ssh))
        .args(["-tt"])
        .arg(&plan.host)
        .arg(words)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", remote.ssh.display()))?;
    // Held open without a word: the command's input never ends under it.
    let _stdin = child.stdin.take();
    let out = child.stdout.take().expect("stdout was piped");
    let err = child.stderr.take().expect("stderr was piped");
    let (_, _, status) = tokio::join!(pump(out, lines), pump(err, lines), child.wait());
    let status = status.map_err(|e| format!("cannot wait for {}: {e}", remote.ssh.display()))?;
    // A signal is what a shell reports it as; ssh's own failure is 255.
    Ok(shell_number(status).unwrap_or(255))
}

// The results folder alone, into a folder of the dog's own, and from there
// into the same folder of the worktree. Links on the host are not copied. A
// link in the worktree is refused as the copy reaches it, since the worker
// can change the worktree meanwhile and no path is checked ahead.
async fn copy_back(remote: &Remote, plan: &Plan) -> Result<(), String> {
    let staging = plan.staging()?;
    let result = stage_and_deliver(remote, plan, &staging).await;
    let _ = std::fs::remove_dir_all(&staging);
    result
}

async fn stage_and_deliver(remote: &Remote, plan: &Plan, staging: &Path) -> Result<(), String> {
    // One left by a run that was ended partway is not part of this one.
    let _ = std::fs::remove_dir_all(staging);
    std::fs::create_dir(staging).map_err(|e| format!("cannot make {}: {e}", staging.display()))?;
    let from = format!(
        "{}:{}/{}/",
        plan.host,
        plan.remote_dir(),
        plan.results.display()
    );
    let mut to = OsString::from(staging);
    to.push("/");
    let args = [
        "-rt",
        "--no-links",
        "--no-perms",
        "--no-owner",
        "--no-group",
    ]
    .map(OsString::from);
    let args: Vec<OsString> = args.into_iter().chain([from.into(), to]).collect();
    rsync(remote, &args).await?;
    deliver::deliver(staging, &plan.worktree, &plan.results)
}

async fn rsync(remote: &Remote, args: &[OsString]) -> Result<(), String> {
    let done = Command::from(spawn::command(&remote.rsync))
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| format!("cannot start {}: {e}", remote.rsync.display()))?;
    if done.status.success() {
        return Ok(());
    }
    let said = String::from_utf8_lossy(&done.stderr);
    let said = said.trim();
    let tail = said
        .char_indices()
        .rev()
        .nth(LONGEST_COMPLAINT)
        .map_or(said, |(at, _)| &said[at..]);
    Err(format!("rsync {}: {tail}", done.status))
}

// Sends each line of `from` as it arrives, with its line ending taken off:
// a pty ends them with a carriage return too.
async fn pump(from: impl AsyncRead + Unpin, lines: &Sender<String>) {
    let mut from = BufReader::new(from);
    let mut line = Vec::new();
    loop {
        line.clear();
        let mut chunk = (&mut from).take(LONGEST_LINE);
        let read = chunk.read_until(b'\n', &mut line);
        if !matches!(read.await, Ok(1..)) {
            return;
        }
        let text = String::from_utf8_lossy(&line);
        let text = text.trim_end_matches(['\n', '\r']);
        if lines.send(text.to_owned()).await.is_err() {
            return;
        }
    }
}

mod deliver;

#[cfg(test)]
mod tests;
