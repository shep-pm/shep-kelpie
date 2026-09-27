//! The relay: what the maintainer's relay session is started with
//!
//! A stopgap for the first build, kept to one background Claude Code
//! session, found again by its fixed name so kelpie never starts a second
//! one. Its settings gate a merge yes behind a permission prompt in the
//! settings themselves, never in the instructions a session could ignore.

use serde_json::{Value, json};

/// The relay's fixed `--name`, so a lookup always finds the same session
pub const NAME: &str = "kelpie-relay";

/// Kelpie's instructions to the relay, appended to its system prompt
pub const INSTRUCTIONS: &str = include_str!("relay-instructions.md");

/// The relay's settings file's contents
///
/// `kelpie relay-answer` carries a no's note or a question's answer, which
/// never merges anything, so it is pre-allowed. `kelpie relay-yes` can
/// carry a merge ruling's yes, so it needs the maintainer's tap every time;
/// the rule names the exact subcommand, never a pattern a free-text answer
/// could widen.
pub fn settings() -> Value {
    json!({
        "crossSessionInbound": "accept",
        "permissions": {
            "allow": ["Bash(kelpie relay-answer *)"],
            "ask": ["Bash(kelpie relay-yes *)"],
        },
    })
}

/// What kelpie sends the relay for one ruling
///
/// The question already carries the exact `shep trigger` command a
/// maintainer typing by hand would use; the relay uses its own
/// `kelpie relay-*` subcommands instead, keyed off `project` and
/// `ruling_id` on their own line so it never has to parse the question to
/// find them. `project` is a [`crate::runner::ProjectName`], which cannot
/// carry a newline or `=`, so it never breaks that line.
pub fn message(project: &str, ruling_id: u64, question: &str) -> String {
    format!("[kelpie]\nproject={project} ruling={ruling_id}\n\n{question}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_merge_yes_needs_a_tap_and_a_note_or_answer_does_not() {
        let s = settings();
        assert_eq!(
            s["permissions"]["allow"],
            json!(["Bash(kelpie relay-answer *)"])
        );
        assert_eq!(s["permissions"]["ask"], json!(["Bash(kelpie relay-yes *)"]));
    }

    #[test]
    fn cross_session_messages_are_accepted() {
        assert_eq!(settings()["crossSessionInbound"], "accept");
    }

    #[test]
    fn the_instructions_name_both_subcommands() {
        assert!(INSTRUCTIONS.contains("kelpie relay-yes"));
        assert!(INSTRUCTIONS.contains("kelpie relay-answer"));
    }

    #[test]
    fn a_message_names_the_project_and_ruling_before_the_question() {
        assert_eq!(
            message("shep", 3, "Merge pull request #71 into main? …"),
            "[kelpie]\nproject=shep ruling=3\n\nMerge pull request #71 into main? …"
        );
    }
}
