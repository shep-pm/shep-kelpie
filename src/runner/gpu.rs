//! The GPU's figures for `status`, sampled off the request path
//!
//! A thread reads the metrics page every [`EVERY`] and keeps the last
//! answer, so `status` never waits on the GPU box: shep gives an action
//! three seconds, and a box that is off takes longer than that to fail.

use std::fmt;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::ports::{Gpu, GpuError, GpuMetrics};
use crate::settings::EndpointUrl;

/// How long a reading stands before the page is read again
const EVERY: Duration = Duration::from_secs(15);

/// How long the thread sleeps with no page to read
const IDLE: Duration = Duration::from_secs(3600);

/// The GPU, as the metrics page last said
#[derive(Debug, Serialize)]
pub struct GpuStatus {
    /// Each GPU on the page
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub gpus: Vec<Gpu>,
    /// Why the page could not be read, or that it has not been yet
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Seconds since that read ended, once there has been one
    #[serde(skip_serializing_if = "Option::is_none")]
    pub age_seconds: Option<u64>,
}

struct Sample {
    at: Instant,
    result: Result<Vec<Gpu>, GpuError>,
}

#[derive(Default)]
struct State {
    url: Option<EndpointUrl>,
    sample: Option<Sample>,
    refresh: bool,
    next: Option<Instant>,
    stop: bool,
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Keeps the GPU's last reading, and reads the page again in the background
///
/// The thread ends once the watch is dropped, or when a read in flight
/// returns.
pub struct GpuWatch {
    shared: Arc<Shared>,
}

impl GpuWatch {
    /// Starts reading `url`, when there is one, with `metrics`
    pub fn start(metrics: Arc<dyn GpuMetrics>, url: Option<EndpointUrl>) -> Self {
        let shared = Arc::new(Shared::default());
        {
            let mut state = shared.lock();
            state.refresh = url.is_some();
            state.url = url;
        }
        let reading = Arc::clone(&shared);
        thread::spawn(move || run(&reading, metrics.as_ref()));
        Self { shared }
    }

    /// Reads `url` from now on, at once when it differs from the last
    pub fn point_at(&self, url: Option<EndpointUrl>) {
        let mut state = self.shared.lock();
        if state.url != url {
            state.refresh = url.is_some();
            state.sample = None;
            state.url = url;
            self.shared.wake.notify_all();
        }
    }

    /// The page being read
    pub fn url(&self) -> Option<EndpointUrl> {
        self.shared.lock().url.clone()
    }

    /// The last reading, or None when there is no page to read
    pub fn status(&self) -> Option<GpuStatus> {
        let state = self.shared.lock();
        state.url.as_ref()?;
        Some(match &state.sample {
            None => GpuStatus {
                gpus: Vec::new(),
                error: Some("the GPU metrics have not been read yet".to_owned()),
                age_seconds: None,
            },
            Some(Sample { at, result }) => {
                let age_seconds = Some(at.elapsed().as_secs());
                match result {
                    Ok(gpus) => GpuStatus {
                        gpus: gpus.clone(),
                        error: None,
                        age_seconds,
                    },
                    Err(e) => GpuStatus {
                        gpus: Vec::new(),
                        error: Some(format!("cannot read the GPU metrics: {e}")),
                        age_seconds,
                    },
                }
            }
        })
    }
}

impl Drop for GpuWatch {
    fn drop(&mut self) {
        self.shared.lock().stop = true;
        self.shared.wake.notify_all();
    }
}

impl fmt::Debug for GpuWatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GpuWatch").finish_non_exhaustive()
    }
}

// The lock is not held while the page is read, so `status` never waits on it.
fn run(shared: &Shared, metrics: &dyn GpuMetrics) {
    let mut state = shared.lock();
    loop {
        if state.stop {
            return;
        }
        let now = Instant::now();
        let due = state.refresh || state.next.is_some_and(|next| next <= now);
        if let (Some(url), true) = (state.url.clone(), due) {
            state.refresh = false;
            drop(state);
            let result = metrics.read(&url);
            state = shared.lock();
            // A page swapped out meanwhile has its own read coming.
            if state.url.as_ref() == Some(&url) {
                state.sample = Some(Sample {
                    at: Instant::now(),
                    result,
                });
                state.next = Some(Instant::now() + EVERY);
            }
            continue;
        }
        let wait = match (&state.url, state.next) {
            (Some(_), Some(next)) => next.saturating_duration_since(now),
            _ => IDLE,
        };
        state = shared
            .wake
            .wait_timeout(state, wait)
            .unwrap_or_else(PoisonError::into_inner)
            .0;
    }
}
