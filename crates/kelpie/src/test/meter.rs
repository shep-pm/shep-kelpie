//! The rig's meter and clock, which change only when a test changes them

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::Rig;
use crate::ports::{Clock, Meter, MeterError, Timestamp, Utilization};

/// A meter that reports what a test sets, and counts its reads
///
/// It starts with the account idle: nothing used in either window, the
/// week `Rig::EPOCH` begins and the session window five hours long.
#[derive(Debug, Clone)]
pub(crate) struct FakeMeter {
    reading: Arc<Mutex<Result<Utilization, MeterError>>>,
    reads: Arc<AtomicUsize>,
}

impl FakeMeter {
    pub(super) fn idle() -> Self {
        Self {
            reading: Arc::new(Mutex::new(Ok(Rig::utilization(0, 0)))),
            reads: Arc::default(),
        }
    }

    /// Reports `usage` from now on
    pub(crate) fn set(&self, usage: Utilization) {
        *self.reading.lock().unwrap() = Ok(usage);
    }

    /// Fails every read with `error`, until a test sets a reading
    pub(crate) fn fail(&self, error: MeterError) {
        *self.reading.lock().unwrap() = Err(error);
    }

    /// How many times usage was read
    pub(crate) fn reads(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }
}

impl Meter for FakeMeter {
    fn read(&self, _now: Timestamp) -> Result<Utilization, MeterError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.reading.lock().unwrap().clone()
    }
}

/// A clock that moves only when a test moves it
#[derive(Debug, Clone)]
pub(crate) struct FakeClock(Arc<AtomicU64>);

impl FakeClock {
    pub(crate) fn at(seconds: u64) -> Self {
        Self(Arc::new(AtomicU64::new(seconds)))
    }

    pub(crate) fn advance(&self, seconds: u64) {
        self.0.fetch_add(seconds, Ordering::SeqCst);
    }
}

impl Clock for FakeClock {
    fn now(&self) -> Timestamp {
        Timestamp(self.0.load(Ordering::SeqCst))
    }
}
