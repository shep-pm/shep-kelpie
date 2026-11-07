//! Which of a project's settings, and kelpie's own webhook, a reread found different
//!
//! A name here is what the log line says is now in effect. `webhook` changes
//! when kelpie's own settings do. Both `Settings` are taken apart without `..`,
//! so a field added to one is a compile error until this decides what `reread`
//! does with it: list it here, or leave it out with a reason.

use crate::settings::{Git, Settings};
use crate::webhook::Webhook;

// A project's settings and the webhook.
type Reach<'a> = (&'a Settings, &'a Option<Webhook>);

// The settings that differ, by key or by table.
pub(super) fn names((old, was): Reach<'_>, (new, now): Reach<'_>) -> Vec<&'static str> {
    let Settings {
        git: old_git,
        ci: old_ci,
        concurrency: old_concurrency,
        budget: old_budget,
        agents: _, // `reread` compares the agents it resolves, with the files
        pacing: old_pacing,
        worker: old_worker,
        rulings: old_rulings,
        contributions: old_contributions,
        dependencies: old_dependencies,
        triage: old_triage,
        tickets: old_tickets,
        hooks: old_hooks,
        skills: old_skills,
        repo_setup: old_repo_setup,
        code_graph: old_code_graph,
        browser: old_browser,
        local_rounds: old_local_rounds,
        builds: old_builds,
        e2e: old_e2e,
    } = old;
    let Settings {
        git: new_git,
        ci: new_ci,
        concurrency: new_concurrency,
        budget: new_budget,
        agents: _,
        pacing: new_pacing,
        worker: new_worker,
        rulings: new_rulings,
        contributions: new_contributions,
        dependencies: new_dependencies,
        triage: new_triage,
        tickets: new_tickets,
        hooks: new_hooks,
        skills: new_skills,
        repo_setup: new_repo_setup,
        code_graph: new_code_graph,
        browser: new_browser,
        local_rounds: new_local_rounds,
        builds: new_builds,
        e2e: new_e2e,
    } = new;
    let Git {
        checkout: _, // waits for the next start
        remote: _,   // waits for the next start
        base: old_base,
        merging: old_merging,
        issues: old_issues,
        maintainer: old_maintainer,
    } = old_git;
    let Git {
        checkout: _,
        remote: _,
        base: new_base,
        merging: new_merging,
        issues: new_issues,
        maintainer: new_maintainer,
    } = new_git;
    [
        ("git.base", old_base != new_base),
        ("git.merging", old_merging != new_merging),
        ("git.issues", old_issues != new_issues),
        ("git.maintainer", old_maintainer != new_maintainer),
        ("ci", old_ci != new_ci),
        ("concurrency", old_concurrency != new_concurrency),
        ("budget", old_budget != new_budget),
        ("pacing", old_pacing != new_pacing),
        ("worker", old_worker != new_worker),
        ("hooks", old_hooks != new_hooks),
        ("skills", old_skills != new_skills),
        ("repo_setup", old_repo_setup != new_repo_setup),
        ("code_graph", old_code_graph != new_code_graph),
        ("browser", old_browser != new_browser),
        ("local_rounds", old_local_rounds != new_local_rounds),
        ("builds", old_builds != new_builds),
        ("e2e", old_e2e != new_e2e),
        ("rulings", old_rulings != new_rulings),
        ("contributions", old_contributions != new_contributions),
        ("dependencies", old_dependencies != new_dependencies),
        ("triage", old_triage != new_triage),
        ("tickets", old_tickets != new_tickets),
        ("webhook", was != now),
    ]
    .into_iter()
    .filter_map(|(name, differs)| differs.then_some(name))
    .collect()
}
