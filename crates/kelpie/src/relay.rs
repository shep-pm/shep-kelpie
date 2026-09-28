//! The relay: what the maintainer's relay session is started with
//!
//! A stopgap for the first build, kept to one background Claude Code
//! session, found again by its fixed name so kelpie never starts a second
//! one. Its settings gate a merge yes behind a permission prompt in the
//! settings themselves, never in the instructions a session could ignore.

use std::path::Path;

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
///
/// `agentPushNotifEnabled` and `inputNeededNotifEnabled` live in the
/// maintainer's own `~/.claude/settings.json` and are dropped along with
/// everything else by `--setting-sources ""`, so the relay carries both
/// itself: without them its `PushNotification` calls never reach a phone.
///
/// The `env` block hands the relay's shell kelpie's `shep_home`: a session
/// started by a runner does not inherit the runner's environment, and
/// `kelpie relay-*` would otherwise trigger the default shepherd.
pub fn settings(shep_home: &Path) -> Value {
    json!({
        "env": { "SHEP_HOME": shep_home.to_string_lossy() },
        "crossSessionInbound": "accept",
        "agentPushNotifEnabled": true,
        "inputNeededNotifEnabled": true,
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
/// find them. `project` must not carry a newline or `=`, or it breaks that
/// line; every caller today passes a project name already validated to
/// exclude both.
pub fn message(project: &str, ruling_id: u64, question: &str) -> String {
    format!("[kelpie]\nproject={project} ruling={ruling_id}\n\n{question}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_merge_yes_needs_a_tap_and_a_note_or_answer_does_not() {
        let s = settings(Path::new("/k/shep"));
        assert_eq!(
            s["permissions"]["allow"],
            json!(["Bash(kelpie relay-answer *)"])
        );
        assert_eq!(s["permissions"]["ask"], json!(["Bash(kelpie relay-yes *)"]));
    }

    #[test]
    fn cross_session_messages_are_accepted() {
        assert_eq!(
            settings(Path::new("/k/shep"))["crossSessionInbound"],
            "accept"
        );
    }

    #[test]
    fn the_relays_shell_gets_kelpies_shepherd() {
        assert_eq!(
            settings(Path::new("/k/shep"))["env"],
            json!({ "SHEP_HOME": "/k/shep" })
        );
    }

    // Measured live on #14: without these, `--setting-sources ""` drops
    // whatever registers the maintainer's phone, and the relay's own
    // `PushNotification` calls fail with "mobile push is disabled".
    #[test]
    fn push_notifications_are_turned_on() {
        let s = settings(Path::new("/k/shep"));
        assert_eq!(s["agentPushNotifEnabled"], true);
        assert_eq!(s["inputNeededNotifEnabled"], true);
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
