//! A kelpie build's own account of itself: `shep-kelpie version --json`

use std::path::Path;
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

use crate::shepherd::SHEP_VERSION;

/// What `version --json` prints: the build's version and the shep version it was made with
///
/// Other keys in the answer are ignored, so a later build may add some.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Build {
    /// Kelpie's own version
    pub kelpie: String,
    /// The shep version kelpie was made with, which the shepherd's minor must match
    pub shep: String,
}

impl Build {
    /// This build
    pub fn this() -> Self {
        Self {
            kelpie: env!("CARGO_PKG_VERSION").to_owned(),
            shep: SHEP_VERSION.to_owned(),
        }
    }

    /// The build at `binary`, as it answers `version --json`
    ///
    /// # Errors
    ///
    /// A message when `binary` does not run, or does not answer with a JSON
    /// object naming both versions.
    pub fn of(binary: &Path) -> Result<Self, String> {
        let output = Command::new(binary)
            .args(["version", "--json"])
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("cannot run {}: {e}", binary.display()))?;
        if !output.status.success() {
            return Err(format!(
                "{} exited {} on `version --json`: {}",
                binary.display(),
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        serde_json::from_slice(&output.stdout).map_err(|e| {
            format!(
                "{} did not answer `version --json` with its kelpie and shep versions: {e}",
                binary.display()
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_build_names_its_kelpie_and_the_shep_it_is_made_with() {
        let json = serde_json::to_value(Build::this()).unwrap();
        assert_eq!(json["kelpie"], env!("CARGO_PKG_VERSION"));
        assert_eq!(json["shep"], SHEP_VERSION);
    }

    #[test]
    fn an_answer_with_more_keys_still_reads() {
        let build: Build =
            serde_json::from_str(r#"{ "kelpie": "0.3.0", "shep": "0.11.0", "commit": "abc" }"#)
                .unwrap();
        assert_eq!(
            (build.kelpie.as_str(), build.shep.as_str()),
            ("0.3.0", "0.11.0")
        );
    }
}
