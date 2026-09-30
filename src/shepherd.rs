//! Kelpie's shepherd, reached over its socket
//!
//! Always through the shep client kelpie is built with, never a `shep` on
//! `PATH`, which can be another version. A shepherd on another shep major
//! or minor is refused, since the requests kelpie sends may not mean the
//! same there. A patch release of the pinned line is taken.

use std::path::Path;

use serde_json::{Map, Value};
use shep_client::shep_core::protocol::request::Response;
use shep_client::shep_core::protocol::{Request, RpcErrorCode};
use shep_client::{Client, ConnectError, RequestError};

/// The shep version kelpie is built with, pinned in the workspace manifest
pub const SHEP_VERSION: &str = "0.11.0";

/// The name kelpie's tables are kept under: `[app.dogs.kelpie]` on a
/// runner sheep, and `[kelpie]` in `dogs.toml`
pub const DOG: &str = "kelpie";

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
    /// The shepherd runs another shep minor or major, named when it said
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
                let (major, minor) = release_line(SHEP_VERSION);
                format!(
                    "kelpie's shepherd at {} runs {running}, and kelpie is built for shep \
                     {SHEP_VERSION} and takes only a {}.{}.x shepherd",
                    shep_home.display(),
                    major.unwrap_or_default(),
                    minor.unwrap_or_default(),
                )
            }
        }
    }
}

/// Connects to the shepherd at `shep_home`, refusing another minor or major
///
/// # Errors
///
/// [`ConnectRefused`] when nothing answers or the version is not the pinned line.
pub async fn connect(shep_home: &Path) -> Result<Client, ConnectRefused> {
    let client = Client::connect(&shep_home.join("run/shep.sock"))
        .await
        .map_err(|e| match e {
            ConnectError::ProtocolMismatch { daemon_version, .. } => {
                ConnectRefused::Skew(daemon_version)
            }
            other => ConnectRefused::Unreachable(other.to_string()),
        })?;
    let running = client.daemon().daemon_version.as_str();
    if release_line(running) != release_line(SHEP_VERSION) {
        return Err(ConnectRefused::Skew(Some(running.to_owned())));
    }
    Ok(client)
}

/// Whether `error` is shep's answer to a request naming no sheep it has
///
/// A trigger on an unknown name is refused with `NotFound`, never answered
/// with an empty list of replies.
pub fn names_no_sheep(error: &RequestError) -> bool {
    matches!(error, RequestError::Rpc(e) if e.code == RpcErrorCode::NotFound)
}

// A version's major and minor.
fn release_line(version: &str) -> (Option<&str>, Option<&str>) {
    let mut parts = version.split('.');
    (parts.next(), parts.next())
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
/// A message naming the shepherd when it cannot be reached, runs another
/// minor, or does not answer with the tables.
pub async fn read_tables(shep_home: &Path, sheep: &str) -> Result<Tables, String> {
    let client = connect(shep_home)
        .await
        .map_err(|e| e.describe(shep_home))?;
    read_tables_with(&client, sheep).await
}

/// [`read_tables`], on a connection already made
///
/// # Errors
///
/// A message naming what the shepherd did not answer with.
pub async fn read_tables_with(client: &Client, sheep: &str) -> Result<Tables, String> {
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
pub async fn read_section(client: &Client) -> Result<String, String> {
    let asked = |e: &dyn core::fmt::Display| {
        format!("kelpie's shepherd did not answer with kelpie's section: {e}")
    };
    match client
        .request(Request::DogConfig { name: DOG.into() })
        .await
        .map_err(|e| asked(&e))?
    {
        Response::DogSection { toml } => Ok(toml.as_str().to_owned()),
        other => Err(asked(&format!("{other:?}"))),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use shep_client::shep_core::config::DogTable;
    use shep_client::shep_core::protocol::{Envelope, HelloAck};
    use shep_client::testing::{fake_daemon_answering_with_ack, sample_ack};
    use tokio::sync::mpsc::UnboundedReceiver;

    use super::*;

    // Bounds every call against a fake shepherd, so a hang fails by name.
    const PATIENCE: Duration = Duration::from_secs(10);

    fn ack(version: &str) -> HelloAck {
        let mut ack = sample_ack();
        ack.daemon_version = version.into();
        ack
    }

    fn table(forge: &str) -> Map<String, Value> {
        let mut table = Map::new();
        table.insert("forge".into(), Value::String(forge.into()));
        table
    }

    // A shepherd on `version` holding kelpie tables for two sheep, and a
    // `[kelpie]` section. It answers while its requests are held.
    async fn shepherd(version: &str) -> (tempfile::TempDir, UnboundedReceiver<Envelope>) {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join("run")).unwrap();
        let socket = home.path().join("run/shep.sock");
        let sent = fake_daemon_answering_with_ack(&socket, ack(version), |request| match request {
            Request::DogSheepSettings { dog } if dog == DOG => Response::DogSheepSettings {
                tables: BTreeMap::from([
                    ("shep".to_owned(), DogTable::from(table("shep-pm/shep"))),
                    ("koji".to_owned(), DogTable::from(table("shep-pm/koji"))),
                ]),
            },
            Request::DogConfig { name } if name == DOG => Response::DogSection {
                toml: "[webhook]\nkind = \"ntfy\"\n".to_owned().into(),
            },
            other => panic!("kelpie asked {other:?}"),
        })
        .await;
        (home, sent)
    }

    async fn read_in_time(home: &Path, sheep: &str) -> Result<Tables, String> {
        tokio::time::timeout(PATIENCE, read_tables(home, sheep))
            .await
            .expect("the tables neither came nor failed in time")
    }

    // Real sockets under a real clock: a paused one would time out the
    // handshake while the fake's socket is merely waiting.
    #[tokio::test]
    async fn a_runner_reads_its_own_sheep_s_table_and_kelpie_s_section() {
        let (home, _sent) = shepherd("0.11.2").await;
        let tables = read_in_time(home.path(), "koji").await.unwrap();
        assert_eq!(tables.project, Some(table("shep-pm/koji")));
        assert_eq!(tables.kelpie, "[webhook]\nkind = \"ntfy\"\n");
    }

    #[tokio::test]
    async fn a_sheep_with_no_table_reads_none() {
        let (home, _sent) = shepherd(SHEP_VERSION).await;
        let tables = read_in_time(home.path(), "webapp").await.unwrap();
        assert_eq!(tables.project, None);
    }

    #[tokio::test]
    async fn no_shepherd_is_named_by_its_home() {
        let home = tempfile::tempdir().unwrap();
        let err = read_in_time(home.path(), "shep").await.unwrap_err();
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
    async fn a_shepherd_on_another_minor_is_refused_by_name() {
        for version in ["0.10.1", "0.12.0"] {
            let (home, mut sent) = shepherd(version).await;
            let err = read_in_time(home.path(), "shep").await.unwrap_err();
            assert!(err.contains(&format!("runs shep {version}")), "{err}");
            assert!(err.contains("takes only a 0.11.x shepherd"), "{err}");
            assert!(sent.try_recv().is_err(), "{version} was asked for a table");
        }
    }

    #[test]
    fn the_checked_version_is_the_one_the_manifest_pins() {
        let manifest = include_str!("../../../Cargo.toml");
        for krate in ["shep-client", "shep-channel"] {
            let pin = format!("{krate} = {{ version = \"={SHEP_VERSION}\"");
            assert!(
                manifest.contains(&pin),
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
