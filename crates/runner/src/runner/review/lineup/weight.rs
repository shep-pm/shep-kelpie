//! How much review a work item's change reports, and how many reads it gets
//!
//! `weigh` reads what a diff touches and reports it, so the project manager
//! has it in front of it when it plans the reads. It decides nothing: the pass
//! runs the plan the project manager set at its pick, and where a project
//! names no project manager it runs kelpie's own rule, [`RULE_READS`] reads.

use std::path::Path;

use super::{changed_files, matches};
use crate::fence;
use crate::package_managers::MANAGERS;
use crate::runner::Runner;
use crate::work_item::{Heavier, Lighter, ReviewWeight, WorkItem};
use crate::workflows::is_workflow_file;

/// Where a repo's workflow files live
const WORKFLOWS: &str = ".github/workflows/";

/// The files a docs-only change touches, so a diff naming only these reports
/// `light (docs only)`
const LIGHT_PATHS: [&str; 2] = ["**/*.md", "docs/**"];

/// The reads kelpie's own rule gives a work item, on a project that names no
/// project manager to plan one
pub(in crate::runner) const RULE_READS: u32 = 2;

impl Runner {
    /// What `item`'s pull request touches, from what `worktree` changes since
    /// `base`, or None where the project names no `agents.pm` and nothing is
    /// weighed
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
        if self.agents.pm.is_none() {
            return Ok(None);
        }
        let full = |why| Ok(Some(ReviewWeight::Full(why)));
        if item.adopted {
            return full(Heavier::Adopted);
        }
        let changed = changed_files(&self.settings.git.checkout, worktree, base)?;
        if changed.is_empty() {
            return full(Heavier::Empty);
        }
        if let Some(why) = changed.iter().find_map(|f| forced(f)) {
            return full(why);
        }
        let light = |file: &String| in_any(&LIGHT_PATHS, file);
        let Some(file) = changed.iter().find(|file| !light(file)) else {
            return Ok(Some(ReviewWeight::Light(Lighter::Docs)));
        };
        Ok(Some(ReviewWeight::Full(Heavier::Touched(file.clone()))))
    }

    /// `item` with its weight read again from the worktree's head, as a fix
    /// turn may have changed what the diff touches, or None when git cannot
    /// take the diff
    pub(super) fn reweighed(&self, item: &WorkItem, base: &str) -> Option<WorkItem> {
        let weight = self.weigh(item, &item.worktree, base).ok()?;
        Some(WorkItem {
            weight,
            ..item.clone()
        })
    }

    /// How many reviewers must read `item`'s pull request: the plan's reads,
    /// else kelpie's own rule for an adopted item, since a stranger may have
    /// written it, else one read on a project that names `agents.pm`, which
    /// plans its reads, else kelpie's own rule
    pub(in crate::runner) fn need_of(&self, item: &WorkItem) -> u32 {
        if let Some(plan) = &item.review_plan {
            return plan.reads.get();
        }
        if item.adopted {
            return self.rule_reads;
        }
        match self.agents.pm {
            Some(_) => 1,
            None => self.rule_reads,
        }
    }
}

// Why `file` always reads full: a workflow, a lockfile or manifest at any
// depth, or a harness's own file.
fn forced(file: &str) -> Option<Heavier> {
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
    None
}

// Whether `file` matches any of `globs`, the way a reviewer's `paths` do.
fn in_any(globs: &[&str], file: &str) -> bool {
    globs.iter().any(|glob| matches(glob, file))
}

#[cfg(any(test, feature = "test-support"))]
impl Runner {
    /// The reads kelpie's own rule gives a work item is set to `reads`, which
    /// only the rig does
    pub fn set_rule_reads(&mut self, reads: u32) {
        self.rule_reads = reads;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::super::super::several::{
        at_review, pm_listing, pm_two_local, reviewer_models, reviewers_until_ci, two_local,
    };
    use crate::ports::{Finding, Severity};
    use crate::runner::Runner;
    use crate::settings::AgentName;
    use crate::test::{Rig, Scripted, ScriptedRound};
    use crate::work_item::{Heavier, Lighter, Phase, Review, ReviewPlan, ReviewWeight};

    // A project that names `agents.pm`, whose candidates are `mine`, `other`
    // and `claude`, and whose reads kelpie's own rule counts.
    fn pm_project() -> Rig {
        pm_two_local()
    }

    fn weight_of(runner: &Mutex<Runner>) -> Option<ReviewWeight> {
        let runner = runner.lock().unwrap();
        runner.current().unwrap().weight.clone()
    }

    // The current work item's plan of `reads` reads
    fn plan(runner: &Mutex<Runner>, reads: u32) {
        let mut runner = runner.lock().unwrap();
        let reads = std::num::NonZeroU32::new(reads).expect("a plan reads at least once");
        runner.state.work_items[0].review_plan = Some(ReviewPlan { reads, about: None });
    }

    // The weight of the pull request with `files` committed on top of what
    // the worker pushed, which is left as it was.
    fn weighed_with(
        runner: &Mutex<Runner>,
        files: &[&str],
    ) -> Result<Option<ReviewWeight>, String> {
        let written: Vec<(&str, &[u8])> = files.iter().map(|f| (*f, &b"changed\n"[..])).collect();
        weighed_writing(runner, &written)
    }

    // The weight of the pull request with each of `files` written on top of
    // what the worker pushed.
    fn weighed_writing(
        runner: &Mutex<Runner>,
        files: &[(&str, &[u8])],
    ) -> Result<Option<ReviewWeight>, String> {
        let runner = runner.lock().unwrap();
        let item = runner.current().unwrap().clone();
        for (file, content) in files {
            let path = item.worktree.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
        crate::test::git(&item.worktree, &["add", "-A", "-f"]);
        crate::test::git(
            &item.worktree,
            &["commit", "--quiet", "--allow-empty", "-m", "more"],
        );
        let base = item.review_base(runner.item_base(&item)).to_owned();
        let weight = runner.weigh(&item, &item.worktree, &base);
        crate::test::git(&item.worktree, &["reset", "--quiet", "--hard", "HEAD~1"]);
        weight
    }

    fn touched(file: &str) -> Option<ReviewWeight> {
        Some(ReviewWeight::Full(Heavier::Touched(file.into())))
    }

    #[test]
    fn a_project_with_no_project_manager_reads_twice_with_no_setting() {
        let rig = two_local();
        let runner = at_review(&rig, "work.txt");

        reviewers_until_ci(&rig, &runner);

        assert_eq!(rig.reviewer.seen().len(), 2, "mine and other read");
        assert!(reviewer_models(&rig).is_empty(), "claude was not needed");
    }

    #[test]
    fn a_project_with_no_project_manager_weighs_nothing() {
        let rig = two_local();
        let runner = at_review(&rig, "README.md");

        reviewers_until_ci(&rig, &runner);

        assert_eq!(weight_of(&runner), None, "nothing is weighed");
        let shown = rig.ask(&runner, "status", None)["work_item"]["review"].clone();
        assert_eq!(shown, serde_json::Value::Null, "{shown}");
    }

    #[test]
    fn a_docs_only_diff_reports_light_and_runs_the_plans_reads() {
        let rig = pm_project();
        let runner = at_review(&rig, "README.md");
        plan(&runner, 2);

        reviewers_until_ci(&rig, &runner);

        assert_eq!(rig.reviewer.seen().len(), 2, "the plan's two reads ran");
        assert_eq!(
            weight_of(&runner),
            Some(ReviewWeight::Light(Lighter::Docs)),
            "the weight reports and decides nothing"
        );
        let phase = rig.ask(&runner, "status", None)["work_item"]["phase"]["state"].clone();
        assert_eq!(phase, "ci");
    }

    #[test]
    fn a_workflow_a_manifest_a_lockfile_and_an_agent_file_each_report_full() {
        let rig = pm_project();
        let runner = at_review(&rig, "README.md");
        let cases = [
            (".github/workflows/ci.yml", Heavier::Ci),
            (".github/workflows/release.yaml", Heavier::Ci),
            ("Cargo.lock", Heavier::Dependencies),
            ("web/package.json", Heavier::Dependencies),
            ("crates/a/Cargo.toml", Heavier::Dependencies),
            (".mcp.json", Heavier::AgentFiles),
            (".claude/skills/x.md", Heavier::AgentFiles),
        ];
        for (file, why) in cases {
            let weight = weighed_with(&runner, &[file]).unwrap();
            assert_eq!(weight, Some(ReviewWeight::Full(why)), "{file}");
        }

        let docs = weighed_with(&runner, &["docs/a/b.md", "notes.md"]).unwrap();
        assert_eq!(docs, Some(ReviewWeight::Light(Lighter::Docs)));
    }

    #[test]
    fn a_change_beyond_the_docs_paths_reports_full() {
        let rig = pm_project();
        let runner = at_review(&rig, "README.md");

        let rst = weighed_with(&runner, &["a/b.rst"]).unwrap();
        let docs = weighed_with(&runner, &["docs/a.md", "notes.md"]).unwrap();

        assert_eq!(rst, touched("a/b.rst"));
        assert_eq!(docs, Some(ReviewWeight::Light(Lighter::Docs)));
    }

    #[test]
    fn a_changed_file_with_a_non_ascii_name_reports_as_it_is_named() {
        let rig = pm_project();
        let runner = at_review(&rig, "README.md");

        let weight = weighed_with(&runner, &[".claude/skills/café.md"]).unwrap();

        assert_eq!(weight, Some(ReviewWeight::Full(Heavier::AgentFiles)));
    }

    #[test]
    fn an_adopted_item_and_an_empty_diff_report_full() {
        let rig = pm_project();
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
    fn a_high_finding_leaves_the_weight_as_the_diff_reads() {
        let rig = pm_project();
        let runner = at_review(&rig, "README.md");
        let guard = runner.lock().unwrap();
        let mut item = guard.current().unwrap().clone();
        item.held.push(Finding::new(
            Severity::High,
            "README.md",
            1,
            "wrong",
            "it is",
        ));
        let base = item.review_base(guard.item_base(&item)).to_owned();

        let docs = guard.weigh(&item, &item.worktree, &base).unwrap();

        assert_eq!(
            docs,
            Some(ReviewWeight::Light(Lighter::Docs)),
            "a finding steps no weight up"
        );
    }

    #[test]
    fn a_diff_git_cannot_take_is_an_error_and_not_a_weight() {
        let rig = pm_project();
        let runner = at_review(&rig, "README.md");
        let guard = runner.lock().unwrap();
        let item = guard.current().unwrap().clone();

        let err = guard
            .weigh(&item, &item.worktree, "no-such-ref")
            .unwrap_err();

        assert!(err.contains("git diff failed"), "{err}");
    }

    #[test]
    fn a_fix_that_pulls_the_diff_into_src_reweighs_full() {
        let rig = pm_project();
        let runner = at_review(&rig, "README.md");
        plan(&runner, 2);
        rig.agent_board.ranks(&["mine", "other", "claude"]);
        let finding = Finding::new(Severity::Medium, "README.md", 1, "wrong", "it is");
        rig.reviewer
            .script([ScriptedRound::Findings(vec![finding])]);
        rig.claude.script([Scripted::Push("src/lib.rs", "fixed\n")]);

        reviewers_until_ci(&rig, &runner);

        assert_eq!(
            rig.reviewer.seen().len(),
            2,
            "the fix is read by a second reviewer"
        );
        assert_eq!(weight_of(&runner), touched("src/lib.rs"));
    }

    #[test]
    fn a_fix_that_ends_the_pass_at_ci_saves_the_weight_it_reread() {
        let rig = pm_listing(&["mine"]);
        let runner = at_review(&rig, "README.md");
        let finding = Finding::new(Severity::Medium, "README.md", 1, "wrong", "it is");
        rig.reviewer
            .script([ScriptedRound::Findings(vec![finding])]);
        rig.claude.script([Scripted::Push("src/lib.rs", "fixed\n")]);

        reviewers_until_ci(&rig, &runner);

        let phase = rig.ask(&runner, "status", None)["work_item"]["phase"]["state"].clone();
        assert_eq!(phase, "ci", "no reviewer is left to read the fix");
        assert_eq!(weight_of(&runner), touched("src/lib.rs"));
    }

    #[test]
    fn a_light_weight_whose_diff_git_cannot_take_goes_on_to_a_round_that_reports_it() {
        let rig = pm_project();
        let runner = at_review(&rig, "README.md");
        let guard = runner.lock().unwrap();
        let mut item = guard.current().unwrap().clone();
        item.weight = Some(ReviewWeight::Light(Lighter::Docs));
        item.arrived = Some("no-such-ref".into());
        let read_once = Review {
            reviewer: AgentName::try_from("mine".to_owned()).ok(),
            unread: false,
            ..Review::first()
        };

        let next = guard.after_round_in(&item, read_once, guard.ports.clock.now());

        let mut saved = item.clone();
        next.apply(&mut saved);
        assert!(matches!(saved.phase, Phase::Review(_)), "{:?}", saved.phase);
        assert_eq!(saved.weight, Some(ReviewWeight::Light(Lighter::Docs)));
    }
}
