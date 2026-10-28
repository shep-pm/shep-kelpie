use super::*;
use crate::github::ApiError;
use crate::ports::Cooldown;

#[tokio::test]
async fn an_installed_app_that_mints_a_token_is_ok() {
    let scene = Scene::new().await;

    let report = scene.report().await;

    assert_eq!(
        ok(&report, "koji: github app"),
        "kelpie's App is installed on shep-pm/koji and mints a token"
    );
}

// Each call keeps a config folder of its own, which holds no login, so
// `claude` has no login but the one the maintainer made with `claude setup-token`.
#[tokio::test]
async fn a_project_on_claude_code_needs_the_login_token_and_names_the_two_commands() {
    let mut scene = Scene::new().await;

    scene.host.token = false;
    let report = scene.report().await;
    let (what, fix) = missing(&report, "koji: claude login");
    assert!(what.contains("CLAUDE_CODE_OAUTH_TOKEN"), "{what}");
    assert!(fix.contains("`claude setup-token`"), "{fix}");
    assert!(
        fix.contains("`shep secret set CLAUDE_CODE_OAUTH_TOKEN`"),
        "{fix}"
    );
    assert!(!report.passed());

    scene.host.token = true;
    let report = scene.report().await;
    let found = ok(&report, "koji: claude login");
    assert!(found.contains("CLAUDE_CODE_OAUTH_TOKEN"), "{found}");
}

// The runner reads the token from its sheep's `env`, which doctor's shell
// does not share, so a token set there is enough.
#[tokio::test]
async fn a_runner_whose_env_sets_the_login_token_has_a_login_without_it_in_the_shell() {
    let mut scene = Scene::new().await;
    scene.host.token = false;
    let secret = "{{secret:CLAUDE_CODE_OAUTH_TOKEN}}";
    scene
        .shepherd
        .sets_env("koji", "CLAUDE_CODE_OAUTH_TOKEN", secret);

    let report = scene.report().await;

    let found = ok(&report, "koji: claude login");
    assert!(found.contains("the runner's `env`"), "{found}");
}

// The runner refuses an empty token, and shep's reply does not show a plain
// value, so a plain key in `env` is no login.
#[tokio::test]
async fn a_runner_whose_env_sets_the_login_token_to_nothing_has_no_login() {
    let mut scene = Scene::new().await;
    scene.host.token = false;
    scene
        .shepherd
        .sets_env("koji", "CLAUDE_CODE_OAUTH_TOKEN", "");

    let report = scene.report().await;

    let (what, _) = missing(&report, "koji: claude login");
    assert!(what.contains("CLAUDE_CODE_OAUTH_TOKEN"), "{what}");
}

// The login does not depend on the App or the keychain: a call's config
// folder is its own whatever repo it works on.
#[tokio::test]
async fn a_project_the_app_does_not_cover_needs_the_login_token_too() {
    let mut scene = Scene::new().await;
    scene.host.token = false;
    scene.runs("golbat", |t| {
        t["git"]["remote"] = json!("someone-else/golbat");
    });

    let report = scene.report().await;

    assert!(subjects(&report).contains(&"golbat: claude login"));
    assert!(subjects(&report).contains(&"koji: claude login"));
}

// No App is unsure: kelpie falls back to the user's own `gh` login, so it does not fail the run.
#[tokio::test]
async fn an_owner_with_no_app_is_unsure_and_names_setup() {
    let scene = Scene::new().await;
    scene.runs("golbat", |t| {
        t["git"]["remote"] = json!("someone-else/golbat");
    });

    let report = scene.report().await;

    let Verdict::Unsure { what, next } = verdict(&report, "golbat: github app") else {
        panic!("{:#?}", report.render());
    };
    assert_eq!(what, "kelpie has no GitHub App for someone-else");
    assert!(next.contains("`shep kelpie github setup`"), "{next}");
    assert!(next.contains("`--org someone-else`"), "{next}");
    assert!(report.passed());
}

#[tokio::test]
async fn github_busy_or_out_of_reach_is_unsure_and_a_refused_app_is_missing() {
    let scene = Scene::new().await;
    for (error, said) in [
        (ApiError::Refused(502), "GitHub answered HTTP 502"),
        (ApiError::RateLimited(Cooldown::default()), "rate limit"),
        (
            ApiError::Unreachable("curl exited 6".into()),
            "cannot reach GitHub",
        ),
    ] {
        scene.github.fail_next(error);
        let report = scene.report().await;
        let Verdict::Unsure { what, .. } = verdict(&report, "koji: github app") else {
            panic!("{:#?}", report.render());
        };
        assert!(what.contains(said), "{what}");
        // The token is asked for once, so the failure is not followed by a mint that works.
        assert!(!subjects(&report).contains(&"koji: maintainer"));
    }

    scene.github.fail_next(ApiError::Refused(401));
    let report = scene.report().await;
    let (what, fix) = missing(&report, "koji: github app");
    assert_eq!(what, "no token mints: GitHub answered HTTP 401");
    assert!(
        fix.contains("`shep kelpie github setup --replace`"),
        "{fix}"
    );
}

#[tokio::test]
async fn an_app_not_installed_on_the_repo_is_missing_and_names_its_install_page() {
    let scene = Scene::new().await;
    scene.runs("rotom", |_| {});

    let report = scene.report().await;

    let (what, fix) = missing(&report, "rotom: github app");
    assert_eq!(what, "kelpie-shep-pm is not installed on shep-pm/rotom");
    assert_eq!(
        fix,
        "install it there from https://github.com/apps/kelpie-shep-pm/installations/new"
    );
    ok(&report, "koji: github app");
    assert!(!report.passed());
}

#[tokio::test]
async fn a_ruling_on_a_covered_repo_mentions_the_login_gh_acts_as_and_doctor_says_so() {
    let scene = Scene::new().await;

    let report = scene.report().await;

    assert_eq!(
        ok(&report, "koji: maintainer"),
        "a ruling mentions @the-maintainer (from gh)"
    );
}

#[tokio::test]
async fn a_login_gh_cannot_give_leaves_a_ruling_mentioning_no_one_and_is_missing() {
    let scene = Scene::new().await;
    scene
        .forge
        .set_viewer_down(ForgeError::Failed("not logged in".into()));

    let report = scene.report().await;

    let (what, fix) = missing(&report, "koji: maintainer");
    assert!(
        what.contains("mentions no one and notifies no one"),
        "{what}"
    );
    assert!(what.contains("not logged in"), "{what}");
    assert!(fix.contains("set `git.maintainer`"), "{fix}");
}

#[tokio::test]
async fn a_maintainer_that_is_set_or_an_app_that_does_not_cover_the_repo_needs_no_line() {
    let scene = Scene::new().await;
    scene.runs("golbat", |t| {
        t["git"]["maintainer"] = json!("octocat");
    });
    let report = scene.report().await;
    assert!(!subjects(&report).contains(&"golbat: maintainer"));

    // Not installed there, so no ruling goes through the App at all.
    let scene = Scene::new().await;
    scene.runs("rotom", |_| {});
    let report = scene.report().await;
    assert!(!subjects(&report).contains(&"rotom: maintainer"));
}

#[tokio::test]
async fn a_worker_on_a_covered_repo_pushes_with_a_token_for_that_repo_alone() {
    let scene = Scene::new().await;

    let report = scene.report().await;

    let said = ok(&report, "koji: workers");
    assert!(said.contains("a token for shep-pm/koji alone"), "{said}");
    assert!(said.contains("protect `main` on GitHub"), "{said}");
}

#[tokio::test]
async fn doctor_says_whether_a_worker_can_change_workflow_files() {
    let scene = Scene::new().await;

    let without = scene.report().await;
    let said = ok(&without, "koji: workers");
    assert!(said.contains("cannot change a workflow file"), "{said}");
    assert!(said.contains("`workflows` permission"), "{said}");

    scene.github.hold_workflows();
    let with = scene.report().await;
    let said = ok(&with, "koji: workers");
    assert!(said.contains("can change a workflow file"), "{said}");
    assert!(said.contains("runs with the repo's secrets"), "{said}");
}

// With no App a worker holds the maintainer's login, and doctor says so, but
// the App is optional, so it does not fail the run.
#[tokio::test]
async fn without_an_app_doctor_says_a_worker_holds_the_maintainers_login() {
    let scene = Scene::new().await;
    scene.runs("golbat", |t| {
        t["git"]["remote"] = json!("someone-else/golbat");
    });

    let report = scene.report().await;

    let Verdict::Unsure { what, next } = verdict(&report, "golbat: workers") else {
        panic!("{:#?}", report.render());
    };
    assert!(what.contains("holds your own gh login"), "{what}");
    assert!(next.contains("shep kelpie github setup"), "{next}");
    assert!(report.passed());
}

// A Windows sandbox user cannot read the maintainer's Credential Manager, so
// doctor does not claim the worker reads the login from gh's config.
#[tokio::test]
async fn on_windows_doctor_says_a_worker_gets_the_maintainers_login_as_gh_token() {
    let mut scene = Scene::new().await;
    scene.host.os = "windows";
    scene.runs("golbat", |t| {
        t["git"]["remote"] = json!("someone-else/golbat");
    });

    let report = scene.report().await;

    let Verdict::Unsure { what, next } = verdict(&report, "golbat: workers") else {
        panic!("{:#?}", report.render());
    };
    assert!(what.contains("gets your gh login as `GH_TOKEN`"), "{what}");
    assert!(!what.contains("holds your own gh login"), "{what}");
    assert!(next.contains("shep kelpie github setup"), "{next}");
}

// An App registered before workers held its tokens lacks the permission.
#[tokio::test]
async fn an_app_without_the_contents_permission_is_missing_and_names_the_fix() {
    let scene = Scene::new().await;
    scene.github.refuse_workers(ApiError::Refused(422));

    let report = scene.report().await;

    let (what, fix) = missing(&report, "koji: workers");
    assert!(what.contains("Contents permission"), "{what}");
    assert!(fix.contains("Contents, to Read and write"), "{fix}");
    ok(&report, "koji: github app");
    assert!(!report.passed());
}

#[tokio::test]
async fn doctor_says_the_reads_of_a_covered_repo_go_through_the_app() {
    let scene = Scene::new().await;

    let report = scene.report().await;

    let said = ok(&report, "koji: reads");
    assert!(said.contains("go through kelpie's App"), "{said}");
    assert!(said.contains("shep-pm/koji"), "{said}");
}

// An App registered before kelpie read through it was never given these.
#[tokio::test]
async fn an_app_without_checks_or_statuses_reads_the_rest_and_doctor_names_the_two() {
    let scene = Scene::new().await;
    scene.github.never_given(&["checks", "statuses"]);

    let report = scene.report().await;

    let Verdict::Unsure { what, next } = verdict(&report, "koji: reads") else {
        panic!("{:#?}", report.render());
    };
    assert!(
        what.contains("except those that need Checks and Commit statuses"),
        "{what}"
    );
    assert!(what.contains("your own gh login"), "{what}");
    assert!(next.contains("Checks and Commit statuses"), "{next}");
    assert!(report.passed(), "an optional gain, not a failure");
}

#[tokio::test]
async fn without_an_app_doctor_says_every_read_goes_through_the_maintainers_login() {
    let scene = Scene::new().await;
    scene.runs("golbat", |t| {
        t["git"]["remote"] = json!("someone-else/golbat");
    });

    let report = scene.report().await;

    let said = ok(&report, "golbat: reads");
    assert!(
        said.contains("does not cover someone-else/golbat"),
        "{said}"
    );
    assert!(said.contains("your own gh login"), "{said}");
}
