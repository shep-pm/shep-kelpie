//! Where a runner's settings come from
//!
//! A project's settings are its runner sheep's `[app.dogs.kelpie]` table,
//! and kelpie's own are its `[kelpie]` section of `dogs.toml`. A project set
//! up before those existed has files under kelpie's home instead. A file
//! still loads while its table is unset, with a notice naming the command
//! that moves it. Kelpie's own settings are all optional, so with neither
//! a section nor a file they are empty. Nothing here deletes a file.

use std::path::Path;

use super::{Settings, SettingsError};
use crate::shepherd::Tables;
use crate::webhook::KelpieSettings;

/// The files a project had before its tables, and whose they are
#[derive(Debug, Clone, Copy)]
pub struct Files<'a> {
    /// The project's name, which its runner is started with
    pub project: &'a str,
    /// The runner's sheep, whose table holds the project's settings
    pub sheep: &'a str,
    /// `<kelpie home>/projects/<project>/settings.toml`
    pub settings: &'a Path,
    /// `<kelpie home>/settings.toml`
    pub kelpie_settings: &'a Path,
}

impl Files<'_> {
    /// The command that moves both files into their tables
    pub fn move_command(&self) -> String {
        if self.sheep == self.project {
            format!("kelpie settings move {}", self.project)
        } else {
            format!("kelpie settings move {} {}", self.project, self.sheep)
        }
    }
}

/// A runner's settings, and a notice for each one still read from a file
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loaded {
    /// The project's settings
    pub settings: Settings,
    /// Kelpie's own settings
    pub kelpie: KelpieSettings,
    /// One line per file read in place of a table
    pub notices: Vec<String>,
}

/// Reads the settings from `tables`, or from `files` for a table left unset
///
/// `home` is the maintainer's home folder, for `~/` in settings.
///
/// # Errors
///
/// [`SettingsError`] naming the table or file that is missing or malformed.
pub fn load(tables: &Tables, files: Files<'_>, home: &Path) -> Result<Loaded, SettingsError> {
    let folder = files.settings.parent().unwrap_or(Path::new("/"));
    let mut notices = Vec::new();
    let mut from_file = |what: &str, path: &Path| {
        notices.push(format!(
            "{what} come from {}, as there is no table for them. `{}` moves them into it.",
            path.display(),
            files.move_command(),
        ));
    };
    let settings = match &tables.project {
        Some(table) => Settings::from_table(table, files.sheep, home, folder)?,
        None => {
            let table = format!("[app.dogs.kelpie] table on the {} sheep", files.sheep);
            let settings = from_file_or_unset(files.settings, table, |p| Settings::load(p, home))?;
            from_file(&format!("{}'s settings", files.project), files.settings);
            settings
        }
    };
    let kelpie = if !tables.kelpie.trim().is_empty() {
        KelpieSettings::from_section(&tables.kelpie)?
    } else if files.kelpie_settings.exists() {
        let kelpie = KelpieSettings::load(files.kelpie_settings)?;
        from_file("kelpie's own settings", files.kelpie_settings);
        kelpie
    } else {
        KelpieSettings::default()
    };
    Ok(Loaded {
        settings,
        kelpie,
        notices,
    })
}

// A file that is not there is named beside the table it stands in for.
fn from_file_or_unset(
    path: &Path,
    table: String,
    load: impl FnOnce(&Path) -> Result<Settings, SettingsError>,
) -> Result<Settings, SettingsError> {
    load(path).map_err(|e| match e {
        SettingsError::Read {
            path,
            kind: std::io::ErrorKind::NotFound,
        } => SettingsError::Unset { table, path },
        other => other,
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::webhook::WebhookKind;

    const EXAMPLE: &str = include_str!("../../settings.example.toml");
    const SECTION: &str = "[webhook]\nkind = \"discord\"\nurl = \"https://discord.example/hook\"\n";
    const KELPIE_FILE: &str = "[webhook]\nkind = \"ntfy\"\nurl = \"https://ntfy.example/t\"\n";

    // Kelpie's home with the files a project had before its tables.
    struct Home {
        dir: tempfile::TempDir,
    }

    impl Home {
        fn with_files() -> Self {
            let home = Self::empty();
            let table = crate::test::project_table(EXAMPLE);
            std::fs::create_dir_all(home.settings().parent().unwrap()).unwrap();
            std::fs::write(home.settings(), toml::to_string(&table).unwrap()).unwrap();
            std::fs::write(home.kelpie_settings(), KELPIE_FILE).unwrap();
            home
        }

        fn empty() -> Self {
            Self {
                dir: tempfile::tempdir().unwrap(),
            }
        }

        fn settings(&self) -> PathBuf {
            self.dir.path().join("projects/shep/settings.toml")
        }

        fn kelpie_settings(&self) -> PathBuf {
            self.dir.path().join("settings.toml")
        }

        fn load(&self, tables: &Tables, sheep: &str) -> Result<Loaded, SettingsError> {
            let (settings, kelpie_settings) = (self.settings(), self.kelpie_settings());
            let files = Files {
                project: "shep",
                sheep,
                settings: &settings,
                kelpie_settings: &kelpie_settings,
            };
            load(tables, files, Path::new("/home/me"))
        }
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
    fn the_tables_win_over_the_files_and_say_nothing() {
        let loaded = Home::with_files().load(&tables(), "shep").unwrap();
        assert_eq!(loaded.settings.forge.as_str(), "shep-pm/from-table");
        assert_eq!(loaded.kelpie.webhook.unwrap().kind, WebhookKind::Discord);
        assert!(loaded.notices.is_empty(), "{:?}", loaded.notices);
    }

    #[test]
    fn with_no_tables_the_old_files_load_with_a_notice_naming_the_move() {
        let home = Home::with_files();
        let loaded = home.load(&Tables::default(), "shep").unwrap();
        assert_eq!(loaded.settings.forge.as_str(), "shep-pm/shep");
        assert_eq!(loaded.kelpie.webhook.unwrap().kind, WebhookKind::Ntfy);
        let [project, kelpie] = loaded.notices.try_into().unwrap();
        assert_eq!(
            project,
            format!(
                "shep's settings come from {}, as there is no table for them. \
                 `kelpie settings move shep` moves them into it.",
                home.settings().display()
            )
        );
        assert!(
            kelpie.starts_with("kelpie's own settings come from "),
            "{kelpie}"
        );
        assert!(kelpie.contains("`kelpie settings move shep`"), "{kelpie}");
        assert!(home.settings().exists() && home.kelpie_settings().exists());
    }

    #[test]
    fn a_sheep_named_apart_from_its_project_is_named_in_the_move() {
        let loaded = Home::with_files()
            .load(&Tables::default(), "shep-runner")
            .unwrap();
        assert!(
            loaded.notices[0].contains("`kelpie settings move shep shep-runner`"),
            "{:?}",
            loaded.notices
        );
    }

    #[test]
    fn with_neither_a_table_nor_a_file_a_project_is_refused_and_kelpie_is_empty() {
        let home = Home::empty();
        let err = home
            .load(&Tables::default(), "shep")
            .unwrap_err()
            .to_string();
        assert_eq!(
            err,
            format!(
                "there is no [app.dogs.kelpie] table on the shep sheep, and no {}",
                home.settings().display()
            )
        );
        let only_project = Tables {
            kelpie: String::new(),
            ..tables()
        };
        let loaded = home.load(&only_project, "shep").unwrap();
        assert_eq!(loaded.kelpie, KelpieSettings::default());
        assert!(loaded.notices.is_empty(), "{:?}", loaded.notices);
    }
}
