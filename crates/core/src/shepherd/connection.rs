//! The connection to a shepherd, and the port that opens it
//!
//! Kelpie asks a shepherd through a [`Connection`], and opens one through a
//! [`Dial`]. [`Socket`] is the real dial: shep's control socket, through
//! the shep client. A test dials a shepherd that lives in its own memory
//! instead, so it binds nothing and runs wherever the tests run.

use std::fmt;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::time::Duration;

use shep_client::shep_core::protocol::request::Response;
use shep_client::shep_core::protocol::{DogSectionToml, HelloAck, Request};
use shep_client::{Client, ConnectError, ReconnectingClient, RequestError};

use super::ConnectRefused;
use crate::home::shepherd_address;

/// What a [`Connection`] answers a request with
pub type Answer<'a> = Pin<Box<dyn Future<Output = Result<Response, RequestError>> + Send + 'a>>;

/// What a [`Connection`] answers a read of a dog's section with
pub type Section<'a> =
    Pin<Box<dyn Future<Output = Result<DogSectionToml, RequestError>> + Send + 'a>>;

/// What a [`Dial`] opens
pub type Opened<'a> =
    Pin<Box<dyn Future<Output = Result<Box<dyn Connection>, ConnectRefused>> + Send + 'a>>;

/// An open connection to a shepherd, which has been through its handshake
///
/// Ask it with `request` or `request_with_deadline`, which `dyn Connection`
/// carries over [`send`](Connection::send).
pub trait Connection: Send + Sync + fmt::Debug {
    /// Sends `body` and waits for the shepherd's answer, for `deadline` or
    /// the shep client's default when `None`
    fn send(&self, body: Request, deadline: Option<Duration>) -> Answer<'_>;

    /// What the shepherd said of itself in the handshake
    fn daemon(&self) -> HelloAck;

    /// The dog `name`'s section of `dogs.toml`, with every `{{secret:...}}` in it resolved
    ///
    /// The shep client answers it, which falls back on a shepherd too old to resolve.
    /// A connection that is not the shep client asks `DogConfigResolved` itself.
    fn dog_config<'a>(&'a self, name: &'a str) -> Section<'a> {
        Box::pin(async move {
            let body = Request::DogConfigResolved { name: name.into() };
            match self.send(body, None).await? {
                Response::DogSection { toml } => Ok(toml),
                other => Err(RequestError::UnexpectedReply {
                    asked: "DogConfigResolved",
                    answered: other.name(),
                }),
            }
        })
    }
}

impl dyn Connection + '_ {
    /// Sends `body` with the shep client's default deadline
    ///
    /// # Errors
    ///
    /// The shepherd's refusal, a timeout, or a connection that closed.
    pub async fn request(&self, body: Request) -> Result<Response, RequestError> {
        self.send(body, None).await
    }

    /// Sends `body` with `deadline`, or the default when `None`
    ///
    /// # Errors
    ///
    /// As `request`.
    pub async fn request_with_deadline(
        &self,
        body: Request,
        deadline: Option<Duration>,
    ) -> Result<Response, RequestError> {
        self.send(body, deadline).await
    }
}

impl<T: Connection + ?Sized> Connection for Box<T> {
    fn send(&self, body: Request, deadline: Option<Duration>) -> Answer<'_> {
        (**self).send(body, deadline)
    }

    fn daemon(&self) -> HelloAck {
        (**self).daemon()
    }

    fn dog_config<'a>(&'a self, name: &'a str) -> Section<'a> {
        (**self).dog_config(name)
    }
}

impl Connection for Client {
    fn send(&self, body: Request, deadline: Option<Duration>) -> Answer<'_> {
        Box::pin(self.request_with_deadline(body, deadline))
    }

    fn daemon(&self) -> HelloAck {
        Self::daemon(self).clone()
    }

    fn dog_config<'a>(&'a self, name: &'a str) -> Section<'a> {
        Box::pin(Self::dog_config(self, name))
    }
}

impl Connection for ReconnectingClient {
    fn send(&self, body: Request, deadline: Option<Duration>) -> Answer<'_> {
        Box::pin(self.request_with_deadline(body, deadline))
    }

    fn daemon(&self) -> HelloAck {
        Self::daemon(self)
    }

    fn dog_config<'a>(&'a self, name: &'a str) -> Section<'a> {
        Box::pin(Self::dog_config(self, name))
    }
}

/// Opens a [`Connection`] to the shepherd at a shep home
pub trait Dial: Send + Sync + fmt::Debug {
    /// Connects to the shepherd at `shep_home`, whatever shep line it runs
    ///
    /// The version is the caller's to check, from [`Connection::daemon`].
    fn open<'a>(&'a self, shep_home: &'a Path) -> Opened<'a>;
}

/// The real dial: shep's control socket under the shep home
#[derive(Debug, Clone, Copy, Default)]
pub struct Socket;

impl Dial for Socket {
    fn open<'a>(&'a self, shep_home: &'a Path) -> Opened<'a> {
        Box::pin(async move {
            let client = Client::connect(&shepherd_address(shep_home))
                .await
                .map_err(|e| match e {
                    ConnectError::ProtocolMismatch { daemon_version, .. } => {
                        ConnectRefused::Skew(daemon_version)
                    }
                    other => ConnectRefused::Unreachable(other.to_string()),
                })?;
            Ok(Box::new(client) as Box<dyn Connection>)
        })
    }
}

// These bind a real socket, to prove the one thing the in-memory shepherd
// cannot: that `Socket` reaches the address shep-core gives a shep home. A
// worker's sandbox refuses the bind, so they run in CI.
#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use shep_client::shep_core::paths::ShepPaths;
    use shep_client::shep_core::protocol::request::Response;
    use shep_client::shep_core::protocol::{RpcError, RpcErrorCode};
    use shep_client::testing::{fake_daemon, fake_daemon_answering_with_ack, sample_ack};

    use super::*;
    use crate::test::short_tempdir;

    // A shep home holding the folder the control socket lives in.
    fn shep_home() -> tempfile::TempDir {
        let home = short_tempdir();
        std::fs::create_dir_all(home.path().join("run")).unwrap();
        home
    }

    // Where shep-core says the shepherd of `home` listens.
    fn shepherd_at(home: &tempfile::TempDir) -> PathBuf {
        ShepPaths::at(home.path().to_path_buf()).socket
    }

    #[tokio::test]
    async fn the_socket_dial_reaches_the_shepherd_under_the_shep_home() {
        let home = shep_home();
        let ack = sample_ack();
        let _heard = fake_daemon_answering_with_ack(&shepherd_at(&home), ack.clone(), |_| {
            Response::Flock(Vec::new())
        })
        .await;

        let connection = Socket.open(home.path()).await.unwrap();

        assert_eq!(connection.daemon().daemon_version, ack.daemon_version);
        let reply = connection.request(Request::ListFlock).await.unwrap();
        assert!(
            matches!(&reply, Response::Flock(rows) if rows.is_empty()),
            "{reply:?}"
        );
    }

    #[tokio::test]
    async fn a_secret_in_a_dog_s_section_reaches_the_socket_connection_resolved() {
        let home = shep_home();
        let _heard = fake_daemon_answering_with_ack(&shepherd_at(&home), sample_ack(), |request| {
            match request {
                Request::DogConfigResolved { .. } => Response::DogSection {
                    toml: "token = \"tk_resolved\"\n".to_owned().into(),
                },
                Request::DogConfig { .. } => Response::DogSection {
                    toml: "token = \"{{secret:ntfy}}\"\n".to_owned().into(),
                },
                other => panic!("kelpie asked {other:?}"),
            }
        })
        .await;

        let connection = Socket.open(home.path()).await.unwrap();
        let section = connection.dog_config("kelpie").await.unwrap();

        assert_eq!(section.as_str(), "token = \"tk_resolved\"\n");
    }

    #[tokio::test]
    async fn the_socket_dial_finds_nothing_when_no_shepherd_listens() {
        let home = shep_home();

        let refused = Socket.open(home.path()).await.unwrap_err();

        assert!(
            matches!(refused, ConnectRefused::Unreachable(_)),
            "{refused:?}"
        );
    }

    #[tokio::test]
    async fn the_socket_dial_reports_the_version_of_a_shepherd_on_another_protocol() {
        let home = shep_home();
        let _hello = fake_daemon(
            &shepherd_at(&home),
            Err(RpcError {
                code: RpcErrorCode::ProtocolMismatch,
                message: "protocol 99".into(),
                daemon_version: Some("9.9.9".into()),
            }),
        )
        .await;

        let refused = Socket.open(home.path()).await.unwrap_err();

        assert!(
            matches!(&refused, ConnectRefused::Skew(Some(v)) if v == "9.9.9"),
            "{refused:?}"
        );
    }
}
