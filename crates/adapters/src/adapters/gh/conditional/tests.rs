//! Conditional reads, against a stand-in for GitHub that answers 304 to a tag it gave

use super::*;

// GitHub's side of the reads: each path's answer, with a tag that changes
// when the answer does, and every request it was sent
#[derive(Default)]
struct FakeGithub {
    answers: Mutex<BTreeMap<String, (u32, Vec<u8>)>>,
    asked: Mutex<Vec<(Key, Option<String>)>>,
    // Whether a request with a tag fails, as `gh` might on a 304
    refuses_tags: AtomicBool,
    // A failure every request gets while it is set
    fails_with: Mutex<Option<String>>,
}

impl FakeGithub {
    fn answers(&self, path: &str, body: &str) {
        let mut answers = self.answers.lock().unwrap();
        let version = answers.get(path).map_or(1, |(v, _)| v + 1);
        answers.insert(path.to_owned(), (version, body.as_bytes().to_vec()));
    }

    fn asked(&self) -> Vec<(String, Option<String>)> {
        (self.asked.lock().unwrap().iter())
            .map(|((_, path), etag)| (path.clone(), etag.clone()))
            .collect()
    }
}

impl Fetch for FakeGithub {
    fn get(
        &self,
        config: Option<&Path>,
        path: &str,
        etag: Option<&str>,
    ) -> Result<Fetched, ForgeError> {
        self.asked.lock().unwrap().push((
            (config.map(Path::to_owned), path.to_owned()),
            etag.map(str::to_owned),
        ));
        if let Some(said) = self.fails_with.lock().unwrap().clone() {
            return Err(ForgeError::Failed(said));
        }
        if etag.is_some() && self.refuses_tags.load(Ordering::Relaxed) {
            return Err(ForgeError::Failed("gh: HTTP 304".to_owned()));
        }
        let (version, body) = (self.answers.lock().unwrap().get(path).cloned())
            .ok_or_else(|| ForgeError::Failed(format!("HTTP 404: {path}")))?;
        let tag = format!("W/\"{path}-{version}\"");
        if etag == Some(tag.as_str()) {
            return Ok(Fetched::NotModified);
        }
        Ok(Fetched::Fresh {
            etag: Some(tag),
            body,
        })
    }
}

fn etags() -> (Etags, Revalidated) {
    let revalidated = Revalidated::default();
    (Etags::new(revalidated.clone()), revalidated)
}

#[test]
fn a_second_read_of_an_unchanged_answer_sends_its_tag_and_uses_the_kept_answer() {
    let (etags, revalidated) = etags();
    let github = FakeGithub::default();
    github.answers("repos/o/r/issues/comments", "[1]");

    let first = etags
        .get(&github, None, "repos/o/r/issues/comments")
        .unwrap();
    let second = etags
        .get(&github, None, "repos/o/r/issues/comments")
        .unwrap();

    assert_eq!((first, second), (b"[1]".to_vec(), b"[1]".to_vec()));
    assert_eq!(
        github.asked(),
        [
            ("repos/o/r/issues/comments".to_owned(), None),
            (
                "repos/o/r/issues/comments".to_owned(),
                Some("W/\"repos/o/r/issues/comments-1\"".to_owned())
            ),
        ]
    );
    assert_eq!(revalidated.take(), 1);
}

#[test]
fn a_changed_answer_replaces_the_kept_one_and_its_tag() {
    let (etags, revalidated) = etags();
    let github = FakeGithub::default();
    github.answers("p", "[1]");
    etags.get(&github, None, "p").unwrap();
    github.answers("p", "[1,2]");

    let changed = etags.get(&github, None, "p").unwrap();
    let kept = etags.get(&github, None, "p").unwrap();

    assert_eq!((changed, kept), (b"[1,2]".to_vec(), b"[1,2]".to_vec()));
    assert_eq!(github.asked()[2].1.as_deref(), Some("W/\"p-2\""));
    assert_eq!(revalidated.take(), 1, "only the unchanged read was a 304");
}

#[test]
fn what_is_kept_is_kept_for_the_login_and_the_path_alone() {
    let (etags, revalidated) = etags();
    let github = FakeGithub::default();
    github.answers("p", "[1]");
    github.answers("q", "[2]");
    let app = Path::new("/reads/o--r/gh");
    etags.get(&github, None, "p").unwrap();

    etags.get(&github, Some(app), "p").unwrap();
    etags.get(&github, None, "q").unwrap();

    assert!(github.asked()[1..].iter().all(|(_, etag)| etag.is_none()));
    assert_eq!(revalidated.take(), 0);

    etags.forget(app);
    etags.get(&github, Some(app), "p").unwrap();
    etags.get(&github, None, "p").unwrap();
    let asked = github.asked();
    assert_eq!(asked[3].1, None, "forgotten with the token");
    assert!(asked[4].1.is_some(), "the login's stays");
}

#[test]
fn a_list_is_read_a_page_at_a_time_and_each_page_is_its_own_conditional_read() {
    let (etags, revalidated) = etags();
    let github = FakeGithub::default();
    let full = format!("[{}]", vec!["{}"; PER_PAGE].join(","));
    github.answers("list?per_page=100&page=1", &full);
    github.answers("list?per_page=100&page=2", "[{}]");

    let first = etags.pages(&github, None, "list?per_page=100").unwrap();
    let again = etags.pages(&github, None, "list?per_page=100").unwrap();

    assert_eq!(first.len(), 2);
    assert_eq!(first, again);
    assert_eq!(github.asked().len(), 4);
    assert_eq!(revalidated.take(), 2);
}

#[test]
fn a_list_inside_an_object_is_read_a_page_at_a_time() {
    let (etags, revalidated) = etags();
    let github = FakeGithub::default();
    let full = format!(r#"{{"check_runs":[{}]}}"#, vec!["{}"; PER_PAGE].join(","));
    github.answers("runs?per_page=100&page=1", &full);
    github.answers("runs?per_page=100&page=2", r#"{"check_runs":[{}]}"#);

    let first = etags
        .pages_in(&github, None, "runs?per_page=100", "check_runs")
        .unwrap();
    let again = etags
        .pages_in(&github, None, "runs?per_page=100", "check_runs")
        .unwrap();

    assert_eq!(first.len(), 2);
    assert_eq!(first, again);
    assert_eq!(github.asked().len(), 4);
    assert_eq!(revalidated.take(), 2);
}

#[test]
fn an_answer_with_no_such_array_is_unreadable() {
    let (etags, _) = etags();
    let github = FakeGithub::default();
    github.answers("runs?per_page=100&page=1", r#"{"other":[]}"#);

    let error = etags
        .pages_in(&github, None, "runs?per_page=100", "check_runs")
        .unwrap_err();

    assert!(matches!(error, ForgeError::Unreadable(_)), "{error:?}");
}

#[test]
fn an_answer_that_is_not_a_list_is_unreadable() {
    let (etags, _) = etags();
    let github = FakeGithub::default();
    github.answers("list?per_page=100&page=1", "{}");

    let error = etags.pages(&github, None, "list?per_page=100").unwrap_err();

    assert!(matches!(error, ForgeError::Unreadable(_)), "{error:?}");
}

#[test]
fn an_answer_with_no_tag_is_not_kept() {
    struct Untagged;
    impl Fetch for Untagged {
        fn get(
            &self,
            _: Option<&Path>,
            _: &str,
            etag: Option<&str>,
        ) -> Result<Fetched, ForgeError> {
            assert_eq!(etag, None);
            Ok(Fetched::Fresh {
                etag: None,
                body: b"[]".to_vec(),
            })
        }
    }
    let (etags, revalidated) = etags();

    etags.get(&Untagged, None, "p").unwrap();
    etags.get(&Untagged, None, "p").unwrap();

    assert_eq!(revalidated.take(), 0);
}

#[test]
fn a_read_that_fails_with_a_tag_is_made_again_without_one_and_no_tag_is_sent_after() {
    let (etags, revalidated) = etags();
    let github = FakeGithub::default();
    github.answers("p", "[1]");
    etags.get(&github, None, "p").unwrap();
    github.refuses_tags.store(true, Ordering::Relaxed);

    let read = etags.get(&github, None, "p").unwrap();
    etags.get(&github, None, "p").unwrap();

    assert_eq!(read, b"[1]".to_vec());
    let asked = github.asked();
    assert_eq!(asked.len(), 4, "{asked:?}");
    assert_eq!(asked[1].1.as_deref(), Some("W/\"p-1\""));
    assert_eq!(asked[2].1, None);
    assert_eq!(asked[3].1, None, "no tag after one failed");
    assert_eq!(revalidated.take(), 0);
}

#[test]
fn a_blip_that_fails_a_tagged_read_is_that_read_s_failure_and_leaves_tags_on() {
    let (etags, revalidated) = etags();
    let github = FakeGithub::default();
    github.answers("p", "[1]");
    etags.get(&github, None, "p").unwrap();
    github
        .fails_with
        .lock()
        .unwrap()
        .replace("HTTP 503".to_owned());

    let error = etags.get(&github, None, "p").unwrap_err();
    github.fails_with.lock().unwrap().take();
    etags.get(&github, None, "p").unwrap();

    assert!(matches!(error, ForgeError::Failed(_)), "{error:?}");
    let asked = github.asked();
    assert_eq!(asked.len(), 3, "no read made again: {asked:?}");
    assert!(asked[2].1.is_some(), "the tag is still sent");
    assert_eq!(revalidated.take(), 1);
}

#[test]
fn a_list_past_the_pages_read_is_unreadable_not_cut_short() {
    let (etags, _) = etags();
    let github = FakeGithub::default();
    let full = format!("[{}]", vec!["{}"; PER_PAGE].join(","));
    for page in 1..=MAX_PAGES {
        github.answers(&format!("list?per_page=100&page={page}"), &full);
    }

    let error = etags.pages(&github, None, "list?per_page=100").unwrap_err();

    assert!(matches!(error, ForgeError::Unreadable(_)), "{error:?}");
}

#[test]
fn a_read_that_fails_with_and_without_a_tag_is_a_failure() {
    let (etags, _) = etags();
    let github = FakeGithub::default();
    github.answers("p", "[1]");
    etags.get(&github, None, "p").unwrap();
    github.answers.lock().unwrap().clear();

    let error = etags.get(&github, None, "p").unwrap_err();

    assert!(matches!(error, ForgeError::Failed(_)), "{error:?}");
}

#[test]
fn the_kept_answers_are_few() {
    let (etags, _) = etags();
    let github = FakeGithub::default();
    for n in 0..=KEPT {
        github.answers(&format!("p{n}"), "[]");
        etags.get(&github, None, &format!("p{n}")).unwrap();
    }

    assert!(etags.kept.lock().unwrap().len() <= KEPT);
}

fn printed(status: i32, stdout: &str, stderr: &str) -> Output {
    Output {
        status: crate::test::exited(status),
        stdout: stdout.as_bytes().to_vec(),
        stderr: stderr.as_bytes().to_vec(),
    }
}

#[test]
fn an_answer_gh_printed_with_its_headers_is_split_into_tag_and_body() {
    let out = printed(
        0,
        "HTTP/2.0 200 OK\r\nContent-Type: application/json\r\nEtag: W/\"abc\"\r\n\r\n[{\"id\":1}]",
        "",
    );

    assert_eq!(
        answer(&out).unwrap(),
        Fetched::Fresh {
            etag: Some("W/\"abc\"".to_owned()),
            body: b"[{\"id\":1}]".to_vec()
        }
    );
}

#[test]
fn a_304_is_read_whether_gh_exits_zero_or_not_and_whichever_stream_says_it() {
    let headers = "HTTP/2.0 304 Not Modified\r\nEtag: W/\"abc\"\r\n\r\n";
    let debug =
        "* Request to https://api.github.com/x\n< HTTP/2.0 304 Not Modified\n< Etag: W/\"abc\"\n";
    for out in [
        printed(0, headers, ""),
        printed(1, headers, "gh: HTTP 304"),
        printed(1, "", debug),
        printed(1, "", "gh: HTTP 304\n"),
    ] {
        assert_eq!(answer(&out).unwrap(), Fetched::NotModified, "{out:?}");
    }
}

#[test]
fn any_other_failure_is_gh_s_own() {
    let out = printed(
        1,
        "HTTP/2.0 404 Not Found\r\n\r\n{}",
        "gh: Not Found (HTTP 404)\n",
    );

    assert!(matches!(answer(&out), Err(ForgeError::Failed(_))));
}

#[test]
fn a_success_with_no_status_is_unreadable() {
    assert!(matches!(
        answer(&printed(0, "[]", "")),
        Err(ForgeError::Unreadable(_))
    ));
}
