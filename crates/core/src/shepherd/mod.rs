//! Kelpie's shepherd, reached over a [`Connection`] a [`Dial`] opens
//!
//! The dial a runner and the commands use is [`Socket`], shep's control
//! socket, always through the shep client kelpie is built with, never a `shep` on
//! `PATH`, which can be another version. A shepherd on the pinned version or
//! a later one is taken, on any line: whether the two speak the same
//! requests is shep's protocol handshake to decide, which refuses a client
//! below the shepherd's oldest protocol. An earlier shepherd is refused,
//! since kelpie leans on what its pinned version shipped.

use std::path::Path;

use serde_json::{Map, Value};
use shep_client::RequestError;
use shep_client::shep_core::protocol::request::Response;
use shep_client::shep_core::protocol::{Request, RpcErrorCode};

mod connection;

pub use connection::{Answer, Connection, Dial, Opened, Section, Socket};

/// The shep version kelpie is built with, pinned in the workspace manifest
pub const SHEP_VERSION: &str = "0.19.0";

/// The name kelpie's tables are kept under: `[app.dogs.kelpie]` on a
/// runner sheep, and `[kelpie]` in `dogs.toml`
pub const DOG: &str = "kelpie";

/// Why the dog has no shepherd channel, and the fix
///
/// Kelpie asks for it at `shep adopt`, which records the ask, so an
/// adoption from before the ask has none until kelpie is adopted again.
pub fn no_channel() -> String {
    let path = std::env::current_exe()
        .map_or_else(|_| "<path to kelpie>".into(), |p| p.display().to_string());
    format!(
        "no shepherd channel, since kelpie was adopted before it asked for one: run \
         `shep adopt {path} --name {DOG}`, then `shep disable {DOG}` and `shep enable {DOG}`"
    )
}

/// The trigger that withdraws a handover still waiting for the quiet point
pub const WITHDRAW: &str = "withdraw_handover";

/// Runs `work` to its end on a runtime of its own, for a caller with none
///
/// # Errors
///
/// `work`'s own error, or why no runtime could start.
pub fn block_on<T>(work: impl Future<Output = Result<T, String>>) -> Result<T, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("cannot start the async runtime: {e}"))?
        .block_on(work)
}

/// Why kelpie's shepherd could not be used
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectRefused {
    /// Nothing answered on the socket, or the handshake failed
    Unreachable(String),
    /// The shepherd runs an earlier shep than [`SHEP_VERSION`], or refused
    /// kelpie's protocol in the handshake, its version named when it said
    Skew(Option<String>),
}

impl ConnectRefused {
    /// What went wrong, naming the shepherd at `shep_home`
    pub fn describe(&self, shep_home: &Path) -> String {
        match self {
            Self::Unreachable(why) => format!(
                "cannot reach kelpie's shepherd at {}: {why}",
                shep_home.display()
            ),
            Self::Skew(running) => {
                let running = running.as_deref().map_or_else(
                    || "a shep version it did not name".to_owned(),
                    |v| format!("shep {v}"),
                );
                format!(
                    "kelpie's shepherd at {} runs {running}, and kelpie is built for shep \
                     {SHEP_VERSION} and takes a shepherd from {SHEP_VERSION} on that accepts \
                     its protocol",
                    shep_home.display(),
                )
            }
        }
    }
}

/// Connects to the shepherd at `shep_home`, refusing one on an earlier shep
/// than [`SHEP_VERSION`]
///
/// # Errors
///
/// [`ConnectRefused`] when nothing answers or kelpie does not take the version.
pub async fn connect(
    dial: &dyn Dial,
    shep_home: &Path,
) -> Result<Box<dyn Connection>, ConnectRefused> {
    let client = connect_any(dial, shep_home).await?;
    let running = client.daemon().daemon_version;
    if !takes(SHEP_VERSION, &running) {
        return Err(ConnectRefused::Skew(Some(running)));
    }
    Ok(client)
}

/// Connects to the shepherd at `shep_home` whatever shep line it runs
///
/// For `upgrade`, which asks a shepherd kelpie may refuse whether the build
/// it installs takes it. The shepherd's version is `client.daemon().daemon_version`.
///
/// # Errors
///
/// [`ConnectRefused::Unreachable`] when nothing answers, and
/// [`ConnectRefused::Skew`] when the shep client speaks a protocol the
/// shepherd does not.
pub async fn connect_any(
    dial: &dyn Dial,
    shep_home: &Path,
) -> Result<Box<dyn Connection>, ConnectRefused> {
    dial.open(shep_home).await
}

/// Whether `error` is shep's answer to a request naming no sheep it has
///
/// A trigger on an unknown name is refused with `NotFound`, never answered
/// with an empty list of replies.
pub fn names_no_sheep(error: &RequestError) -> bool {
    matches!(error, RequestError::Rpc(e) if e.code == RpcErrorCode::NotFound)
}

/// Whether a kelpie built for shep `built` takes a shepherd on `running`:
/// `built` or any later version
///
/// A pre-release of `built` itself is refused, since it may predate what
/// `built` shipped.
pub fn takes(built: &str, running: &str) -> bool {
    match (number(running), number(built)) {
        (Some(r), Some(b)) => r > b || (r == b && !pre_release(running)),
        _ => false,
    }
}

/// A version's major, minor and patch, without a pre-release or build suffix
pub fn number(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.splitn(3, '.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?;
    Some((major, minor, patch[..digits(patch)].parse().ok()?))
}

// Whether `version`'s patch number is followed by a pre-release, `-rc.1`.
fn pre_release(version: &str) -> bool {
    let patch = version.split('.').nth(2).unwrap_or_default();
    patch[digits(patch)..].starts_with('-')
}

// How many ASCII digits `text` starts with.
fn digits(text: &str) -> usize {
    text.find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len())
}

/// Kelpie's two tables, as the shepherd holds them
///
/// `Debug` does not leak either table: `[kelpie]` holds the webhook's URL.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Tables {
    /// The runner sheep's `[app.dogs.kelpie]` table, if it has one
    pub project: Option<Map<String, Value>>,
    /// Kelpie's `[kelpie]` section of `dogs.toml` as TOML text, empty when unset
    pub kelpie: String,
}

impl core::fmt::Debug for Tables {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Tables")
            .field("project", &self.project.is_some())
            .field("kelpie", &!self.kelpie.trim().is_empty())
            .finish()
    }
}

/// Reads `sheep`'s `[app.dogs.kelpie]` table and kelpie's `[kelpie]` section
///
/// # Errors
///
/// A message naming the shepherd when it cannot be reached, is refused, or
/// does not answer with the tables.
pub async fn read_tables(dial: &dyn Dial, shep_home: &Path, sheep: &str) -> Result<Tables, String> {
    let client = connect(dial, shep_home)
        .await
        .map_err(|e| e.describe(shep_home))?;
    read_tables_with(&client, sheep).await
}

/// [`read_tables`], on a connection already made
///
/// # Errors
///
/// A message naming what the shepherd did not answer with.
pub async fn read_tables_with(client: &dyn Connection, sheep: &str) -> Result<Tables, String> {
    let asked = |what: &str, e: &dyn core::fmt::Display| {
        format!("kelpie's shepherd did not answer with {what}: {e}")
    };
    let project = match client
        .request(Request::DogSheepSettings { dog: DOG.into() })
        .await
        .map_err(|e| asked("the sheep's tables", &e))?
    {
        Response::DogSheepSettings { mut tables } => {
            tables.remove(sheep).map(|t| t.as_map().clone())
        }
        other => return Err(asked("the sheep's tables", &format!("{other:?}"))),
    };
    let kelpie = read_section(client).await?;
    Ok(Tables { project, kelpie })
}

/// Kelpie's `[kelpie]` section of `dogs.toml` as TOML text, empty when unset
///
/// # Errors
///
/// A message naming what the shepherd did not answer with.
pub async fn read_section(client: &dyn Connection) -> Result<String, String> {
    let asked = |e: &dyn core::fmt::Display| {
        format!("kelpie's shepherd did not answer with kelpie's section: {e}")
    };
    let section = client.dog_config(DOG).await.map_err(|e| asked(&e))?;
    Ok(section.as_str().to_owned())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use shep_client::shep_core::config::DogTable;
    use tokio::sync::mpsc::UnboundedReceiver;

    use super::*;
    use crate::test::{CALL_PATIENCE, Memory};

    fn table(forge: &str) -> Map<String, Value> {
        let mut table = Map::new();
        table.insert("forge".into(), Value::String(forge.into()));
        table
    }

    // A shepherd on `version` holding kelpie tables for two sheep, and a
    // `[kelpie]` section. It answers while its requests are held.
    fn shepherd(version: &str) -> (tempfile::TempDir, Memory, UnboundedReceiver<Request>) {
        let home = tempfile::tempdir().unwrap();
        let (dial, sent) = Memory::answering(home.path(), version, |request| match request {
            Request::DogSheepSettings { dog } if dog == DOG => Response::DogSheepSettings {
                tables: BTreeMap::from([
                    ("shep".to_owned(), DogTable::from(table("shep-pm/shep"))),
                    ("koji".to_owned(), DogTable::from(table("shep-pm/koji"))),
                ]),
            },
            Request::DogConfigResolved { name } if name == DOG => Response::DogSection {
                toml: "[webhook]\nkind = \"ntfy\"\ntoken = \"tk_resolved\"\n"
                    .to_owned()
                    .into(),
            },
            Request::DogConfig { name } if name == DOG => Response::DogSection {
                toml: "[webhook]\nkind = \"ntfy\"\ntoken = \"{{secret:ntfy}}\"\n"
                    .to_owned()
                    .into(),
            },
            other => panic!("kelpie asked {other:?}"),
        });
        (home, dial, sent)
    }

    async fn read_in_time(dial: &Memory, home: &Path, sheep: &str) -> Result<Tables, String> {
        tokio::time::timeout(CALL_PATIENCE, read_tables(dial, home, sheep))
            .await
            .expect("the tables neither came nor failed in time")
    }

    #[tokio::test]
    async fn a_runner_reads_its_own_sheep_s_table_and_kelpie_s_section() {
        let (home, dial, _sent) = shepherd("0.19.1");
        let tables = read_in_time(&dial, home.path(), "koji").await.unwrap();
        assert_eq!(tables.project, Some(table("shep-pm/koji")));
        assert_eq!(
            tables.kelpie,
            "[webhook]\nkind = \"ntfy\"\ntoken = \"tk_resolved\"\n"
        );
    }

    #[tokio::test]
    async fn a_secret_in_kelpie_s_section_reaches_it_resolved() {
        let (home, dial, _sent) = shepherd(SHEP_VERSION);
        let tables = read_in_time(&dial, home.path(), "koji").await.unwrap();
        assert!(!tables.kelpie.contains("{{secret:"), "{}", tables.kelpie);
        assert!(tables.kelpie.contains("tk_resolved"), "{}", tables.kelpie);
    }

    #[tokio::test]
    async fn a_sheep_with_no_table_reads_none() {
        let (home, dial, _sent) = shepherd(SHEP_VERSION);
        let tables = read_in_time(&dial, home.path(), "webapp").await.unwrap();
        assert_eq!(tables.project, None);
    }

    #[tokio::test]
    async fn no_shepherd_is_named_by_its_home() {
        let (_listening, dial, _sent) = shepherd(SHEP_VERSION);
        let home = tempfile::tempdir().unwrap();
        let err = read_in_time(&dial, home.path(), "shep").await.unwrap_err();
        let expected = format!(
            "cannot reach kelpie's shepherd at {}: ",
            home.path().display()
        );
        assert!(err.starts_with(&expected), "{err}");
    }

    #[test]
    fn a_shepherd_that_names_no_version_is_still_refused() {
        let err = ConnectRefused::Skew(None).describe(Path::new("/k/shep"));
        assert!(err.contains("runs a shep version it did not name"), "{err}");
    }

    #[tokio::test]
    async fn a_shepherd_on_a_later_minor_or_major_is_taken() {
        for version in ["0.20.0", "1.0.0"] {
            let (home, dial, _sent) = shepherd(version);
            let tables = read_in_time(&dial, home.path(), "koji").await.unwrap();
            assert_eq!(tables.project, Some(table("shep-pm/koji")), "{version}");
        }
    }

    // 0.19.0 is the floor, the shep kelpie is built on. 0.12.4 was the first shep whose
    // stop sweeps a sheep's whole lamb tree, so every shepherd kelpie takes has it.
    // A 0.14 shepherd drops a runner's notes as malformed, so it is refused too.
    #[tokio::test]
    async fn a_shepherd_from_before_the_floor_is_refused_naming_the_one_needed() {
        for version in [
            "0.12.4",
            "0.14.0",
            "0.16.0",
            "0.17.0",
            "0.18.2",
            "0.19.0-rc.1",
        ] {
            let (home, dial, mut sent) = shepherd(version);
            let err = read_in_time(&dial, home.path(), "shep").await.unwrap_err();
            assert!(err.contains(&format!("runs shep {version}")), "{err}");
            assert!(err.contains("takes a shepherd from 0.19.0 on"), "{err}");
            assert!(sent.try_recv().is_err(), "{version} was asked for a table");
        }
    }

    #[test]
    fn a_later_version_is_taken_but_not_an_earlier_one_a_pre_release_or_an_unnumbered_one() {
        assert!(takes("0.12.4", "0.12.4"));
        assert!(takes("0.12.4", "0.12.10"));
        assert!(takes("0.12.4", "0.12.5-rc.1"));
        assert!(takes("0.12.4", "0.13.0"));
        assert!(takes("0.12.4", "1.0.0"));
        assert!(takes("0.12.4", "0.12.4+build.7"));
        assert!(!takes("0.12.4", "0.12.4-rc.1"));
        assert!(!takes("0.12.4", "0.12.3"));
        assert!(!takes("0.12.4", "0.11.9"));
        assert!(!takes("0.12.4", "0.12"));
        assert!(!takes("0.12.4", "0.12.x"));
    }

    #[test]
    fn the_checked_version_is_the_one_the_manifest_pins() {
        let manifest: toml::Table = include_str!("../../../../Cargo.toml").parse().unwrap();
        let pins = &manifest["workspace"]["dependencies"];
        for krate in ["shep-client", "shep-channel"] {
            assert_eq!(
                pins[krate]["version"].as_str(),
                Some(format!("={SHEP_VERSION}").as_str()),
                "{krate} is not pinned to {SHEP_VERSION}"
            );
        }
    }

    // A derived Debug would print the webhook's URL from kelpie's section.
    #[test]
    fn debug_does_not_leak_either_table() {
        let tables = Tables {
            project: Some(Map::new()),
            kelpie: "[webhook]\nurl = \"https://s3cr3t\"\n".into(),
        };
        assert_eq!(
            format!("{tables:?}"),
            "Tables { project: true, kelpie: true }"
        );
    }
}
