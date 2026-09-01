//! Bounded latest-frame storage for a future decoder/supervisor adapter.
//!
//! The store keeps at most one `Arc<VideoFrame>`. Publishing replaces the
//! previous frame, so a slow consumer can retain an old `Arc` without causing
//! an unbounded producer queue. Waiting uses a `Condvar`; callers never need a
//! busy loop. This module does not know about Tauri, Tokio, or JPEG decoding.

use std::fmt;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use crate::{FrameStoreError, RtspError, VideoFrame};

struct FrameStoreState {
    latest: Option<Arc<VideoFrame>>,
    next_sequence: u64,
    sequence_exhausted: bool,
    closed: bool,
}

struct FrameStoreInner {
    state: Mutex<FrameStoreState>,
    changed: Condvar,
}

/// A producer-facing latest-frame store. Cloning the store creates another
/// handle to the same bounded state, not another frame queue.
#[derive(Clone)]
pub struct LatestFrameStore {
    inner: Arc<FrameStoreInner>,
}

/// A shareable handle for UI/adapter consumers of a [`LatestFrameStore`].
#[derive(Clone)]
pub struct LatestFrameHandle {
    inner: Arc<FrameStoreInner>,
}

/// Result of waiting for a frame newer than the requested sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameWaitResult {
    /// A newer frame is available. The `Arc` lets consumers retain it while a
    /// producer replaces the store's current frame.
    Frame(Arc<VideoFrame>),
    /// The timeout elapsed before a newer frame or close notification arrived.
    Timeout,
    /// The store was closed, so no further frame can be published.
    Closed,
}

impl LatestFrameStore {
    /// Creates an open empty store. Sequence numbers start at one.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(FrameStoreInner {
                state: Mutex::new(FrameStoreState {
                    latest: None,
                    next_sequence: 1,
                    sequence_exhausted: false,
                    closed: false,
                }),
                changed: Condvar::new(),
            }),
        }
    }

    /// Returns a consumer handle sharing this store's state.
    pub fn handle(&self) -> LatestFrameHandle {
        LatestFrameHandle {
            inner: Arc::clone(&self.inner),
        }
    }

    /// Publishes one frame and replaces the previous latest frame.
    pub fn publish(
        &self,
        jpeg: Vec<u8>,
        received_at: SystemTime,
    ) -> Result<Arc<VideoFrame>, RtspError> {
        publish_inner(&self.inner, jpeg, received_at)
    }

    /// Returns the current latest frame, if any.
    pub fn snapshot(&self) -> Result<Option<Arc<VideoFrame>>, RtspError> {
        self.handle().snapshot()
    }

    /// Waits without busy-polling until a newer frame, timeout, or close.
    pub fn wait_for_newer(
        &self,
        after_sequence: u64,
        timeout: Duration,
    ) -> Result<FrameWaitResult, RtspError> {
        self.handle().wait_for_newer(after_sequence, timeout)
    }

    /// Closes the store and wakes all current waiters. Closing is idempotent.
    pub fn close(&self) -> Result<(), RtspError> {
        close_inner(&self.inner)
    }
}

impl Default for LatestFrameStore {
    fn default() -> Self {
        Self::new()
    }
}

impl LatestFrameHandle {
    /// Returns the current latest frame, if any.
    pub fn snapshot(&self) -> Result<Option<Arc<VideoFrame>>, RtspError> {
        Ok(lock_state(&self.inner)?.latest.clone())
    }

    /// Waits for a frame whose sequence is greater than `after_sequence`.
    /// Spurious condition-variable wakeups are handled by rechecking state.
    pub fn wait_for_newer(
        &self,
        after_sequence: u64,
        timeout: Duration,
    ) -> Result<FrameWaitResult, RtspError> {
        let mut state = lock_state(&self.inner)?;
        let started = Instant::now();

        loop {
            if state.closed {
                return Ok(FrameWaitResult::Closed);
            }
            if let Some(frame) = state.latest.as_ref() {
                if frame.sequence > after_sequence {
                    return Ok(FrameWaitResult::Frame(Arc::clone(frame)));
                }
            }

            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Ok(FrameWaitResult::Timeout);
            }

            let (next_state, wait_result) = self
                .inner
                .changed
                .wait_timeout(state, remaining)
                .map_err(|_| FrameStoreError::Poisoned)?;
            state = next_state;
            if wait_result.timed_out() {
                if state.closed {
                    return Ok(FrameWaitResult::Closed);
                }
                if let Some(frame) = state.latest.as_ref() {
                    if frame.sequence > after_sequence {
                        return Ok(FrameWaitResult::Frame(Arc::clone(frame)));
                    }
                }
                return Ok(FrameWaitResult::Timeout);
            }
        }
    }
}

impl fmt::Debug for LatestFrameStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        debug_store("LatestFrameStore", &self.inner, formatter)
    }
}

impl fmt::Debug for LatestFrameHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        debug_store("LatestFrameHandle", &self.inner, formatter)
    }
}

fn lock_state<'a>(
    inner: &'a FrameStoreInner,
) -> Result<MutexGuard<'a, FrameStoreState>, RtspError> {
    inner
        .state
        .lock()
        .map_err(|_| FrameStoreError::Poisoned.into())
}

fn publish_inner(
    inner: &Arc<FrameStoreInner>,
    jpeg: Vec<u8>,
    received_at: SystemTime,
) -> Result<Arc<VideoFrame>, RtspError> {
    let mut state = lock_state(inner)?;
    if state.closed {
        return Err(FrameStoreError::Closed.into());
    }
    if state.sequence_exhausted {
        return Err(FrameStoreError::SequenceExhausted.into());
    }

    let sequence = state.next_sequence;
    let frame = Arc::new(VideoFrame::new(sequence, received_at, jpeg));
    state.latest = Some(Arc::clone(&frame));
    if sequence == u64::MAX {
        state.sequence_exhausted = true;
    } else {
        state.next_sequence += 1;
    }
    drop(state);
    inner.changed.notify_all();
    Ok(frame)
}

fn close_inner(inner: &Arc<FrameStoreInner>) -> Result<(), RtspError> {
    let mut state = lock_state(inner)?;
    state.closed = true;
    drop(state);
    inner.changed.notify_all();
    Ok(())
}

fn debug_store(
    name: &str,
    inner: &FrameStoreInner,
    formatter: &mut fmt::Formatter<'_>,
) -> fmt::Result {
    let (has_frame, closed, sequence, poisoned) = match inner.state.lock() {
        Ok(state) => (
            state.latest.is_some(),
            state.closed,
            state.latest.as_ref().map(|frame| frame.sequence),
            false,
        ),
        Err(_) => (false, false, None, true),
    };
    formatter
        .debug_struct(name)
        .field("has_frame", &has_frame)
        .field("closed", &closed)
        .field("sequence", &sequence)
        .field("poisoned", &poisoned)
        .finish()
}

#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::{Duration, SystemTime};

    use super::*;
    use crate::{RtspErrorCategory, RtspErrorCode};

    fn frame_bytes(value: u8) -> Vec<u8> {
        vec![0xff, 0xd8, value, 0xff, 0xd9]
    }

    #[test]
    fn publish_assigns_sequences_and_replaces_the_previous_frame() {
        let store = LatestFrameStore::new();
        let first = store
            .publish(frame_bytes(1), SystemTime::UNIX_EPOCH)
            .unwrap();
        let second = store
            .publish(
                frame_bytes(2),
                SystemTime::UNIX_EPOCH + Duration::from_secs(1),
            )
            .unwrap();

        assert_eq!(first.sequence, 1);
        assert_eq!(second.sequence, 2);
        assert_eq!(store.snapshot().unwrap().unwrap().sequence, 2);
        assert_eq!(first.jpeg, frame_bytes(1));
        assert_eq!(second.jpeg, frame_bytes(2));
    }

    #[test]
    fn consumer_arc_can_outlive_replacement() {
        let store = LatestFrameStore::new();
        let held = store
            .publish(frame_bytes(1), SystemTime::UNIX_EPOCH)
            .unwrap();
        let replacement = store
            .publish(frame_bytes(2), SystemTime::UNIX_EPOCH)
            .unwrap();

        assert_eq!(held.sequence, 1);
        assert_eq!(held.jpeg, frame_bytes(1));
        assert_eq!(
            store.snapshot().unwrap().unwrap().sequence,
            replacement.sequence
        );
    }

    #[test]
    fn waiter_wakes_for_new_frame() {
        let store = LatestFrameStore::new();
        let handle = store.handle();
        let publisher = store.clone();
        let waiter = thread::spawn(move || handle.wait_for_newer(0, Duration::from_secs(2)));
        thread::sleep(Duration::from_millis(20));
        publisher
            .publish(frame_bytes(7), SystemTime::UNIX_EPOCH)
            .unwrap();

        assert!(matches!(
            waiter.join().unwrap().unwrap(),
            FrameWaitResult::Frame(frame) if frame.sequence == 1
        ));
    }

    #[test]
    fn waiter_times_out_without_busy_polling() {
        let store = LatestFrameStore::new();
        let started = Instant::now();
        assert_eq!(
            store.wait_for_newer(0, Duration::from_millis(20)).unwrap(),
            FrameWaitResult::Timeout
        );
        assert!(started.elapsed() >= Duration::from_millis(10));
    }

    #[test]
    fn spurious_wakes_do_not_extend_total_timeout() {
        let store = LatestFrameStore::new();
        let inner = Arc::clone(&store.inner);
        let notifier = thread::spawn(move || {
            for _ in 0..4 {
                thread::sleep(Duration::from_millis(8));
                inner.changed.notify_all();
            }
        });
        let started = Instant::now();

        assert_eq!(
            store.wait_for_newer(0, Duration::from_millis(35)).unwrap(),
            FrameWaitResult::Timeout
        );
        let elapsed = started.elapsed();
        notifier.join().unwrap();
        assert!(elapsed >= Duration::from_millis(25));
        assert!(elapsed < Duration::from_millis(500));
    }

    #[test]
    fn close_wakes_waiter_and_is_idempotent() {
        let store = LatestFrameStore::new();
        let handle = store.handle();
        let waiter = thread::spawn(move || handle.wait_for_newer(0, Duration::from_secs(2)));
        thread::sleep(Duration::from_millis(20));
        store.close().unwrap();
        store.close().unwrap();

        assert_eq!(waiter.join().unwrap().unwrap(), FrameWaitResult::Closed);
        assert_eq!(
            store
                .publish(frame_bytes(1), SystemTime::UNIX_EPOCH)
                .unwrap_err()
                .public_info(),
            crate::RtspErrorInfo {
                category: RtspErrorCategory::FrameStore,
                code: RtspErrorCode::FrameStoreClosed,
            }
        );
    }

    #[test]
    fn debug_does_not_include_jpeg_payload() {
        let store = LatestFrameStore::new();
        store
            .publish(vec![0xde, 0xad, 0xbe, 0xef], SystemTime::UNIX_EPOCH)
            .unwrap();
        let debug = format!("{store:?} {:?}", store.snapshot().unwrap().unwrap());

        assert!(debug.contains("has_frame: true"));
        assert!(!debug.contains("deadbeef"));
        assert!(!debug.contains("222"));
    }

    #[test]
    fn poisoned_mutex_returns_structured_error() {
        let store = LatestFrameStore::new();
        let inner = Arc::clone(&store.inner);
        let _ = thread::spawn(move || {
            let _guard = inner.state.lock().unwrap();
            panic!("intentional test-only poison");
        })
        .join();

        let error = store.snapshot().unwrap_err();
        assert_eq!(error.public_info().code, RtspErrorCode::FrameStorePoisoned);
        assert_eq!(error.category(), RtspErrorCategory::FrameStore);
        let debug = format!("{store:?}");
        assert!(debug.contains("poisoned: true"));
        assert!(!debug.contains("deadbeef"));
    }

    #[test]
    fn sequence_max_is_accepted_once_then_overflow_is_structured() {
        let store = LatestFrameStore::new();
        {
            let mut state = store.inner.state.lock().unwrap();
            state.next_sequence = u64::MAX;
        }

        assert_eq!(
            store
                .publish(frame_bytes(1), SystemTime::UNIX_EPOCH)
                .unwrap()
                .sequence,
            u64::MAX
        );
        let error = store
            .publish(frame_bytes(2), SystemTime::UNIX_EPOCH)
            .unwrap_err();
        assert_eq!(
            error.public_info().code,
            RtspErrorCode::FrameSequenceExhausted
        );
    }

    #[test]
    fn frame_debug_is_payload_safe() {
        let frame = VideoFrame::new(1, SystemTime::UNIX_EPOCH, vec![0xde, 0xad]);
        let debug = format!("{frame:?}");
        assert!(debug.contains("jpeg_bytes: 2"));
        assert!(!debug.contains("222"));
    }
}
