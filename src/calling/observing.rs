use crate::events::{self, Correlation, Fact, MutationState, ToolOutcome};
use crate::tools::Ran;
use std::time::Instant;

pub(super) struct Trace {
    correlation: Option<Correlation>,
    began: Option<Instant>,
    started: bool,
    finished: bool,
}

impl Trace {
    pub(super) fn new() -> Self {
        let correlation = events::identity().map(|id| {
            events::requests::capture(Correlation {
                tool_call_id: Some(id),
                ..Correlation::default()
            })
        });
        Self {
            correlation,
            began: correlation.map(|_| Instant::now()),
            started: false,
            finished: false,
        }
    }

    fn tool(&self, outcome: ToolOutcome, terminal: bool) {
        if let Some(correlation) = self.correlation {
            events::emit(|| {
                (
                    correlation,
                    Fact::Tool {
                        outcome,
                        duration_ms: if terminal {
                            self.began.and_then(events::elapsed)
                        } else {
                            None
                        },
                    },
                )
            });
        }
    }

    pub(super) fn enabled(&self) -> bool {
        self.correlation.is_some()
    }

    pub(super) fn start(&mut self) {
        self.started = true;
        self.tool(ToolOutcome::Started, false);
    }

    pub(super) fn mutation(&self, state: MutationState) {
        if let Some(correlation) = self.correlation {
            events::emit(|| (correlation, Fact::Mutation { state }));
        }
    }

    pub(super) fn finish(&mut self, result: &Result<Ran, String>) {
        self.finished = true;
        if !self.enabled() {
            return;
        }
        let outcome = match result {
            Err(_) if self.started => ToolOutcome::Unknown,
            Err(_) => ToolOutcome::Refused,
            Ok(ran) => outcome(ran),
        };
        self.tool(outcome, true);
    }
}

fn outcome(ran: &Ran) -> ToolOutcome {
    match ran
        .files
        .mutation
        .as_ref()
        .map(|value| value.outcome.as_str())
    {
        Some("unknown") => ToolOutcome::Unknown,
        Some("refused") => ToolOutcome::Refused,
        _ if ran.waiting() => ToolOutcome::Waiting,
        _ if ran.failed => ToolOutcome::Failed,
        _ => ToolOutcome::Succeeded,
    }
}

impl Drop for Trace {
    fn drop(&mut self) {
        if !self.finished {
            self.tool(
                if self.started {
                    ToolOutcome::Unknown
                } else {
                    ToolOutcome::Refused
                },
                true,
            );
        }
    }
}
