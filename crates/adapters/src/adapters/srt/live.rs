//! The keychain probes, run under the real runtime where the machine allows it
//!
//! A test needs macOS, the built `shep-kelpie` (a test binary has no
//! `crabpot::run_from_args`), and a keychain that takes a new login without
//! asking. Without them it says why on stderr and passes: a sandbox cannot
//! start inside another, so the runner's own calls skip it too. The logins it probes are its own, put
//! in the keychain with the `security` command as `gh` and git put theirs, and
//! taken out again.

use std::process::{Command, Output, Stdio};

use super::*;

const SECRET: &str = "kelpie-probe-secret";
const GIT_HOST: &str = "kelpie-probe.invalid";

// What a worker reaches: GitHub's API and git host.
const HOSTS: [&str; 2] = ["github.com", "api.github.com"];

fn sh(script: &str) -> Output {
    Command::new("/bin/sh")
        .args(["-c", script])
        .stdin(Stdio::null())
        .output()
        .expect("/bin/sh runs")
}

// A login in the macOS keychain, stored the way `gh` and git store theirs
struct Logins {
    service: String,
}

impl Logins {
    fn store() -> Option<Self> {
        let service = format!("kelpie-probe-{}", std::process::id());
        let generic =
            format!("security add-generic-password -s {service} -a kelpie -w {SECRET} -U");
        let git = format!(
            "printf 'protocol=https\\nhost={GIT_HOST}\\nusername=kelpie\\npassword={SECRET}\\n\\n' \
             | git credential-osxkeychain store"
        );
        // Dropped on the way out, so a half-stored pair is cleaned up too.
        let logins = Self { service };
        (sh(&generic).status.success() && sh(&git).status.success()).then_some(logins)
    }
}

impl Drop for Logins {
    fn drop(&mut self) {
        sh(&format!(
            "security delete-generic-password -s {}",
            self.service
        ));
        sh(&format!(
            "printf 'protocol=https\\nhost={GIT_HOST}\\n\\n' | git credential-osxkeychain erase"
        ));
    }
}

// `script` under the runtime, as a covered call (`blocked`) or an uncovered one runs it.
fn probe(srt: &SandboxRuntime, blocked: bool, script: &str) -> Output {
    let work = crate::test::short_tempdir();
    let policy = Policy {
        write: vec![work.path().canonicalize().unwrap()],
        hosts: HOSTS.map(str::to_owned).into(),
        verify_tls: true,
        no_keychain: blocked,
        ..Policy::default()
    };
    run(srt, &policy, work.path(), script)
}

// `script` under the runtime in `work`, held to `policy`.
fn run(srt: &SandboxRuntime, policy: &Policy, work: &Path, script: &str) -> Output {
    let settings = crate::test::short_tempdir();
    let mut inner = Command::new("/bin/sh");
    inner.args(["-c", script]).current_dir(work);
    let mut wrapped = srt
        .wrap(policy, &settings.path().join("srt.json"), &inner)
        .expect("the runtime wraps the probe");
    wrapped.stdin(Stdio::null());
    wrapped.output().expect("the runtime runs")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

// The runtime that runs a call through the built `shep-kelpie`, or why there is none.
fn runtime() -> Result<SandboxRuntime, String> {
    if !cfg!(target_os = "macos") {
        return Err("there is no keychain off macOS".into());
    }
    Ok(SandboxRuntime::new().run_by(built_kelpie()?))
}

// Any answer from GitHub counts as reached, a rate limit's 403 included: the
// runners share their address, and these probes check the path, not the reply.
fn online() -> bool {
    sh("curl -sS -o /dev/null --max-time 10 https://api.github.com")
        .status
        .success()
}

fn skipped(why: &str) {
    eprintln!("skipped: {why}");
}

#[test]
fn a_covered_call_cannot_ask_the_keychain_for_a_login_and_an_uncovered_one_can() {
    let srt = match runtime() {
        Ok(srt) => srt,
        Err(why) => return skipped(&why),
    };
    let Some(logins) = Logins::store() else {
        return skipped("the keychain did not take a login without asking");
    };
    let find = format!("security find-generic-password -s {} -w", logins.service);
    let git =
        format!("printf 'protocol=https\\nhost={GIT_HOST}\\n\\n' | git credential-osxkeychain get");

    // As shipped, the premise: the command's logins are one call away.
    let open = probe(&srt, false, &find);
    assert!(stdout(&open).contains(SECRET), "{open:?}");
    let open = probe(&srt, false, &git);
    assert!(stdout(&open).contains(SECRET), "{open:?}");

    // Blocked: refused, and no secret in what it prints.
    for script in [&find, &git] {
        let shut = probe(&srt, true, script);
        assert!(!stdout(&shut).contains(SECRET), "{script}: {shut:?}");
    }
    assert!(!probe(&srt, true, &find).status.success());
}

#[test]
fn a_covered_call_still_reaches_github_signs_and_runs_a_program() {
    let srt = match runtime() {
        Ok(srt) => srt,
        Err(why) => return skipped(&why),
    };
    if !online() {
        return skipped("GitHub is out of reach");
    }
    let work: [(&str, &str); 4] = [
        ("curl", "curl -sS -o /dev/null https://api.github.com"),
        (
            "git ls-remote",
            "git ls-remote https://github.com/shep-pm/shep-kelpie HEAD",
        ),
        (
            "python urllib",
            "python3 -c \"import urllib.request as u, urllib.error as e\ntry: u.urlopen('https://api.github.com')\nexcept e.HTTPError: pass\"",
        ),
        (
            "codesign",
            "printf 'int main(void){return 0;}' > t.c && TMPDIR=. cc -o t t.c \\
             && codesign --force --sign - ./t && ./t",
        ),
    ];
    for (name, script) in work {
        let done = probe(&srt, true, script);
        assert!(done.status.success(), "{name}: {done:?}");
    }
}

#[test]
fn a_listed_folder_binds_a_socket_and_one_beside_it_is_refused() {
    let srt = match runtime() {
        Ok(srt) => srt,
        Err(why) => return skipped(&why),
    };
    // srt matches the real path, and the socket's path must fit 104 bytes.
    let work = crate::test::short_tempdir();
    let work = work.path().canonicalize().unwrap();
    let (listed, beside) = (work.join("listed"), work.join("beside"));
    for folder in [&listed, &beside] {
        std::fs::create_dir(folder).unwrap();
    }
    let policy = Policy {
        write: vec![work.clone()],
        socket_folders: vec![listed.clone()],
        ..Policy::default()
    };
    let bind = |folder: &str| {
        let script = format!(
            "python3 -c \"import socket; socket.socket(socket.AF_UNIX).bind('{folder}/s')\""
        );
        run(&srt, &policy, &work, &script)
    };

    let bound = bind("listed");
    assert!(bound.status.success(), "{bound:?}");
    assert!(listed.join("s").exists());
    let refused = bind("beside");
    assert!(!refused.status.success(), "{refused:?}");
    assert!(!beside.join("s").exists());
}
