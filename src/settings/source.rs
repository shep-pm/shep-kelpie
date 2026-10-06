//! Where a runner's settings come from
//!
//! A project's settings are its runner sheep's `[app.dogs.kelpie]` table,
//! and kelpie's own are its `[kelpie]` section of `dogs.toml`. Kelpie's own
//! settings are all optional, so with no section they are empty.

use std::path::Path;

use super::{Settings, SettingsError};
use crate::shepherd::Tables;
use crate::webhook::KelpieSettings;

// The settings file an older kelpie read, in a project's folder and in kelpie's home.
const OLD_FILE: &str = "settings.toml";

/// A runner's settings
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loaded {
    /// The project's settings
    pub settings: Settings,
    /// Kelpie's own settings
    pub kelpie: KelpieSettings,
}

/// Reads the settings from `tables`
///
/// `home` is the maintainer's home folder, for `~/` in settings, and
/// `folder` the project's own folder under kelpie's home, which a relative
/// path in them is taken from.
///
/// # Errors
///
/// [`SettingsError`] naming the table or section that is missing or malformed.
pub fn load(
    tables: &Tables,
    sheep: &str,
    home: &Path,
    folder: &Path,
) -> Result<Loaded, SettingsError> {
    let Some(table) = &tables.project else {
        return Err(SettingsError::Unset {
            table: format!("[app.dogs.kelpie] table on the {sheep} sheep"),
            old_file: Some(folder.join(OLD_FILE)).filter(|file| file.exists()),
        });
    };
    let settings = Settings::from_table(table, sheep, home, folder)?;
    let kelpie = if tables.kelpie.trim().is_empty() {
        // An older kelpie kept its own settings beside the projects' folders.
        let old = folder.parent().map(|home| home.join(OLD_FILE));
        if let Some(file) = old.filter(|file| file.exists()) {
            return Err(SettingsError::Section {
                message: format!(
                    "there is none, and kelpie no longer reads {}: move its keys into the section",
                    file.display()
                ),
            });
        }
        KelpieSettings::default()
    } else {
        KelpieSettings::from_section(&tables.kelpie)?
    };
    Ok(Loaded { settings, kelpie })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::webhook::WebhookKind;

    const EXAMPLE: &str = include_str!("../../settings.example.toml");
    const SECTION: &str = "[webhook]\nkind = \"discord\"\nurl = \"https://discord.example/hook\"\n";
    const HOME: &str = "/home/me";
    const FOLDER: &str = "/home/me/.shep/kelpie/shep";

    fn load_from(tables: &Tables) -> Result<Loaded, SettingsError> {
        load(tables, "shep", Path::new(HOME), Path::new(FOLDER))
    }

    fn tables() -> Tables {
        let mut table = crate::test::project_table(EXAMPLE);
        table.insert("forge".into(), "shep-pm/from-table".into());
        Tables {
            project: Some(table),
            kelpie: SECTION.into(),
        }
    }

    #[test]
    fn the_table_and_the_section_are_what_a_runner_reads() {
        let loaded = load_from(&tables()).unwrap();
        assert_eq!(loaded.settings.forge.as_str(), "shep-pm/from-table");
        assert_eq!(loaded.kelpie.webhook.unwrap().kind, WebhookKind::Discord);
    }

    #[test]
    fn a_sheep_with_no_table_is_refused_naming_it() {
        let err = load_from(&Tables::default()).unwrap_err().to_string();
        assert_eq!(err, "there is no [app.dogs.kelpie] table on the shep sheep");
    }

    #[test]
    fn an_old_file_where_kelpie_used_to_read_it_is_named_when_its_table_is_unset() {
        let home = tempfile::tempdir().unwrap();
        let folder = home.path().join("shep");
        std::fs::create_dir_all(&folder).unwrap();
        let load = |tables: &Tables| load(tables, "shep", Path::new(HOME), &folder);
        let err = load(&Tables::default()).unwrap_err().to_string();
        assert_eq!(err, "there is no [app.dogs.kelpie] table on the shep sheep");

        let file = folder.join("settings.toml");
        std::fs::write(&file, "forge = \"shep-pm/shep\"\n").unwrap();
        let err = load(&Tables::default()).unwrap_err().to_string();
        assert_eq!(
            err,
            format!(
                "there is no [app.dogs.kelpie] table on the shep sheep, and kelpie no longer \
                 reads {}: move its keys into the table",
                file.display()
            )
        );

        let only_project = Tables {
            kelpie: String::new(),
            ..tables()
        };
        assert!(load(&only_project).is_ok());
        let kelpie_file = home.path().join("settings.toml");
        std::fs::write(&kelpie_file, SECTION).unwrap();
        let err = load(&only_project).unwrap_err().to_string();
        assert_eq!(
            err,
            format!(
                "the [kelpie] section of dogs.toml: there is none, and kelpie no longer reads \
                 {}: move its keys into the section",
                kelpie_file.display()
            )
        );
        assert!(load(&tables()).is_ok());
    }

    #[test]
    fn with_no_section_kelpie_s_own_settings_are_empty() {
        let only_project = Tables {
            kelpie: String::new(),
            ..tables()
        };
        let loaded = load_from(&only_project).unwrap();
        assert_eq!(loaded.kelpie, KelpieSettings::default());
    }
}
