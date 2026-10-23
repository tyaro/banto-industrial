//! 線の表と同期の手順（設計 §7.3）を、記憶上のシリアルの線と止めた時計で確かめる。
//!
//! 小さなプロトコル: 要求は `[局番, FC, アドレス]`、応答は `[局番, FC, N, データ × N]`。
//! 署名は `[局番, FC, N]`（正常の応答の先頭 3 バイトと同じ）。局 1 の FC3 の読み取り A と B は
//! 同じ署名を持つので、A の遅れた応答は B の応答と見分けがつかない（§7.3 の危険そのもの）。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use banto_driver_api::{
    AddressError, Capabilities, DataType, DriverAddress, DriverBase, DriverDescriptor, DriverId,
    FieldError, Planned, TagKind, Transport as TransportKind,
};
use tokio::time::Instant;

use super::line::{LineKey, LineOwner, LineTable, SyncState};
use super::*;

const T: Duration = Duration::from_millis(100);
const UNIT: u8 = 1;
const FC_READ: u8 = 3;
const FC_PROBE: u8 = 4;
const ADDR_A: u8 = 0xA0;
const ADDR_B: u8 = 0xB0;

/// 1 回の送信への線の反応。
#[derive(Clone)]
enum Reaction {
    /// 何も返さない（時間切れになる）。
    Silence,
    /// これらのまとまりを順に届ける（1 回の recv に 1 つ）。
    Reply(Vec<Vec<u8>>),
    /// send が `Io` で失敗する。
    SendFails,
    /// send が返らない（送信の途中。呼び手の取り消しで future が drop される）。
    SendStalls,
}

#[derive(Default)]
struct WireState {
    sent: Vec<Vec<u8>>,
    /// 各 send を呼んだ時刻（止めた時計）。
    sent_times: Vec<Instant>,
    /// 1 バイトの送信の時間（`Transport::tx_time` の見積もり）。
    tx_per_byte: Duration,
    script: VecDeque<Reaction>,
    inbox: VecDeque<Vec<u8>>,
    opens: usize,
    closes: usize,
}

#[derive(Clone, Default)]
struct Shared(Arc<Mutex<WireState>>);

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, WireState> {
        self.0.lock().unwrap()
    }
    fn react(&self, reactions: impl IntoIterator<Item = Reaction>) -> &Self {
        self.lock().script.extend(reactions);
        self
    }
    fn sent(&self) -> Vec<Vec<u8>> {
        self.lock().sent.clone()
    }
    fn open(&self) -> Result<Box<dyn Transport>, FatalError> {
        self.lock().opens += 1;
        Ok(Box::new(SerialWire(self.clone())))
    }
}

struct SerialWire(Shared);

impl Drop for SerialWire {
    fn drop(&mut self) {
        self.0.lock().closes += 1;
    }
}

impl Transport for SerialWire {
    fn tx_time(&self, len: usize) -> Duration {
        self.0.lock().tx_per_byte * len as u32
    }
    fn send<'a>(&'a mut self, bytes: &'a [u8]) -> BoxFuture<'a, Result<(), FatalError>> {
        Box::pin(async move {
            let reaction = {
                let mut state = self.0.lock();
                state.sent.push(bytes.to_vec());
                state.sent_times.push(Instant::now());
                state.script.pop_front().unwrap_or(Reaction::Silence)
            };
            match reaction {
                Reaction::Silence => {}
                Reaction::Reply(chunks) => self.0.lock().inbox.extend(chunks),
                Reaction::SendFails => return Err(FatalError::Io("送信の失敗".into())),
                Reaction::SendStalls => std::future::pending::<()>().await,
            }
            Ok(())
        })
    }
    fn recv<'a>(&'a mut self, buf: &'a mut Vec<u8>) -> BoxFuture<'a, Result<usize, FatalError>> {
        Box::pin(async move {
            loop {
                if let Some(chunk) = self.0.lock().inbox.pop_front() {
                    buf.extend_from_slice(&chunk);
                    return Ok(chunk.len());
                }
                // 止めた時計では sleep が時計を進める。期限は呼び手が掛ける。
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
    }
    fn discard_input(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            self.0.lock().inbox.clear();
        })
    }
    fn close(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
}

fn request(fc: u8, addr: u8) -> Step {
    Step::new(vec![UNIT, fc, addr], FrameRule::DriverDecides, 16)
}
fn response(fc: u8, data: &[u8]) -> Vec<u8> {
    let mut frame = vec![UNIT, fc, data.len() as u8];
    frame.extend_from_slice(data);
    frame
}
fn sig(fc: u8, n: u8) -> Signature {
    Signature::new(vec![UNIT, fc, n])
}

/// 同期の手順の選び方。
#[derive(Clone, Copy, PartialEq, Eq)]
enum SyncPolicy {
    /// 同期の手順を持たない（`sync_step` が常に `None`）。
    None,
    /// FC4 の 1 語 → FC4 の 2 語 → FC3 の 2 語の順に、待っている署名と違うものを選ぶ。
    Distinct,
    /// 反証: 待っている署名と同じ（FC3 の 1 語）を選ぶ。
    Colliding,
    /// 契約違反: 同期の step を `Fixed` で区切る（FC4 の 1 語、4 バイト）。
    Fixed,
}

#[derive(Clone)]
struct Conn {
    policy: SyncPolicy,
    /// 書き込みを応答を待たない step（`FrameRule::None`）で送る。
    no_reply_write: bool,
    /// フレーム間の無音（§7.3.1）。
    gap: Duration,
    drops: Arc<AtomicUsize>,
}

impl Drop for Conn {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

impl FrameConnection for Conn {
    fn handshake_next(&mut self, _: Option<&[u8]>) -> Result<Option<Step>, FatalError> {
        Ok(None)
    }
    fn inter_frame_gap(&self) -> Duration {
        self.gap
    }
    fn signature(&self, step: &Step) -> Option<Signature> {
        // 応答の語数は、この試験のプロトコルでは FC の下位ビットで表す（1 語か 2 語）。
        let n = if step.send[2] >= 0xF0 { 2 } else { 1 };
        Some(Signature::new(vec![step.send[0], step.send[1], n]))
    }
    fn sync_step(&mut self, outstanding: &[Signature]) -> Option<Step> {
        let candidates: &[(u8, u8)] = match self.policy {
            SyncPolicy::None => return None,
            SyncPolicy::Distinct => &[(FC_PROBE, 0x00), (FC_PROBE, 0xF0), (FC_READ, 0xF0)],
            // 反証: 待っている署名を見ずに、A と同じ署名の要求を選ぶ。
            SyncPolicy::Colliding => return Some(request(FC_READ, 0x00)),
            SyncPolicy::Fixed => {
                return Some(Step::new(
                    request(FC_PROBE, 0x00).send,
                    FrameRule::Fixed(4),
                    16,
                ))
            }
        };
        candidates
            .iter()
            .map(|&(fc, addr)| request(fc, addr))
            .find(|step| !outstanding.contains(&self.signature(step).unwrap()))
    }
    fn response_matches(&self, signature: &Signature, response: &[u8]) -> bool {
        response.len() >= 3 && &response[..3] == signature.as_bytes()
    }
    fn is_complete(&self, _: &Step, buf: &[u8]) -> Result<FrameCheck, FatalError> {
        if buf.len() < 3 {
            return Ok(FrameCheck::Incomplete);
        }
        let total = 3 + buf[2] as usize;
        Ok(if buf.len() >= total {
            FrameCheck::Complete { len: total as u32 }
        } else {
            FrameCheck::Incomplete
        })
    }
    fn plan_read(&mut self, specs: &[TagSpec]) -> Result<Vec<Planned>, FatalError> {
        specs
            .iter()
            .enumerate()
            .map(|(i, spec)| {
                // 16 進でないアドレス（`BAD_ADDRESS`）は、送る前のドライバの致命。
                let addr = u8::from_str_radix(&spec.address.canonical, 16)
                    .map_err(|_| FatalError::Protocol("計画できないアドレス".into()))?;
                Ok(Planned::new(request(FC_READ, addr), vec![i as u32]))
            })
            .collect()
    }
    fn decode_read(
        &self,
        _: &Planned,
        _: &[TagSpec],
        response: &[u8],
    ) -> Result<Vec<Result<RawValue, ItemError>>, FatalError> {
        Ok(vec![Ok(RawValue::Uint(u64::from(response[3])))])
    }
    fn write_next(
        &mut self,
        items: &[WriteItem],
        previous: Option<&[u8]>,
    ) -> Result<WriteProgress, FatalError> {
        Ok(if previous.is_some() {
            WriteProgress::Done(vec![Ok(()); items.len()])
        } else if self.no_reply_write {
            let step = Step::new(request(0x06, ADDR_A).send, FrameRule::None, 16);
            WriteProgress::Send(Planned::new(step, vec![0]))
        } else {
            WriteProgress::Send(Planned::new(request(0x06, ADDR_A), vec![0]))
        })
    }
}

struct Driver {
    descriptor: DriverDescriptor,
    conn: Conn,
}

impl Driver {
    fn new(policy: SyncPolicy) -> Self {
        let mut descriptor = DriverDescriptor::new(
            DriverId::new("serial-test").unwrap(),
            "0.1.0",
            "試験",
            Capabilities::READ_WRITE,
        );
        descriptor.transports = vec![TransportKind::Serial, TransportKind::SerialOverTcp];
        Self {
            descriptor,
            conn: Conn {
                policy,
                no_reply_write: false,
                gap: Duration::ZERO,
                drops: Arc::default(),
            },
        }
    }
}

impl DriverBase for Driver {
    fn descriptor(&self) -> &DriverDescriptor {
        &self.descriptor
    }
    fn validate_config(&self, _: &Settings) -> Vec<FieldError> {
        Vec::new()
    }
    fn parse_address(&self, text: &str, kind: &TagKind) -> Result<DriverAddress, AddressError> {
        Ok(DriverAddress::new(
            self.descriptor.id.clone(),
            text,
            "a",
            true,
            kind.word_count(),
        ))
    }
    fn offset_address(&self, text: &str, _: &TagKind, _: i64) -> Result<String, AddressError> {
        Ok(text.into())
    }
    fn address_help(&self) -> String {
        String::new()
    }
}

impl FrameDriver for Driver {
    fn create_connection(&self, _: &Settings) -> Result<Box<dyn FrameConnection>, FatalError> {
        Ok(Box::new(self.conn.clone()))
    }
}

/// `plan_read` が断るアドレス（送る前の致命。線は `Synced` のまま返る）。
const BAD_ADDRESS: &str = "ZZ";

fn spec_of(addr: u8) -> TagSpec {
    spec_of_text(&format!("{addr:02X}"))
}
fn spec_of_text(address: &str) -> TagSpec {
    TagSpec::new(
        DriverAddress::new(DriverId::new("serial-test").unwrap(), address, "a", true, 1),
        TagKind::numeric(DataType::U16),
    )
}

fn transport_spec(port: u16) -> TransportSpec {
    TransportSpec::SerialOverTcp {
        host: "mem".into(),
        port,
    }
}

struct Bench {
    table: LineTable,
    spec: TransportSpec,
    wire: Shared,
    driver: Driver,
    settings: Settings,
}

impl Bench {
    fn new(policy: SyncPolicy) -> Self {
        Self {
            table: LineTable::new(),
            spec: transport_spec(1),
            wire: Shared::default(),
            driver: Driver::new(policy),
            settings: Settings::from_pairs(Vec::<(&str, &str)>::new()).unwrap(),
        }
    }
    async fn connect(&self) -> Result<FrameSession, FatalError> {
        self.connect_as(None).await
    }
    async fn connect_as(&self, owner: Option<LineOwner>) -> Result<FrameSession, FatalError> {
        let wire = self.wire.clone();
        FrameSession::with_line(
            &self.driver,
            &self.settings,
            &self.table,
            &self.spec,
            owner,
            move || async move { wire.open() },
            T,
            None,
        )
        .await
    }
    fn line(&self) -> Arc<Line> {
        self.table.line(&LineKey::of(&self.spec).unwrap()).unwrap()
    }
    fn state(&self) -> SyncState {
        self.line().state()
    }
    fn outstanding(&self) -> Vec<Signature> {
        self.line().outstanding()
    }
}

async fn read_one(session: &mut FrameSession, addr: u8) -> Result<RawValue, FatalError> {
    let values = session.read_raw_batch(&[spec_of(addr)]).await?;
    Ok(values.into_iter().next().unwrap().unwrap())
}

/// §7.3 の反証のテスト: A が時間切れになった後、A の応答を 2 回目の捨てる処理の後に届けても、
/// B の読み取りが A の値を返さない。
#[tokio::test(start_paused = true)]
async fn a_late_response_delivered_during_the_sync_is_never_taken_as_the_next_read() {
    let bench = Bench::new(SyncPolicy::Distinct);
    bench.wire.react([
        Reaction::Reply(vec![response(FC_PROBE, &[0x55])]), // 1: 開いた直後の同期
        Reaction::Silence,                                  // 2: A は時間切れ
    ]);
    let mut session = bench.connect().await.unwrap();
    assert_eq!(bench.state(), SyncState::Synced);
    assert!(matches!(
        read_one(&mut session, ADDR_A).await,
        Err(FatalError::Timeout)
    ));
    assert!(!session.is_connected());
    assert_eq!(bench.state(), SyncState::Unsynced);
    assert_eq!(bench.outstanding(), vec![sig(FC_READ, 1)]);
    assert!(bench.line().is_open(), "致命の後もポートは閉じない");
    drop(session);

    bench.wire.react([
        // 3: 同期の要求。届くのは、知らない局のフレーム → A の古い応答 → 同期の応答の順。
        Reaction::Reply(vec![
            vec![2, FC_READ, 1, 0x00],
            response(FC_READ, &[0xAA]),
            response(FC_PROBE, &[0x55]),
        ]),
        Reaction::Reply(vec![response(FC_READ, &[0xBB])]), // 4: B
    ]);
    let mut session = bench.connect().await.unwrap();
    assert_eq!(bench.state(), SyncState::Synced);
    assert!(
        bench.outstanding().is_empty(),
        "同期した: A は捨てられ、待っている要求は空"
    );
    assert_eq!(
        read_one(&mut session, ADDR_B).await.unwrap(),
        RawValue::Uint(0xBB)
    );
    let sent = bench.wire.sent();
    assert_eq!(
        sent,
        vec![
            request(FC_PROBE, 0x00).send,
            request(FC_READ, ADDR_A).send,
            request(FC_PROBE, 0x00).send,
            request(FC_READ, ADDR_B).send,
        ],
        "同期の応答が届くまで B は送られない"
    );
    assert_eq!(bench.wire.lock().opens, 1, "ポートは 1 度しか開かない");
}

/// 同期の要求に A と同じ署名を選ぶドライバは契約違反（C17）で断られる。
#[tokio::test(start_paused = true)]
async fn a_sync_step_with_an_outstanding_signature_is_refused() {
    let bench = Bench::new(SyncPolicy::Colliding);
    bench.wire.react([
        Reaction::Reply(vec![response(FC_READ, &[0x55])]),
        Reaction::Silence,
    ]);
    let mut session = bench.connect().await.unwrap();
    let _ = read_one(&mut session, ADDR_A).await;
    drop(session);
    bench
        .wire
        .react([Reaction::Reply(vec![response(FC_READ, &[0xAA])])]);
    let result = bench.connect().await;
    assert!(
        matches!(&result, Err(FatalError::Protocol(m)) if m.contains("待っている要求と同じ")),
        "{:?}",
        result.as_ref().err()
    );
    assert_eq!(bench.wire.sent().len(), 2, "同期の要求は送られない");
    assert_eq!(bench.state(), SyncState::Unsynced);
}

/// 同期の step を `Fixed` で区切るドライバは契約違反（C17）で断られる。同期の途中に届く古い
/// 応答も同期の step の形で区切るので、応答の長さが違うと区切りを誤る（Codex P2、2026-10-23）。
#[tokio::test(start_paused = true)]
async fn a_sync_step_delimited_by_a_fixed_length_is_refused() {
    let bench = Bench::new(SyncPolicy::Fixed);
    let result = bench.connect().await;
    assert!(
        matches!(&result, Err(FatalError::Protocol(m)) if m.contains("Fixed")),
        "{:?}",
        result.as_ref().err()
    );
    assert!(bench.wire.sent().is_empty(), "同期の要求は送られない");
}

/// 長さの違う古い応答（2 語・5 バイト）と同期の応答（1 語・4 バイト）が 1 回の recv で連結して
/// 届いても、同期の step（`DriverDecides`）が先頭の古い応答をその長さで区切り、1 回目の同期の
/// 要求で同期する。
#[tokio::test(start_paused = true)]
async fn a_stale_response_of_another_length_joined_with_the_sync_response_syncs_at_once() {
    const ADDR_WIDE: u8 = 0xF1; // 2 語の読み取り（署名 FC3・2 語）
    let bench = Bench::new(SyncPolicy::Distinct);
    bench.wire.react([
        Reaction::Reply(vec![response(FC_PROBE, &[0x55])]), // 1: 開いた直後の同期
        Reaction::Silence,                                  // 2: 2 語の A は時間切れ
    ]);
    let mut session = bench.connect().await.unwrap();
    assert!(matches!(
        read_one(&mut session, ADDR_WIDE).await,
        Err(FatalError::Timeout)
    ));
    assert_eq!(bench.outstanding(), vec![sig(FC_READ, 2)]);
    drop(session);

    let mut joined = response(FC_READ, &[0xAA, 0xAB]);
    joined.extend_from_slice(&response(FC_PROBE, &[0x55]));
    bench.wire.react([
        Reaction::Reply(vec![joined]), // 3: 同期の要求（1 回の recv）
        Reaction::Reply(vec![response(FC_READ, &[0xBB])]), // 4: B
    ]);
    let mut session = bench.connect().await.unwrap();
    assert_eq!(bench.state(), SyncState::Synced);
    assert!(bench.outstanding().is_empty());
    assert_eq!(
        read_one(&mut session, ADDR_B).await.unwrap(),
        RawValue::Uint(0xBB)
    );
    assert_eq!(
        bench.wire.sent(),
        vec![
            request(FC_PROBE, 0x00).send,
            request(FC_READ, ADDR_WIDE).send,
            request(FC_PROBE, 0x00).send,
            request(FC_READ, ADDR_B).send,
        ],
        "同期の要求は 1 回で済む"
    );
}

/// §7.3 の取り消しの版: A を送った後、応答の前に外側の上限で future を drop し、セッションを
/// 捨てて新しいセッションを作る。A と同じ局・同じ FC の旧応答が届いても、同期が取れるまで
/// 値を返さず、書き込みも送らない。
#[tokio::test(start_paused = true)]
async fn a_cancelled_step_leaves_the_line_unsynced_and_the_next_session_syncs_before_writing() {
    let bench = Bench::new(SyncPolicy::Distinct);
    bench.wire.react([
        Reaction::Reply(vec![response(FC_PROBE, &[0x55])]), // 1: 同期
        Reaction::Silence,                                  // 2: A（途中で取り消す）
    ]);
    let mut session = bench.connect().await.unwrap();
    let cancelled = tokio::time::timeout(T / 2, session.read_raw_batch(&[spec_of(ADDR_A)])).await;
    assert!(cancelled.is_err(), "外側の上限で future を drop した");
    assert!(!session.is_connected());
    assert_eq!(
        bench.state(),
        SyncState::Unsynced,
        "番人の drop が未同期にする"
    );
    assert_eq!(
        bench.outstanding(),
        vec![sig(FC_READ, 1)],
        "送る前に記録した署名が残る"
    );
    assert!(bench.line().is_open());
    drop(session);

    bench.wire.react([
        // 3: 1 回目の同期の要求（FC4 1 語）。A の古い応答だけが届き、同期の応答は来ない。
        Reaction::Reply(vec![response(FC_READ, &[0xAA])]),
        // 4: 2 回目の同期の要求は別の署名（FC4 2 語）。応答が届く。
        Reaction::Reply(vec![response(FC_PROBE, &[1, 2])]),
        // 5: 書き込み
        Reaction::Reply(vec![response(0x06, &[0])]),
    ]);
    let mut session = bench.connect().await.unwrap();
    assert_eq!(bench.state(), SyncState::Synced);
    assert!(
        bench.outstanding().is_empty(),
        "同期が通ったので、A も時間切れになった 1 回目の同期の要求も待っていない: {:?}",
        bench.outstanding()
    );
    let results = session
        .write_batch(&[WriteItem::new(spec_of(ADDR_A), Value::F64(1.0))])
        .await
        .unwrap();
    assert_eq!(results, vec![Ok(())]);
    let sent = bench.wire.sent();
    assert_eq!(
        sent,
        vec![
            request(FC_PROBE, 0x00).send,
            request(FC_READ, ADDR_A).send,
            request(FC_PROBE, 0x00).send,
            request(FC_PROBE, 0xF0).send,
            request(0x06, ADDR_A).send,
        ],
        "書き込みは同期の応答の後にだけ送られる"
    );
}

/// 同期の要求が 3 回とも時間切れなら「線の同期が取れない」で止まる。次のダイヤルも同期から始まる。
#[tokio::test(start_paused = true)]
async fn three_silent_sync_attempts_fail_the_dial_and_leave_the_line_unsynced() {
    let bench = Bench::new(SyncPolicy::Distinct);
    bench.wire.react([
        Reaction::Reply(vec![response(FC_PROBE, &[0x55])]),
        Reaction::Silence,
    ]);
    let mut session = bench.connect().await.unwrap();
    let _ = read_one(&mut session, ADDR_A).await;
    drop(session);

    let started = Instant::now();
    let result = bench.connect().await;
    assert!(
        matches!(&result, Err(FatalError::Protocol(m)) if m == "線の同期が取れない"),
        "{:?}",
        result.as_ref().err()
    );
    assert_eq!(started.elapsed(), T * 3, "3 回の時間切れ");
    assert_eq!(
        bench.wire.sent()[2..],
        [
            request(FC_PROBE, 0x00).send,
            request(FC_PROBE, 0xF0).send,
            request(FC_READ, 0xF0).send,
        ],
        "毎回別の署名"
    );
    assert_eq!(bench.state(), SyncState::Unsynced);
    assert_eq!(bench.outstanding().len(), 4);
    assert!(bench.line().is_open(), "同期できなくてもポートは閉じない");

    // 区別できる要求が残っていないので、次のダイヤルは送らずに止まる。
    let result = bench.connect().await;
    assert!(matches!(&result, Err(FatalError::Protocol(m)) if m == "線の同期が取れない"));
    assert_eq!(bench.wire.sent().len(), 5);
}

/// §7.3 の 6: 同期の手順を持たないドライバは、開いたばかりの線はそのまま使えるが、
/// 致命の後はその線を使い直さない（ポートを閉じ、接続を止めて開き直すまで断る）。
#[tokio::test(start_paused = true)]
async fn a_driver_without_sync_step_never_reuses_a_line_after_a_fatal_error() {
    let bench = Bench::new(SyncPolicy::None);
    bench.wire.react([
        Reaction::Reply(vec![response(FC_READ, &[0x11])]), // 1: A（同期の要求は無い）
        Reaction::Silence,                                 // 2: B は時間切れ
    ]);
    let mut session = bench.connect().await.unwrap();
    assert_eq!(
        read_one(&mut session, ADDR_A).await.unwrap(),
        RawValue::Uint(0x11)
    );
    assert_eq!(bench.state(), SyncState::Synced);
    assert!(
        bench.outstanding().is_empty(),
        "署名の無いドライバは署名を残さない"
    );
    assert!(matches!(
        read_one(&mut session, ADDR_B).await,
        Err(FatalError::Timeout)
    ));
    assert_eq!(bench.state(), SyncState::Unsynced);
    drop(session);

    for _ in 0..2 {
        let result = bench.connect().await;
        assert!(
            matches!(&result, Err(FatalError::InvalidConfig(m)) if m.contains("使い直せません")),
            "{:?}",
            result.as_ref().err()
        );
    }
    {
        let state = bench.wire.lock();
        assert_eq!(
            (state.opens, state.closes),
            (1, 1),
            "ポートを閉じ、開き直さない"
        );
        assert_eq!(state.sent.len(), 2);
    }
    assert!(!bench.line().is_open());

    // 接続を止めて開き直すと、新しい線として使える。
    assert!(bench.table.close(&LineKey::of(&bench.spec).unwrap()));
    bench
        .wire
        .react([Reaction::Reply(vec![response(FC_READ, &[0x22])])]);
    let mut session = bench.connect().await.unwrap();
    assert_eq!(
        read_one(&mut session, ADDR_A).await.unwrap(),
        RawValue::Uint(0x22)
    );
    assert_eq!(bench.wire.lock().opens, 2);
}

/// 切断（broker が接続を止めた）でポートが閉じ、線は開いた直後の状態に戻る。
#[tokio::test(start_paused = true)]
async fn disconnect_closes_the_port_and_forgets_the_outstanding_signatures() {
    let bench = Bench::new(SyncPolicy::Distinct);
    bench
        .wire
        .react([Reaction::Reply(vec![response(FC_PROBE, &[0x55])])]);
    let mut session = bench.connect().await.unwrap();
    assert!(bench.line().is_open());
    session.disconnect().await;
    {
        let state = bench.wire.lock();
        assert_eq!((state.opens, state.closes), (1, 1));
    }
    assert!(!bench.line().is_open());
    assert_eq!(bench.state(), SyncState::Fresh);
    assert!(bench.outstanding().is_empty());
    assert_eq!(bench.table.open_count(), 0);

    // 開き直すと新しいポートを開き、同期から始める。
    bench
        .wire
        .react([Reaction::Reply(vec![response(FC_PROBE, &[0x55])])]);
    let session = bench.connect().await.unwrap();
    assert_eq!(bench.wire.lock().opens, 2);
    assert_eq!(bench.state(), SyncState::Synced);
    drop(session);
}

/// `Io` の致命は通信路が壊れているのでポートを閉じるが、線の状態と署名は残る。
/// 次のセッションは開き直して同期する。開けないときは借りたままにしない。
#[tokio::test(start_paused = true)]
async fn an_io_failure_closes_the_port_but_keeps_the_line_state() {
    let bench = Bench::new(SyncPolicy::Distinct);
    bench
        .wire
        .react([Reaction::Reply(vec![response(FC_PROBE, &[0x55])])]);
    let mut session = bench.connect().await.unwrap();
    {
        let mut state = bench.wire.lock();
        state.script.push_back(Reaction::Reply(vec![Vec::new()])); // 0 バイト = 切断
    }
    assert!(matches!(
        read_one(&mut session, ADDR_A).await,
        Err(FatalError::Io(_))
    ));
    assert_eq!(bench.wire.lock().closes, 1);
    assert!(!bench.line().is_open());
    assert_eq!(bench.state(), SyncState::Unsynced);
    assert_eq!(bench.outstanding(), vec![sig(FC_READ, 1)]);
    drop(session);

    // 2 回目の同期（1 語の FC4 は待っている要求に無いので使う）。
    bench
        .wire
        .react([Reaction::Reply(vec![response(FC_PROBE, &[0x55])])]);
    let session = bench.connect().await.unwrap();
    assert_eq!(bench.wire.lock().opens, 2);
    assert_eq!(bench.state(), SyncState::Synced);
    drop(session);
}

/// 線は 1 度に 1 つにしか貸さない。開けなければ予約を戻す。
#[tokio::test(start_paused = true)]
async fn a_line_is_leased_to_one_session_at_a_time_and_a_failed_open_releases_it() {
    let bench = Bench::new(SyncPolicy::Distinct);
    let failing = FrameSession::with_line(
        &bench.driver,
        &bench.settings,
        &bench.table,
        &bench.spec,
        None,
        || async { Err(FatalError::Io("開けない".into())) },
        T,
        None,
    )
    .await;
    assert!(matches!(failing, Err(FatalError::Io(_))));
    assert!(!bench.line().is_open());

    bench
        .wire
        .react([Reaction::Reply(vec![response(FC_PROBE, &[0x55])])]);
    let first = bench.connect().await.unwrap();
    let second = bench.connect().await;
    assert!(
        matches!(&second, Err(FatalError::Protocol(m)) if m.contains("使用中")),
        "{:?}",
        second.as_ref().err()
    );
    assert_eq!(bench.wire.lock().opens, 1);
    drop(first);
    assert!(bench.line().is_open(), "捨てたセッションのポートは表に戻る");
    assert_eq!(
        bench.state(),
        SyncState::Synced,
        "正常に完結したまま捨てたので同期したまま"
    );
    let third = bench.connect().await.unwrap();
    assert_eq!(
        bench.wire.lock().sent.len(),
        1,
        "同期したままの線は同期の要求を送らない"
    );
    drop(third);

    // TCP は線の表を通らない。
    let tcp = TransportSpec::Tcp {
        host: "mem".into(),
        port: 1,
    };
    let result = FrameSession::with_line(
        &bench.driver,
        &bench.settings,
        &bench.table,
        &tcp,
        None,
        || async { unreachable!("TCP では開かない") },
        T,
        None,
    )
    .await;
    assert!(matches!(result, Err(FatalError::InvalidConfig(_))));
}

/// 開く待ちの最中に貸し出しの future が drop されても（connect の外側の上限での abort など）、
/// 予約は戻り、同じ鍵で借り直せる（「使用中」で断られ続けない）。
#[tokio::test(start_paused = true)]
async fn a_lease_cancelled_while_opening_releases_the_reservation() {
    let bench = Bench::new(SyncPolicy::Distinct);
    let stuck = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        bench.table.lease(&bench.spec, None, || {
            std::future::pending::<Result<Box<dyn Transport>, FatalError>>()
        }),
    )
    .await;
    assert!(stuck.is_err(), "開く待ちは終わらないので時間切れになる");
    assert!(!bench.line().is_open(), "取り消された予約は戻る");

    let wire = bench.wire.clone();
    let leased = bench
        .table
        .lease(&bench.spec, None, || async move { wire.open() })
        .await;
    assert!(
        leased.is_ok(),
        "借り直せる: {:?}",
        leased.as_ref().err().map(|e| e.to_string())
    );
    assert_eq!(bench.wire.lock().opens, 1);
}

/// 同期した後に別の要求への応答（局番・FC が合わない）が来たら Framing で未同期に戻る。
#[tokio::test(start_paused = true)]
async fn a_response_to_a_different_request_is_framing_and_unsyncs_the_line() {
    let bench = Bench::new(SyncPolicy::Distinct);
    bench.wire.react([
        Reaction::Reply(vec![response(FC_PROBE, &[0x55])]),
        Reaction::Reply(vec![response(FC_PROBE, &[0x55])]), // A への応答が FC4 で来る
    ]);
    let mut session = bench.connect().await.unwrap();
    let result = read_one(&mut session, ADDR_A).await;
    assert!(
        matches!(result, Err(FatalError::Framing(_))),
        "{:?}",
        result.as_ref().err()
    );
    assert_eq!(bench.state(), SyncState::Unsynced);
    assert_eq!(bench.outstanding(), vec![sig(FC_READ, 1)]);
}

/// 反証（監査 P1）: 前のセッションが送る前の致命（`plan_read` の失敗）で線を `Synced` のまま
/// 返した後に借りたセッションも、step の署名を線に記録する。そこで A が時間切れになったら、
/// 次のセッションは A の署名を待っている要求として持ち、A と同じ署名の同期の要求を断る。
/// `sync_line` が `Synced` の早い return を `line_sync` の判定より前に置くと落ちる。
#[tokio::test(start_paused = true)]
async fn a_session_leasing_a_synced_line_still_records_the_signatures_of_its_steps() {
    let bench = Bench::new(SyncPolicy::Colliding);
    bench
        .wire
        .react([Reaction::Reply(vec![response(FC_READ, &[0x55])])]); // 1: 開いた直後の同期
    let mut session = bench.connect().await.unwrap();
    let result = session.read_raw_batch(&[spec_of_text(BAD_ADDRESS)]).await;
    assert!(matches!(result, Err(FatalError::Protocol(_))), "{result:?}");
    assert!(!session.is_connected());
    drop(session);
    assert_eq!(
        bench.state(),
        SyncState::Synced,
        "送る前の致命は線を同期したまま返す"
    );
    assert!(bench.outstanding().is_empty());

    // 同期したままの線を借りる（同期の要求は送らない）。A は時間切れ。
    bench.wire.react([Reaction::Silence]); // 2: A
    let mut session = bench.connect().await.unwrap();
    assert_eq!(
        bench.wire.sent().len(),
        1,
        "同期したままの線は同期の要求を送らない"
    );
    assert!(matches!(
        read_one(&mut session, ADDR_A).await,
        Err(FatalError::Timeout)
    ));
    drop(session);
    assert_eq!(bench.state(), SyncState::Unsynced);
    assert_eq!(
        bench.outstanding(),
        vec![sig(FC_READ, 1)],
        "A の署名が待っている要求に残る"
    );

    // 次のセッション: A と同じ署名の同期の要求は断られ、A の古い応答を同期の応答と取り違えない。
    bench
        .wire
        .react([Reaction::Reply(vec![response(FC_READ, &[0xAA])])]);
    let result = bench.connect().await;
    assert!(
        matches!(&result, Err(FatalError::Protocol(m)) if m.contains("待っている要求と同じ")),
        "{:?}",
        result.as_ref().err()
    );
    assert_eq!(bench.wire.sent().len(), 2, "同期の要求は送られない");
    assert_eq!(bench.state(), SyncState::Unsynced);
}

/// 監査 P2: 同期が通ったら待っている署名の集合を空にする（§7.3 の 2: 古い要求の応答が来るなら
/// 同期の応答より前に来ている）。時間切れと同期を繰り返しても集合が増え続けない。
#[tokio::test(start_paused = true)]
async fn a_successful_sync_clears_every_outstanding_signature() {
    let bench = Bench::new(SyncPolicy::Distinct);
    bench
        .wire
        .react([Reaction::Reply(vec![response(FC_PROBE, &[0x55])])]); // 開いた直後の同期
    let session = bench.connect().await.unwrap();
    drop(session);
    for round in 1..=3 {
        bench.wire.react([
            Reaction::Silence,                                  // A は時間切れ（応答は来ない）
            Reaction::Reply(vec![response(FC_PROBE, &[0x55])]), // 同期の要求（FC4 の 1 語）
        ]);
        let mut session = bench.connect().await.unwrap();
        assert!(matches!(
            read_one(&mut session, ADDR_A).await,
            Err(FatalError::Timeout)
        ));
        drop(session);
        assert_eq!(bench.outstanding(), vec![sig(FC_READ, 1)], "round {round}");
        let session = bench.connect().await.unwrap();
        assert_eq!(bench.state(), SyncState::Synced, "round {round}");
        assert!(
            bench.outstanding().is_empty(),
            "round {round}: 同期が通ったら待っている署名は残らない: {:?}",
            bench.outstanding()
        );
        drop(session);
    }
    let sent = bench.wire.sent();
    assert_eq!(sent.len(), 1 + 3 * 2);
    assert!(
        sent[1..]
            .chunks(2)
            .all(|pair| pair == [request(FC_READ, ADDR_A).send, request(FC_PROBE, 0x00).send]),
        "毎回 FC4 の 1 語で同期できる（集合が増えないので別の署名に逃げない）: {sent:?}"
    );
}

/// 監査 P1（broker の解放の競合）: 線は最後に借りた持ち主の印を覚え、`close_by` は印が合う
/// ときだけ閉じる。古い持ち主の解放は、新しい持ち主が借りた線（状態・署名）を消さない。
#[tokio::test(start_paused = true)]
async fn close_by_closes_only_the_line_its_owner_leased_last() {
    let bench = Bench::new(SyncPolicy::Distinct);
    let key = LineKey::of(&bench.spec).unwrap();
    let (old, new) = (LineOwner::new(), LineOwner::new());
    bench.wire.react([
        Reaction::Reply(vec![response(FC_PROBE, &[0x55])]), // 古い持ち主の同期
        Reaction::Silence,                                  // A は時間切れ → 線は Unsynced
        Reaction::Reply(vec![response(FC_PROBE, &[0x55])]), // 新しい持ち主の同期
    ]);
    let mut session = bench.connect_as(Some(old)).await.unwrap();
    let _ = read_one(&mut session, ADDR_A).await;
    drop(session);
    let line = bench.line();
    assert_eq!(line.state(), SyncState::Unsynced);

    // 新しい持ち主が借りて同期した後、古い持ち主の task が終わる。
    let session = bench.connect_as(Some(new)).await.unwrap();
    assert!(
        !bench.table.close_by(&key, old),
        "古い持ち主の解放は閉じない"
    );
    assert!(
        bench
            .table
            .line(&key)
            .is_some_and(|l| Arc::ptr_eq(&l, &line)),
        "新しい持ち主の線は表に残る"
    );
    assert_eq!(bench.state(), SyncState::Synced);
    assert!(bench.line().is_open());
    drop(session);
    assert!(bench.line().is_open(), "貸し出しを返しても開いたまま");
    assert_eq!(bench.wire.lock().closes, 0);

    // 新しい持ち主の解放は閉じる。
    assert!(bench.table.close_by(&key, new));
    assert!(bench.table.line(&key).is_none());
    assert_eq!(bench.wire.lock().closes, 1);

    // 印を付けずに借りた線（試験）は、どの印の解放でも閉じる。
    bench
        .wire
        .react([Reaction::Reply(vec![response(FC_PROBE, &[0x55])])]);
    drop(bench.connect().await.unwrap());
    assert!(bench.table.close_by(&key, old));
    assert!(bench.table.line(&key).is_none());
}

/// #801: 1 回きりのダイヤル（Hub の接続テスト）は、broker の task が致命の後のバックオフの間に
/// 表へ返した線（貸し出し中でない・印は task・`Unsynced` で待っている署名あり）を借りずに
/// 「使用中」で断る。線の印・状態・署名は変わらず表に残り、1 回きりの解放はそれを閉じない。
#[tokio::test(start_paused = true)]
async fn a_one_shot_dial_never_takes_over_a_line_another_owner_returned() {
    let bench = Bench::new(SyncPolicy::Distinct);
    let key = LineKey::of(&bench.spec).unwrap();
    let task = LineOwner::new();
    bench.wire.react([
        Reaction::Reply(vec![response(FC_PROBE, &[0x55])]), // task の同期
        Reaction::Silence,                                  // A は時間切れ → 線は Unsynced
    ]);
    let mut session = bench.connect_as(Some(task)).await.unwrap();
    let _ = read_one(&mut session, ADDR_A).await;
    drop(session); // 致命の後、線は開いたまま表へ戻る（task はバックオフ中）。
    let line = bench.line();
    assert_eq!(line.state(), SyncState::Unsynced);
    let outstanding = line.outstanding();
    assert!(!outstanding.is_empty(), "待っている署名がある");
    let sent = bench.wire.lock().sent.len();

    let one_shot = LineOwner::one_shot();
    let Err(err) = bench.connect_as(Some(one_shot)).await else {
        panic!("1 回きりのダイヤルは別の持ち主の線を借りない");
    };
    assert!(
        matches!(&err, FatalError::Protocol(m) if m.contains("使用中")),
        "{err:?}"
    );
    assert_eq!(bench.wire.lock().sent.len(), sent, "何も送らない");
    assert!(
        !bench.table.close_by(&key, one_shot),
        "1 回きりの解放は task の線を閉じない"
    );
    assert!(
        bench
            .table
            .line(&key)
            .is_some_and(|l| Arc::ptr_eq(&l, &line)),
        "task の線は表に残る"
    );
    assert!(line.is_open());
    assert_eq!(line.state(), SyncState::Unsynced);
    assert_eq!(line.outstanding(), outstanding, "署名は変わらない");
    assert_eq!(bench.wire.lock().closes, 0);

    // 印は task のまま: task の解放が閉じる。
    assert!(bench.table.close_by(&key, task));
    assert!(bench.table.line(&key).is_none());
}

/// オーナーのレビュー P2（`lease` と `close_by` の競合）: 旧 task が線を返した後、新 task の
/// `lease` が表のロックを取る直前に旧 task の `close_by` を挟む。取り出しと予約は表のロックの
/// 中で一体なので、新 task は表から外れた線を借りない（新しい線が作られ、表に入る）。その
/// セッションが時間切れになると、署名は表の線に残り、セッションを捨てても次のダイヤルに届く。
#[tokio::test(start_paused = true)]
async fn a_close_by_racing_a_lease_never_leaves_the_new_session_on_a_line_outside_the_table() {
    let bench = Bench::new(SyncPolicy::Distinct);
    let key = LineKey::of(&bench.spec).unwrap();
    let (old, new) = (LineOwner::new(), LineOwner::new());
    bench.wire.react([
        Reaction::Reply(vec![response(FC_PROBE, &[0x55])]), // 旧 task の同期
    ]);
    // 1. 旧セッションが線を返す（`leased = false`、印は旧 task）。
    drop(bench.connect_as(Some(old)).await.unwrap());
    let old_line = bench.line();

    // 2〜3. 新 task の `lease` が表のロックを取る直前に、旧 task の `close_by` が線を外す。
    let hook_key = key.clone();
    *bench.table.before_lease_lock.lock().unwrap() = Some(Box::new(move |table| {
        assert!(table.close_by(&hook_key, old), "旧 task の解放が線を外す");
    }));
    bench.wire.react([
        Reaction::Reply(vec![response(FC_PROBE, &[0x55])]), // 新 task の同期（開き直したので Fresh）
        Reaction::Silence,                                  // A は時間切れ
    ]);
    let mut session = bench.connect_as(Some(new)).await.unwrap();
    assert!(
        bench.table.before_lease_lock.lock().unwrap().is_none(),
        "差し込み口が呼ばれた"
    );
    let line = bench
        .table
        .line(&key)
        .expect("新 task が借りた線は表に入っている");
    assert!(!Arc::ptr_eq(&line, &old_line), "外れた古い線ではない");
    assert!(line.is_open());

    // 4. 時間切れ。署名は表の線に残り、セッションを捨てても失われない。
    assert!(read_one(&mut session, ADDR_A).await.is_err());
    drop(session);
    let after = bench.table.line(&key).expect("線は表に残る");
    assert!(Arc::ptr_eq(&after, &line), "借りた線と表の線は同じ");
    assert_eq!(after.state(), SyncState::Unsynced);
    assert_eq!(after.outstanding(), vec![sig(FC_READ, 1)]);
}

/// Codex P1: 古い応答と同期の応答が 1 回の recv でまとめて届く（`SerialOverTcp` でよくある）。
/// 古い応答を捨てた後に `buf` に残った同期の応答を、次の recv を待つ前に調べ、1 回目の試行で同期する。
#[tokio::test(start_paused = true)]
async fn a_sync_response_received_together_with_a_stale_one_syncs_on_the_first_attempt() {
    let bench = Bench::new(SyncPolicy::Distinct);
    bench.wire.react([
        Reaction::Reply(vec![response(FC_PROBE, &[0x55])]), // 1: 開いた直後の同期
        Reaction::Silence,                                  // 2: A は時間切れ
    ]);
    let mut session = bench.connect().await.unwrap();
    assert!(matches!(
        read_one(&mut session, ADDR_A).await,
        Err(FatalError::Timeout)
    ));
    drop(session);
    assert_eq!(bench.outstanding(), vec![sig(FC_READ, 1)]);

    // 3: 同期の要求。A の古い応答と同期の応答を 1 つのまとまりで届け、その後は何も来ない。
    let mut joined = response(FC_READ, &[0xAA]);
    joined.extend(response(FC_PROBE, &[0x55]));
    bench.wire.react([Reaction::Reply(vec![joined])]);
    let _session = bench.connect().await.unwrap();
    assert_eq!(bench.state(), SyncState::Synced);
    assert!(
        bench.outstanding().is_empty(),
        "同期した: 待っている署名は空"
    );
    assert_eq!(bench.wire.sent().len(), 3, "同期の要求は 1 回だけ");
}

fn serial_spec(baud: u32) -> TransportSpec {
    TransportSpec::Serial {
        port: crate::transport::SerialPortName::parse("COM7").unwrap(),
        line: crate::transport::SerialLine {
            baud,
            ..Default::default()
        },
    }
}

/// Codex P1: 線の設定（baud）が変わって開き直したポートは `Synced` を引き継がず、同期から始める。
#[tokio::test(start_paused = true)]
async fn a_synced_line_reopened_with_a_new_baud_starts_fresh_and_syncs_again() {
    let mut bench = Bench::new(SyncPolicy::Distinct);
    bench.spec = serial_spec(9600);
    bench.wire.react([
        Reaction::Reply(vec![response(FC_PROBE, &[0x55])]), // 1: 開いた直後の同期
        Reaction::Reply(vec![response(FC_READ, &[0x11])]),  // 2: A
    ]);
    let mut session = bench.connect().await.unwrap();
    assert_eq!(
        read_one(&mut session, ADDR_A).await.unwrap(),
        RawValue::Uint(0x11)
    );
    drop(session);
    assert_eq!(bench.state(), SyncState::Synced);

    bench.spec = serial_spec(19200);
    let wire = bench.wire.clone();
    let (lease, start) = bench
        .table
        .lease(&bench.spec, None, move || async move { wire.open() })
        .await
        .unwrap();
    assert!(
        matches!(start, LeaseStart::Fresh),
        "開き直したポートは Fresh"
    );
    drop(lease);

    bench.spec = serial_spec(9600);
    bench
        .wire
        .react([Reaction::Reply(vec![response(FC_PROBE, &[0x55])])]); // 3: 同期
    let _session = bench.connect().await.unwrap();
    assert_eq!(bench.state(), SyncState::Synced);
    assert_eq!(bench.wire.lock().opens, 3, "設定が変わるたびに開き直す");
    assert_eq!(
        bench.wire.sent(),
        vec![
            request(FC_PROBE, 0x00).send,
            request(FC_READ, ADDR_A).send,
            request(FC_PROBE, 0x00).send,
        ],
        "開き直した後は同期の要求から送る"
    );
}

/// 線の設定が変わって開き直しても、未同期と待っている署名は残す（機器と線は同じなので、
/// 古い要求の応答がまだ届きうる）。
#[tokio::test(start_paused = true)]
async fn an_unsynced_line_reopened_with_a_new_baud_keeps_its_outstanding_signatures() {
    let mut bench = Bench::new(SyncPolicy::Distinct);
    bench.spec = serial_spec(9600);
    bench.wire.react([
        Reaction::Reply(vec![response(FC_PROBE, &[0x55])]), // 1: 開いた直後の同期
        Reaction::Silence,                                  // 2: A は時間切れ
    ]);
    let mut session = bench.connect().await.unwrap();
    assert!(matches!(
        read_one(&mut session, ADDR_A).await,
        Err(FatalError::Timeout)
    ));
    drop(session);
    assert_eq!(bench.state(), SyncState::Unsynced);

    bench.spec = serial_spec(19200);
    let wire = bench.wire.clone();
    let (lease, start) = bench
        .table
        .lease(&bench.spec, None, move || async move { wire.open() })
        .await
        .unwrap();
    assert!(matches!(start, LeaseStart::Unsynced), "未同期のまま");
    drop(lease);
    assert_eq!(bench.wire.lock().opens, 2, "設定が変わったので開き直した");
    assert_eq!(bench.state(), SyncState::Unsynced);
    assert_eq!(bench.outstanding(), vec![sig(FC_READ, 1)], "署名は残る");
}

/// 応答を待たない書き込みだけを送るベンチ（同期の手順を持たないドライバ。線は `Fresh` で始まる）。
async fn no_reply_bench() -> (Bench, FrameSession) {
    let mut bench = Bench::new(SyncPolicy::None);
    bench.driver.conn.no_reply_write = true;
    let session = bench.connect().await.unwrap();
    assert_eq!(bench.state(), SyncState::Fresh);
    (bench, session)
}

fn write_a() -> Vec<WriteItem> {
    vec![WriteItem::new(spec_of(ADDR_A), Value::F64(1.0))]
}

/// §7.3 の 7（Codex P1）: 応答を待たない step でも、送信が `Io` で失敗したら線は未同期になる。
/// 機器は崩れたフレームを受けたかもしれないので、次のセッションが線をそのまま使い直さない。
#[tokio::test(start_paused = true)]
async fn a_failed_send_of_a_step_without_response_leaves_the_line_unsynced() {
    let (bench, mut session) = no_reply_bench().await;
    bench.wire.react([Reaction::SendFails]);
    let result = session.write_batch(&write_a()).await;
    assert!(matches!(result, Err(FatalError::Io(_))), "{result:?}");
    assert_eq!(bench.state(), SyncState::Unsynced);
    assert!(
        bench.outstanding().is_empty(),
        "応答を待たない step は署名を残さない"
    );
}

/// §7.3 の 7（Codex P1）: 応答を待たない step の送信の途中で future が drop されても、線は未同期になる。
#[tokio::test(start_paused = true)]
async fn a_send_of_a_step_without_response_cancelled_midway_leaves_the_line_unsynced() {
    let (bench, mut session) = no_reply_bench().await;
    bench.wire.react([Reaction::SendStalls]);
    let cancelled = tokio::time::timeout(T / 2, session.write_batch(&write_a())).await;
    assert!(cancelled.is_err(), "外側の上限で future を drop した");
    assert_eq!(bench.wire.sent().len(), 1, "送信は始まっていた");
    assert_eq!(
        bench.state(),
        SyncState::Unsynced,
        "番人の drop が未同期にする"
    );
    assert!(bench.outstanding().is_empty());
}

/// 応答を待たない step を送り終えても、線の状態は変えない（応答の検証が無いので `Synced` にしない）。
#[tokio::test(start_paused = true)]
async fn a_completed_send_of_a_step_without_response_keeps_the_line_state() {
    let (bench, mut session) = no_reply_bench().await;
    let results = session.write_batch(&write_a()).await.unwrap();
    assert_eq!(results, vec![Ok(())]);
    assert_eq!(bench.wire.sent(), vec![request(0x06, ADDR_A).send]);
    assert_eq!(bench.state(), SyncState::Fresh, "送る前と同じ");
    assert!(bench.outstanding().is_empty());
}

const GAP: Duration = Duration::from_millis(10);

/// §7.3.1（Codex P2）: 致命（`Framing`）の直後に、時計を進めずに次のセッションが同じ線を借りても、
/// 最初の同期の要求は前のセッションの最後の受信から `inter_frame_gap` 以上空けて送る
/// （借りた直後のセッションが無音の起点を線の記録から始める）。
#[tokio::test(start_paused = true)]
async fn the_first_sync_request_after_a_fatal_error_keeps_the_inter_frame_gap_of_the_line() {
    let mut bench = Bench::new(SyncPolicy::Distinct);
    bench.driver.conn.gap = GAP;
    bench.wire.react([
        Reaction::Reply(vec![response(FC_PROBE, &[0x55])]), // 1: 同期
        Reaction::Reply(vec![response(FC_PROBE, &[0x55])]), // 2: A への応答が FC4（Framing）
        Reaction::Reply(vec![response(FC_PROBE, &[0x66])]), // 3: 次のセッションの同期
    ]);
    let mut session = bench.connect().await.unwrap();
    let result = read_one(&mut session, ADDR_A).await;
    assert!(matches!(result, Err(FatalError::Framing(_))), "{result:?}");
    // 記憶上の線は send の中で応答を積み、recv はすぐ返す: 最後の受信は A を送った時刻。
    let last_received = Instant::now();
    drop(session);

    let _session = bench.connect().await.unwrap();
    assert_eq!(bench.state(), SyncState::Synced);
    let times = bench.wire.lock().sent_times.clone();
    assert_eq!(times.len(), 3);
    assert!(
        times[2] >= last_received + GAP,
        "同期の要求が前の受信から {:?} しか空いていない",
        times[2] - last_received
    );
}

/// §7.3.1（Codex P2）: 送信の途中で取り消したときも、次のセッションの最初の送信は、取り消した
/// 送信の送り終えの見積もり（送信の開始 + `tx_time`）+ `inter_frame_gap` 以上空ける。
#[tokio::test(start_paused = true)]
async fn the_first_send_after_a_send_cancelled_midway_waits_for_its_estimated_end_and_the_gap() {
    let mut bench = Bench::new(SyncPolicy::Distinct);
    bench.driver.conn.gap = GAP;
    // 3 バイトで 60 ms。A の送信は同期の送り終え（60 ms）+ GAP の 70 ms に始まり、130 ms に送り終える。
    bench.wire.lock().tx_per_byte = Duration::from_millis(20);
    bench.wire.react([
        Reaction::Reply(vec![response(FC_PROBE, &[0x55])]), // 1: 同期
        Reaction::SendStalls,                               // 2: A（送信の途中で取り消す）
        Reaction::Reply(vec![response(FC_PROBE, &[0x66])]), // 3: 次のセッションの同期
    ]);
    let mut session = bench.connect().await.unwrap();
    let cancelled = tokio::time::timeout(
        Duration::from_millis(80),
        session.read_raw_batch(&[spec_of(ADDR_A)]),
    )
    .await;
    assert!(cancelled.is_err(), "外側の上限で future を drop した");
    drop(session);
    let (stalled_at, tx) = {
        let wire = bench.wire.lock();
        assert_eq!(wire.sent.len(), 2, "A の送信は始まっていた");
        (wire.sent_times[1], wire.tx_per_byte * 3)
    };
    assert!(
        Instant::now() < stalled_at + tx,
        "送り終えの見積もりの前に取り消した"
    );

    let _session = bench.connect().await.unwrap();
    let times = bench.wire.lock().sent_times.clone();
    assert_eq!(times.len(), 3);
    assert!(
        times[2] >= stalled_at + tx + GAP,
        "取り消した送信の送り終えの見積もりから {:?} しか空いていない",
        times[2].saturating_duration_since(stalled_at + tx)
    );
}
