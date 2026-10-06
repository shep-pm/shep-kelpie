//! Settings a removed feature left behind, refused by name
//!
//! A key serde would call unknown is refused with the same words as any other
//! stale key, but this one also says why and what to do: delete it when its
//! feature is gone, or move it to what replaced it. Every such key a table
//! carries is named at once, so one edit clears them all.

/// A setting that no longer exists, and why
#[derive(Debug, Clone, Copy)]
pub(crate) struct Removed {
    /// The dotted key
    pub key: &'static str,
    /// Why it went, as the rest of "because ..."
    pub because: &'static str,
    /// What to do instead, such as [`DELETE`]
    pub fix: &'static str,
}

/// The fix for a setting whose feature is gone
pub(crate) const DELETE: &str = "delete it";

/// Refuses `text` when it sets any of `keys`
///
/// A text that is not TOML passes, so the real parser reports it. A table
/// named along with a key inside it is named by that key alone.
///
/// # Errors
///
/// A message naming each of `keys` that `text` sets, in `keys`' order, why
/// it went, and what to do instead.
pub(crate) fn refuse(text: &str, keys: &[Removed]) -> Result<(), String> {
    let Ok(table) = text.parse::<toml::Table>() else {
        return Ok(());
    };
    let set: Vec<&Removed> = keys.iter().filter(|r| sets(&table, r.key)).collect();
    let inner = |outer: &str| {
        let prefix = format!("{outer}.");
        set.iter().any(|r| r.key.starts_with(&prefix))
    };
    let named: Vec<&&Removed> = set.iter().filter(|r| !inner(r.key)).collect();
    let line = |r: &Removed| format!("`{}`, because {}: {}", r.key, r.because, r.fix);
    match named.as_slice() {
        [] => Ok(()),
        [one] => Err(format!(
            "`{}` is no longer a setting, because {}: {}",
            one.key, one.because, one.fix
        )),
        several => {
            let lines: Vec<String> = several.iter().map(|r| format!("- {}", line(r))).collect();
            Err(format!(
                "these are no longer settings:\n{}",
                lines.join("\n")
            ))
        }
    }
}

fn sets(table: &toml::Table, key: &str) -> bool {
    match key.split_once('.') {
        Some((head, rest)) => table
            .get(head)
            .and_then(toml::Value::as_table)
            .is_some_and(|inner| sets(inner, rest)),
        None => table.contains_key(key),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEYS: [Removed; 2] = [
        Removed {
            key: "ruling_channels",
            because: "the relay is gone",
            fix: DELETE,
        },
        Removed {
            key: "models.planner",
            because: "the planning call is gone",
            fix: "name nothing",
        },
    ];

    #[test]
    fn a_dotted_key_is_found_in_its_table_and_nowhere_else() {
        let err = refuse("[models.planner]\nmodel = \"m\"\n", &KEYS).unwrap_err();
        assert_eq!(
            err,
            "`models.planner` is no longer a setting, because the planning call is gone: \
             name nothing"
        );
        let err = refuse("ruling_channels = [\"relay\"]\n", &KEYS).unwrap_err();
        assert_eq!(
            err,
            "`ruling_channels` is no longer a setting, because the relay is gone: delete it"
        );
        assert_eq!(refuse("[models.worker]\nplanner = 1\n", &KEYS), Ok(()));
        assert_eq!(refuse("not toml [", &KEYS), Ok(()));
    }

    #[test]
    fn every_removed_key_a_table_carries_is_named_at_once() {
        let err = refuse(
            "ruling_channels = [\"relay\"]\n[models.planner]\nmodel = \"m\"\n",
            &KEYS,
        )
        .unwrap_err();
        assert_eq!(
            err,
            "these are no longer settings:\n\
             - `ruling_channels`, because the relay is gone: delete it\n\
             - `models.planner`, because the planning call is gone: name nothing"
        );
    }
}
