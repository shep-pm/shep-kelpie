//! The runner's borrowed `status` types built as the protocol's owned ones
//!
//! Each mirror is a struct literal naming every field of what it mirrors, so a
//! field added to the runner's type fails the build until it is mirrored too.

use super::{RulingStatus, Status, WorkItemStatus};
use crate::reply::{Attaching, Board, Place, PmAttaching, Totals};
use crate::runner as r;
use crate::status as wire;

impl From<&Status<'_>> for wire::Status {
    fn from(status: &Status<'_>) -> Self {
        Self {
            project: status.project.to_owned(),
            merging: status.merging,
            work_item: status.work_item.as_ref().map(Into::into),
            work_items: status.work_items.iter().map(Into::into).collect(),
            active_items: status.active_items,
            working: status.working.clone(),
            waiting_for_slot: status.waiting_for_slot.clone(),
            parked: status.parked.clone(),
            pending_rulings: status.pending_rulings,
            adopted: status.adopted.to_vec(),
            contributions: status.contributions.iter().map(Into::into).collect(),
            bumps: status.bumps.iter().map(Into::into).collect(),
            triage: status.triage.as_ref().map(Into::into),
            order: status.order.to_vec(),
            skipped: status.skipped.to_vec(),
            standing: status.standing.clone(),
            red_workflows: status.red_workflows.iter().map(Into::into).collect(),
            rulings: status.rulings.iter().map(Into::into).collect(),
            pm_answers: status.pm_answers.clone(),
            leases: status.leases.to_vec(),
            history: status.history.to_vec(),
            pacer: (&status.pacer).into(),
            skills: status.skills.to_vec(),
            plugins: status.plugins.to_vec(),
            pull_request_template: (&status.pull_request_template).into(),
            gpu: status.gpu.as_ref().map(Into::into),
            disk_free_bytes: status.disk_free_bytes,
            local_leases: status.local_leases.clone(),
            board: status.board.to_path_buf(),
            pm: status.pm.as_ref().map(Into::into),
            build: status.build.map(str::to_owned),
            commit: status.commit.map(str::to_owned),
            draining: status.draining.as_ref().map(Into::into),
            run: status.run,
            board_poll_secs: status.board_poll_secs,
            forge_held_until: status.forge_held_until,
            settings_refused: status.settings_refused.map(Into::into),
            lock: status.lock.clone(),
            passes: status.passes,
        }
    }
}

impl From<&WorkItemStatus<'_>> for wire::WorkItemStatus {
    fn from(item: &WorkItemStatus<'_>) -> Self {
        Self {
            issue: item.issue,
            title: item.title.to_owned(),
            branch: item.branch.to_owned(),
            adopted: item.adopted,
            worktree: item.worktree.to_path_buf(),
            agent: item.agent.clone(),
            pinned: item.pinned,
            waiting: item.waiting.map(Into::into),
            held_up: item.held_up.as_ref().map(Into::into),
            failing: item.failing.as_ref().map(Into::into),
            checks: item.checks.map(str::to_owned),
            build_bytes: item.build_bytes,
            session: item.session.clone(),
            turn: item.turn.clone(),
            phase: item.phase.clone(),
            pull_request: item.pull_request,
            rerunning: item.rerunning.map(str::to_owned),
            bot_reads: item.bot_reads.clone(),
            bots_skipped: item.bots_skipped.to_vec(),
            calls: item.calls,
            cost_usd: item.cost_usd,
            unpriced_calls: item.unpriced_calls,
            by_role: item.by_role,
            budget: wire::BudgetStatus {
                spent_usd: item.budget.spent_usd,
                cap_usd: item.budget.cap_usd,
            },
            qwen: item.qwen,
            attached: item.attached.cloned(),
            local_reviewers_down: item
                .local_reviewers_down
                .iter()
                .map(|a| (*a).clone())
                .collect(),
            reviewers_skipped: item.reviewers_skipped.to_vec(),
            unreviewed: item.unreviewed.map(str::to_owned),
            test_checks: item.test_checks.to_vec(),
            review: item.review.clone(),
            timings: item.timings.clone(),
        }
    }
}

impl From<&RulingStatus<'_>> for wire::RulingStatus {
    fn from(ruling: &RulingStatus<'_>) -> Self {
        Self {
            ruling: (*ruling.0).clone(),
            id: ruling.0.number(),
        }
    }
}

impl From<&r::contributions::Followed<'_>> for wire::Followed {
    fn from(followed: &r::contributions::Followed<'_>) -> Self {
        Self {
            contribution: (*followed.contribution).clone(),
            clock: followed.clock.as_ref().map(Into::into),
        }
    }
}

impl From<&r::contributions::clock::ClockStatus> for wire::ClockStatus {
    fn from(clock: &r::contributions::clock::ClockStatus) -> Self {
        Self {
            waiting_for: clock.waiting_for,
            days_to_nudge: clock.days_to_nudge,
            days_to_quiet: clock.days_to_quiet,
            due: clock.due.map(Into::into),
        }
    }
}

impl From<r::contributions::clock::Due> for wire::ContributionDue {
    fn from(due: r::contributions::clock::Due) -> Self {
        match due {
            r::contributions::clock::Due::Nudge => Self::Nudge,
            r::contributions::clock::Due::Quiet => Self::Quiet,
        }
    }
}

impl From<&r::dependencies::BumpStatus> for wire::BumpStatus {
    fn from(bump: &r::dependencies::BumpStatus) -> Self {
        Self {
            pull_request: bump.pull_request,
            author: bump.author.clone(),
            head: bump.head.clone(),
            standing: bump.standing.clone(),
        }
    }
}

impl From<&r::triage::TriageStatus<'_>> for wire::TriageStatus {
    fn from(triage: &r::triage::TriageStatus<'_>) -> Self {
        Self {
            due: triage.due.iter().map(Into::into).collect(),
            running: triage.running,
            issues: triage.issues.to_vec(),
            daily: triage.daily.as_ref().map(Into::into),
        }
    }
}

impl From<&r::triage::DueStatus> for wire::DueStatus {
    fn from(due: &r::triage::DueStatus) -> Self {
        Self {
            issue: due.issue,
            title: due.title.clone(),
            author: due.author.clone(),
            author_association: due.author_association,
            created: due.created,
        }
    }
}

impl From<&r::triage::DailyStatus> for wire::DailyStatus {
    fn from(daily: &r::triage::DailyStatus) -> Self {
        Self {
            used: daily.used,
            cap: daily.cap,
            opens: daily.opens,
            wait: daily.wait,
        }
    }
}

impl From<&r::RedStatus> for wire::RedStatus {
    fn from(red: &r::RedStatus) -> Self {
        Self {
            workflow: red.workflow.clone(),
            first_run: red.first_run,
            run: red.run,
            head: red.head.clone(),
            since: red.since,
            red_for: red.red_for,
            url: red.url.clone(),
            doing: red.doing.clone(),
        }
    }
}

impl From<&r::PmStatus<'_>> for wire::PmStatus {
    fn from(pm: &r::PmStatus<'_>) -> Self {
        Self {
            agent: pm.agent.clone(),
            session: pm.session.cloned(),
            folder: pm.folder.to_path_buf(),
            answering: pm.answering,
            attached: pm.attached,
            down_until: pm.down_until,
            told: pm.told.to_vec(),
            held: pm.held.iter().map(Into::into).collect(),
            settled: pm.settled.iter().map(Into::into).collect(),
            reply: pm.reply.map(str::to_owned),
        }
    }
}

impl From<&r::pm::Held> for wire::Held {
    fn from(held: &r::pm::Held) -> Self {
        Self {
            issue: held.issue,
            until: held.until,
        }
    }
}

impl From<&r::pm::Settled> for wire::Settled {
    fn from(settled: &r::pm::Settled) -> Self {
        Self {
            ruling: settled.ruling,
            issue: settled.issue,
            how: settled.how.to_owned(),
            recommends: settled.recommends.clone(),
            why: settled.why.clone(),
        }
    }
}

impl From<&r::Draining> for wire::Draining {
    fn from(draining: &r::Draining) -> Self {
        Self {
            calls: draining.calls.iter().map(Into::into).collect(),
            ceiling: draining.ceiling,
            at_tool: draining.at_tool.clone(),
        }
    }
}

impl From<&r::CallRunning> for wire::CallRunning {
    fn from(call: &r::CallRunning) -> Self {
        Self {
            issue: call.issue,
            role: (&call.role).into(),
        }
    }
}

impl From<&r::CallRole> for wire::CallRole {
    fn from(role: &r::CallRole) -> Self {
        match role {
            r::CallRole::Worker => Self::Worker,
            r::CallRole::Reviewer => Self::Reviewer,
            r::CallRole::Pm => Self::Pm,
            r::CallRole::Security => Self::Security,
            r::CallRole::Triager => Self::Triager,
            r::CallRole::Retro => Self::Retro,
            r::CallRole::E2e => Self::E2e,
            r::CallRole::Attached(name) => Self::Attached((*name).to_owned()),
        }
    }
}

impl From<&r::Refusal> for wire::Refusal {
    fn from(refusal: &r::Refusal) -> Self {
        Self {
            reason: refusal.reason.clone(),
            at: refusal.at,
        }
    }
}

impl From<r::flight::Waiting> for wire::Waiting {
    fn from(waiting: r::flight::Waiting) -> Self {
        Self {
            since: waiting.since,
            why: waiting.why,
        }
    }
}

impl From<&r::turn::held_up::HeldUp> for wire::HeldUp {
    fn from(held_up: &r::turn::held_up::HeldUp) -> Self {
        Self {
            since: held_up.since,
            because: held_up.because.clone(),
        }
    }
}

impl From<&r::looks::Failing> for wire::Failing {
    fn from(failing: &r::looks::Failing) -> Self {
        Self {
            since: failing.since,
            failures: failing.failures,
            reason: failing.reason.clone(),
        }
    }
}

impl From<&r::PacerStatus<'_>> for wire::PacerStatus {
    fn from(pacer: &r::PacerStatus<'_>) -> Self {
        Self {
            enabled: pacer.enabled,
            accounts: pacer
                .accounts
                .iter()
                .map(|(account, status)| {
                    (
                        *account,
                        wire::AccountStatus {
                            reading: status.reading.copied(),
                            holding: status.holding.cloned(),
                        },
                    )
                })
                .collect(),
        }
    }
}

impl From<&r::GpuStatus> for wire::GpuStatus {
    fn from(gpu: &r::GpuStatus) -> Self {
        Self {
            gpus: gpu.gpus.clone(),
            error: gpu.error.clone(),
            age_seconds: gpu.age_seconds,
        }
    }
}

impl From<&r::TemplateSource> for wire::TemplateSource {
    fn from(source: &r::TemplateSource) -> Self {
        match source {
            r::TemplateSource::Repo => Self::Repo,
            r::TemplateSource::Owner(repo) => Self::Owner(repo.clone()),
            r::TemplateSource::None => Self::None,
        }
    }
}

impl From<r::Totals> for Totals {
    fn from(totals: r::Totals) -> Self {
        Self {
            project: totals.project,
            items: totals.items,
            issues: totals.issues,
            wall: totals.wall,
            seconds: totals.seconds,
            table: totals.table,
        }
    }
}

impl From<r::Board> for Board {
    fn from(board: r::Board) -> Self {
        Self {
            project: board.project,
            board: board
                .board
                .iter()
                .map(|place| Place {
                    position: place.position,
                    issue: place.issue,
                    title: place.title.clone(),
                    priority: place.priority.map(str::to_owned),
                    blocked: place.blocked,
                    held: place.held,
                })
                .collect(),
        }
    }
}

impl From<r::Attaching> for Attaching {
    fn from(attaching: r::Attaching) -> Self {
        match attaching {
            r::Attaching::Waiting { issue } => Self::Waiting { issue },
            r::Attaching::Ready {
                issue,
                session,
                worktree,
                command,
            } => Self::Ready {
                issue,
                session,
                worktree,
                command,
            },
            r::Attaching::Running { issue } => Self::Running { issue },
        }
    }
}

impl From<r::PmAttaching> for PmAttaching {
    fn from(attaching: r::PmAttaching) -> Self {
        match attaching {
            r::PmAttaching::Waiting => Self::Waiting,
            r::PmAttaching::Ready {
                session,
                folder,
                command,
            } => Self::Ready {
                session,
                folder,
                command,
            },
            r::PmAttaching::Running => Self::Running,
        }
    }
}
