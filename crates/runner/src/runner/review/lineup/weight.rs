//! How much review a work item's change gets
//!
//! With `reviews.light` below `reviews.need`, a change that touches only
//! `reviews.light_paths` runs the light count. Whatever else a change touches
//! runs the full one, and so does an adopted pull request, a diff that
//! names no file and an item that was sent a HIGH finding.

use std::path::Path;

use super::{changed_files, matches};
use crate::fence;
use crate::package_managers::MANAGERS;
use crate::ports::Severity;
use crate::runner::Runner;
use crate::settings::NonBlank;
use crate::work_item::{Heavier, Lighter, ReviewWeight, WorkItem};
use crate::workflows::is_workflow_file;

/// Where a repo's workflow files live
const WORKFLOWS: &str = ".github/workflows/";

impl Runner {
    /// How much review `item`'s pull request gets, from what `worktree`
    /// changes since `base`, or None when `reviews.light` is not below
    /// `reviews.need` and nothing is weighed
    ///
    /// # Errors
    ///
    /// Why the changed files could not be listed.
    pub(super) fn weigh(
        &self,
        item: &WorkItem,
        worktree: &Path,
        base: &str,
    ) -> Result<Option<ReviewWeight>, String> {
        let reviews = &self.settings.reviews;
        if reviews.light_count() >= reviews.need {
            return Ok(None);
        }
        let full = |why| Ok(Some(ReviewWeight::Full(why)));
        if item.adopted {
            return full(Heavier::Adopted);
        }
        if item.held.iter().any(|f| f.severity == Severity::High) {
            return full(Heavier::High);
        }
        let changed = changed_files(&self.settings.git.checkout, worktree, base)?;
        if changed.is_empty() {
            return full(Heavier::Empty);
        }
        if let Some(why) = changed.iter().find_map(|f| forced(f, &reviews.full_paths)) {
            return full(why);
        }
        let light = |file: &String| in_any(&reviews.light_paths, file);
        Ok(Some(match changed.iter().find(|file| !light(file)) {
            Some(file) => ReviewWeight::Full(Heavier::Touched(file.clone())),
            None => ReviewWeight::Light(Lighter::Docs),
        }))
    }

    /// `item` with its weight read again from the worktree's head, as a fix
    /// turn may have changed what the diff touches
    ///
    /// A diff git cannot take leaves the stored weight, and the next round
    /// reports the failure.
    pub(super) fn reweighed(&self, item: &WorkItem, base: &str) -> WorkItem {
        let weight = self.weigh(item, &item.worktree, base);
        WorkItem {
            weight: weight.unwrap_or_else(|_| item.weight.clone()),
            ..item.clone()
        }
    }

    /// How many reviewers must read `item`'s pull request: the light count
    /// for a change weighed light, and `reviews.need` otherwise
    pub(super) fn need_of(&self, item: &WorkItem) -> u32 {
        let reviews = &self.settings.reviews;
        match item.weight.as_ref().is_some_and(ReviewWeight::is_light) {
            true => reviews.light_count().get(),
            false => reviews.need.get(),
        }
    }
}

// Why `file` always gets the full count, if it does: a workflow, a lockfile or
// manifest at any depth, a harness's own file, or one `full_paths` names.
fn forced(file: &str, full_paths: &[NonBlank]) -> Option<Heavier> {
    let path = Path::new(file);
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or(file);
    if file.starts_with(WORKFLOWS) && is_workflow_file(file) {
        return Some(Heavier::Ci);
    }
    let packages = |m: &crate::package_managers::PackageManager| {
        (m.lockfiles.iter().chain(m.manifests)).any(|known| *known == name)
    };
    if MANAGERS.iter().any(packages) {
        return Some(Heavier::Dependencies);
    }
    if fence::owner(path).is_some() {
        return Some(Heavier::AgentFiles);
    }
    in_any(full_paths, file).then(|| Heavier::FullPaths(file.to_owned()))
}

// Whether `file` matches any of `globs`, the way a reviewer's `paths` do.
fn in_any(globs: &[NonBlank], file: &str) -> bool {
    globs.iter().any(|glob| matches(glob.as_str(), file))
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::super::super::several::{
        allow_reviewers, at_review, reviewer_models, reviewers_until_ci, two_local,
    };
    use crate::agents::Tier;
    use crate::ports::{Finding, Severity};
    use crate::runner::{Runner, step};
    use crate::test::{Rig, Scripted, ScriptedRound};
    use crate::work_item::{Heavier, Lighter, ReviewWeight};

    // `two_local`'s project over candidates `mine`, `other` and `claude`,
    // with `reviews.need` 2 and `extra` after it in the `reviews` table.
    fn project(extra: &str) -> Rig {
        let rig = two_local();
        rig.reviewers(&["mine", "other", "claude"]);
        allow_reviewers(&rig, &["mine", "other", "claude"]);
        rig.reviews_need(Some(2));
        rig.edit_settings(|text| format!("{text}{extra}"));
        rig
    }

    fn weight_of(runner: &Mutex<Runner>) -> Option<ReviewWeight> {
        let runner = runner.lock().unwrap();
        runner.current().unwrap().weight.clone()
    }

    // The weight of the pull request with `files` committed on top of what
    // the worker pushed, which is left as it was.
    fn weighed_with(
        runner: &Mutex<Runner>,
        files: &[&str],
    ) -> Result<Option<ReviewWeight>, String> {
        let runner = runner.lock().unwrap();
        let item = runner.current().unwrap().clone();
        for file in files {
            let path = item.worktree.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "changed\n").unwrap();
        }
        crate::test::git(&item.worktree, &["add", "-A", "-f"]);
        crate::test::git(&item.worktree, &["commit", "--quiet", "-m", "more"]);
        let base = item.review_base(runner.item_base(&item)).to_owned();
        let weight = runner.weigh(&item, &item.worktree, &base);
        crate::test::git(&item.worktree, &["reset", "--quiet", "--hard", "HEAD~1"]);
        weight
    }

    #[test]
    fn a_docs_only_diff_runs_the_light_count_and_goes_to_ci() {
        let rig = project("light = 1\n");
        let runner = at_review(&rig, "README.md");

        reviewers_until_ci(&rig, &runner);

        assert_eq!(rig.reviewer.seen().len() + reviewer_models(&rig).len(), 1);
        let phase = rig.ask(&runner, "status", None)["work_item"]["phase"]["state"].clone();
        assert_eq!(phase, "ci");
        assert_eq!(weight_of(&runner), Some(ReviewWeight::Light(Lighter::Docs)));
    }

    #[test]
    fn a_diff_touching_src_runs_the_full_count() {
        let rig = project("light = 1\n");
        let runner = at_review(&rig, "src/lib.rs");

        reviewers_until_ci(&rig, &runner);

        assert_eq!(rig.reviewer.seen().len() + reviewer_models(&rig).len(), 2);
        let touched = Heavier::Touched("src/lib.rs".into());
        assert_eq!(weight_of(&runner), Some(ReviewWeight::Full(touched)));
    }

    #[test]
    fn a_workflow_a_manifest_a_lockfile_an_agent_file_and_a_full_path_each_force_the_full_count() {
        let rig = project("light = 1\nfull_paths = [\"legal/**\"]\n");
        let runner = at_review(&rig, "README.md");
        let cases = [
            (".github/workflows/ci.yml", Heavier::Ci),
            (".github/workflows/release.yaml", Heavier::Ci),
            ("Cargo.lock", Heavier::Dependencies),
            ("web/package.json", Heavier::Dependencies),
            ("crates/a/Cargo.toml", Heavier::Dependencies),
            (".mcp.json", Heavier::AgentFiles),
            (".claude/skills/x.md", Heavier::AgentFiles),
            (
                "legal/terms.md",
                Heavier::FullPaths("legal/terms.md".into()),
            ),
        ];
        for (file, why) in cases {
            let weight = weighed_with(&runner, &[file]).unwrap();
            assert_eq!(weight, Some(ReviewWeight::Full(why)), "{file}");
        }
        let docs = weighed_with(&runner, &["docs/a/b.md", "notes.md"]).unwrap();
        assert_eq!(docs, Some(ReviewWeight::Light(Lighter::Docs)));
    }

    #[test]
    fn a_full_path_beats_a_light_path() {
        let rig = project("light = 1\nfull_paths = [\"docs/adr/**\"]\n");
        let runner = at_review(&rig, "README.md");

        let weight = weighed_with(&runner, &["docs/adr/0001.md", "docs/a.md"]).unwrap();

        let forced = Heavier::FullPaths("docs/adr/0001.md".into());
        assert_eq!(weight, Some(ReviewWeight::Full(forced)));
    }

    #[test]
    fn light_paths_name_what_a_docs_only_diff_may_touch() {
        let touched = |file: &str| Some(ReviewWeight::Full(Heavier::Touched(file.into())));
        let rig = project("light = 1\nlight_paths = [\"**/*.rst\"]\n");
        let runner = at_review(&rig, "guide.rst");
        let rst = weighed_with(&runner, &["a/b.rst"]).unwrap();
        let md = weighed_with(&runner, &["README.md"]).unwrap();
        assert_eq!(rst, Some(ReviewWeight::Light(Lighter::Docs)));
        assert_eq!(md, touched("README.md"));

        let rig = project("light = 1\nlight_paths = []\n");
        let runner = at_review(&rig, "README.md");
        let none = weighed_with(&runner, &["docs/a.md"]).unwrap();
        assert_eq!(none, touched("README.md"), "[] turns path lightening off");
    }

    #[test]
    fn an_adopted_item_and_an_empty_diff_run_the_full_count() {
        let rig = project("light = 1\n");
        let runner = at_review(&rig, "README.md");
        let guard = runner.lock().unwrap();
        let mut item = guard.current().unwrap().clone();
        let (worktree, base) = (item.worktree.clone(), guard.item_base(&item).to_owned());

        assert_eq!(
            guard.weigh(&item, &worktree, "HEAD").unwrap(),
            Some(ReviewWeight::Full(Heavier::Empty))
        );
        item.adopted = true;
        assert_eq!(
            guard.weigh(&item, &worktree, &base).unwrap(),
            Some(ReviewWeight::Full(Heavier::Adopted))
        );
    }

    #[test]
    fn a_diff_git_cannot_take_is_an_error_and_not_a_weight() {
        let rig = project("light = 1\n");
        let runner = at_review(&rig, "README.md");
        let guard = runner.lock().unwrap();
        let item = guard.current().unwrap().clone();

        let err = guard
            .weigh(&item, &item.worktree, "no-such-ref")
            .unwrap_err();

        assert!(err.contains("git diff failed"), "{err}");
    }

    #[test]
    fn a_light_equal_to_need_weighs_nothing() {
        for extra in ["light = 2\n", "light = 5\n", ""] {
            let rig = project(extra);
            let runner = at_review(&rig, "README.md");

            reviewers_until_ci(&rig, &runner);

            assert_eq!(rig.reviewer.seen().len() + reviewer_models(&rig).len(), 2);
            assert_eq!(weight_of(&runner), None, "{extra:?}");
        }
    }

    #[test]
    fn a_light_items_pick_asks_at_tier_light_and_a_full_items_asks_its_own() {
        for (file, tier) in [("README.md", Tier::Light), ("src/lib.rs", Tier::Heavy)] {
            let rig = project("light = 1\n");
            let runner = at_review(&rig, file);
            runner.lock().unwrap().state.work_items[0].tier = Some(Tier::Heavy);

            step(&runner).unwrap();

            let ask = rig.agent_board.asks().pop().unwrap();
            assert_eq!(ask.tier, Some(tier), "{file}");
        }
    }

    // A light item whose first reviewer finds `severity` in the README and
    // whose fix pushes `fix`, run until the pass is over.
    fn found_then_fixed(
        severity: Severity,
        fix: (&'static str, &'static str),
    ) -> (Rig, Mutex<Runner>) {
        let rig = project("light = 1\n");
        let runner = at_review(&rig, "README.md");
        rig.agent_board.ranks(&["mine", "other", "claude"]);
        let finding = Finding::new(severity, "README.md", 1, "wrong", "it is");
        rig.reviewer
            .script([ScriptedRound::Findings(vec![finding])]);
        rig.claude.script([Scripted::Push(fix.0, fix.1)]);
        reviewers_until_ci(&rig, &runner);
        (rig, runner)
    }

    #[test]
    fn a_fix_that_touches_src_sends_a_light_pass_on_to_the_full_count() {
        let (rig, runner) = found_then_fixed(Severity::Medium, ("src/lib.rs", "fixed\n"));

        assert_eq!(
            rig.reviewer.seen().len(),
            2,
            "the fix is read by a second reviewer"
        );
        let touched = Heavier::Touched("src/lib.rs".into());
        assert_eq!(weight_of(&runner), Some(ReviewWeight::Full(touched)));
    }

    #[test]
    fn a_high_finding_from_the_light_reviewer_sends_the_pass_on_to_the_full_count() {
        let (rig, runner) = found_then_fixed(Severity::High, ("README.md", "fixed\n"));

        assert_eq!(
            rig.reviewer.seen().len(),
            2,
            "a docs-only fix is still read twice after a HIGH"
        );
        assert_eq!(weight_of(&runner), Some(ReviewWeight::Full(Heavier::High)));
    }

    #[test]
    fn a_medium_finding_and_a_docs_only_fix_keep_the_pass_light() {
        let (rig, runner) = found_then_fixed(Severity::Medium, ("README.md", "fixed\n"));

        assert_eq!(rig.reviewer.seen().len(), 1);
        assert_eq!(weight_of(&runner), Some(ReviewWeight::Light(Lighter::Docs)));
    }

    #[test]
    fn a_pass_stepped_up_asks_its_next_reviewer_at_the_items_own_tier() {
        let rig = project("light = 1\n");
        let runner = at_review(&rig, "README.md");
        runner.lock().unwrap().state.work_items[0].tier = Some(Tier::Heavy);
        rig.agent_board.ranks(&["mine", "other", "claude"]);
        let finding = Finding::new(Severity::High, "README.md", 1, "wrong", "it is");
        rig.reviewer
            .script([ScriptedRound::Findings(vec![finding])]);
        rig.claude.script([Scripted::Push("README.md", "fixed\n")]);

        reviewers_until_ci(&rig, &runner);

        let tiers: Vec<_> = rig.agent_board.asks().iter().map(|a| a.tier).collect();
        let first = tiers.iter().position(|t| *t == Some(Tier::Light));
        let after = first.map(|at| tiers[at + 1..].to_vec());
        assert!(first.is_some(), "{tiers:?}");
        let heavy = Some(Tier::Heavy);
        assert!(after.unwrap().iter().all(|t| *t == heavy), "{tiers:?}");
    }
}
