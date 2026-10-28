//! The end of a worker's turn: what the call left on `origin`, what the work
//! item's phase becomes, and what a turn that ended without a reply parks on
//!
//! A turn is ended on the pass after its call's news comes back. A turn still
//! marked running when the runner starts was cut short, and `super` resumes its
//! session; a turn stopped with the runner stays running here for the same.

use super::*;

impl Runner {
    pub(in crate::runner) fn end_turn(
        &mut self,
        result: Result<AgentReply, AgentError>,
    ) -> Result<Option<StepReport>, StateError> {
        // A turn stopped with the runner stays running, to resume on restart.
        // Its time so far is saved to the worker, which still runs it.
        if matches!(result, Err(AgentError::Stopped)) {
            return self.save_time().map(|()| None);
        }
        self.builds_turn_ended();
        self.dev_server_ended();
        // A turn a pause ended at a tool result is not a finished one.
        if let Some(paused) = self.end_paused(&result) {
            return paused;
        }
        let now = self.ports.clock.now();
        // A fork worker only commits: its commits go to the contributor's
        // branch before `origin`'s head is read. A push that is held or refused
        // is the gate's to tell, which tries it again, so the worker is not
        // sent back for a push it cannot make.
        let held = result.is_ok()
            && matches!(
                self.push_to_fork(),
                Sent::Moved | Sent::Ended | Sent::Refused(_)
            );
        // Whatever the turn left on `origin` is the worker's own. A head
        // that cannot be read keeps the last one, which errs toward parking.
        let read = self.take_end_read();
        let pushed = self.current().and_then(|item| match &read {
            Some(read) => read.own_push(item.adopted),
            None => self.own_push(),
        });
        // Whether the head on `origin` is the worktree's own commit, which a
        // push by anyone else is not.
        let local = match (&read, self.focus) {
            (Some(read), Some(issue)) => read.worktree_head(issue),
            _ => self.worktree_head(),
        };
        let own_commit = pushed.is_some() && local == pushed;
        let mut next = self.state.clone();
        let Some(item) = self.current_in(&mut next) else {
            return Ok(None);
        };
        let issue = item.issue;
        // A rework or adoption turn that moved nothing on `origin` pushed no fix.
        let before = item.known.head.clone();
        let no_push = pushed.as_ref().is_none_or(|p| Some(p) == before.as_ref());
        let pushed_nothing = before.is_some() && no_push && !held;
        if pushed.is_some() {
            item.known.head = pushed;
        }
        let report = match result {
            Ok(reply) => {
                let question = asked(&reply.text);
                // Only true the very first time: the pull request is
                // discovered once, and every later turn that reaches here
                // (a CI-failure fix, most often) already knows it.
                let discovering = item.pull_request.is_none();
                // Only a worker on a repo the App covers hands its pull request
                // over: any other opens and edits its own, and a file it left
                // is not kelpie's to act on.
                let handed_over = self.app_covers();
                if discovering {
                    // Only this turn's refusal is told: an earlier one no longer
                    // says why there is no pull request.
                    item.pull_request_refused = None;
                    item.pull_request = read
                        .as_ref()
                        .and_then(|read| read.pull_request_from(&item.branch))
                        .unwrap_or_else(|| self.pull_request_from(&item.branch));
                    // A worker whose token opens no pull request wrote its text
                    // for kelpie to open it from, once the branch is pushed.
                    if handed_over && item.pull_request.is_none() && item.known.head.is_some() {
                        match self.open_handed_over(&item.build, &item.branch, self.item_base(item))
                        {
                            Ok(opened) => item.pull_request = opened,
                            // The worker can fix its own file, so it is told why.
                            Err(refusal) => {
                                item.pull_request_refused = Some(pull_request_file::told(&refusal));
                                self.started.push(StepReport::PullRequestNotOpened {
                                    issue,
                                    reason: refusal.to_string(),
                                });
                            }
                        }
                    }
                }
                // A later turn that rewrote the file changes the title and body,
                // and a project's hook may refuse what it wrote.
                let file_refused = match (handed_over, discovering, item.pull_request) {
                    (true, false, Some(number)) => self.edit_handed_over(&item.build, number),
                    _ => None,
                };
                let session = item.session.clone();
                let cost =
                    item.record_call(Role::Worker, now, session, reply.usage, reply.session_cost);
                item.summary = crate::board::briefing::summary(&reply.text);
                item.context = reply.context;
                // A fix turn's reply says what it made of each bot thread sent.
                let fixing = matches!(&item.phase, Phase::Review(r) if matches!(r.stage, ReviewStage::Fixing { .. }));
                if fixing || item.talked.head.is_some() {
                    item.answer_threads(&reply.text);
                }
                // A turn that left its work uncommitted and pushed nothing is
                // sent back once, before the gate can park it on a ruling. A
                // turn to push before a round is that once already.
                let asked = std::mem::take(&mut item.asked_to_commit);
                let pushing =
                    matches!(&item.phase, Phase::Review(r) if r.stage == ReviewStage::Pushing);
                // A worktree git cannot be read in is told and let go: the
                // turn ends as it did before, which errs toward the ruling.
                let uncommitted =
                    match (question.is_none() && no_push && !asked && !pushing).then(|| match &read
                    {
                        Some(read) => read.uncommitted(),
                        None => worktree::uncommitted(&self.settings.git.checkout, &item.worktree)
                            .map_err(|e| e.to_string()),
                    }) {
                        Some(Ok(files)) => files,
                        Some(Err(e)) => {
                            eprintln!("cannot list issue #{issue}'s uncommitted files: {e}");
                            Vec::new()
                        }
                        None => Vec::new(),
                    };
                let commit_first = !uncommitted.is_empty();
                // The branch is handed over when the turn ends with it pushed,
                // asking nothing and holding no work back, and the project's
                // hooks judge it before review reads it. A refusal sends the
                // worker back once, and the next parks the work item.
                let ended = match commit_first || question.is_some() {
                    true => None,
                    false => self.turn_end(item, &reply.text),
                };
                let handed = match commit_first || question.is_some() || ended.is_some() {
                    true => Handover::NotDue,
                    false => self.handover(item),
                };
                let already = item.handover_refused;
                if let Handover::Passed(head) = &handed {
                    item.handed_over = Some(head.clone());
                    item.handover_refused = false;
                }
                let refusal = match (&ended, handed) {
                    (Some(why), _) => Some(Refused::turn(why)),
                    (None, Handover::Refused(why)) => Some(Refused::branch(&why)),
                    _ => file_refused.as_deref().map(Refused::pull_request_file),
                };
                // A turn no hook refused, with its work all committed, spent the
                // worker's send-back: the next refusal, of whatever hook, is
                // a new one and sends it back again.
                if refusal.is_none() && !commit_first {
                    item.handover_refused = false;
                }
                let hook_parks = refusal.clone().filter(|_| already);
                let hook_first = refusal.as_ref().filter(|_| !already);
                let to_worker = commit_first || refusal.is_some();
                item.turn = if to_worker {
                    item.asked_to_commit |= commit_first;
                    item.handover_refused |= hook_first.is_some();
                    // The pull request this turn opened still owes its first
                    // review, which the follow-up's end is too late to see as
                    // a discovery.
                    if discovering
                        && item.pull_request.is_some()
                        && item.resume.is_none()
                        && matches!(item.phase, Phase::Implement)
                    {
                        item.resume = Some(Phase::Review(Review::first()));
                    }
                    Turn::Next {
                        prompt: match &refusal {
                            Some(refused) if !commit_first => refused.prompt.clone(),
                            _ => uncommitted_prompt(&uncommitted),
                        },
                    }
                } else {
                    Turn::Ended { at: now }
                };
                // A question leaves the phase untouched: it interrupted
                // whatever was running, before that turn could be said to
                // have ended normally, and the answer resumes exactly this,
                // captured below before `park` parks it on a ruling.
                let stopped_short = pushed_nothing && awaits_a_push(item);
                if question.is_none() && !stopped_short && !to_worker {
                    if let Some(resume) = item.resume.take() {
                        item.phase = resume;
                        // The follow-up was the worker's one turn to push, so
                        // a round finding the worktree off the pushed head parks.
                        if asked
                            && let Phase::Review(review) = &mut item.phase
                            && review.stage == ReviewStage::Round
                        {
                            review.stage = ReviewStage::Pushing;
                        }
                    } else {
                        match item.phase.clone() {
                            Phase::Implement if discovering && item.pull_request.is_some() => {
                                item.phase = Phase::Review(Review::first());
                            }
                            // A fix sent to CI with no round's read, on purpose:
                            // red CI's, a conflict's.
                            Phase::Implement if item.pull_request.is_some() => {
                                let fixed = !no_push && own_commit;
                                // A fork worker's commit that waits for the gate
                                // to push it is the same fix, under its own head.
                                let waiting = local.clone().filter(|_| held);
                                let sent = item.known.head.clone().filter(|_| fixed);
                                if let Some(head) = waiting.or(sent) {
                                    item.send_unread(head);
                                }
                                item.phase = Phase::Ci {
                                    head: None,
                                    since: now,
                                };
                            }
                            // A fix turn stays fixing: the next step checks it pushed.
                            _ => {}
                        }
                    }
                }
                let session = item.session.clone();
                let (work_item_cost_usd, pull_request) = (item.cost().usd(), item.pull_request);
                match question {
                    // The hooks refused the branch again: the worker's turn to fix
                    // it was spent, so the work item parks, and a yes sends it back.
                    None if hook_parks.is_some() => {
                        let why = hook_parks.map(|refused| refused.parked).unwrap_or_default();
                        failed(self.names(), &mut next, issue, now, why, Vec::new(), false)
                    }
                    None => StepReport::Ended {
                        issue,
                        session,
                        usage: reply.usage,
                        cost_usd: cost.map(Cost::usd),
                        work_item_cost_usd,
                        pull_request,
                    },
                    Some(text) => {
                        let resume = match &item.phase {
                            Phase::Review(review) => Resume::Review(review.clone()),
                            // A CI fix turn sent from a round waiting on its head's run
                            // goes back to that round once answered, and the answer
                            // keeps the saved round in `resume`.
                            Phase::Implement if matches!(item.resume, Some(Phase::Review(_))) => {
                                Resume::Nothing
                            }
                            // A merge ruling's no sends its fix to CI, question or none.
                            Phase::Implement if item.noted_from.is_some() => Resume::Nothing,
                            Phase::Implement if pull_request.is_some() => Resume::ReviewFirst,
                            _ => Resume::Nothing,
                        };
                        let kind = RulingKind::Question {
                            asked: text,
                            resume,
                        };
                        let names = self.names();
                        let (id, question) = park(names, &mut next, issue, pull_request, kind);
                        StepReport::Asked {
                            issue,
                            session,
                            usage: reply.usage,
                            cost_usd: cost.map(Cost::usd),
                            work_item_cost_usd,
                            pull_request,
                            id,
                            question,
                            comment_failed: None,
                        }
                    }
                }
            }
            // A pull request that covers the work item ends it at the next step, with no
            // ruling raised and no turn resumed.
            Err(AgentError::TimedOut(_))
                if item.pull_request.is_none() && self.covered_now(issue, &item.branch) =>
            {
                item.turn = Turn::Ended { at: now };
                self.save(next)?;
                return Ok(None);
            }
            Err(AgentError::TimedOut(_)) if self.resumes_past_ceiling(item, now) => {
                self.save(next)?;
                return Ok(None);
            }
            Err(AgentError::TimedOut(_)) => {
                item.turn = Turn::Ended { at: now };
                timed_out(self.names(), &mut next, issue)
            }
            Err(e) => {
                let restore = self.restorable(issue);
                let setup = matches!(e, AgentError::Setup(_));
                failed(
                    self.names(),
                    &mut next,
                    issue,
                    now,
                    e.to_string(),
                    restore,
                    setup,
                )
            }
        };
        self.save(next)?;
        let mut report = report;
        self.fill_comment_failed(&mut report);
        Ok(Some(report))
    }
}
