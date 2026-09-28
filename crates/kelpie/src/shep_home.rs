//! `SHEP_HOME`, which kelpie's shepherd does not pass to a sheep
//!
//! A sheep starts with only `HOME`, `LANG`, `PATH`, `USER` and its `SHEP_*`
//! variables, so a runner or the dog reads `SHEP_HOME` only when its flock
//! entry sets it in `env`. Both refuse to start without it, rather than
//! fall back to a shepherd that is not kelpie's.

use std::ffi::OsString;
use std::path::PathBuf;

/// What to do about a missing `SHEP_HOME` in a sheep
pub const FLOCKFILE_FIX: &str = "add `env = { SHEP_HOME = \"/path/to/kelpie/shep\" }` \
     to this sheep's entry in the Flockfile, the path being kelpie's shepherd (`~/.kelpie/shep`)";

/// What to do about a missing `SHEP_HOME` in a relay command
pub const RELAY_FIX: &str =
    "the relay's settings set it in their `env` block, so this command did not run from the relay";

/// The shepherd's home, from the `SHEP_HOME` variable
///
/// # Errors
///
/// A message naming `fix` when the variable is unset, empty or not an
/// absolute path: shep expands `~` in a script or folder, never in `env`.
pub fn required(fix: &str) -> Result<PathBuf, String> {
    from(std::env::var_os("SHEP_HOME"), fix)
}

fn from(value: Option<OsString>, fix: &str) -> Result<PathBuf, String> {
    let Some(value) = value.filter(|v| !v.is_empty()) else {
        return Err(format!("SHEP_HOME is not set: {fix}"));
    };
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        return Err(format!(
            "SHEP_HOME is {} and must be an absolute path, since shep does not expand `~` in env: {fix}",
            path.display()
        ));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_set_value_is_the_shepherds_home() {
        assert_eq!(
            from(Some("/k/shep".into()), "x"),
            Ok(PathBuf::from("/k/shep"))
        );
    }

    #[test]
    fn a_tilde_or_relative_path_is_refused_by_name() {
        for value in ["~/.kelpie/shep", "shep", "./shep"] {
            let error = from(Some(value.into()), "add the env line").unwrap_err();
            assert!(error.contains(value), "{error}");
            assert!(error.contains("absolute path"), "{error}");
            assert!(error.contains("add the env line"), "{error}");
        }
    }

    #[test]
    fn an_unset_or_empty_value_names_the_fix() {
        for value in [None, Some(OsString::new())] {
            assert_eq!(
                from(value, "add the env line"),
                Err("SHEP_HOME is not set: add the env line".into())
            );
        }
    }
}
