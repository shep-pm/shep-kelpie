//! What each option of a ruling leads to, worded from what kelpie knows
//!
//! A worker's question carries its options' effects in its own words. The
//! rulings kelpie raises itself, a merge and a stuck work item, get theirs
//! when they are raised: what the board holds behind the ruling, how the
//! pull request stands with CI and the reviewers, and what another review
//! round roughly costs, read from the work item's own calls. An option with
//! no effect worded keeps the phrase its kind always had.

use super::Runner;
use super::ruling::needs_pass;
use super::voice::phrases;
use crate::ports::{Checks, Cost, Role};
use crate::state::{Ruling, RulingKind, StateError};
use crate::ticket::TicketId;
use crate::work_item::WorkItem;

impl Runner {
    /// Words the effects of ruling `id` and keeps them with it
    ///
    /// # Errors
    ///
    /// [`StateError`] when they cannot be saved.
    pub(super) fn word_effects(&mut self, id: TicketId) -> Result<(), StateError> {
        let Some(ruling) = self.state.rulings.iter().find(|r| r.id == id) else {
            return Ok(());
        };
        let effects = self.effects_of(ruling);
        if effects.is_empty() {
            return Ok(());
        }
        let mut next = self.state.clone();
        if let Some(ruling) = next.rulings.iter_mut().find(|r| r.id == id) {
            ruling.effects = effects;
        }
        self.save(next)
    }

    // The effect of each option of `ruling`, in the order its card lists them,
    // or none where kelpie words none
    fn effects_of(&self, ruling: &Ruling) -> Vec<String> {
        let Some(item) = ruling.issue.and_then(|issue| self.state.item(issue)) else {
            return Vec::new();
        };
        let held = self.parked_note(ruling.id);
        match &ruling.kind {
            RulingKind::Merge { .. } => {
                let ci = self.ci_sentence(ruling.pull_request);
                self.merge_effects(item, &ruling.kind, ci, held)
            }
            RulingKind::Stuck(_) => (phrases(&ruling.kind).into_iter())
                .map(|does| join([Some(does), held.clone()]))
                .collect(),
            _ => Vec::new(),
        }
    }

    // What CI said of the pull request, read from the forge as the ruling is
    // raised, or nothing when CI does not hold the merge or the forge cannot say.
    // The gate raises the ruling on skipped checks too, and nothing ran then.
    fn ci_sentence(&self, number: Option<u64>) -> Option<String> {
        if !self.settings.ci.block {
            return None;
        }
        let pr = self.pull_request_view(number?).ok()?;
        match pr.checks {
            Checks::Passed => Some("CI passed.".to_owned()),
            Checks::Skipped => Some("No CI check ran.".to_owned()),
            _ => None,
        }
    }

    // A merge ruling's options: the merge says what it closes, how CI and the
    // reviewers stand, and what waits on the ruling; the ways back say what
    // the next read of the pull request costs
    fn merge_effects(
        &self,
        item: &WorkItem,
        kind: &RulingKind,
        ci: Option<String>,
        held: Option<String>,
    ) -> Vec<String> {
        let merge = join([
            Some(format!(
                "Merges it into main and closes #{}: {}.",
                item.issue, item.title
            )),
            ci,
            reviewed(item, kind),
            held,
        ]);
        let again = join([
            Some("Sends the worker back for a change the whole review reads again.".to_owned()),
            round_cost(item),
        ]);
        if needs_pass(kind) {
            return vec![merge, again];
        }
        let fix = "Sends the worker back for a fix that goes to CI and back to you, with no new \
                   review round.";
        vec![merge, fix.to_owned(), again]
    }
}

// Who read the pull request, where anyone did: the rounds that ran and the
// review bots that read it. A head no round read is said so, whatever
// earlier heads the rounds read.
fn reviewed(item: &WorkItem, kind: &RulingKind) -> Option<String> {
    match kind {
        RulingKind::Merge {
            unreviewed: Some(_),
            ..
        } => return Some("No reviewer read it in its last review.".to_owned()),
        RulingKind::Merge {
            unread_head: true, ..
        } => return Some("No review round read this head.".to_owned()),
        _ => {}
    }
    let said = read_by(item);
    match (said, open_bots(kind)) {
        (Some(said), Some(open)) => Some(format!("{said} {open}")),
        (said, open) => said.or(open),
    }
}

// The bots with threads open on a merge ruling's head, as option A names them
fn open_bots(kind: &RulingKind) -> Option<String> {
    let RulingKind::Merge { open_found, .. } = kind else {
        return None;
    };
    let mut bots: Vec<(&str, usize)> = Vec::new();
    for thread in open_found {
        match bots.iter_mut().find(|(bot, _)| *bot == thread.bot) {
            Some((_, n)) => *n += 1,
            None => bots.push((&thread.bot, 1)),
        }
    }
    let said: Vec<String> = (bots.into_iter())
        .map(|(bot, n)| match n {
            1 => format!("{bot} left 1 thread open on it."),
            n => format!("{bot} left {n} threads open on it."),
        })
        .collect();
    (!said.is_empty()).then(|| said.join(" "))
}

fn read_by(item: &WorkItem) -> Option<String> {
    let rounds = match item.counts.review_rounds {
        0 => None,
        1 => Some("1 review round".to_owned()),
        n => Some(format!("{n} review rounds")),
    };
    let bots: Vec<String> = item.bot_reads.keys().map(ToString::to_string).collect();
    let bots = (!bots.is_empty()).then(|| bots.join(" and "));
    match (rounds, bots) {
        (None, None) => None,
        (Some(rounds), None) => Some(format!("{rounds} read it.")),
        (None, Some(bots)) => Some(format!("{bots} read it.")),
        (Some(rounds), Some(bots)) => Some(format!("{rounds} and {bots} read it.")),
    }
}

// What another review round costs. Roughly: the average priced reviewer
// call times the calls a round makes. A round makes more than one call
// when the reviewer takes a second look. A local round and a review bot's
// read are rounds too, but make no call. None before a call was priced.
fn round_cost(item: &WorkItem) -> Option<String> {
    let reviewer = || item.calls.iter().filter(|c| c.role == Role::Reviewer);
    let priced = || reviewer().filter(|call| !call.unpriced);
    let calls = priced().count();
    let spent = priced().fold(0_u64, |spent, call| spent.saturating_add(call.cost.0));
    if calls == 0 || spent == 0 {
        return None;
    }
    let bot_reads: u32 = item.bot_reads.values().sum();
    let sessions = (item.counts.review_rounds)
        .saturating_sub(item.qwen.rounds)
        .saturating_sub(bot_reads)
        .max(1);
    let per_round = (reviewer().count() as f64 / f64::from(sessions)).max(1.0);
    let each = Cost(spent).usd() / calls as f64 * per_round;
    // Two decimals would print a real, small cost as $0.00.
    Some(if each < 0.005 {
        "Another review round costs under $0.01.".to_owned()
    } else {
        format!("Another review round costs about ${each:.2}.")
    })
}

// The sentences that are there, as one line
fn join<const N: usize>(sentences: [Option<String>; N]) -> String {
    sentences
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::reviewed;
    use crate::ports::{SessionId, Severity};
    use crate::review_bot::Bot;
    use crate::state::{OpenThread, RulingKind};
    use crate::test::ticket;
    use crate::work_item::WorkItem;

    // A work item two review rounds and CodeRabbit read
    fn read_by_coderabbit() -> WorkItem {
        let mut item = WorkItem::new(
            ticket(7),
            "Title of #7",
            "kelpie/7",
            PathBuf::from("wt"),
            PathBuf::from("build"),
            "sonnet-high".to_owned().try_into().unwrap(),
            SessionId("s".to_owned()),
        );
        item.counts.review_rounds = 2;
        item.bot_reads.insert(Bot::Coderabbit, 1);
        item
    }

    fn merge_with(open_found: Vec<OpenThread>) -> RulingKind {
        RulingKind::Merge {
            head: "6b0606e".into(),
            unreviewed: None,
            open_threads: Some("2 from cubic".into()),
            held_by: Vec::new(),
            unread_head: false,
            note_fix: false,
            late_fix: false,
            nits: None,
            open_found,
            disputes: None,
            rivals: Vec::new(),
        }
    }

    fn thread(bot: &str) -> OpenThread {
        OpenThread {
            bot: bot.into(),
            level: Some("P1".into()),
            severity: Severity::High,
            file: "src/a.rs".into(),
            line: 4,
            what: "A token is shared.".into(),
        }
    }

    #[test]
    fn option_a_names_the_bot_whose_threads_are_open_beside_the_bot_that_read() {
        let kind = merge_with(vec![thread("cubic"), thread("cubic")]);

        assert_eq!(
            reviewed(&read_by_coderabbit(), &kind).as_deref(),
            Some("2 review rounds and coderabbit read it. cubic left 2 threads open on it.")
        );
    }

    #[test]
    fn option_a_says_who_read_alone_when_no_thread_is_listed() {
        assert_eq!(
            reviewed(&read_by_coderabbit(), &merge_with(Vec::new())).as_deref(),
            Some("2 review rounds and coderabbit read it.")
        );
    }
}
