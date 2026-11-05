//! Who reviews each round
//!
//! Each round goes to the agent board's pick among the candidates the pass has
//! not run that can run: not one limited to paths the pull request leaves
//! alone, a local one that read nothing twice running, or a bot that has made
//! its `rounds`. It takes the first ranked one the pacer lets start and, for a
//! bot, whose window is open now. A bot left out as one the repo cannot run is
//! in the ask's leave-out for a week, and no candidate a pass that read nothing
//! names as refused. The pass ends at `reviews.need` reads or when the board
//! ranks none left. A kept reviewer resumes its round.

use std::path::Path;
use std::process::Stdio;

use super::super::Runner;
use super::super::pace::Walked;
use super::super::report::Begin;
use crate::board::briefing::date_time;
use crate::lease::door::PickAsk;
use crate::pacer::Scope;
use crate::ports::Timestamp;
use crate::settings::{AgentName, ListedReviewer, NonBlank};
use crate::state::StateError;
use crate::work_item::{BotSkipped, Phase, Review, ReviewWeight, WorkItem};

mod weight;

/// What a round's step does about who reads it
pub(super) enum Chosen {
    /// This reviewer reads
    Reviewer(ListedReviewer),
    /// The pass is over
    Ended,
    /// The board ranked none of the candidates it was asked to rank, which
    /// could run. One kelpie itself left out is not among them.
    Unranked(Vec<AgentName>),
    /// The step does this instead, and is made again
    Instead(Box<Begin>),
    /// The changed files could not be listed, for this reason
    Failed(String),
}

/// Where a round leaves a work item, with the weight read for deciding it
pub(in crate::runner) struct RoundNext {
    phase: Phase,
    // None leaves the item's weight as it is.
    weight: Option<Option<ReviewWeight>>,
}

impl RoundNext {
    /// `phase`, which read no weight
    pub(in crate::runner) fn of(phase: Phase) -> Self {
        Self {
            phase,
            weight: None,
        }
    }

    /// Saves the phase and the weight it was decided on into `item`
    pub(in crate::runner) fn apply(self, item: &mut WorkItem) {
        item.phase = self.phase;
        if let Some(weight) = self.weight {
            item.weight = weight;
        }
    }
}

impl Runner {
    /// Who reviews `review`'s round in `worktree`, diffed from `base`: the
    /// board's first pick the pass has not run that the pacer lets start and,
    /// for a review bot, whose window is open, or that the pass is over
    ///
    /// A board that cannot be reached picks nobody, and the round asks again.
    pub(super) fn choose_reviewer(
        &mut self,
        review: &Review,
        worktree: &Path,
        base: &str,
    ) -> Result<Chosen, StateError> {
        if let Some(reviewer) = review.reviewer.as_ref().and_then(|n| self.listed(n)) {
            return Ok(Chosen::Reviewer(reviewer));
        }
        let due = match self.due(review, worktree, base) {
            Ok(due) => due,
            Err(reason) => return Ok(Chosen::Failed(reason)),
        };
        if let Some(item) = self.current() {
            match self.weigh(item, worktree, base) {
                Err(reason) => return Ok(Chosen::Failed(reason)),
                Ok(weight) if weight != item.weight => {
                    self.update(|item| item.weight = weight)?;
                }
                Ok(_) => {}
            }
        }
        let Some(item) = self.current().filter(|_| !self.enough_read(review)) else {
            return Ok(Chosen::Ended);
        };
        let Some(number) = item.pull_request else {
            return Ok(Chosen::Ended);
        };
        let left_out = self.left_out_of(review);
        let ask = PickAsk {
            kind: item.kind.clone(),
            tier: item
                .weight
                .as_ref()
                .map_or(item.tier, |w| w.tier(item.tier)),
            pin: None,
            slack: Some(self.slack(item.slack)),
            agents: due.iter().map(|r| r.name.clone()).collect(),
            leave_out: left_out.clone(),
        };
        let Ok(picks) = self.board_picks(&ask) else {
            return Ok(Chosen::Instead(Box::new(Begin::Idle)));
        };
        let now = self.ports.clock.now();
        let ranked: Vec<AgentName> = picks.into_iter().map(|pick| pick.agent).collect();
        if ranked.is_empty() {
            // A bot kelpie left out is none the board refused, so it is not a
            // candidate the maintainer is asked about. `unread` names it.
            let refused = (ask.agents.iter())
                .filter(|name| !left_out.contains(name))
                .cloned()
                .collect();
            return Ok(Chosen::Unranked(refused));
        }
        // A bot whose window is closed is passed over, and not waited on.
        if let Some((first, opens)) = ranked
            .first()
            .and_then(|first| Some((first, self.window_opens(first, now)?)))
        {
            let reviewer = first.clone();
            self.update(|item| {
                if let Phase::Review(round) = &mut item.phase {
                    round.reviewer = Some(reviewer.clone());
                }
            })?;
            let skipped = BotSkipped::Window {
                reviewer: first.clone(),
                opens,
            };
            return self
                .pass_over(number, skipped)
                .map(|begin| Chosen::Instead(Box::new(begin)));
        }
        let open: Vec<&AgentName> = (ranked.iter())
            .filter(|agent| self.window_opens(agent, now).is_none())
            .collect();
        if open.is_empty() {
            return Ok(Chosen::Ended);
        }
        Ok(match self.first_unpaced(open, Scope::Turn)? {
            Walked::Opens(name) => (self.listed(&name)).map_or(Chosen::Ended, Chosen::Reviewer),
            Walked::Reads(reads) => Chosen::Instead(Box::new(reads)),
            Walked::Held { first, .. } => Chosen::Instead(Box::new(first.unwrap_or(Begin::Idle))),
        })
    }

    // The candidates `review`'s pass has not run that can run now.
    fn due(
        &self,
        review: &Review,
        worktree: &Path,
        base: &str,
    ) -> Result<Vec<ListedReviewer>, String> {
        let ran = self.ran_of(review);
        let changed = match self.lineup.iter().any(|r| !r.paths.is_empty()) {
            true => changed_files(&self.settings.git.checkout, worktree, base)?,
            false => Vec::new(),
        };
        let down = |r: &ListedReviewer| {
            let item = self.current();
            item.is_some_and(|item| item.local_reviewer_down(&r.name))
        };
        // A bot that owes an adopted pull request a summon reads whatever its reads.
        let read_out = |r: &ListedReviewer| {
            let (Some(bot), Some(item)) = (r.bot(), self.current()) else {
                return false;
            };
            let reads = item.bot_reads.get(&bot.bot).copied().unwrap_or(0);
            let owed = item.summons_owed.contains(&bot.bot);
            !owed && bot.rounds.is_some_and(|rounds| reads >= rounds.get())
        };
        let runs = |r: &ListedReviewer| {
            let touched = touches(&r.paths, &changed);
            let kind = !review.bots_only || r.bot().is_some();
            kind && touched && !(r.is_local() && down(r)) && !read_out(r)
        };
        let due = |r: &&ListedReviewer| !ran.contains(&r.name) && runs(r);
        Ok(self.lineup.iter().filter(due).cloned().collect())
    }

    // Whether `reviews.need` reviewers have read in `review`'s pass.
    fn enough_read(&self, review: &Review) -> bool {
        let enough = |item: &WorkItem| self.reads(review) >= self.need_of(item);
        self.current().is_some_and(enough)
    }

    // How many reviewers have read in `review`'s pass: those whose round ran,
    // but a bot passed over, a reviewer that kept failing, or a local round
    // that reviewed no file.
    fn reads(&self, review: &Review) -> u32 {
        self.current().map_or(0, |item| self.reads_in(item, review))
    }

    fn reads_in(&self, item: &WorkItem, review: &Review) -> u32 {
        u32::try_from(self.readers(item, review).len()).unwrap_or(u32::MAX)
    }

    fn readers(&self, item: &WorkItem, review: &Review) -> Vec<AgentName> {
        let counts = |name: &AgentName| {
            let passed_over = item.bots_skipped.iter().any(|s| s.reviewer() == name);
            !passed_over
                && !item.reviewers_skipped.contains(name)
                && !item.reviewers_blank.contains(name)
        };
        let mut ran = self.ran_of(review);
        ran.retain(counts);
        ran
    }

    /// Why `review`'s pass ends, when `pass_ended` is to hold the work item
    /// for a ruling: `Some(None)` when the project's own list left it no
    /// reader, `Some(Some(reason))` when a reviewer was down or passed over,
    /// the agent board ranked none of `unranked`, or too few read for `reviews.need`
    pub(super) fn pass_unread(
        &self,
        item: &WorkItem,
        review: &Review,
        unranked: &[AgentName],
    ) -> Option<Option<String>> {
        let board = (!unranked.is_empty()).then(|| {
            let names: Vec<_> = unranked.iter().map(ToString::to_string).collect();
            format!("the agent board ranked none of {}", names.join(", "))
        });
        let joined = |missed: Option<String>| match (missed, board.clone()) {
            (Some(missed), Some(board)) => Some(format!("{missed}, and {board}")),
            (missed, board) => missed.or(board),
        };
        if review.unread {
            return Some(joined(self.missed(item, review, true)));
        }
        let (reads, need) = (self.reads_in(item, review), self.need_of(item));
        if review.bots_only || reads >= need {
            return None;
        }
        let why = joined(self.missed(item, review, false))?;
        Some(Some(format!("{reads} of {need} reviewers read it: {why}")))
    }

    /// When the window of `reviewer`, if a review bot, opens, while it is closed at `now`
    pub(in crate::runner) fn window_opens(
        &self,
        reviewer: &AgentName,
        now: Timestamp,
    ) -> Option<Timestamp> {
        let bot = self.listed(reviewer).and_then(|r| r.bot())?;
        self.ports.leases.opens(&bot.bot.lease(), bot.window, now)
    }

    // The reviewers `review`'s pass has run. A state file saved before `ran`
    // names only the last, and its pass ran the list in order up to it.
    fn ran_of(&self, review: &Review) -> Vec<AgentName> {
        let Some(last) = review.last.as_ref().filter(|_| review.ran.is_empty()) else {
            return review.ran.clone();
        };
        match self.lineup.iter().position(|r| &r.name == last) {
            Some(at) => self.lineup[..=at].iter().map(|r| r.name.clone()).collect(),
            None => vec![last.clone()],
        }
    }

    // Who the board is not to pick for `review`'s pass: the reviewers its pass
    // has run, and every bot the project has left out as one its repo cannot
    // run, within [`crate::state::BOT_LEFT_OUT`] of when it was last left out.
    fn left_out_of(&self, review: &Review) -> Vec<AgentName> {
        let mut left_out = self.ran_of(review);
        let now = self.ports.clock.now();
        for left in &self.state.bots_left_out {
            if left.holds(now) && !left_out.contains(&left.reviewer) {
                left_out.push(left.reviewer.clone());
            }
        }
        left_out
    }

    /// Where the review goes once `review`'s reviewer is done: the next
    /// round, or CI at `now` once `reviews.need` reviewers have read or no
    /// candidate is left to run
    ///
    /// A pass with a bot it went on without ends in a round step of its own,
    /// which first reads whether that bot reviewed the head since.
    pub(in crate::runner) fn after_round(&self, review: Review, now: Timestamp) -> RoundNext {
        match self.current() {
            Some(item) => self.after_round_in(item, review, now),
            None => RoundNext::of(Phase::Review(
                Review {
                    ran: self.ran_of(&review),
                    ..review
                }
                .next_round(),
            )),
        }
    }

    /// [`Self::after_round`] for `item` as it will be saved, with the
    /// reviewer just done counted as passed over, if it was
    ///
    /// The caller saves the weight it carries, read again from the worktree,
    /// with the phase. A pass that ends short of `reviews.need` for want of a
    /// reviewer goes on to a round step of its own, which marks the work item
    /// unreviewed.
    pub(in crate::runner) fn after_round_in(
        &self,
        item: &WorkItem,
        review: Review,
        now: Timestamp,
    ) -> RoundNext {
        let ran = self.ran_of(&review);
        let next = Review { ran, ..review }.next_round();
        let base = item.review_base(self.item_base(item));
        // The fix turn that just ended may have changed what the diff touches.
        let (item, read) = self.reweighed(item, &base);
        let item = &item;
        // A diff git could not take is the next round's step to report, and a
        // pass no reviewer read ends in a step of its own, which says so.
        let enough = self.reads_in(item, &next) >= self.need_of(item);
        let over = enough
            || self
                .due(&next, &item.worktree, &base)
                .is_ok_and(|due| due.is_empty());
        let short = self.pass_unread(item, &next, &[]).is_some();
        // Only a bot passed over before this round makes one of its own.
        let skipped_before = self.current().is_some_and(|i| !i.bots_skipped.is_empty());
        let phase = match over && !next.unread && !skipped_before && !short {
            true => Phase::Ci {
                head: None,
                since: now,
            },
            false => Phase::Review(next),
        };
        RoundNext {
            phase,
            weight: read,
        }
    }

    /// Why no reviewer of `review`'s pass read `item`'s pull request, naming
    /// each listed one that was down, left out as one the repo cannot run, or
    /// ran and read nothing, or none when every listed one was passed over by
    /// its own `paths`
    fn missed(&self, item: &WorkItem, review: &Review, with_readers: bool) -> Option<String> {
        let ran = self.ran_of(review);
        let readers = self.readers(item, review);
        let now = self.ports.clock.now();
        let missed: Vec<String> = (self.lineup.iter())
            .filter_map(|r| {
                let name = &r.name;
                let skipped = item.bots_skipped.iter().find(|s| s.reviewer() == name);
                if let Some(skipped) = skipped.filter(|_| ran.contains(name)) {
                    return Some(format!("{name} was passed over: {}", skipped.why()));
                }
                if readers.contains(name) && !with_readers {
                    return None;
                }
                if ran.contains(name) {
                    let why = match item.reviewers_skipped.contains(name) {
                        true => "was passed over after its calls kept failing",
                        false => "reviewed no file",
                    };
                    return Some(format!("{name} {why}"));
                }
                if r.is_local() && item.local_reviewer_down(name) {
                    return Some(format!("{name} is down for this work item"));
                }
                let left = self.state.bot_left_out(name, now)?;
                let (until, why) = (date_time(left.back_in()), &left.why);
                Some(format!(
                    "{name} is left out of every pick until {until}: {why}"
                ))
            })
            .collect();
        (!missed.is_empty()).then(|| missed.join(", "))
    }

    /// Whether `review`'s round is a local reviewer's
    pub(super) fn is_local_round(&self, review: &Review) -> bool {
        match &review.reviewer {
            Some(name) => self.listed(name).is_some_and(|r| r.is_local()),
            // An older state file ran the local round on odd rounds.
            None => {
                review.round % 2 == 1 && self.lineup.first().is_some_and(ListedReviewer::is_local)
            }
        }
    }

    // The listed reviewer named `name`.
    pub(in crate::runner) fn listed(&self, name: &AgentName) -> Option<ListedReviewer> {
        self.lineup.iter().find(|r| &r.name == name).cloned()
    }
}

/// The files `worktree` changes from `base`, as git names them
pub(in crate::runner) fn changed_files(
    repo: &Path,
    worktree: &Path,
    base: &str,
) -> Result<Vec<String>, String> {
    let output = crate::worktree::trusted_command(repo, worktree)
        .map_err(|e| e.to_string())?
        .args(["diff", "--name-only", "--no-renames"])
        .arg(crate::worktree::change_since(base))
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run git diff: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "git diff failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let names = String::from_utf8_lossy(&output.stdout);
    Ok(names.lines().map(str::to_owned).collect())
}

/// Whether a change to the `changed` files is one a reviewer limited to
/// `paths` reads: any change when `paths` is empty
pub(in crate::runner) fn touches(paths: &[NonBlank], changed: &[String]) -> bool {
    paths.is_empty()
        || changed
            .iter()
            .any(|file| paths.iter().any(|glob| matches(glob.as_str(), file)))
}

/// Whether `path` matches `glob`: `*` and `?` stay inside one folder,
/// `**` spans folders, and `**/` may match none
pub(super) fn matches(glob: &str, path: &str) -> bool {
    matches_bytes(glob.as_bytes(), path.as_bytes())
}

fn matches_bytes(glob: &[u8], path: &[u8]) -> bool {
    match glob {
        [] => path.is_empty(),
        [b'*', b'*', b'/', rest @ ..] => {
            matches_bytes(rest, path)
                || path
                    .iter()
                    .enumerate()
                    .any(|(i, b)| *b == b'/' && matches_bytes(rest, &path[i + 1..]))
        }
        [b'*', b'*', rest @ ..] => (0..=path.len()).any(|i| matches_bytes(rest, &path[i..])),
        [b'*', rest @ ..] => {
            let folder = path.iter().position(|b| *b == b'/').unwrap_or(path.len());
            (0..=folder).any(|i| matches_bytes(rest, &path[i..]))
        }
        [b'?', rest @ ..] => path
            .first()
            .is_some_and(|b| *b != b'/' && matches_bytes(rest, &path[1..])),
        [c, rest @ ..] => path.first() == Some(c) && matches_bytes(rest, &path[1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_glob_keeps_single_stars_inside_one_folder() {
        assert!(matches("*.lock", "Cargo.lock"));
        assert!(!matches("*.lock", "sub/Cargo.lock"));
        assert!(matches("**/*.lock", "sub/deeper/Cargo.lock"));
        assert!(matches("**/*.lock", "Cargo.lock"));
        assert!(matches("docs/**", "docs/a/b.md"));
        assert!(matches("a?c", "abc"));
        assert!(!matches("a?c", "a/c"));
        assert!(!matches("Cargo.lock", "Cargo.lockx"));
    }

    #[test]
    fn main_gaining_a_file_after_the_branch_is_cut_does_not_select_a_reviewer() {
        let home = tempfile::tempdir().unwrap();
        let (repo, worktree) = crate::test::linked_worktree(home.path());
        crate::test::main_commits(&repo, "README.md", "readme\n");
        crate::test::cut_from_main(&worktree);
        std::fs::write(worktree.join("work.txt"), "work\n").unwrap();
        crate::test::git(&worktree, &["add", "."]);
        crate::test::git(&worktree, &["commit", "--quiet", "-m", "work"]);
        crate::test::main_commits(&repo, "only_main.txt", "only main has this\n");

        let changed = changed_files(&repo, &worktree, "origin/main").unwrap();

        assert_eq!(changed, ["work.txt"]);
    }
}
