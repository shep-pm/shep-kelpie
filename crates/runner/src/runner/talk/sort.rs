//! Which of the comments on a pull request are requests of the worker

use std::collections::BTreeSet;

use super::COMMENT_REQUEST;
use crate::ports::{Remark, Standing, Talk};
use crate::runner::review_bot::is_reply;
use crate::runner::thread_replies::is_reply_words;
use crate::runner::words::is_free_text;
use crate::work_item::Talked;

/// What was written, to be sent to the worker as one numbered request
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Request {
    /// What the worker's reply names it by once answered: a review thread's
    /// id, or `comment:` and a conversation comment's id
    pub id: String,
    /// The file and line of a review thread
    pub place: Option<String>,
    /// Who wrote what, oldest first
    pub said: Vec<(String, String)>,
}

/// What a read found
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Sorted {
    pub requests: Vec<Request>,
    /// One line for each comment left alone because of who wrote it
    pub ignored: Vec<String>,
    /// The conversation comments handled, requests and ignored alike
    pub comments: Vec<u64>,
    /// Each review thread handled, and the latest comment of it
    pub threads: Vec<(String, u64)>,
    /// The conversation comments the maintainer wrote that are the project
    /// manager's to read, each with its id and what it says
    pub held: Vec<(u64, String)>,
}

/// A login a pending ruling asks to reply
pub(super) struct Left {
    pub login: String,
    /// Whether the ruling takes any text as its answer, a worker's question
    pub takes_text: bool,
    /// Whether it is a merge ruling, whose thread a plain comment is the
    /// worker's to read in
    pub merge: bool,
    /// With a project manager named, the id of the comment the ruling's
    /// thread reads after: a comment up to it is older than that read, which
    /// never sees it, so it is sorted here
    pub after: Option<u64>,
}

/// What a read is sorted by
pub(super) struct Sieve<'a> {
    pub talked: &'a Talked,
    /// The ids of the comments kelpie posted itself
    pub own: &'a BTreeSet<u64>,
    /// The logins a pending ruling asks to reply in its thread, whose replies
    /// that thread reads
    pub left: &'a [Left],
    /// The maintainer's login, where a project manager is named to read what
    /// they write in the conversation, and none where it is not
    pub maintainer: Option<&'a str>,
    /// Whether what is already written is only noted: a pull request kelpie
    /// did not open has comments the worker has seen
    pub quiet: bool,
}

pub(super) fn sort(talk: &Talk, sieve: &Sieve<'_>) -> Sorted {
    let mut sorted = Sorted::default();
    for comment in &talk.comments {
        if sieve.talked.comments.contains(&comment.id) {
            continue;
        }
        if sieve.own.contains(&comment.id) {
            sorted.comments.push(comment.id);
            continue;
        }
        if comment.standing != Standing::Writer {
            sorted.ignored.push(ignored(comment, "the conversation"));
            sorted.comments.push(comment.id);
            continue;
        }
        let aimed_at_a_ruling = sieve.left.iter().any(|left| {
            left.login.eq_ignore_ascii_case(&comment.author)
                && (left.takes_text
                    || (!left.merge && left.after.is_none_or(|after| comment.id > after))
                    || is_reply_words(&comment.body))
        });
        if aimed_at_a_ruling {
            continue;
        }
        sorted.comments.push(comment.id);
        if sieve.quiet {
            continue;
        }
        let theirs =
            (sieve.maintainer).is_some_and(|login| login.eq_ignore_ascii_case(&comment.author));
        if theirs && is_free_text(&comment.body) {
            sorted.held.push((comment.id, comment.body.clone()));
        } else {
            sorted.requests.push(Request {
                id: format!("{COMMENT_REQUEST}{}", comment.id),
                place: None,
                said: vec![(comment.author.clone(), comment.body.clone())],
            });
        }
    }
    for thread in talk.threads.iter().filter(|t| !t.resolved) {
        let seen = sieve.talked.threads.get(&thread.id).copied().unwrap_or(0);
        let Some(latest) = thread
            .remarks
            .iter()
            .map(|r| r.id)
            .max()
            .filter(|l| *l > seen)
        else {
            continue;
        };
        sorted.threads.push((thread.id.clone(), latest));
        let place = match thread.line {
            Some(line) => format!("{}:{line}", thread.path),
            None => thread.path.clone(),
        };
        let mut asked = false;
        let mut said = Vec::new();
        for remark in &thread.remarks {
            let fresh = remark.id > seen;
            match remark.standing {
                Standing::Writer if !is_reply(&remark.body) => {
                    asked |= fresh;
                    said.push((remark.author.clone(), remark.body.clone()));
                }
                Standing::Writer => {}
                _ if fresh => sorted.ignored.push(ignored(remark, &place)),
                _ => {}
            }
        }
        if asked && !sieve.quiet {
            sorted.requests.push(Request {
                id: thread.id.clone(),
                place: Some(place),
                said,
            });
        }
    }
    sorted
}

fn ignored(remark: &Remark, on: &str) -> String {
    let who = match remark.standing {
        Standing::Bot => "an App or a bot",
        _ => "someone without write access",
    };
    format!(
        "ignored comment {} by {} on {on}: {who} is not trusted to ask for changes",
        remark.id, remark.author
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn written(id: u64, body: &str) -> Remark {
        Remark {
            id,
            author: "octocat".into(),
            standing: Standing::Writer,
            body: body.into(),
        }
    }

    fn sorted_with(after: Option<u64>, comments: Vec<Remark>) -> Sorted {
        let talk = Talk {
            comments,
            threads: Vec::new(),
        };
        let left = [Left {
            login: "octocat".into(),
            takes_text: false,
            merge: false,
            after,
        }];
        let sieve = Sieve {
            talked: &Talked::default(),
            own: &BTreeSet::new(),
            left: &left,
            maintainer: Some("octocat"),
            quiet: false,
        };
        sort(&talk, &sieve)
    }

    #[test]
    fn a_maintainer_comment_older_than_a_rulings_place_is_held_not_skipped() {
        let sorted = sorted_with(
            Some(10),
            vec![
                written(7, "Why is this one so large?"),
                written(11, "Later"),
            ],
        );

        assert_eq!(
            sorted.held,
            vec![(7, "Why is this one so large?".to_owned())]
        );
        assert_eq!(
            sorted.comments,
            vec![7],
            "the later one is left to its thread"
        );
    }

    // No ruling waits, so every comment is the comment round's to sort
    fn sorted_bare(maintainer: Option<&str>, body: &str) -> Sorted {
        let talk = Talk {
            comments: vec![written(7, body)],
            threads: Vec::new(),
        };
        let sieve = Sieve {
            talked: &Talked::default(),
            own: &BTreeSet::new(),
            left: &[],
            maintainer,
            quiet: false,
        };
        sort(&talk, &sieve)
    }

    #[test]
    fn a_maintainer_comment_is_held_unless_it_opens_with_an_answer_word() {
        let held = sorted_bare(Some("octocat"), "Why is the cache a map?");
        assert_eq!(held.held.len(), 1);
        assert_eq!(held.requests, Vec::new());
        for text in ["rerun the checks", "restore it", "pm recommend", "71 yes"] {
            let sorted = sorted_bare(Some("octocat"), text);
            assert_eq!(sorted.held, Vec::new(), "{text}");
            assert_eq!(sorted.requests.len(), 1, "{text}");
        }
    }

    #[test]
    fn with_no_project_manager_a_maintainer_comment_is_a_request() {
        let sorted = sorted_bare(None, "Why is the cache a map?");
        assert_eq!(sorted.held, Vec::new());
        assert_eq!(sorted.requests.len(), 1);
    }

    #[test]
    fn with_no_project_manager_a_comment_under_a_yes_or_no_ruling_is_left_to_its_thread() {
        let sorted = sorted_with(None, vec![written(7, "Why is this one so large?")]);

        assert_eq!(sorted.held, Vec::new());
        assert_eq!(sorted.comments, Vec::<u64>::new());
    }
}
