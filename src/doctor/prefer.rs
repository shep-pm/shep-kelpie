//! The names in `[kelpie.agent_board]`'s `prefer` that no agent file holds

use super::Line;
use crate::agent_board::AgentBoardSettings;
use crate::agents::{Agents, Role};

/// One line for each listed name that no implementer or reviewer file holds
///
/// The board ignores such a name and ranks the rest, so the line only says so.
pub(super) fn check(settings: &AgentBoardSettings, agents: &Agents) -> Vec<Line> {
    let ranks = |role| matches!(role, Role::Implementer | Role::Reviewer);
    let mut seen = Vec::new();
    let mut lines = Vec::new();
    for name in &settings.prefer {
        if seen.contains(&name) {
            continue;
        }
        seen.push(name);
        if !agents.get(name).is_some_and(|agent| ranks(agent.role)) {
            lines.push(Line::unsure(
                format!("agent {name}"),
                "`agent_board.prefer` lists it, but no implementer or reviewer file holds it, \
                 so the board ignores it",
                format!("write `{name}.md` in kelpie's home's `agents` folder, or take it out of `prefer`"),
            ));
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doctor::{Report, Verdict};
    use crate::settings::AgentName;
    use crate::test::write_in;

    fn agents() -> Agents {
        let dir = tempfile::tempdir().unwrap();
        for (file, role) in [
            ("build.md", "implementer"),
            ("read.md", "reviewer"),
            ("manage.md", "pm"),
        ] {
            let text = format!(
                "---\nrole: {role}\nharness: stand-in\nmodel: lite\neffort: low\nusage: none\n---\nBody.\n"
            );
            write_in(dir.path(), file, &text);
        }
        Agents::load(dir.path()).unwrap()
    }

    fn listing(names: &[&'static str]) -> AgentBoardSettings {
        AgentBoardSettings {
            prefer: names.iter().map(|n| AgentName::kelpies(n)).collect(),
        }
    }

    #[test]
    fn a_listed_name_with_no_agent_file_is_named_and_never_fails_the_run() {
        let lines = check(
            &listing(&["build", "ghost", "read", "manage", "ghost"]),
            &agents(),
        );

        assert_eq!(lines.len(), 2, "{lines:?}");
        assert_eq!(lines[0].subject, "agent ghost");
        assert_eq!(lines[1].subject, "agent manage");
        for line in &lines {
            let Verdict::Unsure { what, .. } = &line.verdict else {
                panic!("not unsure: {line:?}");
            };
            assert!(what.contains("the board ignores it"), "{what}");
        }
        assert!(
            Report { lines }.passed(),
            "an unsure line never fails the run"
        );
        assert!(check(&listing(&[]), &agents()).is_empty());
        assert!(check(&listing(&["build", "read"]), &agents()).is_empty());
    }
}
