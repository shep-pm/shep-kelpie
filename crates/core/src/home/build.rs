//! A kelpie build's own account of itself: `shep-kelpie version --json`

use std::io::Read;
use std::path::Path;
use std::process::Stdio;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use shep_client::PROTOCOL_VERSION;

use crate::shepherd::SHEP_VERSION;

/// How long a build has to answer `version --json`
pub const TIMEOUT: Duration = Duration::from_secs(20);

// "Text file busy", and how long it is retried.
const ETXTBSY: i32 = 26;
const BUSY: Duration = Duration::from_secs(5);

// How long output left in a pipe is waited for once the build has exited.
const DRAINED: Duration = Duration::from_secs(2);

/// Why a build did not say what it is
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildError {
    message: String,
    // It ran and exited 2 with kelpie's usage: it predates the verb
    predates_version: bool,
}

impl BuildError {
    fn broken(message: String) -> Self {
        Self {
            message,
            predates_version: false,
        }
    }

    /// Whether the build ran and refused `version` as a verb it does not have
    ///
    /// The only way a working build fails to answer: one made before the
    /// verb existed. A build that could not start, hung, died on a signal or
    /// exited any other way is broken.
    pub fn predates_version(&self) -> bool {
        self.predates_version
    }
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<BuildError> for String {
    fn from(error: BuildError) -> Self {
        error.message
    }
}

/// What `version --json` prints: the build's version, the shep version it was
/// made with, and the commit it was made from
///
/// Other keys in the answer are ignored, so a later build may add some.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Build {
    /// Kelpie's own version
    pub kelpie: String,
    /// The shep version kelpie was made with, the earliest shepherd it takes
    pub shep: String,
    /// The shep protocol kelpie speaks, absent from a build from before it said
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<u32>,
    /// The commit the build was made from, absent from a build made where git
    /// could not answer, and ending in `-dirty` when the tree it was made from
    /// held changes that commit does not
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
}

/// The commit this build was made from, in seven characters, with `-dirty`
/// when the tree held changes that commit does not
///
/// `build.rs` bakes it in from git. `None` for a build made outside a git
/// checkout, or where git could not run: such a build says nothing of its
/// commit rather than guessing one.
pub const COMMIT: Option<&str> = option_env!("KELPIE_COMMIT");

/// The version `--version` answers with: this build's, with its commit as
/// semver build metadata when it names one
#[must_use]
pub fn probe_version() -> String {
    version_with_commit(env!("CARGO_PKG_VERSION"), COMMIT)
}

/// `version` with `commit` behind it as semver build metadata, and `version`
/// alone when it names none
#[must_use]
pub fn version_with_commit(version: &str, commit: Option<&str>) -> String {
    commit.map_or_else(
        || version.to_owned(),
        |commit| format!("{version}+{commit}"),
    )
}

impl Build {
    /// This build
    pub fn this() -> Self {
        Self {
            kelpie: env!("CARGO_PKG_VERSION").to_owned(),
            shep: SHEP_VERSION.to_owned(),
            protocol: Some(PROTOCOL_VERSION),
            commit: COMMIT.map(str::to_owned),
        }
    }

    /// The build at `binary`, as it answers `version --json`
    ///
    /// # Errors
    ///
    /// A message when `binary` does not run, does not answer within `TIMEOUT`,
    /// exits non-zero, or does not answer with a JSON object naming both versions.
    pub fn of(binary: &Path) -> Result<Self, BuildError> {
        Self::within(binary, TIMEOUT)
    }

    /// The build at `binary`, giving it `limit` to answer
    ///
    /// # Errors
    ///
    /// As [`Build::of`].
    pub fn within(binary: &Path, limit: Duration) -> Result<Self, BuildError> {
        let started = Instant::now();
        let mut child = loop {
            let spawned = crate::spawn::command(binary)
                .args(["version", "--json"])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn();
            match spawned {
                Ok(child) => break child,
                // A copy made a moment ago may still be held open for writing by
                // a process that forked meanwhile: that ends by itself.
                Err(e) if e.raw_os_error() == Some(ETXTBSY) && started.elapsed() < BUSY => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => {
                    return Err(BuildError::broken(format!(
                        "cannot run {}: {e}",
                        binary.display()
                    )));
                }
            }
        };
        let stdout = drain(child.stdout.take());
        let stderr = drain(child.stderr.take());
        let started = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if started.elapsed() >= limit => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(BuildError::broken(format!(
                        "{} did not answer `version --json` in {}s",
                        binary.display(),
                        limit.as_secs_f32()
                    )));
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                Err(e) => {
                    return Err(BuildError::broken(format!(
                        "cannot wait for {}: {e}",
                        binary.display()
                    )));
                }
            }
        };
        // A child of the build may hold the pipes open past its exit.
        let heard = |from: &Receiver<Vec<u8>>| from.recv_timeout(DRAINED).unwrap_or_default();
        let (stdout, stderr) = (heard(&stdout), heard(&stderr));
        if !status.success() {
            return Err(BuildError {
                message: format!(
                    "{} exited {status} on `version --json`: {}",
                    binary.display(),
                    String::from_utf8_lossy(&stderr).trim()
                ),
                predates_version: status.code() == Some(2),
            });
        }
        serde_json::from_slice(&stdout).map_err(|e| {
            BuildError::broken(format!(
                "{} did not answer `version --json` with its kelpie and shep versions: {e}",
                binary.display()
            ))
        })
    }
}

// Reads a pipe to its end on a thread of its own, so a full pipe never stalls the child.
fn drain(pipe: Option<impl Read + Send + 'static>) -> Receiver<Vec<u8>> {
    let (send, heard) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut bytes);
        }
        let _ = send.send(bytes);
    });
    heard
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test::Fake;

    #[test]
    fn this_build_names_its_kelpie_and_the_shep_it_is_made_with() {
        let json = serde_json::to_value(Build::this()).unwrap();
        assert_eq!(json["kelpie"], env!("CARGO_PKG_VERSION"));
        assert_eq!(json["shep"], SHEP_VERSION);
        assert_eq!(json["protocol"], PROTOCOL_VERSION);
    }

    #[test]
    fn the_version_answer_carries_the_commit_as_build_metadata() {
        assert_eq!(version_with_commit("0.3.0", None), "0.3.0");
        assert_eq!(
            version_with_commit("0.3.0", Some("1f2e3a4")),
            "0.3.0+1f2e3a4"
        );
        assert_eq!(
            version_with_commit("0.3.0", Some("1f2e3a4-dirty")),
            "0.3.0+1f2e3a4-dirty"
        );
    }

    #[test]
    fn a_build_that_hangs_is_given_up_on() {
        let dir = tempfile::tempdir().unwrap();
        let hang = crate::test::write_fake(dir.path(), "hang", &Fake::new().sleeps(30));
        let started = Instant::now();
        let err = Build::within(&hang, Duration::from_millis(200))
            .unwrap_err()
            .to_string();
        assert!(err.contains("did not answer `version --json`"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn a_build_that_exits_non_zero_is_refused_whatever_it_printed() {
        let dir = tempfile::tempdir().unwrap();
        let said = r#"{"kelpie":"0.3.0","shep":"0.11.0"}"#;
        let sick = crate::test::write_fake(dir.path(), "sick", &Fake::new().prints(said).exits(3));
        let err = Build::of(&sick).unwrap_err().to_string();
        assert!(err.contains("exited"), "{err}");
    }

    #[test]
    fn a_build_names_the_commit_it_was_made_from() {
        let dir = tempfile::tempdir().unwrap();
        let said = r#"{"kelpie":"0.3.0","shep":"0.19.0","commit":"1f2e3a4-dirty"}"#;
        let named = crate::test::write_fake(dir.path(), "named", &Fake::new().prints(said));

        let build = Build::of(&named).unwrap();

        assert_eq!(build.commit.as_deref(), Some("1f2e3a4-dirty"));
    }

    #[test]
    fn a_build_from_before_commits_were_recorded_names_none() {
        let dir = tempfile::tempdir().unwrap();
        let said = r#"{"kelpie":"0.3.0","shep":"0.19.0"}"#;
        let old = crate::test::write_fake(dir.path(), "old", &Fake::new().prints(said));

        let build = Build::of(&old).unwrap();

        assert_eq!(build.commit, None);
    }

    #[test]
    fn this_build_names_the_commit_it_was_made_from() {
        let commit = Build::this()
            .commit
            .expect("this build was made from a commit");
        let sha = commit.strip_suffix("-dirty").unwrap_or(&commit);
        assert!(sha.len() >= 7, "{commit}");
        assert!(sha.chars().all(|c| c.is_ascii_hexdigit()), "{commit}");
    }

    #[test]
    fn an_answer_with_more_keys_still_reads() {
        let build: Build =
            serde_json::from_str(r#"{ "kelpie": "0.3.0", "shep": "0.11.0", "extra": "abc" }"#)
                .unwrap();
        assert_eq!(
            (build.kelpie.as_str(), build.shep.as_str()),
            ("0.3.0", "0.11.0")
        );
    }
}
