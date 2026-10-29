use ::tracing::{
    Event, Metadata, Subscriber,
    field::{Field, Visit},
    span::{Attributes, Id, Record},
};
use std::{
    fmt::{self, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
};

const LIMIT: usize = 1024 * 1024;

#[derive(Default)]
struct Buffer {
    text: String,
    overflow: bool,
}

impl Write for Buffer {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        if text.len() > LIMIT.saturating_sub(self.text.len()) {
            self.overflow = true;
            return Err(fmt::Error);
        }
        self.text.push_str(text);
        Ok(())
    }
}

impl Visit for Buffer {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        let _ = writeln!(self, "{field}={value:?}");
    }
}

#[derive(Default)]
struct State {
    captured: Mutex<Buffer>,
    spans: AtomicU64,
    sdk_trace: AtomicUsize,
}

#[derive(Clone, Default)]
pub(super) struct Capture(Arc<State>);

impl Capture {
    pub(super) fn install() -> Self {
        let capture = Self::default();
        ::tracing::subscriber::set_global_default(capture.clone()).expect("isolated subscriber");
        capture
    }

    pub(super) fn verify(&self) {
        let captured = self.0.captured.lock().expect("capture");
        assert!(!captured.overflow, "complete diagnostic capture required");
        assert!(
            self.0.sdk_trace.load(Ordering::Relaxed) > 0,
            "real RMCP TRACE events required"
        );
        assert!(
            captured.text.contains("mcp1."),
            "SDK identifier diagnostics required"
        );
        assert!(
            !captured.text.contains("secret-canary"),
            "secret material in diagnostics"
        );
    }
}

impl Subscriber for Capture {
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        true
    }

    fn max_level_hint(&self) -> Option<::tracing::level_filters::LevelFilter> {
        Some(::tracing::level_filters::LevelFilter::TRACE)
    }

    fn new_span(&self, attributes: &Attributes<'_>) -> Id {
        let mut captured = self.0.captured.lock().expect("capture");
        let _ = writeln!(
            captured,
            "span {} {}",
            attributes.metadata().target(),
            attributes.metadata().name()
        );
        attributes.record(&mut *captured);
        Id::from_u64(self.0.spans.fetch_add(1, Ordering::Relaxed) + 1)
    }

    fn record(&self, _: &Id, values: &Record<'_>) {
        values.record(&mut *self.0.captured.lock().expect("capture"));
    }

    fn event(&self, event: &Event<'_>) {
        let metadata = event.metadata();
        if metadata.target().starts_with("rmcp::") && *metadata.level() == ::tracing::Level::TRACE {
            self.0.sdk_trace.fetch_add(1, Ordering::Relaxed);
        }
        let mut captured = self.0.captured.lock().expect("capture");
        let _ = writeln!(captured, "{} {}", metadata.level(), metadata.target());
        event.record(&mut *captured);
    }

    fn record_follows_from(&self, _: &Id, _: &Id) {}
    fn enter(&self, _: &Id) {}
    fn exit(&self, _: &Id) {}
}

#[test]
fn capture_observes_span_fields_events_and_refuses_overflow() {
    let capture = Capture::default();
    ::tracing::subscriber::with_default(capture.clone(), || {
        let span = ::tracing::trace_span!("probe", secret = ::tracing::field::Empty);
        span.record("secret", "span-secret-canary");
        ::tracing::trace!(value = "event-secret-canary");
    });
    let captured = capture.0.captured.lock().expect("capture");
    assert!(captured.text.contains("span-secret-canary"));
    assert!(captured.text.contains("event-secret-canary"));
    drop(captured);
    let mut buffer = Buffer::default();
    buffer.write_str(&"x".repeat(LIMIT)).expect("exact bound");
    assert!(buffer.write_str("x").is_err());
    assert!(buffer.overflow);
    assert_eq!(buffer.text.len(), LIMIT);
}
