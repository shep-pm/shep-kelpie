//! A review bot's open thread as a merge ruling lists it

use serde::{Deserialize, Serialize};

use crate::ports::Severity;

/// A review bot's open thread, as a merge ruling lists it
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenThread {
    /// The bot that left it
    pub bot: String,
    /// The level the bot wrote, such as `P1`, when it writes one
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
    /// How serious kelpie reads it
    pub severity: Severity,
    /// The file it is on
    pub file: String,
    /// The line, or 0 when the bot names none
    pub line: u32,
    /// What the bot said, to its first sentence
    pub what: String,
}

impl OpenThread {
    /// The level the bot wrote, or kelpie's reading of it in capitals
    #[must_use]
    pub fn label(&self) -> String {
        self.level.clone().unwrap_or_else(|| {
            match self.severity {
                Severity::High => "HIGH",
                Severity::Medium => "MEDIUM",
                Severity::Low => "LOW",
            }
            .to_owned()
        })
    }

    /// Whether it is above a nit
    #[must_use]
    pub fn above_a_nit(&self) -> bool {
        self.severity > Severity::Low
    }
}
