//! S7 D-5 (#1019) の隔離試験用。キューのロック中は bounded channel へ
//! try_send するだけ。書き込み・JSON 化は専用スレッドで行い、欠損した
//! 記録や停止途中のファイルには complete を付けない。

use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use serde::Serialize;

use crate::queue::Row;

const BUFFER_EVENTS: usize = 1_024;
const MAX_EVENTS: usize = 65_536;
const MAX_FILE_BYTES: usize = 64 * 1024 * 1024;
pub(crate) const MAX_IDS: usize = 4_096;
const MAX_TEXT_BYTES: usize = 1_024;

/// 一つの実行に一つだけ作る。停止処理が終わった後に finish する。
pub struct TraceOutput {
    pub(crate) trace: Trace,
    writer: Option<JoinHandle<io::Result<()>>>,
}

#[derive(Clone, Debug)]
pub(crate) struct Trace(Arc<Shared>);

#[derive(Debug)]
struct Shared {
    group_id: i64,
    sender: Mutex<Option<SyncSender<Record>>>,
    lost: AtomicBool,
    complete_requested: AtomicBool,
    next_queue: AtomicU64,
    next_attempt: AtomicU64,
}

#[derive(Clone, Debug)]
pub(crate) struct QueueTrace {
    trace: Trace,
    queue_id: u64,
}

#[derive(Debug, Serialize)]
struct Record {
    queue_id: u64,
    #[serde(flatten)]
    event: Event,
}

#[derive(Debug, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub(crate) enum Event {
    QueueOpened {
        connection_id: i64,
        table: String,
    },
    Enqueued {
        row_id: u128,
        #[serde(flatten)]
        row: Row,
    },
    Overflow {
        row_id: u128,
    },
    RejectedAtZeroCapacity,
    Discarded {
        row_id: u128,
    },
    AckRemoved {
        row_id: u128,
    },
    Handoff {
        row_ids: Vec<u128>,
    },
    QueueRetiring {
        row_ids: Vec<u128>,
    },
    BatchAttempt {
        attempt_id: u64,
        row_ids: Vec<u128>,
    },
    BatchOutcome {
        attempt_id: u64,
        outcome: BatchOutcome,
    },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BatchOutcome {
    Confirmed,
    NotCommitted,
    Unknown,
}

/// バッチを待つ future が捨てられたときも、COMMIT に入ったかで結果を区別する。
pub(crate) struct Attempt {
    queue: QueueTrace,
    id: u64,
    pub(crate) commit_started: bool,
    confirmed: bool,
}

impl TraceOutput {
    /// 存在するファイルを上書きしない。出力先の親は試験者だけが読める場所を使う。
    pub fn create(group_id: i64, path: &Path) -> io::Result<Self> {
        if group_id <= 0 || !path.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "D-5 trace requires a positive group ID and an absolute output path",
            ));
        }
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(path)?;
        let (sender, receiver) = mpsc::sync_channel(BUFFER_EVENTS);
        let shared = Arc::new(Shared {
            group_id,
            sender: Mutex::new(Some(sender)),
            lost: AtomicBool::new(false),
            complete_requested: AtomicBool::new(false),
            next_queue: AtomicU64::new(0),
            next_attempt: AtomicU64::new(0),
        });
        let state = shared.clone();
        let writer = std::thread::Builder::new()
            .name("sink-d5-trace".into())
            .spawn(move || write_trace(file, receiver, &state))?;
        Ok(Self {
            trace: Trace(shared),
            writer: Some(writer),
        })
    }

    /// 呼び出し元はランタイムを先に drop し、全 producer/flusher の終了を確かめる。
    pub fn finish(mut self, success: bool) -> io::Result<()> {
        self.trace
            .0
            .complete_requested
            .store(success, Ordering::Relaxed);
        self.trace.close();
        self.writer
            .take()
            .expect("trace writer")
            .join()
            .map_err(|_| io::Error::other("D-5 trace writer panicked"))?
    }
}

impl Drop for TraceOutput {
    fn drop(&mut self) {
        self.trace.close();
    }
}

impl Trace {
    pub(crate) fn queue(
        &self,
        group_id: i64,
        connection_id: i64,
        table: &str,
    ) -> Option<QueueTrace> {
        if group_id != self.0.group_id {
            return None;
        }
        let queue = QueueTrace {
            trace: self.clone(),
            queue_id: self.0.next_queue.fetch_add(1, Ordering::Relaxed),
        };
        if table.len() > MAX_TEXT_BYTES {
            queue.invalidate();
        } else {
            queue.record(Event::QueueOpened {
                connection_id,
                table: table.to_owned(),
            });
        }
        Some(queue)
    }

    fn close(&self) {
        self.0
            .sender
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
    }
}

impl QueueTrace {
    pub(crate) fn invalidate(&self) {
        self.trace.0.lost.store(true, Ordering::Relaxed);
    }

    pub(crate) fn record(&self, event: Event) {
        let state = &self.trace.0;
        if state.lost.load(Ordering::Relaxed) {
            return;
        }
        let sender = state.sender.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(sender) = sender.as_ref() {
            if sender
                .try_send(Record {
                    queue_id: self.queue_id,
                    event,
                })
                .is_err()
            {
                self.invalidate();
            }
        }
    }

    pub(crate) fn enqueue(&self, row_id: u128, row: &Row) {
        if row.external_name.len() > MAX_TEXT_BYTES
            || row.quality.len() > MAX_TEXT_BYTES
            || row.value.is_some_and(|value| !value.is_finite())
        {
            self.invalidate();
        } else {
            self.record(Event::Enqueued {
                row_id,
                row: row.clone(),
            });
        }
    }

    pub(crate) fn attempt(&self, row_ids: Vec<u128>) -> Attempt {
        let id = self.trace.0.next_attempt.fetch_add(1, Ordering::Relaxed);
        self.record(Event::BatchAttempt {
            attempt_id: id,
            row_ids,
        });
        Attempt {
            queue: self.clone(),
            id,
            commit_started: false,
            confirmed: false,
        }
    }
}

impl Attempt {
    pub(crate) fn confirm(mut self) {
        self.confirmed = true;
    }
}

impl Drop for Attempt {
    fn drop(&mut self) {
        let outcome = if self.confirmed {
            BatchOutcome::Confirmed
        } else if self.commit_started {
            BatchOutcome::Unknown
        } else {
            BatchOutcome::NotCommitted
        };
        self.queue.record(Event::BatchOutcome {
            attempt_id: self.id,
            outcome,
        });
    }
}

fn write_trace(file: File, receiver: mpsc::Receiver<Record>, state: &Shared) -> io::Result<()> {
    let mut writer = BufWriter::new(file);
    let mut remaining_bytes = MAX_FILE_BYTES;
    let mut count = 0;
    let mut seen_queue = false;
    let mut attempts = HashSet::new();
    write_line(
        &mut writer,
        &mut remaining_bytes,
        &serde_json::json!({"event": "start", "version": 1, "group_id": state.group_id}),
    )?;
    for record in receiver {
        if count == MAX_EVENTS {
            state.lost.store(true, Ordering::Relaxed);
            continue;
        }
        count += 1;
        seen_queue |= matches!(record.event, Event::QueueOpened { .. });
        let unmatched_outcome = match &record.event {
            Event::BatchAttempt { attempt_id, .. } => {
                attempts.insert(*attempt_id);
                false
            }
            Event::BatchOutcome { attempt_id, .. } => !attempts.remove(attempt_id),
            _ => false,
        };
        if unmatched_outcome {
            state.lost.store(true, Ordering::Relaxed);
        }
        write_line(&mut writer, &mut remaining_bytes, &record)?;
    }
    if !state.complete_requested.load(Ordering::Relaxed)
        || state.lost.load(Ordering::Relaxed)
        || !seen_queue
        || !attempts.is_empty()
    {
        return Err(io::Error::other("D-5 trace incomplete"));
    }
    write_line(
        &mut writer,
        &mut remaining_bytes,
        &serde_json::json!({"event": "complete", "records": count}),
    )
}

fn write_line(
    writer: &mut BufWriter<File>,
    remaining: &mut usize,
    value: &impl Serialize,
) -> io::Result<()> {
    let line = serde_json::to_vec(value)?;
    *remaining = remaining
        .checked_sub(line.len() + 1)
        .ok_or_else(|| io::Error::other("D-5 trace exceeded the file size limit"))?;
    writer.write_all(&line)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queue::RowQueue;
    use serde_json::{json, Value};

    fn row() -> Row {
        Row {
            ts_ms: 123,
            tag_id: 9,
            external_name: "test.source.tag".into(),
            value: Some(45.0),
            quality: "good".into(),
        }
    }

    fn records(path: &Path) -> Vec<Value> {
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn selected_queue_keeps_exact_ids_across_handoff_and_overflow_during_insert() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trace.jsonl");
        let output = TraceOutput::create(42, &path).unwrap();
        assert!(TraceOutput::create(42, &path).is_err());
        assert!(output.trace.queue(41, 1, "excluded").is_none());

        let mut queue = RowQueue::new(2);
        queue.enable_trace(output.trace.queue(42, 1, "test.rows").unwrap());
        // 同じ五列でも row_id が違う。送信中の最古の行だけを overflow させる。
        queue.push(row());
        queue.push(row());
        queue.trace_config_boundary(true);
        let mut batch = queue.peek_batch(2);
        let attempt = batch.trace_attempt().unwrap();
        queue.push(row());
        queue.commit(&batch);
        attempt.confirm();
        let mut remaining = queue.peek_batch(2);
        drop(remaining.trace_attempt().unwrap());
        let mut remaining = queue.peek_batch(2);
        let mut ambiguous = remaining.trace_attempt().unwrap();
        ambiguous.commit_started = true;
        drop(ambiguous);
        queue.trace_config_boundary(false);
        assert_eq!(queue.discard_all(), 1);
        output.finish(true).unwrap();

        let lines = records(&path);
        assert_eq!(lines[0], json!({"event":"start","version":1,"group_id":42}));
        assert_eq!(
            lines[2],
            json!({"event":"enqueued","queue_id":0,"row_id":0,
            "ts_ms":123,"tag_id":9,"external_name":"test.source.tag","value":45.0,"quality":"good"})
        );
        assert_eq!(
            lines[4],
            json!({"event":"handoff","queue_id":0,"row_ids":[0,1]})
        );
        assert_eq!(lines[5]["row_ids"], json!([0, 1]));
        assert_eq!(
            lines[6],
            json!({"event":"overflow","queue_id":0,"row_id":0})
        );
        assert_eq!(
            lines[8],
            json!({"event":"ack_removed","queue_id":0,"row_id":1})
        );
        assert_eq!(lines[9]["outcome"], "confirmed");
        assert_eq!(lines[11]["outcome"], "not_committed");
        assert_eq!(lines[13]["outcome"], "unknown");
        assert_eq!(lines[14]["row_ids"], json!([2]));
        assert_eq!(
            lines[15],
            json!({"event":"discarded","queue_id":0,"row_id":2})
        );
        assert_eq!(lines[16], json!({"event":"complete","records":15}));
    }

    #[test]
    fn an_open_attempt_or_an_unrepresentable_row_prevents_completion() {
        let dir = tempfile::tempdir().unwrap();
        for open_attempt in [true, false] {
            let path = dir.path().join(format!("incomplete-{open_attempt}.jsonl"));
            let output = TraceOutput::create(42, &path).unwrap();
            let trace = output.trace.queue(42, 1, "test.rows").unwrap();
            let attempt = if open_attempt {
                trace.enqueue(0, &row());
                Some(trace.attempt(vec![0]))
            } else {
                let mut oversized = row();
                oversized.external_name = "a".repeat(MAX_TEXT_BYTES + 1);
                trace.enqueue(0, &oversized);
                None
            };
            assert!(output.finish(true).is_err());
            assert!(records(&path)
                .iter()
                .all(|record| record["event"] != "complete"));
            drop(attempt);
        }
    }
}
