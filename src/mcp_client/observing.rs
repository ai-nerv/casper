use crate::events::{self, Correlation, Fact, ServerOutcome};
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(test)]
mod native;
#[cfg(test)]
mod tests;

#[derive(Default)]
enum State {
    Starting,
    Ready,
    #[default]
    Finished,
}

#[derive(Default)]
pub(super) struct Trace {
    correlation: Option<Correlation>,
    state: State,
    closing: AtomicBool,
}

impl Trace {
    pub(super) fn new() -> Self {
        let trace = Self {
            correlation: events::identity().map(|id| {
                events::requests::capture(Correlation {
                    request_id: Some(id),
                    ..Correlation::default()
                })
            }),
            state: State::Starting,
            closing: AtomicBool::new(false),
        };
        trace.emit(ServerOutcome::Connecting);
        trace
    }

    fn emit(&self, outcome: ServerOutcome) {
        if let Some(correlation) = self.correlation {
            events::emit(|| (correlation, Fact::ToolServer { outcome }));
        }
    }

    pub(super) fn connected(&mut self) {
        if matches!(self.state, State::Starting) {
            self.state = State::Ready;
            self.emit(ServerOutcome::Connected);
        }
    }

    pub(super) fn finish(&mut self, outcome: ServerOutcome) {
        if matches!(self.state, State::Starting) {
            self.state = State::Finished;
            self.emit(outcome);
        }
    }

    pub(super) fn closing(&self) {
        if matches!(self.state, State::Ready) && !self.closing.swap(true, Ordering::Relaxed) {
            self.emit(ServerOutcome::CloseRequested);
        }
    }
}

impl Drop for Trace {
    fn drop(&mut self) {
        match self.state {
            State::Starting => self.emit(ServerOutcome::Unknown),
            State::Ready => self.emit(ServerOutcome::Disconnected),
            State::Finished => {}
        }
    }
}
