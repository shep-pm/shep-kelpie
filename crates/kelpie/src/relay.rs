//! The relay: what the maintainer's relay session is started with
//!
//! A stopgap for the first build, kept to one background Claude Code
//! session, found again by its fixed name so kelpie never starts a second
//! one. Its settings gate a merge yes behind a permission prompt, and
//! refuse everything but its two kelpie commands, in the settings
//! themselves, never in the instructions a session could ignore.

use std::path::Path;

use serde_json::{Value, json};

use crate::state::RulingKind;

pub mod gate;
pub mod rule;

/// The relay's fixed `--name`, so a lookup always finds the same session
pub const NAME: &str = "kelpie-relay";

// `{kelpie}` stands for kelpie's own path, which `instructions` fills in.
const INSTRUCTIONS: &str = include_str!("relay-instructions.md");

/// Kelpie's instructions to the relay, appended to its system prompt, with
/// its commands run as `kelpie`
pub fn instructions(kelpie: &str) -> String {
    INSTRUCTIONS.replace("{kelpie}", kelpie)
}

/// Kelpie's own path as the relay types it, when it can be typed bare
///
/// The relay's `PATH` need not hold kelpie, so it runs kelpie by this
/// absolute path, and its permission rules name the same words. `None`
/// for a path the shell would split, expand or read as relative.
pub fn bare(kelpie: &Path) -> Option<&str> {
    let text = kelpie.to_str()?;
    let plain = |c: char| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '+');
    (kelpie.is_absolute() && text.chars().all(plain)).then_some(text)
}

/// The relay's settings file's contents, with its commands run as `kelpie`
///
/// `kelpie relay-answer` carries a no's note or a question's answer, which
/// never merges anything, so it is pre-allowed. `kelpie relay-yes` can
/// carry a merge ruling's yes, so it needs the maintainer's tap every time;
/// the rule names the exact subcommand, never a pattern a free-text answer
/// could widen. Every other tool call is refused by [`gate`], never asked.
///
/// `agentPushNotifEnabled` and `inputNeededNotifEnabled` live in the
/// maintainer's own `~/.claude/settings.json` and are dropped along with
/// everything else by `--setting-sources ""`, so the relay carries both
/// itself: without them its `PushNotification` calls never reach a phone.
///
/// The `env` block hands the relay's shell kelpie's `shep_home`: a session
/// started by a runner does not inherit the runner's environment, and
/// `kelpie relay-*` would otherwise trigger the default shepherd.
pub fn settings(shep_home: &Path, kelpie: &str) -> Value {
    json!({
        "env": { "SHEP_HOME": shep_home.to_string_lossy() },
        "crossSessionInbound": "accept",
        "agentPushNotifEnabled": true,
        "inputNeededNotifEnabled": true,
        "permissions": {
            "allow": [format!("Bash({kelpie} relay-answer *)")],
            "ask": [format!("Bash({kelpie} relay-yes *)")],
        },
        "hooks": {
            "PreToolUse": [{
                "matcher": "*",
                "hooks": [{ "type": "command", "command": format!("{kelpie} relay-gate {kelpie}") }],
            }],
        },
    })
}

/// Which answer a ruling takes, so the relay picks its command from the
/// message rather than from the question's wording
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wants {
    /// A worker's question, answered with `<id> answer <text>`
    Answer,
    /// Every other ruling: `<id> yes`, or `<id> no <note>`
    YesOrNo,
}

impl Wants {
    /// What a ruling of `kind` takes
    pub fn of(kind: &RulingKind) -> Self {
        match kind {
            RulingKind::Question { .. } => Self::Answer,
            _ => Self::YesOrNo,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Answer => "answer",
            Self::YesOrNo => "yes-or-no",
        }
    }
}

/// What kelpie sends the relay for one ruling
///
/// The question already carries the exact `shep trigger` command a
/// maintainer typing by hand would use; the relay uses its own
/// `kelpie relay-*` subcommands instead, keyed off the header line so it
/// never has to parse the question to find them. `project` must not carry
/// a newline or `=`, or it breaks that line; every caller today passes a
/// project name already validated to exclude both.
pub fn message(project: &str, ruling_id: u64, wants: Wants, question: &str) -> String {
    let wants = wants.as_str();
    format!("[kelpie]\nproject={project} ruling={ruling_id} wants={wants}\n\n{question}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::confine::Verdict;

    const KELPIE: &str = "/k/bin/kelpie";

    fn settings() -> Value {
        super::settings(Path::new("/k/shep"), KELPIE)
    }

    // The instructions with their line wrapping undone.
    fn instructions_text() -> String {
        instructions(KELPIE)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn a_merge_yes_needs_a_tap_and_a_note_or_answer_does_not() {
        let s = settings();
        assert_eq!(
            s["permissions"]["allow"],
            json!(["Bash(/k/bin/kelpie relay-answer *)"])
        );
        assert_eq!(
            s["permissions"]["ask"],
            json!(["Bash(/k/bin/kelpie relay-yes *)"])
        );
    }

    // Measured on #80: in the default mode an unnamed command asks on the
    // phone, and `dontAsk` refuses `relay-yes` too. The gate refuses it.
    #[test]
    fn the_settings_refuse_shep_and_every_other_command() {
        let s = settings();
        let hook = &s["hooks"]["PreToolUse"][0];
        assert_eq!(hook["matcher"], "*");
        let command = hook["hooks"][0]["command"].as_str().unwrap();
        let [program, role, kelpie] = command.split(' ').collect::<Vec<_>>()[..] else {
            panic!("{command}");
        };
        assert_eq!((program, role), (KELPIE, "relay-gate"));
        for denied in [
            "shep trigger shep rule '1 yes'",
            "shep daemon reload",
            "echo hi",
        ] {
            let call = json!({ "tool_name": "Bash", "tool_input": { "command": denied } });
            let verdict = gate::judge(call.to_string().as_bytes(), kelpie);
            assert!(matches!(verdict, Verdict::Refuse(_)), "{denied}");
        }
        assert!(s["permissions"].get("defaultMode").is_none(), "{s}");
    }

    #[test]
    fn cross_session_messages_are_accepted() {
        assert_eq!(settings()["crossSessionInbound"], "accept");
    }

    #[test]
    fn the_relays_shell_gets_kelpies_shepherd() {
        assert_eq!(settings()["env"], json!({ "SHEP_HOME": "/k/shep" }));
    }

    // Measured live on #14: without these, `--setting-sources ""` drops
    // whatever registers the maintainer's phone, and the relay's own
    // `PushNotification` calls fail with "mobile push is disabled".
    #[test]
    fn push_notifications_are_turned_on() {
        let s = settings();
        assert_eq!(s["agentPushNotifEnabled"], true);
        assert_eq!(s["inputNeededNotifEnabled"], true);
    }

    #[test]
    fn the_instructions_run_kelpie_by_its_absolute_path() {
        let text = instructions_text();
        assert!(text.contains("`/k/bin/kelpie relay-yes <project> <id>`"));
        assert!(text.contains("`/k/bin/kelpie relay-answer <project> '<id> answer <text>'`"));
        assert!(text.contains("`/k/bin/kelpie relay-answer <project> '<id> no <note>'`"));
        assert!(!text.contains("{kelpie}"), "{text}");
        let bare = text.replace(KELPIE, "");
        assert!(!bare.contains("`kelpie relay-"), "{text}");
    }

    #[test]
    fn the_instructions_say_a_failure_is_reported_never_worked_around() {
        let text = instructions_text();
        assert!(text.contains("tell the maintainer its output word for word"));
        assert!(text.contains("never try another command"));
    }

    #[test]
    fn a_question_takes_an_answer_and_every_other_ruling_a_yes_or_no() {
        let question = RulingKind::Question {
            asked: String::new(),
            resume: crate::state::Resume::Nothing,
        };
        assert_eq!(Wants::of(&question), Wants::Answer);
        let merge = RulingKind::Merge { head: "abc".into() };
        assert_eq!(Wants::of(&merge), Wants::YesOrNo);
        let text = instructions_text();
        assert!(text.contains("`wants=answer`"), "{text}");
        assert!(text.contains("`wants=yes-or-no`"), "{text}");
    }

    #[test]
    fn only_a_plain_absolute_path_is_typed_bare() {
        assert_eq!(
            bare(Path::new("/Users/m/.kelpie/bin/kelpie")),
            Some("/Users/m/.kelpie/bin/kelpie")
        );
        assert_eq!(bare(Path::new("kelpie")), None);
        assert_eq!(bare(Path::new("/opt/my kelpie/kelpie")), None);
        assert_eq!(bare(Path::new("/opt/$HOME/kelpie")), None);
    }

    #[test]
    fn a_message_names_the_project_ruling_and_answer_before_the_question() {
        assert_eq!(
            message(
                "shep",
                3,
                Wants::YesOrNo,
                "Merge pull request #71 into main? …"
            ),
            "[kelpie]\nproject=shep ruling=3 wants=yes-or-no\n\nMerge pull request #71 into main? …"
        );
        assert_eq!(
            message("shep", 4, Wants::Answer, "The worker asks: …"),
            "[kelpie]\nproject=shep ruling=4 wants=answer\n\nThe worker asks: …"
        );
    }
}
