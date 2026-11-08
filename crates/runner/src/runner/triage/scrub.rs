//! Research and wording that can leave this machine

use super::super::Runner;
use super::super::contributions::post::refused_text;
use super::super::representative::IssuePost;
use super::facts::refused_facts;
use super::marker::unmarked;
use super::research::Found;
use crate::local_paths::{Leak, Surface};
use crate::ports::ForgeError;

impl Runner {
    pub(super) fn keep_triage_refusal(&mut self, issue: u64, error: &ForgeError, body: String) {
        if !refused_text(error) {
            return;
        }
        let tries = self.triage_tries.entry(issue).or_default();
        if let ForgeError::LocalPath { leak, .. } = error {
            tries.refused = Some(*leak);
        }
        tries.refused_bodies.push(body);
        tries.body = None;
    }

    pub(super) fn repeats_refused_triage(&mut self, issue: u64, body: &str) -> bool {
        let repeats = self
            .triage_tries
            .get(&issue)
            .is_some_and(|t| t.refused_bodies.iter().any(|refused| refused == body));
        if repeats {
            self.triage_tries.entry(issue).or_default().body = None;
            self.triage_failed(issue, "its wording repeats a refused comment", true);
        }
        repeats
    }

    pub(super) fn word_triage(&mut self, issue: u64, post: &IssuePost<'_>) -> Option<String> {
        for attempt in 0..2 {
            let leak = self.triage_tries.get(&issue).and_then(|t| t.refused);
            let facts = match leak {
                Some(leak) => refused_facts(post.facts, leak),
                None => post.facts.to_owned(),
            };
            let words = match self.represent_issue(
                issue,
                &IssuePost {
                    facts: &facts,
                    ..*post
                },
            ) {
                Ok(words) => words,
                Err(e) => {
                    self.triage_failed(issue, &e.to_string(), true);
                    return None;
                }
            };
            match self.local.find(&words, Surface::Prose) {
                None => {
                    if let Some(tries) = self.triage_tries.get_mut(&issue) {
                        tries.refused = None;
                    }
                    return Some(unmarked(&words));
                }
                Some(leak) => {
                    self.triage_tries.entry(issue).or_default().refused = Some(leak);
                    if attempt == 1 {
                        self.triage_failed(issue, &format!("its wording still names {leak}"), true);
                    }
                }
            }
        }
        None
    }

    pub(super) fn scrub_triage_title<'a>(&self, issue: u64, title: &'a str) -> &'a str {
        match self.local.find(title, Surface::Prose) {
            Some(leak) => {
                self.notes
                    .push(format!("triage #{issue}: 1 titles left out, naming {leak}"));
                ""
            }
            None => title,
        }
    }

    pub(super) fn scrub_triage(&self, issue: u64, found: &Found) -> Found {
        let mut found = found.clone();
        self.scrub_triage_parts(issue, "questions", &mut found.open, |q| {
            self.local
                .find(&q.question, Surface::Prose)
                .or_else(|| self.local.find(&q.recommended, Surface::Prose))
        });
        for (part, items) in [
            ("files", &mut found.files),
            ("tests", &mut found.tests),
            ("criteria", &mut found.criteria),
        ] {
            self.scrub_triage_parts(issue, part, items, |text| {
                self.local.find(text, Surface::Prose)
            });
        }
        found
    }

    fn scrub_triage_parts<T>(
        &self,
        issue: u64,
        part: &str,
        items: &mut Vec<T>,
        find: impl Fn(&T) -> Option<Leak>,
    ) {
        let mut leaks = Vec::new();
        items.retain(|item| match find(item) {
            Some(leak) => {
                leaks.push(leak);
                false
            }
            None => true,
        });
        for leak in [Leak::Path, Leak::Private, Leak::Lan] {
            let count = leaks.iter().filter(|&&found| found == leak).count();
            if count != 0 {
                self.notes.push(format!(
                    "triage #{issue}: {count} {part} left out, naming {leak}"
                ));
            }
        }
    }
}
