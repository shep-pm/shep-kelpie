//! `[app.dogs.kelpie.reviews]`: how many reviewers read each work item

use std::num::NonZeroU32;

use schemars::JsonSchema;
use serde::Deserialize;

/// Two reviewers, the second shown the first's fix
const NEED: u32 = 2;

/// How the review pass is sized
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Reviews {
    /// How many reviewers read each work item, each once and in series, so a
    /// later one sees the earlier one's fix. Each is the agent board's pick
    /// at that moment. At least 1. 2 when absent.
    #[serde(default = "default_need")]
    pub need: NonZeroU32,
}

impl Default for Reviews {
    fn default() -> Self {
        Self {
            need: default_need(),
        }
    }
}

fn default_need() -> NonZeroU32 {
    NonZeroU32::new(NEED).expect("two is not zero")
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::super::Settings;

    const EXAMPLE: &str = include_str!("../../../../settings.example.toml");

    fn parse(extra: &str) -> Result<Settings, String> {
        let table = crate::test::project_table(&format!("{EXAMPLE}\n{extra}"));
        Settings::from_table(&table, "shep", Path::new("/home/me"), Path::new("/p"))
            .map_err(|e| e.to_string())
    }

    #[test]
    fn need_is_two_when_absent_and_zero_is_refused() {
        assert_eq!(parse("").unwrap().reviews.need.get(), 2);
        let empty = parse("[app.dogs.kelpie.reviews]\n").unwrap();
        assert_eq!(empty.reviews.need.get(), 2);
        let one = parse("[app.dogs.kelpie.reviews]\nneed = 1\n").unwrap();
        assert_eq!(one.reviews.need.get(), 1);

        let err = parse("[app.dogs.kelpie.reviews]\nneed = 0\n").unwrap_err();
        assert!(err.contains("reviews") && err.contains("need"), "{err}");
    }
}
