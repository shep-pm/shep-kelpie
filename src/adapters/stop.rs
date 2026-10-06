//! Ending every call in flight as the runner stops

use std::thread;

use super::{ClaudeCli, LocalReviewer};

/// Ends every call in flight on `claude` and `reviewer`, and refuses new
/// ones, both at once
///
/// Each runs its own stop ladder, so a stop with sessions and local rounds
/// both running takes one ladder's grace, not two, inside the runner's stop
/// budget.
pub fn stop_calls(claude: &ClaudeCli, reviewer: &LocalReviewer) {
    thread::scope(|scope| {
        scope.spawn(|| claude.stop());
        reviewer.stop();
    });
}

#[cfg(test)]
mod tests {
    use std::process::Command;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::adapters::process::{Processes, STOP_GRACE};

    // Real time: real children that ignore SIGTERM, so each ladder runs its
    // whole grace before its SIGKILL.
    #[test]
    fn a_stop_with_a_session_and_a_local_round_running_takes_one_ladder() {
        let dir = tempfile::tempdir().unwrap();
        let (claude, reviewer) = (ClaudeCli::default(), LocalReviewer::default());
        let markers = [dir.path().join("session"), dir.path().join("round")];
        let ignoring = |processes: Processes, marker: &std::path::Path| {
            let script = format!("trap '' TERM; touch '{}'; sleep 30", marker.display());
            thread::spawn(move || processes.output(Command::new("sh").args(["-c", &script])))
        };
        let calls = [
            ignoring(claude.processes.clone(), &markers[0]),
            ignoring(reviewer.processes.clone(), &markers[1]),
        ];
        let started = Instant::now();
        while !markers.iter().all(|m| m.exists()) {
            assert!(started.elapsed() < Duration::from_secs(10), "no call began");
            thread::sleep(Duration::from_millis(20));
        }

        let stopping = Instant::now();
        stop_calls(&claude, &reviewer);
        let took = stopping.elapsed();

        assert!(took < STOP_GRACE + Duration::from_secs(2), "{took:?}");
        for call in calls {
            assert!(call.join().unwrap().is_err(), "a call ran to its end");
        }
    }
}
