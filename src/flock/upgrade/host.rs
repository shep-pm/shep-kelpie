//! The machine an upgrade really runs on, and the command line that starts it

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::Duration;

use super::fleet::Shepherd;
use super::install::Install;
use super::{Machine, Source, parse, run};

/// Runs `shep kelpie upgrade <args>`
pub fn main(args: &[String]) -> ExitCode {
    let ran = parse(args).and_then(|job| {
        let shep_home = crate::shep_home::required(crate::shep_home::FLOCK_FIX)?;
        let home = kelpie_home()?;
        run(
            &job,
            &Install::under(&home),
            std::process::id(),
            &Host,
            &Shepherd::at(shep_home),
        )
    });
    match ran {
        Ok(lines) => {
            for line in lines {
                println!("{line}");
            }
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("shep kelpie upgrade: {message}");
            ExitCode::FAILURE
        }
    }
}

// Kelpie's home is `KELPIE_HOME`, or `~/.kelpie`, as the runner reads it.
fn kelpie_home() -> Result<PathBuf, String> {
    std::env::var_os("KELPIE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".kelpie")))
        .ok_or_else(|| "HOME is not set".to_owned())
}

/// The real machine: `cargo`, `codesign` and `ps`
#[derive(Debug, Clone, Copy)]
pub struct Host;

impl Machine for Host {
    fn build(&self, source: &Source, staging: &Path) -> Result<PathBuf, String> {
        let repository = env!("CARGO_PKG_REPOSITORY");
        // cargo's `--rev` takes a commit, not a branch name.
        let source = match source {
            Source::Ref(name) => Source::Ref(resolve(repository, name)?),
            other => other.clone(),
        };
        let args = cargo_args(repository, &source, staging)?;
        let status = Command::new("cargo")
            .args(&args)
            .stdin(Stdio::null())
            .status()
            .map_err(|e| format!("cannot run cargo: {e}"))?;
        if !status.success() {
            return Err(format!("cargo install failed: {status}"));
        }
        Ok(staging.join("root/bin").join(env!("CARGO_PKG_NAME")))
    }

    fn shep_version(&self, binary: &Path) -> Result<String, String> {
        let output = Command::new(binary)
            .arg("shep-version")
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("cannot run it: {e}"))?;
        if !output.status.success() {
            return Err(format!("it exited {}", output.status));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    // macOS kills a binary whose signature no longer matches its bytes.
    fn sign(&self, binary: &Path) -> Result<(), String> {
        if !cfg!(target_os = "macos") {
            return Ok(());
        }
        let status = Command::new("codesign")
            .args(["-f", "-s", "-"])
            .arg(binary)
            .stdin(Stdio::null())
            .status()
            .map_err(|e| format!("cannot run codesign: {e}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("codesign failed on {}: {status}", binary.display()))
        }
    }

    fn alive(&self, pid: u32) -> bool {
        Command::new("ps")
            .args(["-p", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    fn sleep(&self, time: Duration) {
        std::thread::sleep(time);
    }
}

// The commit a branch, tag or commit of `repository` names.
fn resolve(repository: &str, name: &str) -> Result<String, String> {
    if (7..=40).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok(name.to_owned());
    }
    let output = Command::new("git")
        .args(["ls-remote", repository])
        .args([
            format!("refs/heads/{name}"),
            format!("refs/tags/{name}^{{}}"),
            format!("refs/tags/{name}"),
        ])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "git could not read {repository}: {}",
            output.status
        ));
    }
    pick_commit(&String::from_utf8_lossy(&output.stdout), name)
        .ok_or_else(|| format!("{name} is no branch, tag or commit of {repository}"))
}

/// The commit in a listing of the remote's refs for `name`: its branch, else its tag's commit
pub fn pick_commit(listing: &str, name: &str) -> Option<String> {
    let found = |wanted: &str| {
        listing.lines().find_map(|line| {
            let (commit, reference) = line.split_once('\t')?;
            (reference == wanted).then(|| commit.to_owned())
        })
    };
    found(&format!("refs/heads/{name}"))
        .or_else(|| found(&format!("refs/tags/{name}^{{}}")))
        .or_else(|| found(&format!("refs/tags/{name}")))
}

/// The `cargo install` arguments that build `source` of `repository` in `staging`
///
/// The target folder is kept between upgrades, so a second build reuses the
/// first one's dependencies, and is not the one kelpie's own sessions build in.
///
/// # Errors
///
/// A message for a source that is a file rather than something to build.
pub fn cargo_args(
    repository: &str,
    source: &Source,
    staging: &Path,
) -> Result<Vec<OsString>, String> {
    let mut args: Vec<OsString> = ["install", "--git", repository, "--locked", "--force"]
        .map(OsString::from)
        .into();
    match source {
        Source::Release(tag) => args.extend(["--tag".into(), tag.into()]),
        Source::Ref(rev) => args.extend(["--rev".into(), rev.into()]),
        Source::Binary(_) => return Err("a binary is installed as it is, not built".to_owned()),
    }
    args.extend([
        "--root".into(),
        staging.join("root").into(),
        "--target-dir".into(),
        staging.join("target").into(),
    ]);
    Ok(args)
}
