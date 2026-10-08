//! Kelpie's own tools, under `<kelpie home>/tools`
//!
//! `shep kelpie tools install` fills the folder with the sandbox runtime every
//! agent runs in, at the version [`PACKAGE`] pins.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Kelpie's own copy of the sandbox runtime, under `<kelpie home>/tools`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tools(PathBuf);

/// The `package.json` kelpie installs its tools from
pub const PACKAGE: &str = include_str!("tools/package.json");

impl Tools {
    /// The tools under `kelpie_home`
    pub fn under(kelpie_home: &Path) -> Self {
        Self::at(kelpie_home.join("tools"))
    }

    /// The tools in `dir` itself
    pub fn at(dir: PathBuf) -> Self {
        Self(dir)
    }

    /// Installs the pinned packages with `npm`
    ///
    /// # Errors
    ///
    /// The step that failed, and why.
    pub fn install(&self) -> Result<(), String> {
        std::fs::create_dir_all(&self.0)
            .and_then(|()| std::fs::write(self.0.join("package.json"), PACKAGE))
            .map_err(|e| format!("cannot write {}: {e}", self.0.display()))?;
        let npm = ["install", "--no-audit", "--no-fund"];
        run(crate::spawn::command("npm").args(npm).current_dir(&self.0))
    }

    /// The folder itself
    #[inline]
    pub fn dir(&self) -> &Path {
        &self.0
    }

    /// The sandbox runtime's command line, `srt`
    pub fn sandbox(&self) -> PathBuf {
        self.0
            .join("node_modules/@anthropic-ai/sandbox-runtime/dist/cli.js")
    }
}

// Runs `command` with its output passed through, as an install shows progress.
fn run(command: &mut Command) -> Result<(), String> {
    let program = command.get_program().to_string_lossy().into_owned();
    let status = command
        .status()
        .map_err(|e| format!("cannot run {program}: {e}"))?;
    if !status.success() {
        return Err(format!("{program} failed: {status}"));
    }
    Ok(())
}
