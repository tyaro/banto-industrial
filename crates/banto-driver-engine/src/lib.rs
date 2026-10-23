//! フレーム型のドライバを、期限つきの Session として動かす実行系（設計 §5）。
//!
//! 接続と手順の途中の状態は 1 個の Core が所有する。操作中は Session から取り出し、
//! 成功した時だけ戻すため、外側の future のキャンセルでも接続を再利用しない。
//! broker は Frame をこの Session で実行する。接続の外側期限は TCP 接続 +
//! 32 step × response_timeout + CLOSE_TIMEOUT + ゆとりを含む。
//! シリアル（`Serial`・`SerialOverTcp`）は線の表（[`line`]、設計 §7.3）を通す: ポートは
//! セッションではなく表が持ち、致命の後も閉じない。新しいセッションは同じポートを借りて、
//! ハンドシェイクの前に同期の手順（`sync_line`）を走らせる。step は送る前に署名を線に記録し、
//! 完結して検証の通った応答でだけ外す（[`line::InFlightGuard`]）。TCP は表を通らず、
//! 振る舞いは変わらない。
//! 各 step の送信の前に、ドライバの `inter_frame_gap`（Modbus RTU の t3.5）だけ、
//! 前のフレームの最後のバイト（送り終え・受け終えの遅い方）から無音を空ける（§7.3.1）。
//! シリアルの send は flush（Unix では同期の tcdrain で runtime を止める）を呼ばないので、
//! 送り終えた時刻は `Transport::tx_time` で見積もる。
//! 公開する crate は API → engine → conformance の順に公開する。

pub mod decode;
pub mod line;
pub mod transport;

use std::sync::Arc;
use std::time::Duration;

use banto_driver_api::{
    BoxFuture, ConnectContext, FatalError, FrameCheck, FrameConnection, FrameDriver, FrameRule,
    ItemError, PrefixEncoding, PrefixSpec, RawValue, Session, Settings, Signature, Step, TagSpec,
    Value, WriteItem, WriteProgress,
};
use line::{LeaseStart, Line, LineKey, LineLease, LineOwner, LineTable};
use tokio::time::{sleep_until, timeout, timeout_at, Instant};
use transport::{Transport, TransportSpec};

pub const MAX_HANDSHAKE_STEPS: usize = 32;
/// 同期の手順（設計 §7.3 の 2）の試行の上限。時間切れのたびに別の署名で選び直す。
pub const MAX_SYNC_ATTEMPTS: usize = 3;
/// broker の停止の上限（2 秒）より短い、致命時の後始末の上限。
pub const CLOSE_TIMEOUT: Duration = Duration::from_millis(100);
/// `inter_frame_gap` の上限。これを超える値はドライバの契約違反として、送信の前に
/// `FatalError::Protocol` で断る（縮めない。API は「エンジンが守る最小の無音」と定めている）。
/// 待ちは step の応答の期限 T の中に入る（T がこれより短ければ、待ちの途中で期限が来て `Timeout` になる）。
/// 300 baud 8N1 の t3.5 でも約 117 ms。
pub const MAX_INTER_FRAME_GAP: Duration = Duration::from_millis(500);
/// 同期の手順（設計 §7.3 の 2）の途中で、1 つのフレームを区切るときの長さの上限（バイト）。
/// 同期の途中に届く古い応答は同期の step の形で区切るが、その長さは同期の step の `max_len`
/// に縛らない（古い応答が同期の応答より長くても、区切れた長さで捨てて後ろのバイトを残す）。
/// バッファが際限なく育たないよう、`max(同期の step の max_len, これ)` で止め、超えたら捨てる。
/// 64 KiB は、シリアルの線に流れるどの応答（Modbus RTU の ADU は 256 バイト、SLMP も数 KiB）
/// よりも十分に大きく、受信の溜めとしては小さい。エンジンに既存の上限は無いので明示の値を置く。
pub const MAX_SYNC_FRAME_LEN: u32 = 64 * 1024;

/// 適合テストとホストの診断で観測する出来事。通信の結果は変えない。
#[derive(Debug, Clone)]
pub enum EngineEvent {
    Violation {
        check: &'static str,
        detail: String,
    },
    DriverFatal {
        method: &'static str,
        error: FatalError,
    },
    StepStarted(Step),
    WriteStep(Vec<u32>),
}

/// 観測の実装は待機せずに返すこと。未指定なら契約違反だけをログに残す。
/// `StepStarted` の送信バイトにはハンドシェイクの認証情報などの秘密値が含まれうるため、
/// 観測側は送信バイトやイベント全体の Debug 表示をログに残してはならない。
pub trait EngineEvents: Send + Sync + 'static {
    fn record(&self, event: EngineEvent);
}

/// 通信路。TCP はセッションが所有し、シリアルは線の表から借りる（設計 §7.3 の 4）。
enum Wire {
    Direct(Box<dyn Transport>),
    Leased(LineLease),
}

impl Wire {
    fn transport(&mut self) -> &mut dyn Transport {
        match self {
            Self::Direct(t) => t.as_mut(),
            Self::Leased(l) => l,
        }
    }

    fn line(&self) -> Option<&Arc<Line>> {
        match self {
            Self::Direct(_) => None,
            Self::Leased(l) => Some(l.line()),
        }
    }
}

struct Core {
    conn: Box<dyn FrameConnection>,
    wire: Wire,
    /// シリアルの線で、ドライバが同期の手順（`sync_step`）を持つ。step の署名を求める。
    line_sync: bool,
    response_timeout: Duration,
    can_write: bool,
    events: Option<Arc<dyn EngineEvents>>,
    /// 最後に送り終えた・受け終えた時刻の遅い方（フレーム間の無音の起点）。シリアルの線を
    /// 借りたセッションは線の記録（`Line::quiet_from`）から始め、[`Core::note_quiet`] で
    /// 両方を同じに保つ（貸し出し中は線をこのセッションだけが使うので、二重でも食い違わない。
    /// TCP は線が無いのでこちらだけ）。
    last_quiet_from: Option<Instant>,
}

/// 前のフレームの最後のバイトの時刻 `from` から `gap` が過ぎるまで待つ（`gap` は呼び手が
/// `MAX_INTER_FRAME_GAP` 以下を確かめ済み）。`gap` が 0 か `from` が無いなら待たない。
async fn wait_quiet(from: Option<Instant>, gap: Duration) {
    if let (Some(from), false) = (from, gap.is_zero()) {
        tokio::time::sleep_until(from + gap).await;
    }
}

/// 1 接続につき 1 個だけを作り、読み書きを直列に呼ぶ。
pub struct FrameSession {
    core: Option<Core>,
}

impl FrameSession {
    /// トランスポートを開き、プロトコルのハンドシェイクを完了する。
    pub async fn connect(
        driver: &dyn FrameDriver,
        settings: &Settings,
        spec: &TransportSpec,
        cx: &ConnectContext,
        events: Option<Arc<dyn EngineEvents>>,
    ) -> Result<Self, FatalError> {
        Self::connect_as(driver, settings, spec, cx, events, None).await
    }

    /// [`connect`](Self::connect) に、シリアルの線に記録する持ち主の印を添える（broker の
    /// 接続の task ごとに 1 つ。[`LineTable::close_by`] が見る）。TCP では使わない。
    pub async fn connect_as(
        driver: &dyn FrameDriver,
        settings: &Settings,
        spec: &TransportSpec,
        cx: &ConnectContext,
        events: Option<Arc<dyn EngineEvents>>,
        owner: Option<LineOwner>,
    ) -> Result<Self, FatalError> {
        if LineKey::of(spec).is_some() {
            let connect_timeout = cx.connect_timeout;
            return Self::with_line(
                driver,
                settings,
                LineTable::global(),
                spec,
                owner,
                || spec.connect(connect_timeout),
                cx.response_timeout,
                events,
            )
            .await;
        }
        let transport = spec.connect(cx.connect_timeout).await?;
        Self::with_transport(driver, settings, transport, cx.response_timeout, events).await
    }

    /// シリアルの線（設計 §7.3）を `table` から借りて接続する。線が閉じていれば `open` で開く。
    /// 借りた直後、ハンドシェイクの前に同期の手順を走らせる。`connect` は本番の表
    /// （[`LineTable::global`]）とポートを開く `open` でこれを呼ぶ。試験は独立の表と記憶上の
    /// 通信路を渡す。`spec` は `Serial` か `SerialOverTcp`（`Tcp` は `InvalidConfig`）。
    /// `owner` は線に記録する持ち主の印（[`LineTable::close_by`]）。
    #[allow(clippy::too_many_arguments)]
    pub async fn with_line<F>(
        driver: &dyn FrameDriver,
        settings: &Settings,
        table: &LineTable,
        spec: &TransportSpec,
        owner: Option<LineOwner>,
        open: impl FnOnce() -> F,
        response_timeout: Duration,
        events: Option<Arc<dyn EngineEvents>>,
    ) -> Result<Self, FatalError>
    where
        F: std::future::Future<Output = Result<Box<dyn Transport>, FatalError>>,
    {
        let (lease, start) = table.lease(spec, owner, open).await?;
        Self::start(
            driver,
            settings,
            Wire::Leased(lease),
            Some((table, start)),
            response_timeout,
            events,
        )
        .await
    }

    /// ホストが開いた通信路も、実通信と同じ検証と期限で実行する。
    pub async fn with_transport(
        driver: &dyn FrameDriver,
        settings: &Settings,
        transport: Box<dyn Transport>,
        response_timeout: Duration,
        events: Option<Arc<dyn EngineEvents>>,
    ) -> Result<Self, FatalError> {
        Self::start(
            driver,
            settings,
            Wire::Direct(transport),
            None,
            response_timeout,
            events,
        )
        .await
    }

    async fn start(
        driver: &dyn FrameDriver,
        settings: &Settings,
        mut wire: Wire,
        line: Option<(&LineTable, LeaseStart)>,
        response_timeout: Duration,
        events: Option<Arc<dyn EngineEvents>>,
    ) -> Result<Self, FatalError> {
        let conn = match driver.create_connection(settings) {
            Ok(conn) => conn,
            Err(error) => {
                record_driver_error(&events, "create_connection", &error);
                let _ = timeout(CLOSE_TIMEOUT, wire.transport().close()).await;
                return Err(error);
            }
        };
        // 借りた線は、前のセッションが最後に送り終えた（見積もり）・受けた時刻から無音を測る（§7.3.1）。
        let last_quiet_from = wire.line().and_then(|line| line.quiet_from());
        let mut core = Core {
            conn,
            wire,
            line_sync: false,
            response_timeout,
            can_write: driver.descriptor().capabilities.write,
            events,
            last_quiet_from,
        };
        let result = async {
            if let Some((table, start)) = line {
                core.sync_line(table, start).await?;
            }
            core.handshake().await
        }
        .await;
        match result {
            Ok(()) => Ok(Self { core: Some(core) }),
            Err(error) => Err(core.fail(error).await),
        }
    }

    /// 借りているシリアルの線（TCP は `None`）。試験と診断が同期の状態を見る。
    pub fn line(&self) -> Option<&Arc<Line>> {
        self.core.as_ref().and_then(|core| core.wire.line())
    }

    pub fn is_connected(&self) -> bool {
        self.core.is_some()
    }

    /// C11 の純粋なドライバ呼び出し用。I/O や内部の受信バッファは公開しない。
    pub fn connection(&self) -> Option<&dyn FrameConnection> {
        self.core.as_ref().map(|core| core.conn.as_ref())
    }

    /// C11 の適合テスト専用の観測口。通常の読み書きは Session 経由で行う。
    /// ここを直接呼ぶ場合の違反処理は適合テスト側が担う。
    #[doc(hidden)]
    pub fn connection_mut(&mut self) -> Option<&mut (dyn FrameConnection + 'static)> {
        self.core.as_mut().map(|core| core.conn.as_mut())
    }

    /// 適合テストでは f64 への変換前の整数・浮動小数点・文字列を照合する。
    /// 計画・送受信・検証は Session::read_batch と同じ実行系を通る。
    pub async fn read_raw_batch(
        &mut self,
        specs: &[TagSpec],
    ) -> Result<Vec<Result<RawValue, ItemError>>, FatalError> {
        let mut core = self.take_core()?;
        match core.read(specs).await {
            Ok(values) => {
                self.core = Some(core);
                Ok(values)
            }
            Err(error) => Err(core.fail(error).await),
        }
    }

    fn take_core(&mut self) -> Result<Core, FatalError> {
        self.core
            .take()
            .ok_or_else(|| FatalError::Protocol("接続は破棄済みです".into()))
    }
}

fn emit(events: &Option<Arc<dyn EngineEvents>>, event: EngineEvent) {
    if let Some(events) = events {
        events.record(event);
    }
}

fn record_driver_error(
    events: &Option<Arc<dyn EngineEvents>>,
    method: &'static str,
    error: &FatalError,
) {
    emit(
        events,
        EngineEvent::DriverFatal {
            method,
            error: error.clone(),
        },
    );
    if matches!(error, FatalError::Io(_) | FatalError::Timeout) {
        let detail = format!(
            "{method} が {error:?} を返した（フレーム型は Io / Timeout を返してはならない）"
        );
        tracing::warn!(%detail, "フレーム型ドライバの契約違反");
        emit(
            events,
            EngineEvent::Violation {
                check: "C4",
                detail,
            },
        );
    }
}

impl Session for FrameSession {
    fn read_batch<'a>(
        &'a mut self,
        specs: &'a [TagSpec],
    ) -> BoxFuture<'a, Result<Vec<Result<Value, ItemError>>, FatalError>> {
        Box::pin(async move {
            self.read_raw_batch(specs).await.map(|values| {
                specs
                    .iter()
                    .zip(values)
                    .map(|(spec, value)| {
                        value.and_then(|raw| decode::decode_value(&spec.kind, raw))
                    })
                    .collect()
            })
        })
    }

    fn write_batch<'a>(
        &'a mut self,
        items: &'a [WriteItem],
    ) -> BoxFuture<'a, Result<Vec<Result<(), ItemError>>, FatalError>> {
        Box::pin(async move {
            let mut core = self.take_core()?;
            match core.write(items).await {
                Ok(values) => {
                    self.core = Some(core);
                    Ok(values)
                }
                Err(error) => Err(core.fail(error).await),
            }
        })
    }

    fn disconnect(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            if let Some(mut core) = self.core.take() {
                // 切断はシリアルの線を閉じる唯一の正常な道（設計 §7.3 の 4）。別れの送信が
                // 期限で切られても閉じるよう、先に印を付ける。
                if let Wire::Leased(lease) = &mut core.wire {
                    lease.close_on_drop();
                }
                if let Some(step) = core.conn.farewell() {
                    // 別れの応答は待たず、送信（と手前の無音の待ち）にも期限を掛ける。
                    let gap = core.conn.inter_frame_gap();
                    if gap > MAX_INTER_FRAME_GAP {
                        // 応答を待たない後始末なので、送らずに閉じる（違反の記録は残す）。
                        let _ = core.gap_violation(gap);
                        core.close_for_disconnect().await;
                        return;
                    }
                    let from = core.last_quiet_from;
                    let _ = timeout(core.response_timeout.min(Duration::from_secs(1)), async {
                        wait_quiet(from, gap).await;
                        let send_start = Instant::now();
                        // 線に送り終えの見積もりを残す（変換器の側の送信待ち。§7.3.1）。
                        let tx = core.wire.transport().tx_time(step.send.len());
                        core.note_quiet(send_start + tx);
                        let transport = core.wire.transport();
                        if transport.send(&step.send).await.is_ok() {
                            // シリアルの send は drain しないので、閉じると
                            // 送信待ちのフレームが切れるので、送り終えの見積もりまで待ってから閉じる。
                            sleep_until(send_start + transport.tx_time(step.send.len())).await;
                        }
                    })
                    .await;
                }
                core.close_for_disconnect().await;
            }
        })
    }
}

impl Core {
    /// 致命の後始末。TCP は閉じる。シリアルはポートを表に戻す（`Io` なら通信路が壊れているので
    /// 閉じる。線の状態と署名は残り、次のセッションが開き直して同期する）。
    async fn fail(&mut self, error: FatalError) -> FatalError {
        if let (Wire::Leased(lease), FatalError::Io(_)) = (&mut self.wire, &error) {
            lease.close_on_drop();
        }
        self.close().await;
        error
    }

    /// 切断: シリアルの線はポートを閉じて開いた直後の状態に戻す。
    async fn close_for_disconnect(&mut self) {
        if let Wire::Leased(lease) = &mut self.wire {
            let _ = timeout(CLOSE_TIMEOUT, lease.close_port()).await;
            return;
        }
        self.close().await;
    }

    /// 線を借りた直後の同期の手順（設計 §7.3 の 2・5・6）。TCP では呼ばれない。
    async fn sync_line(&mut self, table: &LineTable, start: LeaseStart) -> Result<(), FatalError> {
        let Wire::Leased(lease) = &mut self.wire else {
            return Ok(());
        };
        // 「同期の手順を持つか」は start の種類にかかわらず先に決める。前のセッションが `Synced`
        // のまま線を返した（送る前の致命）後に借りたセッションでも、step の署名を線に記録する。
        self.line_sync = self.conn.sync_step(&[]).is_some();
        if start == LeaseStart::Synced {
            return Ok(());
        }
        if !self.line_sync {
            return match start {
                // 持たないドライバは、開いたばかりの線はそのまま使う。
                LeaseStart::Fresh | LeaseStart::Synced => Ok(()),
                // §7.3 の 6: 致命の後の線は使い直さない。ポートを閉じて止める。
                LeaseStart::Unsynced => Err(table.quarantine(lease)),
            };
        }
        let line = Arc::clone(lease.line());
        for _attempt in 0..MAX_SYNC_ATTEMPTS {
            let outstanding = line.outstanding();
            let Some(step) = self.conn.sync_step(&outstanding) else {
                break;
            };
            self.validate_step(&step)?;
            if matches!(step.expect, FrameRule::None) {
                return Err(
                    self.violation_fatal("C17", "sync_step が応答を待たない step を返した".into())
                );
            }
            // 同期の途中に届くフレームは、古い要求への応答も含めてすべて同期の step の形で
            // 区切る。`Fixed` は応答そのものから長さが分からず、形の違う古い応答を誤って
            // 区切るので断る（`LengthPrefixed`・`Terminator`・`DriverDecides` だけを受ける）。
            if matches!(step.expect, FrameRule::Fixed(_)) {
                return Err(self.violation_fatal(
                    "C17",
                    "sync_step の step の expect が Fixed（応答そのものから区切れる形にする）"
                        .into(),
                ));
            }
            let signature = self.signature_of(&step)?;
            if outstanding.contains(&signature) {
                return Err(self.violation_fatal(
                    "C17",
                    format!("sync_step の署名 {signature:?} が待っている要求と同じ"),
                ));
            }
            emit(&self.events, EngineEvent::StepStarted(step.clone()));
            let guard = line.begin(Some(signature.clone()));
            let deadline = self.send_step(&step).await?;
            let mut buf = Vec::new();
            // 古い応答は同期の step の `max_len` より長いことがあるので、区切りは大きい上限で
            // 行い、同期の応答として受けるのは `max_len` 以下のときだけにする。
            let limit = step.max_len.max(MAX_SYNC_FRAME_LEN) as usize;
            let synced = loop {
                match self.recv_frame(&step, &mut buf, deadline, limit).await {
                    Ok(len) => {
                        let frame = &buf[..len];
                        let fits = len <= step.max_len as usize;
                        if fits && self.conn.response_matches(&signature, frame) {
                            break true;
                        }
                        // 待っている要求への古い応答なら、その要求はもう待っていない。
                        let stale = outstanding
                            .iter()
                            .find(|s| self.conn.response_matches(s, frame));
                        if let Some(stale) = stale {
                            line.forget_outstanding(stale);
                        }
                        tracing::debug!(
                            line = %line.key(),
                            stale = stale.is_some(),
                            len,
                            fits,
                            "同期の手順の途中で届いたフレームを捨てた"
                        );
                        buf.drain(..len);
                    }
                    Err(FatalError::Timeout) => break false,
                    Err(FatalError::Framing(detail)) => {
                        // 崩れた受信・`limit` までに区切れない受信は捨てて待ち続ける（期限は延びない）。
                        // 区切れたフレームは `max_len` を超えても上の `Ok` で区切れた長さだけ捨てる。
                        tracing::debug!(line = %line.key(), %detail, "同期の手順の受信を捨てた");
                        buf.clear();
                    }
                    Err(error) => return Err(error),
                }
            };
            if synced {
                // 同期した: 古い要求の応答が来るなら同期の応答より前に来ている（§7.3 の 2）ので、
                // 待っている署名はすべて外す。
                guard.complete_sync();
                return Ok(());
            }
            // 時間切れ: 番人の drop が未同期にし、署名は待っている要求に残る。
            drop(guard);
        }
        Err(FatalError::Protocol("線の同期が取れない".into()))
    }

    fn signature_of(&mut self, step: &Step) -> Result<Signature, FatalError> {
        self.conn.signature(step).ok_or_else(|| {
            self.violation_fatal("C17", "シリアルの線で送る step に署名が無い".into())
        })
    }

    fn driver_fatal(&self, method: &'static str, error: FatalError) -> FatalError {
        record_driver_error(&self.events, method, &error);
        error
    }

    fn violation_fatal(&self, check: &'static str, detail: String) -> FatalError {
        tracing::warn!(%check, %detail, "フレーム型ドライバの契約違反");
        emit(
            &self.events,
            EngineEvent::Violation {
                check,
                detail: detail.clone(),
            },
        );
        FatalError::Protocol(detail)
    }

    fn gap_violation(&self, gap: Duration) -> FatalError {
        self.violation_fatal(
            "C9",
            format!("inter_frame_gap {gap:?} が上限 {MAX_INTER_FRAME_GAP:?} を超えている"),
        )
    }

    async fn close(&mut self) {
        let _ = timeout(CLOSE_TIMEOUT, self.wire.transport().close()).await;
    }

    async fn handshake(&mut self) -> Result<(), FatalError> {
        let mut previous = None;
        for steps in 0..=MAX_HANDSHAKE_STEPS {
            match self
                .conn
                .handshake_next(previous.as_deref())
                .map_err(|e| self.driver_fatal("handshake_next", e))?
            {
                None => return Ok(()),
                Some(_) if steps == MAX_HANDSHAKE_STEPS => {
                    return Err(self.violation_fatal(
                        "C7",
                        format!("ハンドシェイクが {MAX_HANDSHAKE_STEPS} step で終わらない"),
                    ))
                }
                Some(step) => previous = Some(self.exchange(&step).await?),
            }
        }
        unreachable!()
    }

    async fn read(
        &mut self,
        specs: &[TagSpec],
    ) -> Result<Vec<Result<RawValue, ItemError>>, FatalError> {
        let planned = self
            .conn
            .plan_read(specs)
            .map_err(|e| self.driver_fatal("plan_read", e))?;
        if planned.len() > specs.len().saturating_add(8) {
            return Err(self.violation_fatal(
                "C9",
                "読み取りの手順が specs.len() + 8 step を超えた".into(),
            ));
        }
        let mut seen = vec![false; specs.len()];
        for p in &planned {
            for &cover in &p.covers {
                let Some(slot) = seen.get_mut(cover as usize) else {
                    return Err(self.violation_fatal(
                        "C3",
                        format!("plan_read の covers に範囲外の添字 {cover}"),
                    ));
                };
                if *slot {
                    return Err(self.violation_fatal(
                        "C3",
                        format!("plan_read の covers で添字 {cover} が 2 回出た"),
                    ));
                }
                *slot = true;
            }
        }
        let mut out =
            vec![Err(ItemError::Unsupported("計画に含まれない".into())); specs.len()];
        for p in &planned {
            let response = self.exchange(&p.step).await?;
            let decoded = self
                .conn
                .decode_read(p, specs, &response)
                .map_err(|e| self.driver_fatal("decode_read", e))?;
            if decoded.len() != p.covers.len() {
                return Err(self.violation_fatal(
                    "C3",
                    format!(
                        "decode_read が {} 件を返した（covers は {} 件）",
                        decoded.len(),
                        p.covers.len()
                    ),
                ));
            }
            for (&cover, value) in p.covers.iter().zip(decoded) {
                out[cover as usize] = value;
            }
        }
        Ok(out)
    }

    async fn write(
        &mut self,
        items: &[WriteItem],
    ) -> Result<Vec<Result<(), ItemError>>, FatalError> {
        if !self.can_write {
            return Ok(vec![
                Err(ItemError::Unsupported(
                    "このドライバは書き込みに対応していません".into()
                ));
                items.len()
            ]);
        }
        let max_steps = items.len().saturating_mul(3).saturating_add(8);
        let mut previous = None;
        for steps in 0..=max_steps {
            match self
                .conn
                .write_next(items, previous.as_deref())
                .map_err(|e| self.driver_fatal("write_next", e))?
            {
                WriteProgress::Done(results) => {
                    if results.len() != items.len() {
                        return Err(self.violation_fatal("C3", format!("書き込みの結果の件数が要求と合わない（Done が {} 件、items は {} 件）", results.len(), items.len())));
                    }
                    return Ok(results);
                }
                WriteProgress::Send(_) if steps == max_steps => {
                    return Err(self.violation_fatal(
                        "C16",
                        format!("書き込みの手順が上限 {max_steps} step を超えた"),
                    ))
                }
                WriteProgress::Send(planned) => {
                    let mut seen = std::collections::BTreeSet::new();
                    for &cover in &planned.covers {
                        if cover as usize >= items.len() || !seen.insert(cover) {
                            return Err(self.violation_fatal(
                                "C10",
                                format!("write_next の covers に範囲外か重複の添字 {cover}"),
                            ));
                        }
                    }
                    if let Some(events) = &self.events {
                        events.record(EngineEvent::WriteStep(planned.covers.clone()));
                    }
                    previous = Some(self.exchange(&planned.step).await?);
                }
                other => {
                    return Err(self.violation_fatal(
                        "C10",
                        format!("write_next が未知の WriteProgress を返した: {other:?}"),
                    ))
                }
            }
        }
        unreachable!()
    }

    async fn exchange(&mut self, step: &Step) -> Result<Vec<u8>, FatalError> {
        self.validate_step(step)?;
        if let Some(events) = &self.events {
            events.record(EngineEvent::StepStarted(step.clone()));
        }
        // シリアルの線（設計 §7.3 の 7）: 送る前に署名を記録し「応答待ち」にする。番人は
        // 検証の通った完結でだけ外す。それ以外の終わり方（致命・取り消し）では drop が未同期にする。
        // 応答を待たない step にも署名なしの番人を付ける: 送信が時間切れ・`Io`・取り消し（一部の
        // バイトだけ出た後の drop を含む）で終わると、機器は崩れたフレームを受けたかもしれない。
        let awaits_response = !matches!(step.expect, FrameRule::None);
        let tracked = match self.wire.line() {
            Some(line) => {
                let line = Arc::clone(line);
                let signature = if awaits_response && self.line_sync {
                    Some(self.signature_of(step)?)
                } else {
                    None
                };
                Some((line.begin(signature.clone()), signature))
            }
            None => None,
        };
        let deadline = self.send_step(step).await?;
        if !awaits_response {
            // 送り終えた。応答の検証は無いので `Synced` にはせず、線の状態を変えずに外す。
            if let Some((guard, _)) = tracked {
                guard.disarm();
            }
            return Ok(Vec::new());
        }
        let mut buf = Vec::new();
        let len = self
            .recv_frame(step, &mut buf, deadline, step.max_len as usize)
            .await?;
        buf.truncate(len);
        if let Some((guard, signature)) = tracked {
            // 同期した後に別の要求への応答が来たら Framing（未同期に戻る。§7.3 の末尾）。
            if let Some(signature) = &signature {
                if !self.conn.response_matches(signature, &buf) {
                    return Err(FatalError::Framing(
                        "応答が送った要求と合わない（局番・機能コード）".into(),
                    ));
                }
            }
            guard.complete();
        }
        Ok(buf)
    }

    /// 無音を待って `step` を送る。返る期限は、受信にもそのまま使う。
    async fn send_step(&mut self, step: &Step) -> Result<Instant, FatalError> {
        // 期限は待ちの前に決める。フレーム間の無音（設計 §7.3.1）の待ちも step の応答の期限 T の中
        // （長さは MAX_INTER_FRAME_GAP 以下）。broker の外側の上限 `2T × step 上限 + 1 秒` の前提を保つ。
        let deadline = Instant::now()
            .checked_add(self.response_timeout)
            .ok_or_else(|| {
                FatalError::InvalidConfig("応答の期限が時刻の範囲を超えました".into())
            })?;
        let gap = self.conn.inter_frame_gap();
        if gap > MAX_INTER_FRAME_GAP {
            return Err(self.gap_violation(gap));
        }
        timeout_at(deadline, wait_quiet(self.last_quiet_from, gap))
            .await
            .map_err(|_| FatalError::Timeout)?;
        // tx_time の見積もりの起点は、待ちの後の時刻。
        let send_start = Instant::now();
        let estimate = send_start + self.wire.transport().tx_time(step.send.len());
        // 送る前に送り終えの見積もりを線に残す。送信の途中で取り消されても（future の drop）、
        // 次のセッションは送信待ちに残った途中のバイトの後から無音を測る（§7.3.1、Codex P2）。
        self.note_quiet(estimate);
        timeout_at(deadline, self.wire.transport().send(&step.send))
            .await
            .map_err(|_| FatalError::Timeout)??;
        // 送り終えた時刻の見積もり: send が返った時刻と、開始 + バイト数 × 1 文字の時間の遅い方。
        self.note_quiet(Instant::now().max(estimate));
        Ok(deadline)
    }

    /// フレーム間の無音の起点を `at` 以降に寄せる（早めない）。借りた線にも記録し、次に借りる
    /// セッションへ引き継ぐ（§7.3.1）。
    fn note_quiet(&mut self, at: Instant) {
        self.last_quiet_from = Some(self.last_quiet_from.map_or(at, |t| t.max(at)));
        if let Some(line) = self.wire.line() {
            line.note_quiet(at);
        }
    }

    /// `buf` に完結した応答が 1 つ入るまで受け、その長さを返す（`buf` の先頭の `len` バイト。
    /// 残りは呼び手が捨てる）。`buf` に前の受信の残りがあれば、recv の前にそれを調べる。
    /// `limit` は 1 つのフレームの長さの上限で、超えたら（区切れても区切れなくても）`Framing`。
    /// 普通の step は `max_len`（超えたら致命）、同期の手順は [`MAX_SYNC_FRAME_LEN`] を渡す。
    async fn recv_frame(
        &mut self,
        step: &Step,
        buf: &mut Vec<u8>,
        deadline: Instant,
        limit: usize,
    ) -> Result<usize, FatalError> {
        loop {
            // 先に `buf` に残っているバイトを調べる。同期の手順では、古い応答と同期の応答が
            // 1 回の受信でまとめて届き、古い応答を捨てた後に同期の応答がもう `buf` にある（§7.3）。
            // それ以上は届かないので、recv を先に待つと時間切れになる。
            if !buf.is_empty() {
                if let Some(len) = self.check_complete(step, buf, limit)? {
                    if len > limit {
                        return Err(FatalError::Framing("応答が max_len を超えた".into()));
                    }
                    return Ok(len);
                }
                if buf.len() >= limit {
                    return Err(FatalError::Framing(
                        "応答が max_len までに完結しない".into(),
                    ));
                }
            }
            // recv も先頭ノイズの破棄も、同じ期限を延長しない。
            if Instant::now() >= deadline {
                return Err(FatalError::Timeout);
            }
            let before = buf.len();
            let n = timeout_at(deadline, self.wire.transport().recv(buf))
                .await
                .map_err(|_| FatalError::Timeout)??;
            self.note_quiet(Instant::now());
            if n == 0 {
                return Err(FatalError::Io("相手が接続を閉じた".into()));
            }
            if buf.len().checked_sub(before) != Some(n) {
                return Err(FatalError::Protocol(
                    "Transport::recv の件数と受信が合いません".into(),
                ));
            }
            // 受けたバイトはループの頭で調べる。
        }
    }
    fn validate_step(&mut self, step: &Step) -> Result<(), FatalError> {
        let problem = match &step.expect {
            FrameRule::None => None,
            _ if step.max_len == 0 => Some("max_len が 0".to_owned()),
            FrameRule::Fixed(n) if *n == 0 || *n > step.max_len => {
                Some(format!("Fixed({n}) が 1..=max_len（{}）の外", step.max_len))
            }
            FrameRule::Terminator(t) if t.is_empty() || t.len() > step.max_len as usize => {
                Some("Terminator が空か max_len を超えた".to_owned())
            }
            FrameRule::LengthPrefixed(p)
                if p.width == 0 || (p.encoding == PrefixEncoding::Binary && p.width > 8) =>
            {
                Some(format!("LengthPrefixed の width {} は読めない", p.width))
            }
            FrameRule::LengthPrefixed(p)
                if p.offset
                    .checked_add(u32::from(p.width))
                    .is_none_or(|n| n > step.max_len) =>
            {
                Some("長さ欄が max_len の外です".into())
            }
            FrameRule::LengthPrefixed(p)
                if !matches!(
                    p.encoding,
                    PrefixEncoding::Binary | PrefixEncoding::AsciiHex | PrefixEncoding::AsciiDec
                ) =>
            {
                Some("未知の長さ欄の符号化です".into())
            }
            FrameRule::Fixed(_)
            | FrameRule::LengthPrefixed(_)
            | FrameRule::Terminator(_)
            | FrameRule::DriverDecides => None,
            other => Some(format!("未知の FrameRule: {other:?}")),
        };
        match problem {
            Some(p) => Err(self.violation_fatal("C9", format!("step が受信できない: {p}"))),
            None => Ok(()),
        }
    }

    fn check_complete(
        &mut self,
        step: &Step,
        buf: &mut Vec<u8>,
        limit: usize,
    ) -> Result<Option<usize>, FatalError> {
        match &step.expect {
            FrameRule::Fixed(n) => Ok((buf.len() >= *n as usize).then_some(*n as usize)),
            FrameRule::LengthPrefixed(p) => self.prefixed_len(p, limit, buf),
            FrameRule::Terminator(t) => Ok(buf
                .windows(t.len())
                .position(|w| w == &t[..])
                .map(|i| i + t.len())),
            FrameRule::DriverDecides => loop {
                if buf.is_empty() {
                    return Ok(None);
                }
                let check = self.conn.as_ref().is_complete(step, buf);
                match check {
                    Err(e) => return Err(self.driver_fatal("is_complete", e)),
                    Ok(FrameCheck::Incomplete) => return Ok(None),
                    Ok(FrameCheck::Complete { len }) => {
                        if len == 0 || len as usize > buf.len() {
                            return Err(self.violation_fatal(
                                "C9",
                                format!(
                                    "is_complete が Complete {{ len: {len} }}（受信は {} バイト）",
                                    buf.len()
                                ),
                            ));
                        }
                        return Ok(Some(len as usize));
                    }
                    Ok(FrameCheck::Discard(n)) => {
                        if n == 0 || n as usize > buf.len() {
                            return Err(self.violation_fatal(
                                "C9",
                                format!(
                                    "is_complete が Discard({n})（受信は {} バイト。1 以上・受信以下）",
                                    buf.len()
                                ),
                            ));
                        }
                        buf.drain(..n as usize);
                    }
                    Ok(other) => {
                        return Err(self.violation_fatal(
                            "C9",
                            format!("is_complete が未知の FrameCheck を返した: {other:?}"),
                        ))
                    }
                }
            },
            other => {
                Err(self
                    .violation_fatal("C9", format!("このエンジンが知らない FrameRule: {other:?}")))
            }
        }
    }

    fn prefixed_len(
        &mut self,
        p: &PrefixSpec,
        max_len: usize,
        buf: &[u8],
    ) -> Result<Option<usize>, FatalError> {
        let start = p.offset as usize;
        let need = start
            .checked_add(usize::from(p.width))
            .ok_or_else(|| self.violation_fatal("C9", "長さ欄の位置が範囲外です".into()))?;
        if buf.len() < need {
            return Ok(None);
        }
        let field = &buf[start..need];
        let value: Option<u64> = match p.encoding {
            PrefixEncoding::Binary => {
                let mut v = 0u64;
                if p.big_endian {
                    for &b in field {
                        v = (v << 8) | u64::from(b);
                    }
                } else {
                    for &b in field.iter().rev() {
                        v = (v << 8) | u64::from(b);
                    }
                }
                Some(v)
            }
            PrefixEncoding::AsciiHex | PrefixEncoding::AsciiDec => {
                let radix = if p.encoding == PrefixEncoding::AsciiHex {
                    16
                } else {
                    10
                };
                field.iter().try_fold(0u64, |value, &byte| {
                    let digit = match byte {
                        b'0'..=b'9' => u64::from(byte - b'0'),
                        b'a'..=b'f' if radix == 16 => u64::from(byte - b'a' + 10),
                        b'A'..=b'F' if radix == 16 => u64::from(byte - b'A' + 10),
                        _ => return None,
                    };
                    value.checked_mul(radix)?.checked_add(digit)
                })
            }
            other => {
                return Err(self.violation_fatal(
                    "C9",
                    format!("このエンジンが知らない PrefixEncoding: {other:?}"),
                ))
            }
        };
        let Some(value) = value else {
            return Err(FatalError::Framing(format!(
                "長さ欄 {field:02X?} が読めない"
            )));
        };
        let total = value as i128 + i128::from(p.adjust);
        if total < need as i128 || total > max_len as i128 {
            return Err(FatalError::Framing(format!(
                "長さ欄が総長 {total} を示す（{need}..={max_len} の外）"
            )));
        }
        let total = total as usize;
        Ok((buf.len() >= total).then_some(total))
    }
}

#[cfg(test)]
mod line_tests;
#[cfg(test)]
mod tests;
