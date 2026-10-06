//! Sessions in the maintainer's own terminal
//!
//! `attach` starts a work item's session in the foreground, with the
//! terminal's input and output, and so can any other session the maintainer
//! drives. Such a session is not a lamb. While it runs, an interrupt is the
//! session's to answer, so kelpie outlives it and can tidy up after it ends.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fmt;
use std::path::PathBuf;
use std::process::{Command, ExitStatus};

use serde::{Deserialize, Serialize};
use tokio::signal::unix::{SignalKind, signal};

/// A command the runner hands the maintainer's terminal to run
///
/// Debug does not show its variables' values.
// wire format: the `attach` trigger's answer carries it
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Foreground {
    /// The program
    pub program: String,
    /// Its arguments
    pub args: Vec<String>,
    /// The folder it runs in
    pub cwd: PathBuf,
    /// The variables it sets, and the ones it removes as null
    pub env: BTreeMap<String, Option<String>>,
}

impl fmt::Debug for Foreground {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Foreground")
            .field("program", &self.program)
            .field("args", &self.args)
            .field("cwd", &self.cwd)
            .field("env", &self.env.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl Foreground {
    /// `command` as the terminal will run it
    ///
    /// # Errors
    ///
    /// A message when it has no folder, or a word of it is not UTF-8.
    pub fn of(command: &Command) -> Result<Self, String> {
        let text = |word: &OsStr| {
            word.to_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{} is not UTF-8", word.display()))
        };
        let cwd = command
            .get_current_dir()
            .ok_or("the session's command names no folder")?;
        Ok(Self {
            program: text(command.get_program())?,
            args: command.get_args().map(text).collect::<Result<_, _>>()?,
            cwd: cwd.to_owned(),
            env: (command.get_envs())
                .map(|(name, value)| Ok((text(name)?, value.map(text).transpose()?)))
                .collect::<Result<_, String>>()?,
        })
    }

    /// The command, which inherits the terminal and kelpie's own variables
    pub fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command.args(&self.args).current_dir(&self.cwd);
        for (name, value) in &self.env {
            match value {
                Some(value) => command.env(name, value),
                None => command.env_remove(name),
            };
        }
        command
    }
}

/// Runs `command` in the foreground until it exits, and says how it ended
///
/// `started` is told the session's pid once it runs. An interrupt from the
/// terminal reaches the session, which answers it as it chooses, and does
/// not end kelpie. A SIGTERM or SIGHUP to kelpie is passed on to the
/// session, and kelpie still waits for it to end.
///
/// # Errors
///
/// A message when the command cannot start, or kelpie cannot take the
/// signals it holds off or passes on.
pub async fn run(command: Command, started: impl AsyncFnOnce(u32)) -> Result<ExitStatus, String> {
    let listen = |kind| signal(kind).map_err(|e| format!("cannot take signals: {e}"));
    let mut interrupts = listen(SignalKind::interrupt())?;
    let mut ends = listen(SignalKind::terminate())?;
    let mut hangups = listen(SignalKind::hangup())?;
    let program = command.get_program().display().to_string();
    let mut child = tokio::process::Command::from(command)
        .spawn()
        .map_err(|e| format!("cannot run {program}: {e}"))?;
    let pid = child.id();
    if let Some(pid) = pid {
        started(pid).await;
    }
    loop {
        tokio::select! {
            status = child.wait() => {
                return status.map_err(|e| format!("cannot wait for {program}: {e}"));
            }
            _ = interrupts.recv() => {}
            _ = ends.recv() => pass_on(pid, "TERM"),
            _ = hangups.recv() => pass_on(pid, "HUP"),
        }
    }
}

// Sends the session `signal`, which it answers before kelpie goes on.
fn pass_on(pid: Option<u32>, signal: &str) {
    if let Some(pid) = pid {
        let _ = Command::new("kill")
            .arg(format!("-{signal}"))
            .arg(pid.to_string())
            .status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_crosses_to_the_terminal_whole() {
        let mut command = Command::new("node");
        command
            .args(["srt.js", "--", "claude", "--resume", "s 1"])
            .current_dir("/k/worktrees/7")
            .env("CLAUDE_CODE_TMPDIR", "/k/worker/settings-7.tmp")
            .env_remove("CLAUDE_CODE_SSE_PORT");
        let foreground = Foreground::of(&command).unwrap();
        let text = serde_json::to_string(&foreground).unwrap();
        let back: Foreground = serde_json::from_str(&text).unwrap();
        assert_eq!(back, foreground);
        let again = Foreground::of(&back.command()).unwrap();
        assert_eq!(again, foreground);
        assert_eq!(
            back.env.get("CLAUDE_CODE_SSE_PORT"),
            Some(&None),
            "a removal stays one"
        );
    }

    #[test]
    fn a_command_with_no_folder_is_refused() {
        let err = Foreground::of(&Command::new("claude")).unwrap_err();
        assert_eq!(err, "the session's command names no folder");
    }

    // A lazy derive would print the variables' values, which can be a token.
    #[test]
    fn debug_names_variables_without_their_values() {
        let foreground = Foreground {
            program: "claude".into(),
            args: vec!["--resume".into(), "s".into()],
            cwd: "/k/wt".into(),
            env: BTreeMap::from([("TOKEN".to_owned(), Some("s3cr3t".to_owned()))]),
        };
        assert_eq!(
            format!("{foreground:?}"),
            "Foreground { program: \"claude\", args: [\"--resume\", \"s\"], \
             cwd: \"/k/wt\", env: [\"TOKEN\"] }"
        );
    }

    #[tokio::test]
    async fn a_session_s_exit_is_its_status_and_its_pid_is_told() {
        let mut command = Command::new("sh");
        command.args(["-c", "exit 3"]);
        let mut told = None;
        let status = run(command, async |pid| told = Some(pid)).await.unwrap();
        assert_eq!(status.code(), Some(3));
        assert!(told.is_some_and(|pid| pid > 0), "{told:?}");
        let err = run(Command::new("/no/such/claude"), async |_| {})
            .await
            .unwrap_err();
        assert!(err.starts_with("cannot run /no/such/claude: "), "{err}");
    }

    const SESSION: &str = "KELPIE_TEST_SESSION";

    #[test]
    #[ignore = "a child process of a_sigterm_to_kelpie_reaches_the_session_it_then_waits_for"]
    fn session_child() {
        let Ok(script) = std::env::var(SESSION) else {
            return;
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let status = runtime.block_on(run(Command::new(script), async |_| {}));
        println!("ended {:?}", status.unwrap().code());
    }

    #[test]
    fn a_sigterm_to_kelpie_reaches_the_session_it_then_waits_for() {
        use std::io::{BufRead, BufReader};
        use std::process::Stdio;

        // A session that never ends unless it is told to.
        const PATIENCE: std::time::Duration = std::time::Duration::from_secs(30);
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("claude");
        crate::test::write_script(
            &script,
            "#!/bin/sh\ntrap 'echo got TERM; exit 7' TERM\necho ready\n\
             while :; do sleep 1; done\n",
        );
        let mut kelpie = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "terminal::tests::session_child",
                "--ignored",
                "--nocapture",
            ])
            .env(SESSION, &script)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let (send, lines) = std::sync::mpsc::channel();
        let out = BufReader::new(kelpie.stdout.take().unwrap());
        std::thread::spawn(move || {
            for line in out.lines().map_while(Result::ok) {
                let _ = send.send(line);
            }
        });
        let next = |want: &str| loop {
            let line = lines
                .recv_timeout(PATIENCE)
                .expect("the session went quiet");
            if line.contains(want) {
                return line;
            }
        };
        next("ready");
        let sent = Command::new("kill")
            .args(["-TERM", &kelpie.id().to_string()])
            .status()
            .unwrap();
        assert!(sent.success());
        next("got TERM");
        assert_eq!(next("ended"), "ended Some(7)", "kelpie waited for it");
        assert!(kelpie.wait().unwrap().success());
    }
}
