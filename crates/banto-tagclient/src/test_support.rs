//! Test-only tracing capture used to prove diagnostic logging never echoes
//! secrets or endpoint paths.
//!
//! This deliberately avoids adding `tracing-subscriber` or `tracing-test` as
//! a dependency: proving secret redaction only needs each event's field
//! values rendered into a string, which a minimal hand-written
//! `tracing::Subscriber` gives us directly without pulling in a formatting
//! or filtering framework this crate has no other use for.

#![cfg(test)]

use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata, Subscriber};

/// Rendered lines from captured events, one per event, in emission order.
#[derive(Clone, Default)]
pub(crate) struct CapturedLog {
    lines: Arc<Mutex<Vec<String>>>,
}

impl CapturedLog {
    /// True if any captured event line contains `needle` verbatim.
    pub(crate) fn contains(&self, needle: &str) -> bool {
        self.lines
            .lock()
            .unwrap()
            .iter()
            .any(|line| line.contains(needle))
    }
}

struct FieldRenderer {
    line: String,
}

impl Visit for FieldRenderer {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let _ = write!(self.line, " {}={value:?}", field.name());
    }
}

struct RecordingSubscriber {
    lines: Arc<Mutex<Vec<String>>>,
}

impl Subscriber for RecordingSubscriber {
    fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _span: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _span: &Id, _values: &Record<'_>) {}

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut renderer = FieldRenderer {
            line: String::new(),
        };
        event.record(&mut renderer);
        self.lines.lock().unwrap().push(renderer.line);
    }

    fn enter(&self, _span: &Id) {}

    fn exit(&self, _span: &Id) {}
}

/// Install a capturing subscriber as the per-thread default and return a
/// handle to read captured events, plus a guard that restores the previous
/// subscriber when dropped.
///
/// Every test in this crate runs on the single-threaded runtime that
/// `#[tokio::test]` provides by default, so a thread-local default stays
/// active across `.await` points for as long as the guard is held.
pub(crate) fn capture() -> (CapturedLog, tracing::subscriber::DefaultGuard) {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let subscriber = RecordingSubscriber {
        lines: Arc::clone(&lines),
    };
    let guard = tracing::subscriber::set_default(subscriber);
    (CapturedLog { lines }, guard)
}
