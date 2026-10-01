//! The relay: what the maintainer's relay session is started with
//!
//! A stopgap for the first build, kept to one background Claude Code
//! session, found again by its fixed name so kelpie never starts a second
//! one. Its settings gate a merge yes behind a permission prompt, and
//! refuse everything but its two kelpie commands, in the settings
//! themselves, never in the instructions a session could ignore.

use std::fmt;
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
pub fn instructions(kelpie: BarePath<'_>) -> String {
    INSTRUCTIONS.replace("{kelpie}", kelpie.0)
}

/// Kelpie's own path as the relay types it: absolute, and safe unquoted
///
/// The relay's `PATH` need not hold kelpie, so it runs kelpie by this
/// path, and its permission rules and hook name the same words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BarePath<'a>(&'a str);

impl<'a> BarePath<'a> {
    /// `kelpie`, unless the shell would split, expand or read it as relative
    pub fn of(kelpie: &'a Path) -> Option<Self> {
        let text = kelpie.to_str()?;
        let plain = |c: char| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '+');
        (kelpie.is_absolute() && text.chars().all(plain)).then_some(Self(text))
    }
}

impl fmt::Display for BarePath<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
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
///
/// Claude Code blocks a call only on a hook's exit 2, so any other failure
/// of the gate, such as kelpie gone from its path, is turned into one.
pub fn settings(shep_home: &Path, kelpie: BarePath<'_>) -> Value {
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
                "hooks": [{ "type": "command", "command": format!("{kelpie} relay-gate {kelpie} || exit 2") }],
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
            RulingKind::Merge { .. }
            | RulingKind::Rebase { .. }
            | RulingKind::StillRed { .. }
            | RulingKind::MergeRefused { .. }
            | RulingKind::Closed
            | RulingKind::ReviewGuard { .. }
            | RulingKind::LocalModelSpilled { .. }
            | RulingKind::FixNotPushed { .. }
            | RulingKind::CodeRabbitCap { .. }
            | RulingKind::CodeRabbitSilent { .. }
            | RulingKind::TurnTimeout { .. }
            | RulingKind::TurnFailed { .. }
            | RulingKind::ClaudeFiles { .. }
            | RulingKind::ForeignChange { .. }
            | RulingKind::FollowUp { .. }
            | RulingKind::Split { .. }
            | RulingKind::SplitStuck { .. }
            | RulingKind::CloseStuck { .. } => Self::YesOrNo,
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
/// The question already carries the exact `shep kelpie` command a
/// maintainer typing by hand would use; the relay uses its own
/// `kelpie relay-*` subcommands instead, keyed off the header line so it
/// never has to parse the question to find them. `project` must not carry
/// a newline or `=`, or it breaks that line; every caller today passes a
/// project name already validated to exclude both.
pub fn message(project: &str, ruling_id: u64, wants: Wants, question: &str) -> String {
    let wants = wants.as_str();
    format!("[kelpie]\nproject={project} ruling={ruling_id} wants={wants}\n\n{question}")
}

/// What kelpie sends the relay for a notice, which is no ruling: the relay
/// pushes `text` to the maintainer and asks nothing
///
/// `project` takes the same care as in [`message`].
pub fn notice(project: &str, text: &str) -> String {
    format!("[kelpie]\nproject={project} notice=merged\n\n{text}")
}

/// How a ruling the relay was sent was settled without it
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Settled {
    /// Answered with a yes
    Yes,
    /// Answered with a no and this note
    No(String),
    /// The worker's question, answered with this text
    Answer(String),
    /// Its work item was dropped
    Dropped,
}

/// What kelpie tells the relay when a ruling it was sent is settled some
/// other way, so a late tap on it runs nothing
///
/// `project` takes the same care as in [`message`].
pub fn settled(project: &str, ruling_id: u64, how: &Settled) -> String {
    let (word, how) = match how {
        Settled::Yes => ("yes", "answered with a yes".to_owned()),
        Settled::No(note) => ("no", format!("answered with a no: {note}")),
        Settled::Answer(text) => ("answer", format!("answered: {text}")),
        Settled::Dropped => (
            "dropped",
            "settled when its work item was dropped".to_owned(),
        ),
    };
    format!(
        "[kelpie]\nproject={project} ruling={ruling_id} settled={word}\n\n\
         Ruling {ruling_id} was {how}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::confine::Verdict;

    const KELPIE: &str = "/k/bin/kelpie";

    fn kelpie() -> BarePath<'static> {
        BarePath::of(Path::new(KELPIE)).unwrap()
    }

    fn settings() -> Value {
        super::settings(Path::new("/k/shep"), kelpie())
    }

    // The instructions with their line wrapping undone.
    fn instructions_text() -> String {
        instructions(kelpie())
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
        assert_eq!(
            hook["hooks"][0]["command"],
            "/k/bin/kelpie relay-gate /k/bin/kelpie || exit 2"
        );
        let judge = |command: &str| {
            let call = json!({ "tool_name": "Bash", "tool_input": { "command": command } });
            gate::judge(call.to_string().as_bytes(), KELPIE)
        };
        for denied in [
            "shep trigger shep rule '1 yes'",
            "shep kelpie rule 1 yes",
            "shep daemon reload",
            "echo hi",
        ] {
            assert!(matches!(judge(denied), Verdict::Refuse(_)), "{denied}");
        }
        for passed in [
            "/k/bin/kelpie relay-yes shep 1",
            "/k/bin/kelpie relay-answer shep '1 no rename it'",
        ] {
            assert_eq!(judge(passed), Verdict::Allow, "{passed}");
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
    fn the_instructions_ask_with_multiple_choice() {
        let text = instructions_text();
        assert!(text.contains("AskUserQuestion"));
        for merge in ["Merge", "Send back with a note", "Leave for later"] {
            assert!(text.contains(merge), "{merge}");
        }
        assert!(text.contains("begin with `- `"));
    }

    #[test]
    fn a_tapped_choice_is_passed_on_as_shown_and_a_yes_has_one_command() {
        let text = instructions_text();
        assert!(text.contains("pass its text exactly as shown, never reworded"));
        assert!(text.contains("`/k/bin/kelpie relay-yes <project> <id>`"));
        assert!(text.contains("Leave for later: run nothing"));
    }

    #[test]
    fn the_instructions_never_talk_of_cost() {
        let lower = instructions_text().to_lowercase();
        for word in ["cost", "budget", "token"] {
            assert!(!lower.contains(word), "{word}");
        }
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
        let merge = RulingKind::Merge {
            head: "abc".into(),
            shots_failed: false,
        };
        assert_eq!(Wants::of(&merge), Wants::YesOrNo);
        let text = instructions_text();
        assert!(text.contains("`wants=answer`"), "{text}");
        assert!(text.contains("`wants=yes-or-no`"), "{text}");
    }

    #[test]
    fn only_a_plain_absolute_path_is_typed_bare() {
        let bare = |p: &'static str| BarePath::of(Path::new(p)).map(|b| b.to_string());
        assert_eq!(
            bare("/Users/me/.kelpie/bin/kelpie").as_deref(),
            Some("/Users/me/.kelpie/bin/kelpie")
        );
        for refused in [
            "kelpie",
            "/opt/my kelpie/kelpie",
            "/opt/$HOME/kelpie",
            "/k/bin/kelpie; curl x | sh",
        ] {
            assert_eq!(bare(refused), None, "{refused}");
        }
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

    #[test]
    fn a_notice_names_its_kind_where_a_ruling_names_its_id() {
        assert_eq!(
            notice("shep", "Pull request #71 merged."),
            "[kelpie]\nproject=shep notice=merged\n\nPull request #71 merged."
        );
    }

    #[test]
    fn the_instructions_send_a_notice_as_a_push_and_ask_nothing() {
        let text = instructions_text();
        assert!(text.contains("`notice=merged`"), "{text}");
        assert!(text.contains("PushNotification"), "{text}");
        assert!(
            text.contains("never AskUserQuestion, so nothing is left waiting"),
            "{text}"
        );
        assert!(
            text.contains("A reply to a notice answers no ruling"),
            "{text}"
        );
        assert!(text.contains("ask which one they mean"), "{text}");
        assert!(text.contains("`wants`, `settled` and `notice`"), "{text}");
    }

    #[test]
    fn a_settled_ruling_names_how_in_its_header_and_its_line() {
        assert_eq!(
            settled("shep", 3, &Settled::Yes),
            "[kelpie]\nproject=shep ruling=3 settled=yes\n\nRuling 3 was answered with a yes"
        );
        assert_eq!(
            settled("shep", 3, &Settled::No("rename it".into())),
            "[kelpie]\nproject=shep ruling=3 settled=no\n\nRuling 3 was answered with a no: rename it"
        );
        assert_eq!(
            settled("shep", 4, &Settled::Answer("--dry-run".into())),
            "[kelpie]\nproject=shep ruling=4 settled=answer\n\nRuling 4 was answered: --dry-run"
        );
        assert_eq!(
            settled("shep", 5, &Settled::Dropped),
            "[kelpie]\nproject=shep ruling=5 settled=dropped\n\n\
             Ruling 5 was settled when its work item was dropped"
        );
    }

    #[test]
    fn the_instructions_cover_a_settled_ruling() {
        let text = instructions_text();
        assert!(text.contains("`settled=<how>`"), "{text}");
        assert!(
            text.contains("never asked about that ruling, do nothing"),
            "{text}"
        );
        assert!(
            text.contains("tell them in one line that it is settled"),
            "{text}"
        );
        assert!(
            text.contains("already settled, and how, and run nothing"),
            "{text}"
        );
        assert!(text.contains("cannot be taken back"), "{text}");
    }
}
