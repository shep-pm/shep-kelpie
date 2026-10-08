use std::path::PathBuf;

use super::*;
use crate::ports::{Reviewer, Severity};
use crate::settings::{ContextSize, EndpointUrl, LocalRound, ModelHost, NonBlank};
use crate::test::{Answer, Elsewhere, StandInEndpoint, git, linked_worktree, unreachable_url};

const DIFF: &str = "\
diff --git a/src/lib.rs b/src/lib.rs
index 1111111..2222222 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -3,4 +3,5 @@ fn a() {
 keep
--- not a header
+added
+++ not a header either
 tail
\\ No newline at end of file
diff --git a/gone.rs b/gone.rs
deleted file mode 100644
--- a/gone.rs
+++ /dev/null
@@ -1,1 +0,0 @@
-old
diff --git a/logo.png b/logo.png
Binary files a/logo.png and b/logo.png differ
";

#[test]
fn a_diff_reads_as_numbered_hunks_per_file() {
    assert_eq!(
        parse(DIFF),
        vec![
            FileDiff {
                path: "src/lib.rs".into(),
                hunks: vec![
                    "@@ -3,4 +3,5 @@ fn a() {\n\
                     \x20    3  keep\n\
                     \x20      --- not a header\n\
                     \x20    4 +added\n\
                     \x20    5 +++ not a header either\n\
                     \x20    6  tail\n"
                        .into()
                ],
            },
            FileDiff {
                path: "gone.rs".into(),
                hunks: vec!["@@ -1,1 +0,0 @@\n       -old\n".into()],
            },
        ]
    );
}

#[test]
fn hunks_pack_into_chunks_that_name_their_file() {
    let file = |path: &str, hunks: &[&str]| FileDiff {
        path: path.into(),
        hunks: hunks.iter().map(|h| (*h).to_owned()).collect(),
    };
    let files = [file("a", &["1111\n", "2222\n"]), file("b", &["3333\n"])];
    assert_eq!(
        chunks(&files, 100),
        ["### a\n1111\n2222\n### b\n3333\n"],
        "everything fits in one"
    );
    assert_eq!(
        chunks(&files, 16),
        ["### a\n1111\n2222\n", "### b\n3333\n"],
        "a file starts a new chunk once the old one is full"
    );
    assert_eq!(
        chunks(&files, 11),
        ["### a\n1111\n", "### a\n2222\n", "### b\n3333\n"],
        "a file split across chunks is named in each"
    );
}

#[test]
fn a_hunk_too_big_for_a_chunk_is_cut_between_lines() {
    let big = FileDiff {
        path: "a".into(),
        hunks: vec!["one\ntwo\nthree\n".into()],
    };
    assert_eq!(chunks(&[big], 14), ["### a\none\ntwo\n", "### a\nthree\n"]);
    let line = FileDiff {
        path: "a".into(),
        hunks: vec!["a line longer than the budget\n".into()],
    };
    assert_eq!(chunks(&[line], 10).len(), 1, "a line too long still goes");
}

#[test]
fn a_new_start_line_is_read_from_the_hunk_header() {
    assert_eq!(new_start("@@ -12,7 +14,9 @@ fn name"), 14);
    assert_eq!(new_start("@@ -1 +1 @@"), 1);
    assert_eq!(new_start("@@ -1,1 +0,0 @@"), 0);
}

// A repo with `origin/main` at its first commit and one commit on top.
// A repo and a worktree of it whose commit changes `src/lib.rs` to `change`
fn repo(dir: &Path, change: &str) -> (PathBuf, PathBuf) {
    let (repo, worktree) = linked_worktree(dir);
    std::fs::create_dir_all(worktree.join("src")).unwrap();
    std::fs::write(worktree.join("src/lib.rs"), "fn a() {}\n").unwrap();
    git(&worktree, &["add", "."]);
    git(&worktree, &["commit", "--quiet", "-m", "init"]);
    let base = git(&worktree, &["rev-parse", "HEAD"]);
    git(
        &worktree,
        &["update-ref", "refs/remotes/origin/main", &base],
    );
    std::fs::write(worktree.join("src/lib.rs"), change).unwrap();
    git(&worktree, &["commit", "--quiet", "-am", "change"]);
    (repo, worktree)
}

fn linked((repo, worktree): &(PathBuf, PathBuf)) -> Linked<'_> {
    Linked { repo, worktree }
}

fn local(url: &str, context: u32) -> LocalRound {
    LocalRound::Endpoint(Endpoint {
        host: ModelHost::Url(EndpointUrl::try_from(url.to_owned()).unwrap()),
        model: NonBlank::try_from("coder".to_owned()).unwrap(),
        context: ContextSize::try_from(i64::from(context)).unwrap(),
        lease: None,
    })
}

#[test]
fn a_round_sends_the_diff_with_the_prompt_and_reads_the_findings() {
    let dir = tempfile::tempdir().unwrap();
    let worktree = repo(dir.path(), "fn a() {}\nfn b() { panic!() }\n");
    let server = StandInEndpoint::start([Answer::Says(
        "<think>HIGH|src/lib.rs:1|a thought|not a finding</think>\n\
         MEDIUM|src/lib.rs:2|b panics|callers crash",
    )]);
    let out = dir.path().join("out");
    let findings = LocalReviewer::default()
        .round(
            &local(server.url(), 8192),
            linked(&worktree),
            "origin/main",
            &out,
            3,
            "",
        )
        .unwrap();
    assert_eq!(
        findings,
        vec![Finding {
            severity: Severity::Medium,
            file: "src/lib.rs".into(),
            line: 2,
            what: "b panics".into(),
            why: "callers crash".into(),
        }]
    );
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request["model"], "coder");
    assert_eq!(request["temperature"], 0);
    assert_eq!(request["max_tokens"], 2048);
    assert_eq!(request["messages"][0]["content"], PROMPT);
    let diff = request["messages"][1]["content"].as_str().unwrap();
    assert!(diff.starts_with("### src/lib.rs\n@@ "), "{diff}");
    assert!(diff.contains("     2 +fn b() { panic!() }\n"), "{diff}");
    assert!(out.join("round-3.txt.done").is_file());
    assert!(out.join("round-3/reply-0.json").is_file());
}

#[test]
fn a_diff_bigger_than_the_context_goes_in_several_requests() {
    let dir = tempfile::tempdir().unwrap();
    let big: String = (0..2000).map(|i| format!("fn f{i}() {{}}\n")).collect();
    let worktree = repo(dir.path(), &big);
    let server = StandInEndpoint::start([]);
    let out = dir.path().join("out");
    let findings = LocalReviewer::default()
        .round(
            &local(server.url(), 4096),
            linked(&worktree),
            "origin/main",
            &out,
            1,
            "",
        )
        .unwrap();
    assert!(findings.is_empty(), "CLEAN from every chunk");
    let requests = server.requests();
    assert!(requests.len() > 1, "{} requests", requests.len());
    let reply = 4096 / REPLY_SHARE;
    let prompt = PROMPT.len().div_ceil(BYTES_PER_TOKEN) + FRAMING_TOKENS;
    let budget = (4096 - reply - prompt) * BYTES_PER_TOKEN;
    for request in &requests {
        let diff = request["messages"][1]["content"].as_str().unwrap();
        assert!(diff.len() <= budget, "{} > {budget}", diff.len());
        assert!(diff.starts_with("### src/lib.rs\n"), "{diff}");
    }
}

#[test]
fn an_empty_diff_asks_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let worktree = repo(dir.path(), "fn a() {}\nfn b() {}\n");
    let server = StandInEndpoint::start([]);
    let out = dir.path().join("out");
    let findings = LocalReviewer::default()
        .round(
            &local(server.url(), 8192),
            linked(&worktree),
            "HEAD",
            &out,
            1,
            "",
        )
        .unwrap();
    assert!(findings.is_empty());
    assert!(server.requests().is_empty());
    assert!(out.join("round-1.txt.done").is_file());
}

#[test]
fn a_refused_or_unreadable_reply_fails_the_round() {
    let dir = tempfile::tempdir().unwrap();
    let worktree = repo(dir.path(), "fn a() {}\nfn b() {}\n");
    let server = StandInEndpoint::start([
        Answer::Status(500, r#"{"error":"model not loaded"}"#),
        Answer::Status(200, "not json"),
    ]);
    let reviewer = LocalReviewer::default();
    let local = local(server.url(), 8192);
    let out = dir.path().join("out");
    let err = reviewer
        .round(&local, linked(&worktree), "origin/main", &out, 1, "")
        .unwrap_err();
    let url = format!("{}/chat/completions", server.url());
    assert_eq!(
        err,
        ReviewerError::Failed(format!(
            "{url} answered HTTP 500: {{\"error\":\"model not loaded\"}}"
        ))
    );
    let err = reviewer
        .round(&local, linked(&worktree), "origin/main", &out, 2, "")
        .unwrap_err();
    assert_eq!(err, ReviewerError::Unreadable("not json".into()));
}

#[test]
fn a_reply_cut_off_while_thinking_fails_the_round() {
    let dir = tempfile::tempdir().unwrap();
    let worktree = repo(dir.path(), "fn a() {}\nfn b() {}\n");
    let server = StandInEndpoint::start([Answer::Says(
        "<think>HIGH|src/lib.rs:2|maybe this|or not, let me check the",
    )]);
    let out = dir.path().join("out");
    let err = LocalReviewer::default()
        .round(
            &local(server.url(), 8192),
            linked(&worktree),
            "origin/main",
            &out,
            1,
            "",
        )
        .unwrap_err();
    let url = format!("{}/chat/completions", server.url());
    assert_eq!(
        err,
        ReviewerError::Failed(format!(
            "{url}'s reply ended inside its thinking, so nothing was reviewed"
        ))
    );
}

#[test]
fn a_reply_with_no_findings_that_is_not_clean_fails_the_round() {
    let dir = tempfile::tempdir().unwrap();
    let worktree = repo(dir.path(), "fn a() {}\nfn b() {}\n");
    let server = StandInEndpoint::start([
        Answer::Says(""),
        Answer::Says("<think>long thoughts</think>"),
        Answer::Says("The code looks fine."),
        Answer::Says("  CLEAN\n"),
    ]);
    let reviewer = LocalReviewer::default();
    let local = local(server.url(), 8192);
    let out = dir.path().join("out");
    let url = server.url();
    for said in ["", "", "The code looks fine."] {
        assert_eq!(
            reviewer.round(&local, linked(&worktree), "origin/main", &out, 1, ""),
            Err(ReviewerError::Failed(format!(
                "{url}'s reply is neither findings nor CLEAN: {said}"
            )))
        );
        assert!(!out.join("round-1.txt.done").exists());
    }
    assert_eq!(
        reviewer.round(&local, linked(&worktree), "origin/main", &out, 1, ""),
        Ok(vec![])
    );
}

#[test]
fn a_retried_round_never_reads_the_last_tries_reply() {
    let dir = tempfile::tempdir().unwrap();
    let worktree = repo(dir.path(), "fn a() {}\nfn b() {}\n");
    let server = StandInEndpoint::start([
        Answer::Says("MEDIUM|src/lib.rs:2|b is new|it matters"),
        Answer::Status(200, ""),
    ]);
    let reviewer = LocalReviewer::default();
    let local = local(server.url(), 8192);
    let out = dir.path().join("out");
    let first = reviewer.round(&local, linked(&worktree), "origin/main", &out, 1, "");
    assert_eq!(first.unwrap().len(), 1);
    let again = reviewer.round(&local, linked(&worktree), "origin/main", &out, 1, "");
    assert_eq!(again, Err(ReviewerError::Unreadable(String::new())));
}

#[test]
fn the_start_check_asks_the_server_for_its_models() {
    let server = StandInEndpoint::start([]);
    let reviewer = LocalReviewer::default();
    assert_eq!(reviewer.check(&local(server.url(), 8192)), Ok(()));
    assert!(server.requests().is_empty(), "no chat request was sent");
    let url = unreachable_url();
    let err = reviewer.check(&local(&url, 8192)).unwrap_err();
    assert!(
        err.starts_with(&format!("cannot reach {url}/models: ")),
        "{err}"
    );
}

// Ollama's `/api/ps` with one loaded model.
fn ps(name: &str, size: u64, vram: u64) -> String {
    json!({ "models": [{
        "name": name, "model": name, "size": size, "size_vram": vram,
        "context_length": 8192, "expires_at": "2026-09-29T21:14:03+01:00",
    }] })
    .to_string()
}

// The placement is read only under the GPU lock kelpie holds, so a round
// here holds one, in a folder of its own.
fn leased(url: &str) -> LocalRound {
    let mut local = local(url, 8192);
    let LocalRound::Endpoint(endpoint) = &mut local else {
        unreachable!()
    };
    endpoint.lease = Some(crate::settings::LeaseName::gpu());
    local
}

fn round(
    dir: &Path,
    reviewer: &LocalReviewer,
    local: &LocalRound,
) -> Result<Vec<Finding>, ReviewerError> {
    // A second round in `dir` reviews the worktree the first made.
    let worktree = match dir.join("wt").exists() {
        true => (dir.join("repo"), dir.join("wt")),
        false => repo(dir, "fn a() {}\nfn b() {}\n"),
    };
    reviewer.round(
        local,
        linked(&worktree),
        "origin/main",
        &dir.join("out"),
        1,
        "",
    )
}

fn reviewer(dir: &Path) -> LocalReviewer {
    LocalReviewer::default().with_temp_dir(dir.join("tmp"))
}

#[test]
fn a_model_fully_on_the_gpu_runs_and_is_seated_for_status() {
    let dir = tempfile::tempdir().unwrap();
    let recorded = include_str!("../../../../fixtures/ollama-ps.json");
    let server = StandInEndpoint::start([]).with_ps(recorded);
    let mut local = leased(server.url());
    let LocalRound::Endpoint(endpoint) = &mut local else {
        unreachable!()
    };
    endpoint.model = NonBlank::try_from("Qwen3-Coder:30b".to_owned()).unwrap();
    let reviewer = reviewer(dir.path());
    assert_eq!(reviewer.seat(), None, "nothing read before a round");
    assert_eq!(round(dir.path(), &reviewer, &local), Ok(vec![]));
    assert_eq!(server.requests().len(), 1, "the round ran");
    let seat = reviewer.seat().unwrap();
    assert_eq!(seat.name, "qwen3-coder:30b", "names match in any case");
    assert_eq!(seat.gpu_percent(), 100);
    assert_eq!(seat.context_length, Some(32768));
    assert_eq!(
        seat.expires_at.as_deref(),
        Some("2026-09-29T21:14:03.118463+01:00")
    );
}

#[test]
fn a_model_partly_on_the_cpu_fails_the_round_before_it_asks_anything() {
    let dir = tempfile::tempdir().unwrap();
    let server = StandInEndpoint::start([]).with_ps(&ps("coder:latest", 1000, 400));
    let reviewer = reviewer(dir.path());
    let result = round(dir.path(), &reviewer, &leased(server.url()));
    assert_eq!(
        result,
        Err(ReviewerError::Spilled(
            "the local model coder:latest is 40% on the GPU, \
             so its rounds would run at CPU speed"
                .into()
        ))
    );
    assert!(server.requests().is_empty(), "no chat request was sent");
    assert_eq!(reviewer.seat().unwrap().gpu_percent(), 40);
}

#[test]
fn a_round_kelpie_holds_no_lock_for_is_not_checked() {
    let dir = tempfile::tempdir().unwrap();
    let server = StandInEndpoint::start([]).with_ps(&ps("coder", 1000, 0));
    let reviewer = reviewer(dir.path());
    let unleased = local(server.url(), 8192);
    assert_eq!(round(dir.path(), &reviewer, &unleased), Ok(vec![]));
    assert_eq!(reviewer.seat(), None);
}

#[test]
fn a_model_that_is_not_loaded_yet_is_not_checked() {
    let dir = tempfile::tempdir().unwrap();
    let server = StandInEndpoint::start([]).with_ps(&ps("another:7b", 1000, 0));
    let reviewer = reviewer(dir.path());
    assert_eq!(
        round(dir.path(), &reviewer, &leased(server.url())),
        Ok(vec![])
    );
    assert_eq!(reviewer.seat(), None);
}

#[test]
fn a_server_with_no_api_ps_is_not_checked() {
    let dir = tempfile::tempdir().unwrap();
    let server = StandInEndpoint::start([]);
    let reviewer = reviewer(dir.path());
    let local = leased(server.url());
    assert_eq!(round(dir.path(), &reviewer, &local), Ok(vec![]));
    assert_eq!(server.requests().len(), 1, "the round ran");
    assert_eq!(reviewer.seat(), None);
    let page = StandInEndpoint::start([]).with_ps("<html>hello</html>");
    let local = leased(page.url());
    assert_eq!(round(dir.path(), &reviewer, &local), Ok(vec![]));
}

fn command(script: &Path, host: &str, model: Option<&str>) -> LocalRound {
    LocalRound::Command(crate::settings::LocalCommand {
        command: script.to_owned(),
        ollama: Some(EndpointUrl::try_from(host.to_owned()).unwrap()),
        ollama_model: model.map(|m| NonBlank::try_from(m.to_owned()).unwrap()),
        lease: Some(crate::settings::LeaseName::gpu()),
    })
}

#[test]
fn a_command_names_its_ollama_host_and_is_stopped_before_it_runs() {
    let dir = tempfile::tempdir().unwrap();
    let ran = dir.path().join("ran");
    let script = dir.path().join("review");
    crate::test::write_script(&script, &format!("#!/bin/sh\ntouch '{}'\n", ran.display()));
    let server = StandInEndpoint::start([]).with_ps(&ps("anything:1b", 1000, 0));
    let reviewer = reviewer(dir.path());
    let result = round(
        dir.path(),
        &reviewer,
        &command(&script, server.host(), None),
    );
    assert!(
        matches!(result, Err(ReviewerError::Spilled(_))),
        "{result:?}"
    );
    assert!(!ran.exists(), "the command never started");
}

#[test]
fn a_command_that_names_its_model_ignores_the_host_s_others() {
    let dir = tempfile::tempdir().unwrap();
    let ran = dir.path().join("ran");
    let script = dir.path().join("review");
    crate::test::write_script(&script, &format!("#!/bin/sh\ntouch '{}'\n", ran.display()));
    let server = StandInEndpoint::start([]).with_ps(&ps("someone-elses:70b", 1000, 0));
    let reviewer = reviewer(dir.path());
    let ours = command(&script, server.host(), Some("mine:14b"));
    let _ = round(dir.path(), &reviewer, &ours);
    assert!(
        ran.exists(),
        "another model's spill does not fail this round"
    );
    let theirs = command(&script, server.host(), Some("someone-elses:70b"));
    let result = round(dir.path(), &reviewer, &theirs);
    assert!(
        matches!(result, Err(ReviewerError::Spilled(_))),
        "{result:?}"
    );
}

#[test]
fn an_ollama_host_that_cannot_be_reached_fails_the_round_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let url = unreachable_url();
    let err = round(dir.path(), &reviewer(dir.path()), &leased(&url)).unwrap_err();
    let ReviewerError::Failed(why) = err else {
        panic!("{err:?}")
    };
    assert!(
        why.starts_with("cannot reach http://127.0.0.1:1/api/ps: "),
        "{why}"
    );
}

// The round runs outside the worker's sandbox, and its worker can write
// the `commondir` git follows to the repo whose config it reads.
#[test]
fn a_worktree_whose_commondir_points_elsewhere_is_refused_and_runs_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let checkout = repo(dir.path(), "fn a() {}\nfn b() {}\n");
    let elsewhere = Elsewhere::copy_of(&checkout.0, dir.path());
    elsewhere.as_common_dir_of(&checkout.1);
    let server = StandInEndpoint::start([]);
    let out = dir.path().join("out");

    let err = LocalReviewer::default()
        .round(
            &local(server.url(), 8192),
            linked(&checkout),
            "origin/main",
            &out,
            1,
            "",
        )
        .unwrap_err();
    let ReviewerError::Failed(why) = err else {
        panic!("{err:?}")
    };
    assert!(why.contains("is not this work item's worktree"), "{why}");
    assert!(!elsewhere.ran.exists(), "the round started the program");
    assert!(server.requests().is_empty(), "nothing was reviewed");
    elsewhere.assert_plain_git_starts_it(&checkout.1);
}

#[test]
fn a_round_behind_a_gateway_sends_its_key_from_curls_stdin_and_holds_no_lock() {
    use crate::settings::{Gateway, GatewayName, Gateways, KeyVar};
    let dir = tempfile::tempdir().unwrap();
    let checkout = repo(dir.path(), "fn a() {}\nfn b() {}\n");
    let server = StandInEndpoint::start([Answer::Says("CLEAN")]).like_paddock("pk-r", &[]);
    let name = GatewayName::try_from("paddock".to_owned()).unwrap();
    let gateway = Gateway {
        url: EndpointUrl::try_from(server.host().to_owned()).unwrap(),
        key_env: KeyVar::try_from("PADDOCK_KEY".to_owned()).unwrap(),
    };
    let table = std::collections::BTreeMap::from([(name.clone(), gateway)]);
    let reviewer = LocalReviewer::default()
        .with_temp_dir(dir.path().join("locks"))
        .with_gateways(Gateways::reading(table, |_| Some("pk-r".into())));
    let LocalRound::Endpoint(mut endpoint) = local("http://unused/v1", 8192) else {
        unreachable!()
    };
    endpoint.host = ModelHost::Gateway(name);
    let round = LocalRound::Endpoint(endpoint);
    reviewer.check(&round).unwrap();
    let out = dir.path().join("out");
    let findings = reviewer.round(&round, linked(&checkout), "origin/main", &out, 1, "");
    assert_eq!(findings, Ok(Vec::new()));
    assert_eq!(
        server.seen(),
        [
            "GET /v1/models HTTP/1.1",
            "POST /v1/chat/completions HTTP/1.1"
        ]
    );
    assert_eq!(server.authorizations()[1].as_deref(), Some("Bearer pk-r"));
    assert!(!dir.path().join("locks").exists(), "no lock was taken");
}
