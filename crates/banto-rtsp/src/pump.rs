//! Synchronous, generic reader pumps for one FFmpeg stdout/stderr session.
//!
//! These functions own no threads and implement no restart policy. They use a
//! fixed stack buffer, pass stdout through the JPEG decoder/latest-frame store,
//! and pass stderr through the streaming sanitizer before diagnostics storage.

use std::fmt;
use std::io::Read;
use std::time::SystemTime;

use crate::{
    FfmpegDiagnostics, FfmpegInputFile, FfmpegLogStreamSanitizer, JpegFrameDecoder,
    LatestFrameStore, PumpError, PumpStream, RtspError,
};

const READ_BUFFER_BYTES: usize = 8 * 1024;

/// Non-secret counters returned when a pump reaches EOF.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PumpSummary {
    pub bytes_read: u64,
    pub frames_published: u64,
    pub first_frame_seen: bool,
}

impl PumpSummary {
    const fn new() -> Self {
        Self {
            bytes_read: 0,
            frames_published: 0,
            first_frame_seen: false,
        }
    }
}

impl fmt::Debug for PumpSummary {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PumpSummary")
            .field("bytes_read", &self.bytes_read)
            .field("frames_published", &self.frames_published)
            .field("first_frame_seen", &self.first_frame_seen)
            .finish()
    }
}

/// Reads FFmpeg stdout until EOF and publishes every complete JPEG.
///
/// The input-file guard is explicitly cleaned immediately after the first
/// frame is published. Before that point every return path relies on the guard's
/// Drop cleanup. Decoder and frame-store errors retain their existing
/// [`RtspError`] variants; only reader I/O is classified as a pump error.
pub fn pump_jpeg_stream<R: Read>(
    reader: R,
    decoder: &mut JpegFrameDecoder,
    store: &LatestFrameStore,
    input_guard: Option<FfmpegInputFile>,
) -> Result<PumpSummary, RtspError> {
    pump_jpeg_stream_with_first_frame(reader, decoder, store, input_guard, None::<fn(SystemTime)>)
}

pub(crate) fn pump_jpeg_stream_with_first_frame<R, F>(
    mut reader: R,
    decoder: &mut JpegFrameDecoder,
    store: &LatestFrameStore,
    mut input_guard: Option<FfmpegInputFile>,
    mut first_frame: Option<F>,
) -> Result<PumpSummary, RtspError>
where
    R: Read,
    F: FnOnce(SystemTime),
{
    let mut summary = PumpSummary::new();
    let mut buffer = [0u8; READ_BUFFER_BYTES];

    loop {
        let count = reader.read(&mut buffer).map_err(|error| PumpError::Read {
            stream: PumpStream::Stdout,
            kind: error.kind(),
        })?;
        if count == 0 {
            return Ok(summary);
        }
        summary.bytes_read = summary.bytes_read.saturating_add(count as u64);

        for jpeg in decoder.push(&buffer[..count])? {
            let received_at = SystemTime::now();
            store.publish(jpeg, received_at)?;
            summary.frames_published = summary.frames_published.saturating_add(1);
            if !summary.first_frame_seen {
                summary.first_frame_seen = true;
                if let Some(guard) = input_guard.take() {
                    guard.cleanup()?;
                }
                if let Some(report) = first_frame.take() {
                    report(received_at);
                }
            }
        }
    }
}

/// Reads FFmpeg stderr until EOF, sanitizes bytes, and stores bounded entries.
///
/// Raw bytes are never decoded, logged, or stored before sanitization. Each
/// non-empty sanitized chunk becomes one bounded diagnostics entry; no line
/// accumulator is used. On a read failure, retained sanitizer carry is finished
/// and stored before returning [`PumpError`]. If that final diagnostics write
/// fails, the diagnostics error takes precedence because it signals that the
/// safety/observability path itself could not retain the already-sanitized tail.
pub fn pump_stderr<R: Read>(
    mut reader: R,
    mut sanitizer: FfmpegLogStreamSanitizer,
    diagnostics: &FfmpegDiagnostics,
) -> Result<PumpSummary, RtspError> {
    let mut summary = PumpSummary::new();
    let mut buffer = [0u8; READ_BUFFER_BYTES];

    loop {
        match reader.read(&mut buffer) {
            Ok(0) => {
                store_sanitized(sanitizer.finish(), diagnostics)?;
                return Ok(summary);
            }
            Ok(count) => {
                summary.bytes_read = summary.bytes_read.saturating_add(count as u64);
                store_sanitized(sanitizer.push(&buffer[..count]), diagnostics)?;
            }
            Err(error) => {
                store_sanitized(sanitizer.finish(), diagnostics)?;
                return Err(PumpError::Read {
                    stream: PumpStream::Stderr,
                    kind: error.kind(),
                }
                .into());
            }
        }
    }
}

fn store_sanitized(sanitized: Vec<u8>, diagnostics: &FfmpegDiagnostics) -> Result<(), RtspError> {
    if sanitized.is_empty() {
        return Ok(());
    }
    let text = String::from_utf8_lossy(&sanitized);
    diagnostics.push_sanitized(&text)
}

#[cfg(test)]
mod tests {
    use std::io::{self, Cursor};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::{FfmpegDiagnosticsHandle, RtspCredentials, RtspEndpoint, RtspErrorCode};

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn endpoint() -> RtspEndpoint {
        RtspEndpoint::new("rtsp://camera.example/live").unwrap()
    }

    fn test_path(label: &str) -> PathBuf {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "banto-rtsp-pump-{label}-{}-{id}",
            std::process::id()
        ))
    }

    fn input_guard(label: &str) -> (PathBuf, FfmpegInputFile) {
        let path = test_path(label);
        let guard = FfmpegInputFile::create_new(
            &path,
            &endpoint(),
            None,
            crate::RtspTransport::Tcp,
            std::time::Duration::from_secs(5),
        )
        .unwrap();
        (path, guard)
    }

    fn diagnostics(
        max_entries: usize,
        max_entry_bytes: usize,
    ) -> (FfmpegDiagnostics, FfmpegDiagnosticsHandle) {
        FfmpegDiagnostics::new(max_entries, max_entry_bytes).unwrap()
    }

    struct ByteReader<R> {
        inner: R,
    }

    impl<R: Read> Read for ByteReader<R> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let limit = buffer.len().min(1);
            self.inner.read(&mut buffer[..limit])
        }
    }

    struct FailingReader {
        bytes: Cursor<Vec<u8>>,
        fail_after: usize,
        delivered: usize,
    }

    impl FailingReader {
        fn new(bytes: impl Into<Vec<u8>>, fail_after: usize) -> Self {
            Self {
                bytes: Cursor::new(bytes.into()),
                fail_after,
                delivered: 0,
            }
        }
    }

    impl Read for FailingReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.delivered >= self.fail_after {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, "test failure"));
            }
            let remaining = self.fail_after - self.delivered;
            let limit = buffer.len().min(remaining);
            let count = self.bytes.read(&mut buffer[..limit])?;
            self.delivered += count;
            Ok(count)
        }
    }

    #[test]
    fn stdout_handles_split_markers_and_publishes_latest_frame() {
        let bytes = vec![0xff, 0xd8, 1, 0xff, 0xd9, 0xff, 0xd8, 2, 0xff, 0xd9];
        let reader = ByteReader {
            inner: Cursor::new(bytes.clone()),
        };
        let mut decoder = JpegFrameDecoder::new(64).unwrap();
        let store = LatestFrameStore::new();
        let handle = store.handle();

        let summary = pump_jpeg_stream(reader, &mut decoder, &store, None).unwrap();

        assert_eq!(summary.bytes_read, bytes.len() as u64);
        assert_eq!(summary.frames_published, 2);
        assert!(summary.first_frame_seen);
        let latest = handle.snapshot().unwrap().unwrap();
        assert_eq!(latest.sequence, 2);
        assert_eq!(latest.jpeg, [0xff, 0xd8, 2, 0xff, 0xd9]);
    }

    #[test]
    fn first_frame_removes_input_file() {
        let (path, guard) = input_guard("first-frame");
        let mut decoder = JpegFrameDecoder::new(32).unwrap();
        let store = LatestFrameStore::new();

        let summary = pump_jpeg_stream(
            Cursor::new([0xff, 0xd8, 0xff, 0xd9]),
            &mut decoder,
            &store,
            Some(guard),
        )
        .unwrap();

        assert!(summary.first_frame_seen);
        assert!(!path.exists());
    }

    #[test]
    fn first_frame_callback_runs_once_with_published_timestamp() {
        let bytes = vec![0xff, 0xd8, 1, 0xff, 0xd9, 0xff, 0xd8, 2, 0xff, 0xd9];
        let mut decoder = JpegFrameDecoder::new(64).unwrap();
        let store = LatestFrameStore::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let reported_at = Arc::new(Mutex::new(None));
        let callback_calls = Arc::clone(&calls);
        let callback_at = Arc::clone(&reported_at);
        let callback_store = store.clone();

        let summary = pump_jpeg_stream_with_first_frame(
            Cursor::new(bytes),
            &mut decoder,
            &store,
            None,
            Some(move |received_at| {
                callback_calls.fetch_add(1, Ordering::Relaxed);
                let published_at = callback_store
                    .snapshot()
                    .unwrap()
                    .expect("first frame must already be published")
                    .received_at;
                *callback_at.lock().unwrap() = Some((received_at, published_at));
            }),
        )
        .unwrap();

        assert_eq!(summary.frames_published, 2);
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        let (reported_at, published_at) = reported_at.lock().unwrap().unwrap();
        assert_eq!(reported_at, published_at);
    }

    #[test]
    fn no_frame_eof_uses_guard_drop_cleanup() {
        let (path, guard) = input_guard("no-frame");
        let mut decoder = JpegFrameDecoder::new(32).unwrap();
        let store = LatestFrameStore::new();

        let summary =
            pump_jpeg_stream(Cursor::new([1, 2, 3]), &mut decoder, &store, Some(guard)).unwrap();

        assert!(!summary.first_frame_seen);
        assert!(!path.exists());
    }

    #[test]
    fn stdout_read_error_is_structured_and_cleans_input_file() {
        let (path, guard) = input_guard("stdout-error");
        let reader = FailingReader::new([1, 2, 3], 2);
        let mut decoder = JpegFrameDecoder::new(32).unwrap();
        let store = LatestFrameStore::new();

        let error = pump_jpeg_stream(reader, &mut decoder, &store, Some(guard)).unwrap_err();

        assert_eq!(error.public_info().code, RtspErrorCode::StdoutReadFailed);
        assert!(!path.exists());
    }

    #[test]
    fn oversized_stdout_frame_keeps_existing_structured_error() {
        let mut decoder = JpegFrameDecoder::new(4).unwrap();
        let store = LatestFrameStore::new();
        let error = pump_jpeg_stream(
            Cursor::new([0xff, 0xd8, 1, 2, 3]),
            &mut decoder,
            &store,
            None,
        )
        .unwrap_err();

        assert_eq!(error.public_info().code, RtspErrorCode::FrameTooLarge);
    }

    #[test]
    fn stderr_one_byte_chunks_never_store_endpoint_or_credentials() {
        let credentials = RtspCredentials::new("viewer", "example-pass");
        let sanitizer = crate::FfmpegLogSanitizer::new(&endpoint(), Some(&credentials)).stream();
        let input = b"rtsp://viewer:example-pass@camera.example/live rtsp://camera.example/live";
        let reader = ByteReader {
            inner: Cursor::new(input),
        };
        let (diagnostics, handle) = diagnostics(16, 128);

        pump_stderr(reader, sanitizer, &diagnostics).unwrap();
        let text = handle.snapshot().unwrap().join("");

        assert!(!text.contains("camera.example"));
        assert!(!text.contains("viewer"));
        assert!(!text.contains("example-pass"));
        assert!(text.contains("[REDACTED]"));
    }

    #[test]
    fn stderr_invalid_utf8_does_not_panic() {
        let sanitizer = crate::FfmpegLogSanitizer::new(&endpoint(), None).stream();
        let (diagnostics, handle) = diagnostics(4, 64);

        let summary = pump_stderr(
            Cursor::new([0xff, b'o', b'k', 0xfe]),
            sanitizer,
            &diagnostics,
        )
        .unwrap();

        assert_eq!(summary.bytes_read, 4);
        assert!(!handle.snapshot().unwrap().is_empty());
    }

    #[test]
    fn long_input_without_newlines_remains_bounded() {
        let sanitizer = crate::FfmpegLogSanitizer::new(&endpoint(), None).stream();
        let (diagnostics, handle) = diagnostics(3, 17);

        pump_stderr(Cursor::new(vec![b'x'; 50_000]), sanitizer, &diagnostics).unwrap();
        let entries = handle.snapshot().unwrap();

        assert!(entries.len() <= 3);
        assert!(entries.iter().all(|entry| entry.len() <= 17));
    }

    #[test]
    fn stderr_read_error_finishes_and_stores_sanitized_carry() {
        let sanitizer = crate::FfmpegLogSanitizer::new(&endpoint(), None).stream();
        let input = b"prefix rtsp://camera.example/live";
        let reader = FailingReader::new(input, input.len());
        let (diagnostics, handle) = diagnostics(4, 128);

        let error = pump_stderr(reader, sanitizer, &diagnostics).unwrap_err();
        let text = handle.snapshot().unwrap().join("");

        assert_eq!(error.public_info().code, RtspErrorCode::StderrReadFailed);
        assert_eq!(text, "prefix [REDACTED]");
        assert!(!text.contains("camera.example"));
    }

    #[test]
    fn diagnostics_error_takes_priority_while_finishing_after_read_error() {
        let sanitizer = crate::FfmpegLogSanitizer::new(&endpoint(), None).stream();
        let input = b"sanitized tail";
        let reader = FailingReader::new(input, input.len());
        let (diagnostics, _) = diagnostics(2, 64);
        diagnostics.close().unwrap();

        let error = pump_stderr(reader, sanitizer, &diagnostics).unwrap_err();

        assert_eq!(error.public_info().code, RtspErrorCode::DiagnosticsClosed);
    }

    #[test]
    fn summary_debug_contains_only_counters() {
        let summary = PumpSummary {
            bytes_read: 12,
            frames_published: 1,
            first_frame_seen: true,
        };
        let debug = format!("{summary:?}");

        assert!(debug.contains("bytes_read"));
        assert!(debug.contains("frames_published"));
        assert!(!debug.contains("camera.example"));
    }
}
