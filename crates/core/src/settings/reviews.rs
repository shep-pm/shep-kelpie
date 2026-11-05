//! `[app.dogs.kelpie.reviews]`: how many reviewers read each work item

use std::num::NonZeroU32;

use schemars::JsonSchema;
use serde::Deserialize;

use super::NonBlank;

/// Two reviewers, the second shown the first's fix
const NEED: u32 = 2;

/// The files a docs-only change touches, when the project names none
const LIGHT_PATHS: [&str; 2] = ["**/*.md", "docs/**"];

/// The most lines a change the project manager tagged light may alter
const LIGHT_MAX_LINES: u32 = 10;

/// How the review pass is sized
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Reviews {
    /// How many reviewers read each work item, each once and in series, so a
    /// later one sees the earlier one's fix. Each is the agent board's pick
    /// at that moment. At least 1. 2 when absent.
    #[serde(default = "default_need")]
    pub need: NonZeroU32,
    /// How many reviewers read a work item that changes only docs, never more
    /// than `need`. `need` when absent.
    #[serde(default)]
    pub light: Option<NonZeroU32>,
    /// Globs of the files a docs-only change touches, so a pull request that
    /// changes only these gets `light` reviewers. Markdown files and `docs/`
    /// when absent, and none when empty.
    #[serde(default = "default_light_paths")]
    pub light_paths: Vec<NonBlank>,
    /// Globs of the files that always get `need` reviewers, even beside files
    /// that match `light_paths`. None when absent.
    #[serde(default)]
    pub full_paths: Vec<NonBlank>,
    /// Whether a small change the project manager tagged light gets the
    /// `light` count of reviewers too. Off when absent.
    #[serde(default)]
    pub pm_lightens: bool,
    /// The most lines, added plus deleted, a change may alter and still get
    /// the `light` count from the project manager's tag. At least 1. 10 when
    /// absent.
    #[serde(default = "default_light_max_lines")]
    pub light_max_lines: NonZeroU32,
}

impl Reviews {
    /// How many reviewers read a work item that changes only docs: `light`,
    /// or `need` when `light` is absent or above it
    pub fn light_count(&self) -> NonZeroU32 {
        self.light.map_or(self.need, |light| light.min(self.need))
    }
}

impl Default for Reviews {
    fn default() -> Self {
        Self {
            need: default_need(),
            light: None,
            light_paths: default_light_paths(),
            full_paths: Vec::new(),
            pm_lightens: false,
            light_max_lines: default_light_max_lines(),
        }
    }
}

fn default_need() -> NonZeroU32 {
    NonZeroU32::new(NEED).expect("two is not zero")
}

fn default_light_max_lines() -> NonZeroU32 {
    NonZeroU32::new(LIGHT_MAX_LINES).expect("ten is not zero")
}

fn default_light_paths() -> Vec<NonBlank> {
    LIGHT_PATHS
        .iter()
        .map(|glob| NonBlank::try_from((*glob).to_owned()).expect("a glob is not blank"))
        .collect()
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

    fn paths(list: &[crate::settings::NonBlank]) -> Vec<&str> {
        list.iter().map(|g| g.as_str()).collect()
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

    #[test]
    fn light_reads_as_need_when_absent_and_is_capped_at_need() {
        let absent = parse("[app.dogs.kelpie.reviews]\nneed = 3\n").unwrap();
        assert_eq!(absent.reviews.light, None);
        assert_eq!(absent.reviews.light_count().get(), 3);

        let one = parse("[app.dogs.kelpie.reviews]\nneed = 3\nlight = 1\n").unwrap();
        assert_eq!(one.reviews.light_count().get(), 1);

        let equal = parse("[app.dogs.kelpie.reviews]\nneed = 2\nlight = 2\n").unwrap();
        assert_eq!(equal.reviews.light_count().get(), 2);

        let above = parse("[app.dogs.kelpie.reviews]\nneed = 2\nlight = 5\n").unwrap();
        assert_eq!(
            above.reviews.light_count().get(),
            2,
            "a light above need reads as need"
        );
    }

    #[test]
    fn light_paths_default_to_markdown_and_docs_and_full_paths_to_none() {
        let bare = parse("[app.dogs.kelpie.reviews]\n").unwrap();
        assert_eq!(paths(&bare.reviews.light_paths), ["**/*.md", "docs/**"]);
        assert!(bare.reviews.full_paths.is_empty());

        let set = parse(
            "[app.dogs.kelpie.reviews]\nlight_paths = [\"**/*.rst\"]\nfull_paths = [\"docs/adr/**\"]\n",
        )
        .unwrap();
        assert_eq!(paths(&set.reviews.light_paths), ["**/*.rst"]);
        assert_eq!(paths(&set.reviews.full_paths), ["docs/adr/**"]);

        let off = parse("[app.dogs.kelpie.reviews]\nlight_paths = []\n").unwrap();
        assert!(
            off.reviews.light_paths.is_empty(),
            "[] turns path lightening off"
        );
    }

    #[test]
    fn a_zero_light_is_refused() {
        let err = parse("[app.dogs.kelpie.reviews]\nlight = 0\n").unwrap_err();
        assert!(err.contains("reviews") && err.contains("light"), "{err}");
    }

    #[test]
    fn pm_lightens_is_off_and_light_max_lines_is_ten_when_absent_and_zero_lines_is_refused() {
        let bare = parse("[app.dogs.kelpie.reviews]\n").unwrap();
        assert!(!bare.reviews.pm_lightens);
        assert_eq!(bare.reviews.light_max_lines.get(), 10);

        let set =
            parse("[app.dogs.kelpie.reviews]\npm_lightens = true\nlight_max_lines = 25\n").unwrap();
        assert!(set.reviews.pm_lightens);
        assert_eq!(set.reviews.light_max_lines.get(), 25);

        let err = parse("[app.dogs.kelpie.reviews]\nlight_max_lines = 0\n").unwrap_err();
        assert!(
            err.contains("reviews") && err.contains("light_max_lines"),
            "{err}"
        );
    }
}
