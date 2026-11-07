//! Every project's settings on this shepherd, written into the project
//! manager's folder before it wakes, since `pm_reach` keeps it out of
//! kelpie's homes
//!
//! Each file is a copy of one project's table as the last look read it, with
//! what could be a secret dropped (`shown_table`) and every text that names
//! this machine withheld, as the board's text is. Kelpie's own section is
//! never here, and kelpie reads none of it back. The folder is emptied first,
//! so a project removed leaves nothing.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Map, Value};

use crate::home::ProjectName;
use crate::runner::Runner;
use crate::settings::{shown_table, toml_text};

/// The folder in the project manager's that holds the copies
const SETTINGS: &str = "settings";

impl Runner {
    /// Takes `tables` as every project's, from the next wake
    ///
    /// They are each project's `[app.dogs.kelpie]` table, this project's
    /// included, as the shepherd holds them.
    pub fn set_peer_tables(&mut self, tables: BTreeMap<String, Map<String, Value>>) {
        self.peer_tables = tables;
    }

    /// The tables [`Self::set_peer_tables`] last took
    pub fn peer_tables(&self) -> &BTreeMap<String, Map<String, Value>> {
        &self.peer_tables
    }

    /// Writes `settings/<project>.toml` in `folder` for each project, and
    /// returns the board's line naming them
    ///
    /// A copy that could not be written is noted there and in the log.
    pub(super) fn pm_settings(&mut self, folder: &Path) -> String {
        let dir = folder.join(SETTINGS);
        let cleared = match std::fs::remove_dir_all(&dir) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => std::fs::create_dir_all(&dir),
        };
        let mut failed = Vec::new();
        match cleared {
            Err(e) => failed.push((
                "every project".to_owned(),
                format!("cannot clear it: {}", e.kind()),
            )),
            Ok(()) => {
                for (name, table) in &self.peer_tables {
                    if let Err(why) = self.write_copy(&dir, name, table) {
                        failed.push((format!("`{name}`"), why));
                    }
                }
            }
        }
        let mut line = format!(
            "- Every project's settings on this shepherd, in `{SETTINGS}/<project>.toml`: a copy \
             with secrets and this machine's paths left out, which kelpie never reads back, so \
             an edit changes nothing"
        );
        for (whose, why) in failed {
            line.push_str(&format!("; no settings for {whose}: {why}"));
            self.notes
                .push(format!("the project manager's settings of {whose}: {why}"));
        }
        line.push('\n');
        line
    }

    fn write_copy(&self, dir: &Path, name: &str, table: &Map<String, Value>) -> Result<(), String> {
        let name = ProjectName::try_from(name).map_err(|e| e.to_string())?;
        let shown = self.withheld(shown_table(table));
        let text = toml_text(&shown).map_err(|e| format!("it is not TOML: {e}"))?;
        std::fs::write(dir.join(format!("{name}.toml")), text)
            .map_err(|e| format!("cannot write it: {}", e.kind()))
    }

    // `table` with each text that names this machine withheld.
    fn withheld(&self, table: Map<String, Value>) -> Map<String, Value> {
        table
            .into_iter()
            .map(|(key, value)| (key, self.withheld_value(value)))
            .collect()
    }

    fn withheld_value(&self, value: Value) -> Value {
        match value {
            Value::String(text) => Value::String(self.shown(text)),
            Value::Object(inner) => Value::Object(self.withheld(inner)),
            Value::Array(items) => {
                Value::Array(items.into_iter().map(|v| self.withheld_value(v)).collect())
            }
            other => other,
        }
    }
}
