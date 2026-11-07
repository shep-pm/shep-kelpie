//! codebase-memory-mcp, the code graph server, installed from its release
//!
//! Kelpie runs the binary itself and writes the MCP config for each call. The
//! tool's own `install` is never run, since it writes to the maintainer's
//! agent config files.

use std::path::{Path, PathBuf};

use super::{Tools, run};

/// The release `tools install` fetches, which the digests below belong to
pub const RELEASE: &str = "v0.11.0";

const REPO: &str = "https://github.com/DeusData/codebase-memory-mcp/releases/download";
const PROGRAM: &str = "codebase-memory-mcp";

/// One release asset: the archive's name and the SHA-256 of its bytes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Asset {
    pub(super) name: &'static str,
    pub(super) sha256: &'static str,
}

/// The asset for a machine, from `std::env::consts::{OS, ARCH}`, if the release has one
///
/// The Linux assets are the `portable` builds, which link nothing from the machine.
pub(super) fn asset(os: &str, arch: &str) -> Option<Asset> {
    let (name, sha256) = match (os, arch) {
        ("macos", "aarch64") => (
            "codebase-memory-mcp-darwin-arm64.tar.gz",
            "4dee7f38b63740e6751d7a7ed7eb10291c1f2a3ea2415f599dc68370ca0a2d18",
        ),
        ("macos", "x86_64") => (
            "codebase-memory-mcp-darwin-amd64.tar.gz",
            "dbf1c73bfcbde64e7dde4cd1320da7afc02e2c972ee1789ae039521411f5132e",
        ),
        ("linux", "x86_64") => (
            "codebase-memory-mcp-linux-amd64-portable.tar.gz",
            "1f9e8293eb2bc5c05cfa27a7e8fc033da6d729ffad525ccfcdaa3fd606306683",
        ),
        ("linux", "aarch64") => (
            "codebase-memory-mcp-linux-arm64-portable.tar.gz",
            "d62eeb224d5ee3eba3070938ec62cf1033f10b041ec1c4b2fb67f7aef390cc7b",
        ),
        _ => return None,
    };
    Some(Asset { name, sha256 })
}

impl Tools {
    /// The code graph server's executable, `codebase-memory-mcp`
    pub fn code_graph(&self) -> PathBuf {
        self.0.join("bin").join(PROGRAM)
    }

    /// Whether the code graph server is installed
    pub fn has_code_graph(&self) -> bool {
        self.code_graph().is_file()
    }

    /// Fetches the pinned release of the code graph server, as `asset` names it
    ///
    /// A machine the release has no build for, which has no `asset`, is left without one, which
    /// returns `Ok(false)`: its calls run without a graph.
    ///
    /// # Errors
    ///
    /// The step that failed, and why.
    pub(super) fn install_code_graph(
        &self,
        asset: Option<Asset>,
        curl: &str,
        progress: bool,
    ) -> Result<bool, String> {
        let Some(asset) = asset else {
            return Ok(false);
        };
        let staging = self.0.join("code-graph-staging");
        let fetched = self.fetch_code_graph(&asset, curl, progress, &staging);
        // Whatever the fetch left is not worth keeping, whether it worked or not.
        let _ = std::fs::remove_dir_all(&staging);
        fetched.map(|()| true)
    }

    fn fetch_code_graph(
        &self,
        asset: &Asset,
        curl: &str,
        progress: bool,
        staging: &Path,
    ) -> Result<(), String> {
        let _ = std::fs::remove_dir_all(staging);
        std::fs::create_dir_all(staging)
            .map_err(|e| format!("cannot make {}: {e}", staging.display()))?;
        let archive = staging.join(asset.name);
        let url = format!("{REPO}/{RELEASE}/{}", asset.name);
        // curl draws its bar on stderr, so a pipe or a log gets none of it.
        let quiet_or_bar = if progress {
            "--progress-bar"
        } else {
            "--silent"
        };
        let fetch = crate::spawn::command(curl)
            .args([
                "--fail",
                quiet_or_bar,
                "--show-error",
                "--location",
                "--output",
            ])
            .arg(&archive)
            .arg(&url)
            .current_dir(staging)
            .status();
        let fetch = fetch.map_err(|e| format!("cannot run {curl}: {e}"))?;
        if !fetch.success() {
            return Err(format!("cannot fetch {url}: curl {fetch}"));
        }
        self.unpack_code_graph(&archive, asset.sha256, staging)
    }

    /// Puts the server out of `archive` at [`Tools::code_graph`], if the
    /// archive's SHA-256 is `sha256`
    pub(super) fn unpack_code_graph(
        &self,
        archive: &Path,
        sha256: &str,
        staging: &Path,
    ) -> Result<(), String> {
        let found = digest(archive)?;
        if found != sha256 {
            return Err(format!(
                "{} has SHA-256 {found}, not the {sha256} that {RELEASE} was pinned at: \
                 read what changed in the release before kelpie runs it",
                archive.display()
            ));
        }
        let out = staging.join("unpacked");
        std::fs::create_dir_all(&out).map_err(|e| format!("cannot make {}: {e}", out.display()))?;
        run(crate::spawn::command("tar")
            .arg("-xzf")
            .arg(archive)
            .arg("-C")
            .arg(&out))?;
        let binary = find(&out)
            .ok_or_else(|| format!("{} holds no file named {PROGRAM}", archive.display()))?;
        let to = self.code_graph();
        let folder = to.parent().unwrap_or(&self.0);
        std::fs::create_dir_all(folder)
            .map_err(|e| format!("cannot make {}: {e}", folder.display()))?;
        make_executable(&binary)?;
        // A rename keeps a call that starts during the install from finding half a file.
        std::fs::rename(&binary, &to).map_err(|e| format!("cannot write {}: {e}", to.display()))
    }
}

// The SHA-256 of `file`, as `shasum` or `sha256sum` prints it.
pub(super) fn digest(file: &Path) -> Result<String, String> {
    for (program, args) in [("shasum", &["-a", "256"][..]), ("sha256sum", &[][..])] {
        let Ok(output) = crate::spawn::command(program).args(args).arg(file).output() else {
            continue;
        };
        if !output.status.success() {
            return Err(format!("{program} failed on {}", file.display()));
        }
        let text = String::from_utf8_lossy(&output.stdout);
        return text
            .split_whitespace()
            .next()
            .map(str::to_owned)
            .ok_or_else(|| format!("{program} printed no digest for {}", file.display()));
    }
    Err("neither shasum nor sha256sum is on PATH, so the download cannot be checked".to_owned())
}

// The regular file named `codebase-memory-mcp` anywhere under `dir`.
fn find(dir: &Path) -> Option<PathBuf> {
    let mut entries: Vec<_> = std::fs::read_dir(dir).ok()?.flatten().collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let kind = entry.file_type().ok()?;
        if kind.is_file() && entry.file_name() == PROGRAM {
            return Some(path);
        }
        if kind.is_dir()
            && let Some(found) = find(&path)
        {
            return Some(found);
        }
    }
    None
}

#[cfg(unix)]
fn make_executable(file: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| format!("cannot make {} executable: {e}", file.display()))
}

// Windows has no mode bits: a file runs by its extension.
#[cfg(not(unix))]
fn make_executable(_: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // A tarball holding `entry` as a script, and its digest.
    fn archive(dir: &Path, entry: &str) -> (PathBuf, String) {
        let src = dir.join("src");
        let at = src.join(entry);
        std::fs::create_dir_all(at.parent().unwrap()).unwrap();
        std::fs::write(&at, "#!/bin/sh\nexit 0\n").unwrap();
        let tarball = dir.join("release.tar.gz");
        let made = std::process::Command::new("tar")
            .arg("-czf")
            .arg(&tarball)
            .arg("-C")
            .arg(&src)
            .arg(".")
            .status()
            .unwrap();
        assert!(made.success());
        let sha = digest(&tarball).unwrap();
        (tarball, sha)
    }

    #[test]
    fn each_machine_the_release_builds_for_has_a_pinned_archive() {
        for (os, arch, name) in [
            ("macos", "aarch64", "darwin-arm64"),
            ("macos", "x86_64", "darwin-amd64"),
            ("linux", "x86_64", "linux-amd64-portable"),
            ("linux", "aarch64", "linux-arm64-portable"),
        ] {
            let asset = asset(os, arch).unwrap();
            assert_eq!(asset.name, format!("codebase-memory-mcp-{name}.tar.gz"));
            assert_eq!(asset.sha256.len(), 64, "{os} {arch}");
        }
        assert_eq!(asset("windows", "x86_64"), None);
        assert_eq!(asset("linux", "riscv64"), None);
    }

    #[cfg(unix)]
    #[test]
    fn the_server_is_unpacked_from_a_checked_archive_and_made_executable() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let (tarball, sha) = archive(dir.path(), "nested/codebase-memory-mcp");
        let tools = Tools::at(dir.path().join("tools"));
        assert!(!tools.has_code_graph());
        tools
            .unpack_code_graph(&tarball, &sha, &dir.path().join("staging"))
            .unwrap();
        assert!(tools.has_code_graph());
        let mode = std::fs::metadata(tools.code_graph()).unwrap().permissions();
        assert_eq!(mode.mode() & 0o111, 0o111);
    }

    #[test]
    fn an_archive_that_is_not_the_pinned_one_installs_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (tarball, _) = archive(dir.path(), "codebase-memory-mcp");
        let tools = Tools::at(dir.path().join("tools"));
        let err = tools
            .unpack_code_graph(&tarball, &"0".repeat(64), &dir.path().join("staging"))
            .unwrap_err();
        assert!(err.contains(RELEASE), "{err}");
        assert!(!tools.has_code_graph());
    }

    #[test]
    fn an_archive_without_the_server_installs_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (tarball, sha) = archive(dir.path(), "README.md");
        let tools = Tools::at(dir.path().join("tools"));
        let err = tools
            .unpack_code_graph(&tarball, &sha, &dir.path().join("staging"))
            .unwrap_err();
        assert!(err.contains("holds no file named"), "{err}");
        assert!(!tools.has_code_graph());
    }
}
