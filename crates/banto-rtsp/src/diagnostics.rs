//! Bounded storage for stderr text that has already been sanitized.
//!
//! Raw FFmpeg stderr must never be passed to this module. Callers must first
//! process it through the streaming log sanitizer and only then call
//! [`FfmpegDiagnostics::push_sanitized`]. The consumer handle intentionally
//! exposes only [`FfmpegDiagnosticsHandle::snapshot`]; it cannot push or close.

use std::collections::VecDeque;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};

use crate::{DiagnosticsError, RtspError};

const TRUNCATED_MARKER: &str = "[truncated]";

struct DiagnosticsState {
    entries: VecDeque<String>,
    closed: bool,
}

struct DiagnosticsInner {
    max_entries: usize,
    max_entry_bytes: usize,
    state: Mutex<DiagnosticsState>,
}

/// Producer for a bounded collection of already-sanitized FFmpeg diagnostics.
#[derive(Clone)]
pub struct FfmpegDiagnostics {
    inner: Arc<DiagnosticsInner>,
}

impl FfmpegDiagnostics {
    /// Creates a bounded diagnostics store and its read-only consumer handle.
    pub fn new(
        max_entries: usize,
        max_entry_bytes: usize,
    ) -> Result<(Self, FfmpegDiagnosticsHandle), RtspError> {
        if max_entries == 0 || max_entry_bytes == 0 {
            return Err(DiagnosticsError::InvalidConfig.into());
        }

        let inner = Arc::new(DiagnosticsInner {
            max_entries,
            max_entry_bytes,
            state: Mutex::new(DiagnosticsState {
                entries: VecDeque::new(),
                closed: false,
            }),
        });
        Ok((
            Self {
                inner: Arc::clone(&inner),
            },
            FfmpegDiagnosticsHandle { inner },
        ))
    }

    /// Stores text only after it has passed through the streaming sanitizer.
    /// Raw FFmpeg stderr must not be supplied to this method.
    pub fn push_sanitized(&self, entry: &str) -> Result<(), RtspError> {
        let mut state = lock_state(&self.inner)?;
        if state.closed {
            return Err(DiagnosticsError::Closed.into());
        }

        let entry = truncate_utf8(entry, self.inner.max_entry_bytes);
        if state.entries.len() == self.inner.max_entries {
            state.entries.pop_front();
        }
        state.entries.push_back(entry);
        Ok(())
    }

    /// Closes the producer. Closing more than once is harmless.
    pub fn close(&self) -> Result<(), RtspError> {
        let mut state = lock_state(&self.inner)?;
        state.closed = true;
        Ok(())
    }
}

/// Read-only consumer for sanitized diagnostic snapshots.
#[derive(Clone)]
pub struct FfmpegDiagnosticsHandle {
    inner: Arc<DiagnosticsInner>,
}

impl FfmpegDiagnosticsHandle {
    /// Returns the retained entries from oldest to newest.
    pub fn snapshot(&self) -> Result<Vec<String>, RtspError> {
        let state = lock_state(&self.inner)?;
        Ok(state.entries.iter().cloned().collect())
    }
}

impl fmt::Debug for FfmpegDiagnostics {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        debug_store("FfmpegDiagnostics", &self.inner, formatter)
    }
}

impl fmt::Debug for FfmpegDiagnosticsHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        debug_store("FfmpegDiagnosticsHandle", &self.inner, formatter)
    }
}

fn lock_state(inner: &DiagnosticsInner) -> Result<MutexGuard<'_, DiagnosticsState>, RtspError> {
    inner
        .state
        .lock()
        .map_err(|_| DiagnosticsError::Poisoned.into())
}

fn truncate_utf8(entry: &str, max_bytes: usize) -> String {
    if entry.len() <= max_bytes {
        return entry.to_owned();
    }

    if TRUNCATED_MARKER.len() <= max_bytes {
        let content_limit = max_bytes - TRUNCATED_MARKER.len();
        let end = floor_char_boundary(entry, content_limit);
        let mut truncated = String::with_capacity(max_bytes);
        truncated.push_str(&entry[..end]);
        truncated.push_str(TRUNCATED_MARKER);
        truncated
    } else {
        entry[..floor_char_boundary(entry, max_bytes)].to_owned()
    }
}

fn floor_char_boundary(value: &str, mut index: usize) -> usize {
    index = index.min(value.len());
    while index > 0 && !value.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn debug_store(
    name: &str,
    inner: &DiagnosticsInner,
    formatter: &mut fmt::Formatter<'_>,
) -> fmt::Result {
    match inner.state.lock() {
        Ok(state) => formatter
            .debug_struct(name)
            .field("max_entries", &inner.max_entries)
            .field("max_entry_bytes", &inner.max_entry_bytes)
            .field("entry_count", &state.entries.len())
            .field("closed", &state.closed)
            .field("poisoned", &false)
            .finish(),
        Err(_) => formatter
            .debug_struct(name)
            .field("max_entries", &inner.max_entries)
            .field("max_entry_bytes", &inner.max_entry_bytes)
            .field("entry_count", &0usize)
            .field("closed", &false)
            .field("poisoned", &true)
            .finish(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RtspErrorCategory, RtspErrorCode};

    #[test]
    fn zero_configuration_is_rejected() {
        for (entries, bytes) in [(0, 1), (1, 0), (0, 0)] {
            let error = FfmpegDiagnostics::new(entries, bytes).unwrap_err();
            assert_eq!(
                error,
                RtspError::Diagnostics(DiagnosticsError::InvalidConfig)
            );
            assert_eq!(error.category(), RtspErrorCategory::Diagnostics);
            assert_eq!(
                error.public_info().code,
                RtspErrorCode::InvalidDiagnosticsConfig
            );
        }
    }

    #[test]
    fn oldest_entries_are_evicted_at_capacity() {
        let (store, handle) = FfmpegDiagnostics::new(2, 64).unwrap();
        store.push_sanitized("first").unwrap();
        store.push_sanitized("second").unwrap();
        store.push_sanitized("third").unwrap();

        assert_eq!(handle.snapshot().unwrap(), ["second", "third"]);
    }

    #[test]
    fn truncation_respects_utf8_byte_limit() {
        let cases = [
            ("日本語です", 13usize),
            ("before😀after", 15usize),
            ("😀", 3usize),
        ];
        for (input, limit) in cases {
            let (store, handle) = FfmpegDiagnostics::new(1, limit).unwrap();
            store.push_sanitized(input).unwrap();
            let snapshot = handle.snapshot().unwrap();
            assert!(snapshot[0].len() <= limit);
            assert!(std::str::from_utf8(snapshot[0].as_bytes()).is_ok());
        }
    }

    #[test]
    fn close_is_idempotent_and_rejects_later_pushes() {
        let (store, _) = FfmpegDiagnostics::new(2, 32).unwrap();
        store.close().unwrap();
        store.close().unwrap();

        let error = store.push_sanitized("already sanitized").unwrap_err();
        assert_eq!(error, RtspError::Diagnostics(DiagnosticsError::Closed));
        assert_eq!(error.public_info().code, RtspErrorCode::DiagnosticsClosed);
    }

    #[test]
    fn cloned_handle_reads_shared_snapshot() {
        let (store, handle) = FfmpegDiagnostics::new(2, 32).unwrap();
        let cloned = handle.clone();
        store.push_sanitized("visible").unwrap();

        assert_eq!(cloned.snapshot().unwrap(), ["visible"]);
    }

    #[test]
    fn poisoned_mutex_returns_structured_error() {
        let (store, handle) = FfmpegDiagnostics::new(2, 32).unwrap();
        let inner = Arc::clone(&store.inner);
        let _ = std::thread::spawn(move || {
            let _guard = inner.state.lock().unwrap();
            panic!("poison diagnostics mutex");
        })
        .join();

        let error = handle.snapshot().unwrap_err();
        assert_eq!(error, RtspError::Diagnostics(DiagnosticsError::Poisoned));
        assert_eq!(error.public_info().code, RtspErrorCode::DiagnosticsPoisoned);
    }

    #[test]
    fn debug_exposes_metadata_without_entry_text() {
        let (store, handle) = FfmpegDiagnostics::new(2, 32).unwrap();
        store.push_sanitized("sensitive diagnostic body").unwrap();
        let debug = format!("{store:?} {handle:?}");

        assert!(debug.contains("entry_count"));
        assert!(debug.contains("max_entry_bytes"));
        assert!(!debug.contains("sensitive"));
        assert!(!debug.contains("diagnostic body"));
    }
}
