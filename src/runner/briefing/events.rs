//! The board events a save makes, read from the state before and after it

use crate::state::ProjectState;
use crate::work_item::{Phase, ReviewCallState, ReviewStage, Turn, WorkItem};

/// How much of a ruling's question or a failure an event quotes, in characters
const QUOTE: usize = 160;

/// What changed on the board between `was` and `now`, oldest change first,
/// with each failure and question passed through `shown`
pub(super) fn changes(
    was: &ProjectState,
    now: &ProjectState,
    shown: &dyn Fn(String) -> String,
) -> Vec<String> {
    let mut found = Vec::new();
    if was.run != now.run {
        found.push(format!("project {}", run_word(now)));
    }
    for item in &now.work_items {
        match was.item(item.issue) {
            None => found.push(opened(item)),
            Some(before) => item_changes(before, item, shown, &mut found),
        }
    }
    for item in &was.work_items {
        if now.item(item.issue).is_none() {
            found.push(closed(item, now));
        }
    }
    for ruling in &now.rulings {
        if !was.rulings.iter().any(|r| r.id == ruling.id) {
            let on = ruling.issue.map_or(String::new(), |n| format!(" on #{n}"));
            let question = shown(quote(&ruling.question));
            found.push(format!("ruling {}{on} raised: {question}", ruling.id));
        }
    }
    for ruling in &was.rulings {
        if !now.rulings.iter().any(|r| r.id == ruling.id) {
            let on = ruling.issue.map_or(String::new(), |n| format!(" on #{n}"));
            found.push(format!("ruling {}{on} answered", ruling.id));
        }
    }
    found
}

fn run_word(state: &ProjectState) -> &'static str {
    match state.run {
        crate::state::RunState::Running => "started",
        crate::state::RunState::Paused => "paused",
    }
}

fn opened(item: &WorkItem) -> String {
    let how = match (item.adopted, item.rework, item.pull_request) {
        (true, _, Some(pr)) => format!(", adopting PR #{pr}"),
        (_, true, Some(pr)) => format!(", reworking PR #{pr}"),
        _ => String::new(),
    };
    format!(
        "#{}: work item opened on {}{how}: \"{}\"",
        item.issue, item.agent, item.title
    )
}

fn closed(item: &WorkItem, now: &ProjectState) -> String {
    let finished = now.history.iter().rev().find(|f| f.issue == item.issue);
    match (finished, item.pull_request) {
        (Some(f), Some(pr)) if f.merged => {
            format!("#{}: PR #{pr} merged, work item done", item.issue)
        }
        _ => format!("#{}: work item dropped", item.issue),
    }
}

fn item_changes(
    was: &WorkItem,
    now: &WorkItem,
    shown: &dyn Fn(String) -> String,
    found: &mut Vec<String>,
) {
    let n = now.issue;
    if std::mem::discriminant(&was.turn) != std::mem::discriminant(&now.turn) {
        match &now.turn {
            Turn::Running { .. } => found.push(format!("#{n}: worker turn started")),
            Turn::Ended { .. } => found.push(format!("#{n}: worker turn ended")),
            Turn::Next { .. } => found.push(format!("#{n}: next worker turn due")),
            Turn::Failed { reason, .. } => {
                found.push(format!(
                    "#{n}: worker turn failed: {}",
                    shown(quote(reason))
                ));
            }
            Turn::Due => {}
        }
    }
    if let (None, Some(pr)) = (was.pull_request, now.pull_request) {
        found.push(format!("#{n}: pull request #{pr} opened"));
    }
    match (was.review_call, now.review_call) {
        (ReviewCallState::Idle, ReviewCallState::Running { .. }) => {
            found.push(format!("#{n}: review call started"));
        }
        (ReviewCallState::Running { .. }, ReviewCallState::Idle) => {
            found.push(format!("#{n}: review call ended"));
        }
        _ => {}
    }
    let (before, after) = (phase_word(&was.phase), phase_word(&now.phase));
    if before != after {
        found.push(format!("#{n}: {before} to {after}"));
    }
}

/// A phase in a few words, as an event names it
pub(super) fn phase_word(phase: &Phase) -> String {
    match phase {
        Phase::Implement => "implement".to_owned(),
        Phase::Review(review) => {
            let stage = match &review.stage {
                ReviewStage::Fixing { .. } => ", fixing",
                _ => "",
            };
            format!("review round {}{stage}", review.round)
        }
        Phase::Ci { .. } => "CI".to_owned(),
        Phase::Ruling { id } => format!("ruling {id}"),
        Phase::Merge { .. } => "merge".to_owned(),
        Phase::Done { .. } => "done".to_owned(),
    }
}

/// The first line of `text`, cut short past [`QUOTE`] characters, with its
/// backticks made quotes so nothing in it opens code on the board
pub(super) fn quote(text: &str) -> String {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let line = line.trim().replace('`', "'");
    match line.char_indices().nth(QUOTE) {
        Some((end, _)) => format!("{} …", &line[..end]),
        None => line,
    }
}

#[cfg(test)]
mod tests {
    use super::quote;

    #[test]
    fn a_quote_is_its_first_line_without_backticks() {
        assert_eq!(quote("\n  Add `--flag`\n## Forged\n"), "Add '--flag'");
    }
}
