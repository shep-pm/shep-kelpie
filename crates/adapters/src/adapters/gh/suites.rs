//! The workflow runs with no check run
//!
//! A workflow file GitHub cannot read still records a failed run on the
//! commit, but with no check run, so the pull request's check rollup never
//! names it. Held workflows also leave no check run. A commit's suites
//! name both, one per workflow run.

use std::path::Path;

use serde::Deserialize;

use super::{gh_as, run_of, unreadable};
use crate::ports::{Checks, ForgeError, Queued};
use crate::settings::ForgeSlug;

// What the board's listing asks of a commit's check suites, as a literal
// `concat!` takes. GitHub prices a check run count as a request per suite, so
// the listing counts its rollup's check runs by the suite each names instead.
macro_rules! suites_fields {
    () => {
        "checkSuites(first: 50) { nodes { id conclusion workflowRun { url file { path } } } }"
    };
}
pub(super) use suites_fields;

const SUITES: &str = "checkSuites(first: 50) { nodes { conclusion \
     checkRuns(first: 1) { totalCount } workflowRun { url file { path } } } }";

/// A commit's check suites, as `SUITES` asks for them
#[derive(Deserialize, Default)]
pub(super) struct Suites {
    nodes: Vec<Suite>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Suite {
    // Only in the board's listing
    #[serde(default)]
    id: Option<String>,
    conclusion: Option<String>,
    // None in the board's listing until `counted`
    #[serde(default)]
    check_runs: Option<Count>,
    // None for a suite another App posts, such as a review bot's
    workflow_run: Option<Run>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Count {
    total_count: u64,
}

#[derive(Deserialize)]
struct Run {
    url: Option<String>,
    file: Option<File>,
}

#[derive(Deserialize)]
struct File {
    path: String,
}

impl Suites {
    // Each workflow that failed with no check run, by its file
    fn unstarted(&self) -> Vec<String> {
        self.nodes
            .iter()
            .filter(|s| s.check_runs.as_ref().is_some_and(|c| c.total_count == 0))
            .filter(|s| matches!(s.conclusion.as_deref(), Some("FAILURE" | "STARTUP_FAILURE")))
            .filter_map(|s| s.workflow_run.as_ref()?.file.as_ref())
            .map(|f| format!("{}: started no job", f.path))
            .collect()
    }

    fn held(&self) -> Vec<Queued> {
        self.nodes
            .iter()
            .filter(|s| s.conclusion.as_deref() == Some("ACTION_REQUIRED"))
            .filter(|s| s.check_runs.as_ref().is_some_and(|c| c.total_count == 0))
            .filter_map(|s| {
                let run = s.workflow_run.as_ref()?;
                Some(Queued {
                    name: run.file.as_ref()?.path.clone(),
                    run: run.url.as_deref().and_then(run_of),
                    running: false,
                    held: true,
                })
            })
            .collect()
    }

    /// These suites, each counting the check runs of `runs`, by the suite each names
    pub(super) fn counted<'a>(mut self, runs: impl Iterator<Item = &'a str>) -> Self {
        let runs: Vec<&str> = runs.collect();
        for suite in &mut self.nodes {
            if let Some(id) = &suite.id {
                let total_count = runs.iter().filter(|run| *run == id).count() as u64;
                suite.check_runs = Some(Count { total_count });
            }
        }
        self
    }

    /// `checks` with held workflows pending and workflows that could not start failed
    pub(super) fn fold(&self, checks: Checks) -> Checks {
        let held = self.held();
        if !held.is_empty() {
            let mut queued = match checks {
                Checks::Pending(queued) => queued,
                _ => Vec::new(),
            };
            queued.extend(held);
            return Checks::Pending(queued);
        }
        let unstarted = self.unstarted();
        if unstarted.is_empty() {
            return checks;
        }
        match checks {
            Checks::Pending(_) => checks,
            Checks::Failed(mut failed) => {
                failed.extend(unstarted);
                Checks::Failed(failed)
            }
            Checks::None | Checks::Passed | Checks::Skipped => Checks::Failed(unstarted),
        }
    }
}

/// The check suites of commit `head`, as the login in `config` where there is one
pub(super) fn of_commit(
    config: Option<&Path>,
    repo: &ForgeSlug,
    head: &str,
) -> Result<Suites, ForgeError> {
    let (owner, name) = repo.as_str().split_once('/').unwrap_or_default();
    let query = format!(
        "query=query($owner: String!, $name: String!, $oid: GitObjectID!) {{ \
         repository(owner: $owner, name: $name) {{ object(oid: $oid) {{ \
         ... on Commit {{ {SUITES} }} }} }} }}"
    );
    let (owner, name, oid) = (
        format!("owner={owner}"),
        format!("name={name}"),
        format!("oid={head}"),
    );
    parse_commit(&gh_as(
        config,
        &[
            "api", "graphql", "-f", &query, "-f", &owner, "-f", &name, "-f", &oid,
        ],
    )?)
}

fn parse_commit(stdout: &[u8]) -> Result<Suites, ForgeError> {
    #[derive(Deserialize)]
    struct Reply {
        data: Data,
    }
    #[derive(Deserialize)]
    struct Data {
        repository: Repository,
    }
    #[derive(Deserialize)]
    struct Repository {
        object: Option<Commit>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Commit {
        check_suites: Suites,
    }
    let reply: Reply = serde_json::from_slice(stdout).map_err(|_| unreadable(stdout))?;
    Ok(reply
        .data
        .repository
        .object
        .map(|c| c.check_suites)
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Recorded from gh 2.96 on this repo: `of_commit`'s query for d247998b, the
    // head of pull request #1171, whose test.yml GitHub could not read.
    const UNSTARTED: &str = include_str!("../../../../../fixtures/gh-graphql-commit-suites.json");

    fn suites() -> Suites {
        parse_commit(UNSTARTED.as_bytes()).unwrap()
    }

    #[test]
    fn a_workflow_that_started_no_job_turns_a_green_rollup_red() {
        let failed = Checks::Failed(vec![".github/workflows/test.yml: started no job".into()]);
        assert_eq!(suites().fold(Checks::Passed), failed);
        assert_eq!(suites().fold(Checks::Skipped), failed);
        assert_eq!(suites().fold(Checks::None), failed);
    }

    #[test]
    fn a_suite_held_for_approval_keeps_the_rollup_pending() {
        let held = br#"{"data":{"repository":{"object":{"checkSuites":{"nodes":[
            {"conclusion":"ACTION_REQUIRED","checkRuns":{"totalCount":0},
             "workflowRun":{"url":"https://github.com/shep-pm/shep/actions/runs/42","file":{"path":".github/workflows/test.yml"}}}
        ]}}}}}"#;
        let suites = parse_commit(held).unwrap();
        for rollup in [
            Checks::Skipped,
            Checks::Passed,
            Checks::None,
            Checks::Failed(vec!["lint".into()]),
        ] {
            let Checks::Pending(queued) = suites.fold(rollup) else {
                panic!("a held suite must keep the rollup pending");
            };
            assert_eq!(queued.len(), 1);
            assert_eq!(queued[0].name, ".github/workflows/test.yml");
            assert_eq!(queued[0].run, Some(42));
            assert!(queued[0].held);
        }
    }

    #[test]
    fn a_suite_that_failed_to_start_still_counts_as_failed() {
        assert_eq!(
            suites().fold(Checks::Passed),
            Checks::Failed(vec![".github/workflows/test.yml: started no job".into()])
        );
    }

    #[test]
    fn a_red_rollup_names_the_workflow_that_started_no_job_too() {
        assert_eq!(
            suites().fold(Checks::Failed(vec!["lint".into()])),
            Checks::Failed(vec![
                "lint".into(),
                ".github/workflows/test.yml: started no job".into()
            ])
        );
    }

    #[test]
    fn a_pending_rollup_stays_pending() {
        assert_eq!(
            suites().fold(Checks::Pending(Vec::new())),
            Checks::Pending(Vec::new())
        );
    }

    #[test]
    fn suites_with_check_runs_or_from_another_app_change_nothing() {
        let suites = br#"{"data":{"repository":{"object":{"checkSuites":{"nodes":[
            {"conclusion":"FAILURE","checkRuns":{"totalCount":0},"workflowRun":null},
            {"conclusion":"FAILURE","checkRuns":{"totalCount":3},"workflowRun":{"file":{"path":"a.yml"}}},
            {"conclusion":"SUCCESS","checkRuns":{"totalCount":0},"workflowRun":{"file":{"path":"b.yml"}}}
        ]}}}}}"#;
        let suites = parse_commit(suites).unwrap();
        assert_eq!(suites.fold(Checks::Passed), Checks::Passed);
    }

    #[test]
    fn a_commit_the_forge_does_not_know_has_no_suites() {
        let gone = br#"{"data":{"repository":{"object":null}}}"#;
        assert_eq!(
            parse_commit(gone).unwrap().fold(Checks::Passed),
            Checks::Passed
        );
    }

    #[test]
    #[ignore = "asks the real GitHub about shep-pm/shep-kelpie through gh, about 1 s"]
    fn the_real_gh_answers_the_query_in_the_shape_parsed() {
        let kelpie = ForgeSlug::try_from("shep-pm/shep-kelpie".to_owned()).unwrap();
        let suites = of_commit(None, &kelpie, "d247998b46f0e30a7c81c251a340db5afe7be5aa").unwrap();
        assert!(matches!(suites.fold(Checks::Passed), Checks::Failed(_)));
    }

    #[test]
    fn an_answer_in_another_shape_is_unreadable() {
        assert!(matches!(
            parse_commit(br#"{"data":{}}"#),
            Err(ForgeError::Unreadable(_))
        ));
    }
}
