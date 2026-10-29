//! What each series counts, and where in a ledger line, the state or the
//! settings it comes from

use serde::Serialize;

use super::{Family, Label, Recorder};
use crate::board::check::kinds;
use crate::ports::SeriesKind::{Counter, Gauge};
use crate::settings::Settings;
use crate::state::ProjectState;
use crate::usage::{CallLine, FinishedLine, Line};
use crate::work_item::Phase;

const CALLS: Family = Family {
    name: "kelpie_calls_total",
    kind: Counter,
    help: "Model calls that ended, by role, call kind, agent, harness, effort and how they ended.",
};
const TOKENS: Family = Family {
    name: "kelpie_tokens_total",
    kind: Counter,
    help: "Tokens the calls used as their harness reported them, by type, agent and effort.",
};
const COST: Family = Family {
    name: "kelpie_cost_usd_total",
    kind: Counter,
    help: "What the calls whose harness reported a cost cost, in US dollars, by agent and effort.",
};
const COMPACTIONS: Family = Family {
    name: "kelpie_compactions_total",
    kind: Counter,
    help: "Compactions a harness did by itself partway through a call.",
};
const UNPRICED: Family = Family {
    name: "kelpie_unpriced_calls_total",
    kind: Counter,
    help: "Calls whose harness reported no cost.",
};
const CALL_SECONDS: Family = Family {
    name: "kelpie_call_seconds_total",
    kind: Counter,
    help: "Seconds the calls ran, start to end; over kelpie_calls_total for a mean.",
};
const WORK_ITEMS: Family = Family {
    name: "kelpie_work_items",
    kind: Gauge,
    help: "Open work items, by phase.",
};
const FINISHED: Family = Family {
    name: "kelpie_work_items_finished_total",
    kind: Counter,
    help: "Work items finished: merged, closed with no change, or dropped, by the agent that built them and the kind of work.",
};
const ITEM_SECONDS: Family = Family {
    name: "kelpie_work_item_seconds_total",
    kind: Counter,
    help: "Wall seconds of the finished work items, from creation to finish, by the agent that built them and the kind of work.",
};
const ITEM_COST: Family = Family {
    name: "kelpie_work_item_cost_usd_total",
    kind: Counter,
    help: "What the finished work items cost, in US dollars, by agent and kind. A work item with a call whose harness reported no cost is left out.",
};
const REVIEW_ROUNDS: Family = Family {
    name: "kelpie_review_rounds_total",
    kind: Counter,
    help: "Reviewers' rounds that came back with an answer, on finished work items, by the agent that built them and the kind of work.",
};
const FIX_TURNS: Family = Family {
    name: "kelpie_fix_turns_total",
    kind: Counter,
    help: "Worker turns sent to fix a round's held findings, on finished work items, by the agent that built them and the kind of work.",
};
const RULINGS: Family = Family {
    name: "kelpie_rulings_total",
    kind: Counter,
    help: "Rulings raised, on finished work items, by the agent that built them and the kind of work.",
};
const FINDINGS: Family = Family {
    name: "kelpie_findings_total",
    kind: Counter,
    help: "Findings above a nit sent to the worker, each once, on finished work items, by severity.",
};
const FEATURE_ENABLED: Family = Family {
    name: "kelpie_feature_enabled",
    kind: Gauge,
    help: "1 when the project's settings turn the feature on, 0 when not.",
};
/// How often a feature ran, by how it went
pub const FEATURE_RUNS: Family = Family {
    name: "kelpie_feature_runs_total",
    kind: Counter,
    help: "Times a feature ran for a call, by outcome. An enabled feature with no ok run is broken.",
};
const CI_FAILURES: Family = Family {
    name: "kelpie_ci_failures_total",
    kind: Counter,
    help: "Work item CI runs that failed, by whether the worker could have run the tests that failed.",
};

// Every phase, so one with no work item reads 0 rather than vanishing
const PHASES: [&str; 6] = ["implement", "review", "ci", "ruling", "merge", "done"];

/// A feature a project turns on, as its series name it
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feature {
    /// `code_graph`: codebase-memory-mcp for workers and reviewers
    CodeGraph,
    /// `browser`: the Playwright MCP server behind kelpie's bridge
    Browser,
    /// `dev_server`: the project's server from `main`'s launch file, started for a turn's browser
    DevServer,
    /// `hooks`: the project's own commands at kelpie's points
    Hooks,
}

impl Feature {
    const ALL: [Self; 4] = [Self::CodeGraph, Self::Browser, Self::DevServer, Self::Hooks];

    fn as_str(self) -> &'static str {
        match self {
            Self::CodeGraph => "code_graph",
            Self::Browser => "browser",
            Self::DevServer => "dev_server",
            Self::Hooks => "hooks",
        }
    }

    // `dev_server`: whether the browser is on and `main` has a launch file that gives a server
    fn enabled(self, settings: &Settings, dev_server: bool) -> bool {
        match self {
            Self::CodeGraph => settings.code_graph.enabled,
            Self::Browser => settings.browser.enabled,
            Self::DevServer => dev_server,
            Self::Hooks => !settings.hooks.is_empty(),
        }
    }
}

/// Whether the tests that failed a work item's CI are ones its worker could have
/// run in its own sandbox, which decides whether the worker could have caught the
/// failure. `crates/runner/src/runner/ci_failure/mod.rs` reads it from the failed runs' logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CiTests {
    /// Every failing test is one `docs/agents/sandbox-red-tests.md` lists as red in
    /// a worker's sandbox, so no worker could have run it
    SandboxOnly,
    /// At least one failing test is one a worker could have run
    Runnable,
    /// No test failed: lint, docs, fmt, or a build that never ran one
    NoTests,
    /// A job's `timeout-minutes` ended the run and no log names a failing test
    TimedOut,
}

impl CiTests {
    fn as_str(self) -> &'static str {
        match self {
            Self::SandboxOnly => "sandbox-only",
            Self::Runnable => "runnable",
            Self::NoTests => "none",
            Self::TimedOut => "timed-out",
        }
    }
}

impl Recorder {
    /// Counts what a ledger line records, as it is written
    pub fn line(&self, line: &Line) {
        match line {
            Line::Call(call) => self.call(call),
            Line::Finished(finished) => self.finished(finished),
        }
    }

    /// Sets the work items by phase, from `state` as saved
    pub fn work_items(&self, state: &ProjectState) {
        for phase in PHASES {
            let open = (state.work_items.iter())
                .filter(|item| phase_name(&item.phase) == phase)
                .count();
            #[expect(clippy::cast_precision_loss, reason = "a count of work items")]
            self.set(&WORK_ITEMS, vec![("phase", phase.to_owned())], open as f64);
        }
    }

    /// Sets which features `settings` turn on, where `dev_server` is whether the dev server
    /// is on, which only the runner can read from `main`
    pub fn features(&self, settings: &Settings, dev_server: bool) {
        for feature in Feature::ALL {
            let on = f64::from(u8::from(feature.enabled(settings, dev_server)));
            self.set(&FEATURE_ENABLED, vec![feature_label(feature)], on);
        }
    }

    /// Counts one run of `feature` for a call, which went as `outcome`
    pub fn feature_ran(&self, feature: Feature, outcome: &'static str) {
        let labels = vec![feature_label(feature), ("outcome", outcome.to_owned())];
        self.add(&FEATURE_RUNS, labels, 1.0);
    }

    /// Counts one of a work item's failed CI runs, by whether its worker could have
    /// run the tests that failed it
    pub fn ci_failure(&self, tests: CiTests) {
        self.add(
            &CI_FAILURES,
            vec![("tests", tests.as_str().to_owned())],
            1.0,
        );
    }

    fn call(&self, call: &CallLine) {
        let role = ("role", call.role.as_str().to_owned());
        let harness = ("harness", known(call.harness.as_deref()));
        let model = ("model", known(call.model.as_deref()));
        let agent = ("agent", known(call.agent.as_deref()));
        let effort = ("effort", known(call.effort.as_deref()));
        let kind = ("kind", wire(&call.kind));
        let calls = vec![
            role.clone(),
            kind.clone(),
            agent.clone(),
            harness.clone(),
            effort.clone(),
            ("ended", wire(&call.ended)),
        ];
        self.add(&CALLS, calls, 1.0);
        let usage = call.usage;
        let tokens = [
            ("input", usage.input),
            ("output", usage.output),
            ("cache_read", usage.cache_read),
            ("cache_write", usage.cache_write),
        ];
        for (kind, count) in tokens {
            let labels = vec![
                role.clone(),
                agent.clone(),
                harness.clone(),
                model.clone(),
                effort.clone(),
                ("type", kind.into()),
            ];
            #[expect(
                clippy::cast_precision_loss,
                reason = "a call's tokens stay far below 2^53"
            )]
            self.add(&TOKENS, labels, count as f64);
        }
        match call.cost_usd {
            Some(usd) => {
                let labels = vec![role.clone(), agent, harness.clone(), model, effort];
                self.add(&COST, labels, usd);
            }
            None if call.unpriced => self.add(&UNPRICED, vec![role.clone(), harness.clone()], 1.0),
            None => {}
        }
        if call.compactions > 0 {
            let labels = vec![role.clone(), harness];
            self.add(&COMPACTIONS, labels, f64::from(call.compactions));
        }
        if let Some(seconds) = call.seconds {
            #[expect(
                clippy::cast_precision_loss,
                reason = "a call's seconds stay far below 2^53"
            )]
            self.add(&CALL_SECONDS, vec![role, kind], seconds as f64);
        }
    }

    fn finished(&self, finished: &FinishedLine) {
        let outcome = match (finished.merged, finished.closed) {
            (true, _) => "merged",
            (false, true) => "closed",
            (false, false) => "dropped",
        };
        // The agent and kind the work item's track record is kept under (ADR 0018), on every
        // series of the finished line but the findings and the red heads, which stay whole.
        let agent = ("agent", known(finished.agent.as_deref()));
        let kind = ("kind", item_kind(finished.kind.as_deref()));
        let labels = || vec![("outcome", outcome.to_owned()), agent.clone(), kind.clone()];
        let who = || vec![agent.clone(), kind.clone()];
        self.add(&FINISHED, labels(), 1.0);
        #[expect(
            clippy::cast_precision_loss,
            reason = "a work item's seconds stay far below 2^53"
        )]
        self.add(&ITEM_SECONDS, labels(), finished.wall as f64);
        self.add(&REVIEW_ROUNDS, who(), f64::from(finished.review_rounds));
        self.add(&FIX_TURNS, who(), f64::from(finished.fix_turns));
        // A line with a call whose harness reported no cost holds a cost_usd that is a lower
        // bound, so it counts nowhere in the dollars series.
        if finished.worker.unpriced_calls == 0 && finished.reviewer.unpriced_calls == 0 {
            self.add(&ITEM_COST, who(), finished.cost_usd);
        }
        self.add(&RULINGS, vec![agent, kind], f64::from(finished.rulings));
        for (severity, n) in [
            ("high", finished.held.high),
            ("medium", finished.held.medium),
        ] {
            self.add(
                &FINDINGS,
                vec![("severity", severity.to_owned())],
                f64::from(n),
            );
        }
    }
}

fn feature_label(feature: Feature) -> Label {
    ("feature", feature.as_str().to_owned())
}

// The glossary's name of a phase
fn phase_name(phase: &Phase) -> &'static str {
    match phase {
        Phase::Implement => "implement",
        Phase::Review(_) => "review",
        Phase::Ci { .. } => "ci",
        Phase::Ruling { .. } => "ruling",
        Phase::Merge { .. } => "merge",
        Phase::Done { .. } => "done",
    }
}

fn known(value: Option<&str>) -> String {
    value.unwrap_or("unknown").to_owned()
}

// The work item's kind as a label: one of the kinds an issue's `kind:` label names, spelled as
// `kinds` spells it, or `none` where the line has none or names one kelpie no longer has. The
// nine kinds are the bounded set ADR 0012 asks for, so a retired kind adds no series.
fn item_kind(kind: Option<&str>) -> String {
    kinds()
        .find(|named| kind.is_some_and(|had| had.eq_ignore_ascii_case(named)))
        .unwrap_or("none")
        .to_owned()
}

// The name an enum has in the ledger, so a series and a ledger line agree
fn wire(value: &impl Serialize) -> String {
    let value = serde_json::to_value(value).ok();
    (value.as_ref().and_then(serde_json::Value::as_str)).map_or_else(String::new, str::to_owned)
}
