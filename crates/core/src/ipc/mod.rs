//! Local IPC: a Unix socket on macOS and Linux, a named pipe on Windows
//!
//! The lease door, the bridges and the forwarders each listen at a socket path.
//! This is the one place that path becomes a transport. The async half is
//! shep's own (`shep_core::transport`), so kelpie's door and shep's speak the
//! same way. [`blocking`] is the same for the threads serving a bridge.
//!
//! A socket path keeps naming the endpoint everywhere. On Windows [`address`]
//! turns it into a pipe's name, so a path under one home never names the pipe
//! another home's kelpie opened.

use std::io;
use std::path::{Path, PathBuf};

use shep_client::shep_core::transport;

pub use transport::{
    ClientStream, ServerReadHalf, ServerStream, ServerWriteHalf, connected_pair, split,
};

pub mod blocking;
#[cfg(test)]
mod tests;

/// The longest path a Unix socket may have, one byte short of macOS's 104
pub const LONGEST_SOCKET: usize = 103;

/// Whether `socket` is too long to bind, which only a Unix socket's path can be
#[must_use]
pub fn too_long(socket: &Path) -> bool {
    cfg!(unix) && socket.as_os_str().len() > LONGEST_SOCKET
}

/// Where the transport listens for the endpoint `socket` names
///
/// The path itself on Unix. On Windows `\\.\pipe\kelpie-<name>-<digest>`, the
/// digest taken over the whole path, so two homes get two pipes. The result is
/// only path-shaped: never take its parent or ask whether it exists.
#[must_use]
pub fn address(socket: &Path) -> PathBuf {
    #[cfg(unix)]
    {
        socket.to_owned()
    }
    #[cfg(windows)]
    {
        // A folder that exists is named by what it resolves to, so a link or
        // short name for it makes no second pipe.
        let resolved = socket
            .parent()
            .and_then(|folder| folder.canonicalize().ok())
            .zip(socket.file_name())
            .map(|(folder, name)| folder.join(name));
        PathBuf::from(pipe_name(resolved.as_deref().unwrap_or(socket)))
    }
}

// Windows reads a path without regard to case, to which slash divides it, to
// `.` and `..` or to a verbatim prefix, so every spelling of one path must
// name one pipe, or two dogs could both open it.
#[cfg(any(windows, test))]
fn pipe_name(socket: &Path) -> String {
    use sha1::{Digest, Sha1};
    // Bounds the readable half: a pipe's name may be 256 characters.
    const MAX_STEM: usize = 48;

    let lower = socket.to_string_lossy().to_lowercase().replace('/', r"\");
    let plain = match lower.strip_prefix(r"\\?\") {
        Some(verbatim) => verbatim.strip_prefix(r"unc\").unwrap_or(verbatim),
        None => &lower,
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in plain.split('\\') {
        match part {
            "" | "." => {}
            ".." => {
                // The drive is a root: nothing is above it.
                if parts.len() > 1 {
                    parts.pop();
                }
            }
            part => parts.push(part),
        }
    }
    let whole = parts.join(r"\");
    let digest = Sha1::digest(whole.as_bytes());
    let digest: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
    let file = parts.last().copied().unwrap_or_default();
    let stem: String = file
        .rsplit_once('.')
        .map_or(file, |(stem, _)| stem)
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(MAX_STEM)
        .collect();
    format!(r"\\.\pipe\kelpie-{stem}-{digest}")
}

/// Binds `socket` for the maintainer's user alone, through `bind`
///
/// On Unix a socket left by a process that is gone is replaced, and one a live
/// process answers on is left alone. `bind` is given a private name beside
/// `socket`, whose mode is cut to 0600 before it is renamed into place, so no
/// client reaches a socket that is half made. On Windows `bind` is given
/// `socket` itself: the pipe is writable only by its creator and
/// administrators, and a second process cannot create it. `rival` names what
/// else may answer there (`dog`, `runner`), for the message.
///
/// # Errors
///
/// A message when the path is too long, taken by a live process or something
/// else, or cannot be bound.
pub fn open_owner_only<L>(
    socket: &Path,
    rival: &str,
    bind: impl FnOnce(&Path) -> io::Result<L>,
) -> Result<L, String> {
    #[cfg(unix)]
    {
        open_socket(socket, rival, bind)
    }
    #[cfg(windows)]
    {
        bind(socket).map_err(|e| match e.kind() {
            io::ErrorKind::PermissionDenied => {
                format!("another {rival} answers at {}", socket.display())
            }
            _ => format!("cannot open {}: {e}", socket.display()),
        })
    }
}

#[cfg(unix)]
fn open_socket<L>(
    socket: &Path,
    rival: &str,
    bind: impl FnOnce(&Path) -> io::Result<L>,
) -> Result<L, String> {
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};

    let private = socket.with_extension(format!("{}.new", std::process::id()));
    crate::home::socket_fits(&private)?;
    match std::fs::symlink_metadata(socket) {
        Ok(meta) if meta.file_type().is_socket() => {
            if blocking::Stream::connect(socket).is_ok() {
                return Err(format!("another {rival} answers at {}", socket.display()));
            }
        }
        Ok(_) => return Err(format!("{} is not a socket", socket.display())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("cannot read {}: {e}", socket.display())),
    }
    let _ = std::fs::remove_file(&private);
    let listener = bind(&private).map_err(|e| format!("cannot open {}: {e}", private.display()))?;
    let placed = std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o600))
        .and_then(|()| std::fs::rename(&private, socket));
    if let Err(e) = placed {
        let _ = std::fs::remove_file(&private);
        return Err(format!("cannot open {}: {e}", socket.display()));
    }
    Ok(listener)
}

/// Connects to the endpoint `socket` names
///
/// # Errors
///
/// Whatever the OS says, such as nothing listening there.
pub async fn connect(socket: &Path) -> io::Result<ClientStream> {
    transport::connect(&address(socket)).await
}

/// Where connections to an endpoint arrive
#[derive(Debug)]
pub struct Listener(transport::Listener);

impl Listener {
    /// Listens at the endpoint `socket` names
    ///
    /// # Errors
    ///
    /// Whatever the OS says: on Unix the path is taken or cannot be written,
    /// on Windows the pipe is already served.
    pub fn bind(socket: &Path) -> io::Result<Self> {
        transport::Listener::bind(&address(socket)).map(Self)
    }

    /// The next connection
    ///
    /// # Errors
    ///
    /// Whatever the OS says. The caller decides whether to go on.
    pub async fn accept(&mut self) -> io::Result<ServerStream> {
        self.0.accept().await
    }
}
