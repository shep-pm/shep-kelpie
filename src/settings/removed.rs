//! Settings the relay's removal left behind, refused by name
//!
//! A key serde would call unknown is refused with the same words as any other
//! stale key, but this one also says why: the relay is gone, so the setting
//! has nothing left to do and is deleted rather than ignored.

/// Refuses `text` when it sets one of `keys`, each a dotted key
///
/// A text that is not TOML passes, so the real parser reports it.
///
/// # Errors
///
/// A message naming the first of `keys` that `text` sets and saying to
/// delete it.
pub(crate) fn refuse(text: &str, keys: &[&str]) -> Result<(), String> {
    let Ok(table) = text.parse::<toml::Table>() else {
        return Ok(());
    };
    match keys.iter().find(|key| sets(&table, key)) {
        Some(key) => Err(format!(
            "`{key}` is no longer a setting, because the relay is gone: delete it"
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

    #[test]
    fn a_dotted_key_is_found_in_its_table_and_nowhere_else() {
        let keys = ["ruling_channels", "models.relay"];
        let err = refuse("[models.relay]\nmodel = \"m\"\n", &keys).unwrap_err();
        assert!(
            err.starts_with("`models.relay` is no longer a setting"),
            "{err}"
        );
        assert!(err.ends_with("delete it"), "{err}");
        let err = refuse("ruling_channels = [\"relay\"]\n", &keys).unwrap_err();
        assert!(err.starts_with("`ruling_channels` is no longer"), "{err}");
        assert_eq!(refuse("[models.worker]\nrelay = 1\n", &keys), Ok(()));
        assert_eq!(refuse("not toml [", &keys), Ok(()));
    }
}
