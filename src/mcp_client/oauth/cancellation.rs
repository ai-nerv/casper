use super::Error;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

pub(super) struct Cancellation {
    stopped: AtomicBool,
    parent: Option<Arc<AtomicBool>>,
    deadline: Instant,
}

impl Cancellation {
    pub(super) fn deadline(&self) -> Instant {
        self.deadline
    }

    pub(super) fn check(&self) -> Result<(), Error> {
        if self.stopped.load(Ordering::Acquire)
            || self
                .parent
                .as_ref()
                .is_some_and(|flag| flag.load(Ordering::Acquire))
        {
            Err(Error::Cancelled)
        } else if Instant::now() >= self.deadline {
            Err(Error::Timeout)
        } else {
            Ok(())
        }
    }
}

pub(super) struct Operation(pub(super) Arc<Cancellation>);

impl Operation {
    pub(super) fn new(timeout: Duration, parent: Option<Arc<AtomicBool>>) -> Self {
        Self(Arc::new(Cancellation {
            stopped: AtomicBool::new(false),
            parent,
            deadline: Instant::now() + timeout,
        }))
    }
}

impl Drop for Operation {
    fn drop(&mut self) {
        self.0.stopped.store(true, Ordering::Release);
    }
}
