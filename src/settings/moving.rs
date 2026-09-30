//! `shep kelpie settings move`: a project's files into their tables
//!
//! Writes the project's settings file as its runner sheep's
//! `[app.dogs.kelpie]` table, and kelpie's own file as its `[kelpie]`
//! section of `dogs.toml`, both through the shepherd. A table that is
//! already set is never overwritten, and no file is deleted.

use std::path::Path;

use shep_client::shep_core::config::DogTable;
use shep_client::shep_core::protocol::Request;
use shep_client::shep_core::protocol::request::Response;

use super::source::Files;
use super::{Settings, table_of};
use crate::shepherd::{self, DOG};
use crate::webhook::KelpieSettings;

/// Moves `files` into their tables on the shepherd at `shep_home`
///
/// Returns one line per file, saying what became of it. `home` is the
/// maintainer's home folder, for `~/` in settings.
///
/// # Errors
///
/// A message naming the file, table or shepherd that stopped the move.
/// A file moved before the error stays moved.
pub async fn move_files(
    shep_home: &Path,
    files: Files<'_>,
    home: &Path,
) -> Result<Vec<String>, String> {
    let client = shepherd::connect(shep_home)
        .await
        .map_err(|e| e.describe(shep_home))?;
    let tables = shepherd::read_tables_with(&client, files.sheep).await?;
    let mut done = Vec::new();

    let table_name = format!("the [app.dogs.kelpie] table on {}", files.sheep);
    match read(files.settings)? {
        None => done.push("not there, so nothing moved".to_owned()),
        Some(text) => {
            Settings::load(files.settings, home).map_err(|e| e.to_string())?;
            let table = table_of(&text)?;
            match &tables.project {
                Some(set) if *set == table => done.push(format!("{table_name} already holds it")),
                Some(_) => return Err(differs(files.settings, &table_name)),
                None => {
                    let request = Request::SetSheepDogSettings {
                        name: files.sheep.to_owned(),
                        dog: DOG.to_owned(),
                        table: Some(DogTable::from(table)),
                    };
                    match client.request(request).await {
                        Ok(Response::SheepDogSettingsSet { .. }) => {}
                        Ok(other) => return Err(format!("the shepherd answered {other:?}")),
                        Err(e) => return Err(format!("the shepherd refused {table_name}: {e}")),
                    }
                    done.push(format!("moved into {table_name}"));
                }
            }
        }
    }

    let section_name = "kelpie's [kelpie] section of dogs.toml";
    match read(files.kelpie_settings)? {
        None => done.push("not there, so nothing moved".to_owned()),
        Some(text) => {
            let file = KelpieSettings::load(files.kelpie_settings).map_err(|e| e.to_string())?;
            if tables.kelpie.trim().is_empty() {
                // The file's own text, so its comments move with it.
                let request = Request::SetDogConfig {
                    name: DOG.to_owned(),
                    toml: text.into(),
                };
                match client.request(request).await {
                    Ok(Response::DogConfigSet { .. }) => {}
                    Ok(other) => return Err(format!("the shepherd answered {other:?}")),
                    // shep writes a dog's section only once the dog is adopted.
                    Err(e) => {
                        return Err(format!(
                            "the shepherd refused {section_name}: {e}. Adopt kelpie first, \
                             `shep adopt /path/to/kelpie --name kelpie` then \
                             `shep disable kelpie`, and run this again"
                        ));
                    }
                }
                done.push(format!("moved into {section_name}"));
            } else if KelpieSettings::from_section(&tables.kelpie).map_err(|e| e.to_string())?
                == file
            {
                done.push(format!("{section_name} already holds it"));
            } else {
                return Err(differs(files.kelpie_settings, section_name));
            }
        }
    }
    Ok(done
        .into_iter()
        .zip([files.settings, files.kelpie_settings])
        .map(|(what, file)| format!("{}: {what}", file.display()))
        .collect())
}

// A file that is not there has nothing to move; any other failure stops.
fn read(path: &Path) -> Result<Option<String>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

fn differs(file: &Path, table: &str) -> String {
    format!(
        "{table} is already set and differs from {}, so both were left as they are. \
         Edit the table in lookout, or remove it there to move the file.",
        file.display()
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::time::Duration;

    use serde_json::{Map, Value};
    use shep_client::shep_core::protocol::Envelope;
    use shep_client::testing::{fake_daemon_answering_with_ack, sample_ack};
    use tokio::sync::mpsc::UnboundedReceiver;

    use super::*;
    use crate::shepherd::SHEP_VERSION;

    // Bounds every call against a fake shepherd, so a hang fails by name.
    const PATIENCE: Duration = Duration::from_secs(10);
    const EXAMPLE: &str = include_str!("../../settings.example.toml");
    const KELPIE_FILE: &str =
        "# kelpie's own\n[webhook]\nkind = \"ntfy\"\nurl = \"https://ntfy.example/t\"\n";

    // Kelpie's home with both old files, and a shepherd holding `project`
    // and `kelpie` as the shep sheep's table and kelpie's section.
    struct Scene {
        home: tempfile::TempDir,
        sent: UnboundedReceiver<Envelope>,
    }

    impl Scene {
        async fn new(project: Option<Map<String, Value>>, kelpie: &str) -> Self {
            let home = tempfile::tempdir().unwrap();
            let scene_home = home.path();
            std::fs::create_dir_all(scene_home.join("projects/shep")).unwrap();
            std::fs::create_dir(scene_home.join("run")).unwrap();
            let file = toml::to_string(&crate::test::project_table(EXAMPLE)).unwrap();
            std::fs::write(scene_home.join("projects/shep/settings.toml"), file).unwrap();
            std::fs::write(scene_home.join("settings.toml"), KELPIE_FILE).unwrap();
            let mut ack = sample_ack();
            ack.daemon_version = SHEP_VERSION.into();
            let kelpie = kelpie.to_owned();
            let socket = scene_home.join("run/shep.sock");
            let sent = fake_daemon_answering_with_ack(&socket, ack, move |request| match request {
                Request::DogSheepSettings { .. } => Response::DogSheepSettings {
                    tables: project
                        .clone()
                        .map(|t| ("shep".to_owned(), DogTable::from(t)))
                        .into_iter()
                        .collect::<BTreeMap<_, _>>(),
                },
                Request::DogConfig { .. } => Response::DogSection {
                    toml: kelpie.clone().into(),
                },
                Request::SetSheepDogSettings { name, dog, .. } => Response::SheepDogSettingsSet {
                    name: name.clone(),
                    dog: dog.clone(),
                },
                Request::SetDogConfig { name, .. } => Response::DogConfigSet { name: name.clone() },
                other => panic!("kelpie asked {other:?}"),
            })
            .await;
            Self { home, sent }
        }

        fn file(&self, path: &str) -> PathBuf {
            self.home.path().join(path)
        }

        async fn move_in_time(&self) -> Result<Vec<String>, String> {
            let (settings, kelpie_settings) = (
                self.file("projects/shep/settings.toml"),
                self.file("settings.toml"),
            );
            let files = Files {
                project: "shep",
                sheep: "shep",
                settings: &settings,
                kelpie_settings: &kelpie_settings,
            };
            let home = Path::new("/home/me");
            tokio::time::timeout(PATIENCE, move_files(self.home.path(), files, home))
                .await
                .expect("the move neither ended nor failed in time")
        }

        // The writes the shepherd was sent, in order.
        fn writes(&mut self) -> Vec<Request> {
            std::iter::from_fn(|| self.sent.try_recv().ok())
                .map(|e| e.body)
                .filter(|r| {
                    matches!(
                        r,
                        Request::SetSheepDogSettings { .. } | Request::SetDogConfig { .. }
                    )
                })
                .collect()
        }
    }

    // Real sockets under a real clock: a paused one would time out the
    // handshake while the fake's socket is merely waiting.
    #[tokio::test]
    async fn the_move_writes_an_equal_table_and_the_file_s_own_section() {
        let mut scene = Scene::new(None, "").await;
        let lines = scene.move_in_time().await.unwrap();
        let [project, kelpie] = scene.writes().try_into().unwrap();
        let Request::SetSheepDogSettings {
            name,
            dog,
            table: Some(table),
        } = project
        else {
            panic!("{project:?}");
        };
        assert_eq!((name.as_str(), dog.as_str()), ("shep", "kelpie"));
        assert_eq!(table.as_map(), &crate::test::project_table(EXAMPLE));
        let Request::SetDogConfig { name, toml } = kelpie else {
            panic!("{kelpie:?}");
        };
        assert_eq!((name.as_str(), toml.as_str()), ("kelpie", KELPIE_FILE));
        assert!(
            lines[0].ends_with("moved into the [app.dogs.kelpie] table on shep"),
            "{lines:?}"
        );
        assert!(scene.file("projects/shep/settings.toml").exists());
        assert!(scene.file("settings.toml").exists());
    }

    #[tokio::test]
    async fn a_file_that_is_not_there_moves_nothing() {
        let mut scene = Scene::new(None, "").await;
        std::fs::remove_file(scene.file("settings.toml")).unwrap();
        let lines = scene.move_in_time().await.unwrap();
        assert!(
            lines[1].ends_with("settings.toml: not there, so nothing moved"),
            "{lines:?}"
        );
        let writes = scene.writes();
        assert!(
            matches!(writes.as_slice(), [Request::SetSheepDogSettings { .. }]),
            "{writes:?}"
        );
    }

    #[tokio::test]
    async fn a_second_move_changes_nothing() {
        let table = crate::test::project_table(EXAMPLE);
        let mut scene = Scene::new(Some(table), KELPIE_FILE).await;
        let lines = scene.move_in_time().await.unwrap();
        assert_eq!(scene.writes(), []);
        assert!(
            lines.iter().all(|l| l.ends_with("already holds it")),
            "{lines:?}"
        );
    }

    #[tokio::test]
    async fn a_table_already_set_otherwise_is_left_alone() {
        let mut table = crate::test::project_table(EXAMPLE);
        table.insert("ci".into(), Value::Bool(false));
        let mut scene = Scene::new(Some(table), "").await;
        let err = scene.move_in_time().await.unwrap_err();
        assert!(
            err.starts_with("the [app.dogs.kelpie] table on shep is already set"),
            "{err}"
        );
        assert_eq!(scene.writes(), []);
    }

    // The project's table moves first, so it stays moved when kelpie's stops.
    #[tokio::test]
    async fn a_section_already_set_otherwise_is_left_alone_after_the_table_moves() {
        let other = "[webhook]\nkind = \"discord\"\nurl = \"https://discord.example/h\"\n";
        let mut scene = Scene::new(None, other).await;
        let err = scene.move_in_time().await.unwrap_err();
        assert!(
            err.starts_with("kelpie's [kelpie] section of dogs.toml is already set"),
            "{err}"
        );
        assert!(err.contains("both were left as they are"), "{err}");
        let writes = scene.writes();
        assert!(
            matches!(writes.as_slice(), [Request::SetSheepDogSettings { .. }]),
            "{writes:?}"
        );
    }
}
