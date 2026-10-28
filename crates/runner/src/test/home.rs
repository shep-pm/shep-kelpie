//! The folder a rig works in

use std::path::Path;

use super::*;

impl Rig {
    /// The stand-ins over `home`, which holds whatever it holds
    pub(super) fn stand_ins(project: &str, home: Home) -> Self {
        let meter = FakeMeter::idle();
        let clock = FakeClock::at(Self::EPOCH);
        let rig = Self {
            project: ProjectName::try_from(project).unwrap(),
            claude: FakeClaude::metered(meter.clone()),
            forge: FakeForge::new(home.path().join("origin.git")),
            git: FakeGit::default(),
            meter,
            codex_meter: FakeMeter::idle(),
            reviewer: FakeReviewer::default(),
            local_leases: LocalReviewer::default(),
            counted_leases: Arc::default(),
            alerts: FakeAlerts::on(clock.clone()),
            disk: FakeDisk::default(),
            leases: FakeLeases::default(),
            tests: FakeTests::default(),
            dev_servers: FakeDevServers::default(),
            e2e: FakeE2e::default(),
            pages: FakePages::default(),
            bridges: super::FakeBridges::default(),
            harness_mcp: Mutex::new(Arc::new(crate::adapters::McpRules)),
            samples: super::RecordedSink::default(),
            classifier: FakeClassifier::new(),
            github: FakeGithub::new(clock.clone(), "shep-pm"),
            pace: Arc::new(LocalPace::with(CallPacer::new(usize::MAX, usize::MAX))),
            agent_board: FakeAgentBoard::default(),
            clock,
            home,
        };
        rig.forge.threads.use_clock(rig.clock.clone());
        rig.github.use_threads(rig.forge.coderabbit.clone());
        rig.forge.use_app_comments(Arc::new(rig.github.clone()));
        rig
    }

    /// A rig over the home an earlier rig made, as another image of the
    /// runner sees it after a handover: its files are there and the stand-ins
    /// are new
    pub fn over(project: &str, home: &Path) -> Self {
        Self::stand_ins(project, Home::Kept(home.to_owned()))
    }
}
