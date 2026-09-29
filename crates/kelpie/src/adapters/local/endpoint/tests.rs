use std::path::PathBuf;

use super::*;
use crate::ports::{Reviewer, Severity};
use crate::settings::{ContextSize, EndpointUrl, LocalRound, NonBlank};
use crate::test::{Answer, StandInEndpoint, git, unreachable_url};

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
fn repo(dir: &Path, change: &str) -> PathBuf {
    let worktree = dir.join("wt");
    std::fs::create_dir_all(worktree.join("src")).unwrap();
    git(&worktree, &["init", "--quiet", "-b", "main"]);
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
    worktree
}

fn local(url: &str, context: u32) -> LocalRound {
    LocalRound::Endpoint(Endpoint {
        url: EndpointUrl::try_from(url.to_owned()).unwrap(),
        model: NonBlank::try_from("coder".to_owned()).unwrap(),
        context: ContextSize::try_from(i64::from(context)).unwrap(),
        gpu_lease: false,
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
            &worktree,
            "origin/main",
            &out,
            3,
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
            &worktree,
            "origin/main",
            &out,
            1,
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
        .round(&local(server.url(), 8192), &worktree, "HEAD", &out, 1)
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
        .round(&local, &worktree, "origin/main", &out, 1)
        .unwrap_err();
    let url = format!("{}/chat/completions", server.url());
    assert_eq!(
        err,
        ReviewerError::Failed(format!(
            "{url} answered HTTP 500: {{\"error\":\"model not loaded\"}}"
        ))
    );
    let err = reviewer
        .round(&local, &worktree, "origin/main", &out, 2)
        .unwrap_err();
    assert_eq!(err, ReviewerError::Unreadable("not json".into()));
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
    let first = reviewer.round(&local, &worktree, "origin/main", &out, 1);
    assert_eq!(first.unwrap().len(), 1);
    let again = reviewer.round(&local, &worktree, "origin/main", &out, 1);
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
