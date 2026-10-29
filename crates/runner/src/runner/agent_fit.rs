//! The `agent-fit` ruling: what the board says of the agent that just finished an item
//!
//! A finished line names the agent that built the item and the kind of work it
//! was. The runner asks the agent board whether that agent still fits that kind,
//! and the finding it answers with raises the ruling on the item's pull request.
//! Kelpie never writes an agent's `kinds`: a yes and a no both change no file,
//! and the line the board suggests is for the maintainer to write into
//! `agents/<name>.md`. A board that cannot be reached raises nothing, and the
//! next finished line asks again.

use super::Runner;
use super::ruling::question;
use crate::agent_board::Measure;
use crate::board::briefing::elapsed;
use crate::lease::door::{FitAsk, Fits};
use crate::settings::AgentName;
use crate::state::{AgentFit, AgentMeasure, Ruling, RulingKind};
use crate::ticket::TicketId;

impl Runner {
    /// Asks the board whether `agent` still fits the work kind `kind`, and raises
    /// the ruling its finding says is due on the finished item's pull request
    /// `number`.
    ///
    /// A finished line with no agent, no kind, or no pull request asks nothing,
    /// and a board that cannot be reached raises nothing: the next finished line
    /// asks it again.
    pub(super) fn agent_fit(
        &mut self,
        agent: Option<&str>,
        kind: Option<&str>,
        number: Option<u64>,
    ) {
        let (Some(agent), Some(kind), Some(number)) = (agent, kind, number) else {
            return;
        };
        let Ok(agent) = AgentName::try_from(agent.to_owned()) else {
            return;
        };
        let ask = FitAsk {
            agent,
            kind: kind.to_owned(),
        };
        let Some(Fits::No(finding)) = self.ports.agent_board.fit(&ask) else {
            return;
        };
        self.raise_fit(
            number,
            AgentFit {
                work_kind: finding.kind,
                worse: finding.worse,
                better: finding.better,
                measure: kept(&finding.measure),
                suggested_kinds: finding.suggested_kinds,
            },
        );
    }

    // Saves the ruling on pull request `number` and posts its comment. A ruling
    // already waiting under that number keeps its place: the number answers a
    // ruling, so two cannot share it, and the board marks the pair when it
    // answers, so the finding comes again after the agent's next items.
    fn raise_fit(&mut self, number: u64, fit: AgentFit) {
        let id = TicketId::from_pull_request(number);
        if self.state.rulings.iter().any(|r| r.id == id) {
            return;
        }
        let kind = RulingKind::AgentFit(fit);
        let text = question(id, Some(number), &kind);
        let mut next = self.state.clone();
        next.rulings.push(Ruling {
            pull_request: Some(number),
            raised: Some(self.ports.clock.now()),
            ..Ruling::new(id, text, kind)
        });
        if let Err(e) = self.save(next) {
            self.notes.push(format!(
                "cannot save the agent-fit ruling on #{number}: {e}"
            ));
            return;
        }
        if let Some(why) = self.post_ruling(Some(number), id) {
            self.notes.push(format!(
                "cannot post the agent-fit ruling on #{number}: {why}"
            ));
        }
    }
}

/// What the board found, as the ruling shows it: each agent's figure beside the
/// other's, and the `kinds` line it suggests for the agent file
pub(in crate::runner) fn said(fit: &AgentFit) -> String {
    let (worse, better) = figures(&fit.measure);
    let rows = format!(
        "| `{}` | {worse} |\n| `{}` | {better} |",
        fit.worse, fit.better
    );
    format!(
        "`{}` does clearly worse at `{}` than `{}`, on their {}.\n\n\
         | agent | {} |\n|---|---|\n{rows}\n\n{}",
        fit.worse,
        fit.work_kind,
        fit.better,
        read(&fit.measure),
        column(&fit.measure),
        suggested(fit),
    )
}

/// The `kinds` the worse agent's file should name, as a line for that file
fn suggested(fit: &AgentFit) -> String {
    let file = format!("`agents/{}.md`", fit.worse);
    if !fit.suggested_kinds.is_empty() {
        let named = (fit.suggested_kinds.iter())
            .map(|kind| format!("\"{kind}\""))
            .collect::<Vec<_>>()
            .join(", ");
        return format!("Suggested `kinds` for {file}: `kinds = [{named}]`");
    }
    format!(
        "The board suggests {file} take no kind at all, which no agent file can say: `kinds` names \
         at least one, and a file that names none takes every kind."
    )
}

// What the two records are read against, in the sentence that leads the table
fn read(measure: &AgentMeasure) -> &'static str {
    match measure {
        AgentMeasure::MergeRate { .. } => "merge rate",
        AgentMeasure::Hours { .. } => "median hours",
        AgentMeasure::HeavyFix { .. } => "share of items with 3 or more fix turns",
    }
}

// What the table's second column is called
fn column(measure: &AgentMeasure) -> &'static str {
    match measure {
        AgentMeasure::MergeRate { .. } => "merge rate",
        AgentMeasure::Hours { .. } => "median hours",
        AgentMeasure::HeavyFix { .. } => "items with 3 or more fix turns",
    }
}

// Each agent's figure, as the table shows it, worse first
fn figures(measure: &AgentMeasure) -> (String, String) {
    match measure {
        AgentMeasure::MergeRate { worse, better } | AgentMeasure::HeavyFix { worse, better } => {
            (percent(*worse), percent(*better))
        }
        AgentMeasure::Hours { worse, better } => (hours(*worse), hours(*better)),
    }
}

// A whole percent as `50%`
fn percent(part: u32) -> String {
    format!("{part}%")
}

// Whole minutes as `45m` or `3h05m`
fn hours(minutes: u32) -> String {
    elapsed(u64::from(minutes) * 60)
}

// The finding's figures, as the ruling keeps them: rates in whole percents and
// hours in whole minutes, so the state file holds no fraction
fn kept(measure: &Measure) -> AgentMeasure {
    let rate = |share: f64| (share * 100.0).round() as u32;
    let minutes = |hours: f64| (hours * 60.0).round() as u32;
    match measure {
        Measure::MergeRate { worse, better } => AgentMeasure::MergeRate {
            worse: rate(*worse),
            better: rate(*better),
        },
        Measure::HeavyFix { worse, better } => AgentMeasure::HeavyFix {
            worse: rate(*worse),
            better: rate(*better),
        },
        Measure::Hours { worse, better } => AgentMeasure::Hours {
            worse: minutes(*worse),
            better: minutes(*better),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use serde_json::{Value, json};

    use super::*;
    use crate::agent_board::{Finding, Measure};
    use crate::ports::{AgentError, PullRequestState};
    use crate::runner::{CHECKS_SETTLE, StepReport, step};
    use crate::settings::Harness;
    use crate::test::{Rig, Scripted};

    // The board's finding about the rig's implementer at the kind its issue is labelled for
    fn finding() -> Finding {
        Finding {
            kind: "fix".into(),
            worse: "sonnet-high".into(),
            better: "opus-high".into(),
            measure: Measure::MergeRate {
                worse: 0.5,
                better: 1.0,
            },
            suggested_kinds: vec!["docs".into()],
        }
    }

    // A project whose worker on issue 7, labelled `kind:fix`, is parked on merge
    // ruling 71 with CI green, and whose board says it does not fit `fix`
    fn ruled(project: &str) -> (Rig, Mutex<Runner>) {
        let (rig, runner, _) = Rig::parked_set(project, |rig| {
            rig.forge.label(7, "kind:fix");
            rig.agent_board
                .does_not_fit("sonnet-high", "fix", finding());
        });
        (rig, runner)
    }

    // Answers merge ruling 71 and steps to the item's end
    fn merged(rig: &Rig, runner: &Mutex<Runner>) {
        rig.ask(runner, "rule", Some("71 yes"));
        assert!(
            matches!(step(runner).unwrap(), Some(StepReport::MarkedReady { .. })),
            "the yes did not ready the pull request"
        );
        rig.clock.advance(CHECKS_SETTLE);
        let ended = step(runner).unwrap();
        assert!(
            matches!(ended, Some(StepReport::Finished { merged: true, .. })),
            "{ended:?}"
        );
    }

    // The project's state file, as JSON
    fn state(rig: &Rig) -> Value {
        let text = std::fs::read_to_string(rig.paths().state).unwrap();
        serde_json::from_str(&text).unwrap()
    }

    // Each agent file the rig's project holds, by name
    fn agent_files(rig: &Rig) -> BTreeMap<String, String> {
        let mut files = BTreeMap::new();
        for entry in std::fs::read_dir(rig.paths().agents).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if !name.ends_with(".md") {
                continue;
            }
            files.insert(name.to_string(), std::fs::read_to_string(&path).unwrap());
        }
        files
    }

    #[test]
    fn a_finding_raises_an_agent_fit_ruling_on_the_items_pull_request() {
        let (rig, runner) = ruled("koji");
        merged(&rig, &runner);

        assert_eq!(
            rig.agent_board.fit_asks(),
            [FitAsk {
                agent: AgentName::kelpies("sonnet-high"),
                kind: "fix".into(),
            }],
            "the finished line names the agent that built the item and its kind"
        );
        let state = state(&rig);
        let rulings = state["rulings"].as_array().unwrap();
        assert_eq!(rulings.len(), 1, "{state}");
        assert_eq!(rulings[0]["id"], "71");
        assert_eq!(rulings[0]["pull_request"], 71);
        assert_eq!(rulings[0]["kind"]["kind"], "agent-fit");
        assert_eq!(rulings[0]["kind"]["work_kind"], "fix");
        assert_eq!(rulings[0]["kind"]["worse"], "sonnet-high");
        assert_eq!(rulings[0]["kind"]["better"], "opus-high");
        assert_eq!(
            rulings[0]["kind"]["measure"],
            json!({"merge-rate": {"worse": 50, "better": 100}})
        );
        assert_eq!(rulings[0]["kind"]["suggested_kinds"], json!(["docs"]));
        assert_eq!(
            state["work_items"].as_array().unwrap().len(),
            0,
            "a finished item parks on nothing"
        );
    }

    #[test]
    fn the_ruling_shows_both_records_and_the_suggested_kinds() {
        let (rig, runner) = ruled("koji");
        merged(&rig, &runner);

        let saved = state(&rig);
        let question = saved["rulings"][0]["question"].as_str().unwrap();
        assert!(
            question.starts_with("Pull request #71 finished on `sonnet-high`."),
            "{question}"
        );
        assert!(
            question.contains(
                "`sonnet-high` does clearly worse at `fix` than `opus-high`, on their merge rate."
            ),
            "{question}"
        );
        assert!(
            question.contains(
                "| agent | merge rate |\n|---|---|\n| `sonnet-high` | 50% |\n| `opus-high` | 100% |"
            ),
            "{question}"
        );
        assert!(
            question
                .contains("Suggested `kinds` for `agents/sonnet-high.md`: `kinds = [\"docs\"]`"),
            "{question}"
        );
        assert!(
            question.contains(
                "`shep kelpie rule 71 yes` and `shep kelpie rule 71 no <note>` change no file"
            ),
            "{question}"
        );

        let (_, comment) = rig
            .forge
            .comments()
            .into_iter()
            .find(|(thread, _)| *thread == 71)
            .expect("the ruling's comment is posted on the pull request");
        assert!(
            comment.contains("| `sonnet-high` | 50% |\n| `opus-high` | 100% |"),
            "{comment}"
        );
        assert!(
            comment.contains("Suggested `kinds` for `agents/sonnet-high.md`"),
            "{comment}"
        );
    }

    #[test]
    fn a_yes_changes_no_agent_file() {
        let (rig, runner) = ruled("koji");
        merged(&rig, &runner);
        let before = agent_files(&rig);
        assert!(
            !before.values().any(|text| text.contains("kinds")),
            "the rig's agent files name no kinds to begin with: {before:?}"
        );

        rig.ask(&runner, "rule", Some("71 yes"));
        assert_eq!(
            rig.ask(&runner, "status", None)["rulings"],
            json!([]),
            "the ruling is answered"
        );
        assert_eq!(agent_files(&rig), before, "kelpie wrote no agent file");
        assert_eq!(rig.forge.merges().len(), 1, "the merge ruling merged it");
    }

    #[test]
    fn an_unreachable_board_raises_nothing() {
        let (rig, runner, _) = Rig::parked_set("koji", |rig| {
            rig.forge.label(7, "kind:fix");
            rig.forge.label(8, "kind:fix");
        });
        rig.agent_board
            .does_not_fit("sonnet-high", "fix", finding());
        rig.agent_board.unreachable();
        merged(&rig, &runner);

        assert_eq!(
            rig.agent_board.fit_asks().len(),
            1,
            "the finished line asked"
        );
        assert_eq!(
            rig.ask(&runner, "status", None)["rulings"],
            json!([]),
            "a board that cannot answer raises nothing"
        );

        // The board answers again: the next finished line asks it, and its
        // finding raises the ruling then.
        rig.agent_board.ranks_first(&["sonnet-high"]);
        rig.ask(&runner, "add", Some("8"));
        rig.forge.open_pull_request(72, "kelpie/8", &[8]);
        rig.claude.script([Scripted::Push("other.txt", "other\n")]);
        assert!(
            matches!(step(&runner).unwrap(), Some(StepReport::Ended { .. })),
            "the worker on issue 8 pushed"
        );
        rig.forge.set_state(72, PullRequestState::Merged);
        let ended = rig.verdict(&runner);
        assert!(
            matches!(
                ended,
                Some(StepReport::Finished {
                    pull_request: Some(72),
                    merged: true,
                    ..
                })
            ),
            "{ended:?}"
        );

        assert_eq!(
            rig.agent_board.fit_asks().len(),
            2,
            "the next finished line asked again"
        );
        let rulings = state(&rig)["rulings"].as_array().unwrap().clone();
        assert_eq!(rulings.len(), 1, "{rulings:#?}");
        assert_eq!(rulings[0]["pull_request"], 72);
        assert_eq!(rulings[0]["kind"]["kind"], "agent-fit");
    }

    #[test]
    fn an_item_with_no_pull_request_raises_nothing() {
        let rig = Rig::new("rotom");
        rig.forge.label(7, "kind:fix");
        rig.agent_board
            .does_not_fit("sonnet-high", "fix", finding());
        let runner = rig.open().unwrap();
        rig.ask(&runner, "start", None);
        rig.ask(&runner, "add", Some("7"));
        rig.claude.script([Scripted::Fail(AgentError::Failed(
            Harness::ClaudeCode,
            "overloaded".into(),
        ))]);
        step(&runner).unwrap(); // the turn fails, and its ruling is raised
        rig.ask(&runner, "rule", Some("7 no not worth another go"));
        let ended = step(&runner).unwrap();
        assert!(
            matches!(
                ended,
                Some(StepReport::Finished {
                    pull_request: None,
                    ..
                })
            ),
            "{ended:?}"
        );

        assert_eq!(
            rig.agent_board.fit_asks(),
            Vec::new(),
            "nothing to raise a ruling on asks nothing"
        );
        assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));
    }
}
