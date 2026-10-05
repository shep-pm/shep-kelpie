//! Settings a removed feature left behind, refused by name
//!
//! A key serde would call unknown is refused with the same words as any other
//! stale key, but this one also says why: its feature is gone, so the setting
//! has nothing left to do and is deleted rather than ignored.

/// A setting that no longer exists, and why
#[derive(Debug, Clone, Copy)]
pub(crate) struct Removed {
    /// The dotted key
    pub key: &'static str,
    /// Why it went, as the rest of "because ..."
    pub because: &'static str,
}

/// Refuses `text` when it sets one of `keys`
///
/// A text that is not TOML passes, so the real parser reports it.
///
/// # Errors
///
/// A message naming the first of `keys` that `text` sets, why it went, and
/// saying to delete it.
pub(crate) fn refuse(text: &str, keys: &[Removed]) -> Result<(), String> {
    let Ok(table) = text.parse::<toml::Table>() else {
        return Ok(());
    };
    match keys.iter().find(|removed| sets(&table, removed.key)) {
        Some(Removed { key, because }) => Err(format!(
            "`{key}` is no longer a setting, because {because}: delete it"
        )),
        None => Ok(()),
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
        },
        Removed {
            key: "models.planner",
            because: "the planning call is gone",
        },
    ];

    #[test]
    fn a_dotted_key_is_found_in_its_table_and_nowhere_else() {
        let err = refuse("[models.planner]\nmodel = \"m\"\n", &KEYS).unwrap_err();
        assert_eq!(
            err,
            "`models.planner` is no longer a setting, because the planning call is gone: \
             delete it"
        );
        let err = refuse("ruling_channels = [\"relay\"]\n", &KEYS).unwrap_err();
        assert_eq!(
            err,
            "`ruling_channels` is no longer a setting, because the relay is gone: delete it"
        );
        assert_eq!(refuse("[models.worker]\nplanner = 1\n", &KEYS), Ok(()));
        assert_eq!(refuse("not toml [", &KEYS), Ok(()));
    }
}
