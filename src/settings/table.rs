//! A project's settings as its runner sheep's `[app.dogs.kelpie]` table
//!
//! shep stores the table without reading it, and hands it over as JSON.
//! Kelpie writes it back out as TOML and reads that with the settings
//! file's own parser, so a table and a file take the same keys, defaults
//! and checks. A refusal names the setting as a dotted key.

use std::path::Path;

use serde_json::{Map, Value};

use super::{Settings, SettingsError};

impl Settings {
    /// Reads and checks `sheep`'s `[app.dogs.kelpie]` table
    ///
    /// `~/` expands against `home`, and a relative `worker.instructions_file`
    /// is taken from `folder`, the project's own folder under kelpie's home.
    ///
    /// # Errors
    ///
    /// [`SettingsError::Table`] naming the setting that is missing, unknown
    /// or malformed.
    pub fn from_table(
        table: &Map<String, Value>,
        sheep: &str,
        home: &Path,
        folder: &Path,
    ) -> Result<Self, SettingsError> {
        let refused = |message| SettingsError::Table {
            sheep: sheep.to_owned(),
            message,
        };
        let text = toml_text(table).map_err(refused)?;
        let mut settings: Self = toml::from_str(&text).map_err(|e| refused(located(&text, &e)))?;
        settings.expand(home);
        settings.relative_to(folder);
        settings.review.local.check().map_err(refused)?;
        Ok(settings)
    }
}

/// A settings file's text as the table `shep kelpie settings move` writes
///
/// # Errors
///
/// The parser's message when `text` is not TOML.
pub fn table_of(text: &str) -> Result<Map<String, Value>, String> {
    let table: toml::Table = toml::from_str(text).map_err(|e| e.to_string())?;
    match serde_json::to_value(table) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => Err("a TOML document is always a table".into()),
        Err(e) => Err(e.to_string()),
    }
}

fn toml_text(table: &Map<String, Value>) -> Result<String, String> {
    let table: toml::Table =
        serde_json::from_value(Value::Object(table.clone())).map_err(|e| e.to_string())?;
    toml::to_string(&table).map_err(|e| e.to_string())
}

// The parser's message and the dotted setting it points at, with its value:
// a project's table holds no secret. Line numbers would count kelpie's own
// rendering, so they are left out.
fn located(text: &str, error: &toml::de::Error) -> String {
    let message = error.message().trim();
    let whole = |start: usize, end: usize| start == 0 && end >= text.trim_end().len();
    let Some(span) = error.span().filter(|s| !whole(s.start, s.end)) else {
        return message.to_owned();
    };
    let before = &text[..span.start];
    let start = before.rfind('\n').map_or(0, |i| i + 1);
    let line = text[start..].lines().next().unwrap_or_default().trim();
    let header = |line: &str| line.trim_matches(['[', ']']).to_owned();
    let setting = match line.split_once('=') {
        _ if line.starts_with('[') => header(line),
        None => line.to_owned(),
        Some((key, value)) => {
            let key = key.trim();
            let value = value.trim();
            match before[..start].lines().rev().find(|l| l.starts_with('[')) {
                Some(table) => format!("{}.{key} = {value}", header(table)),
                None => format!("{key} = {value}"),
            }
        }
    };
    format!("`{setting}`: {message}")
}
