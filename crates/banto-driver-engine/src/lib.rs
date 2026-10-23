//! フレーム型のドライバを、期限つきの Session として動かす実行系（設計 §5）。
//!
//! 接続と手順の途中の状態は 1 個の Core が所有する。操作中は Session から取り出し、
//! 成功した時だけ戻すため、外側の future のキャンセルでも接続を再利用しない。
//! broker は Frame をこの Session で実行する。接続の外側期限は TCP 接続 +
//! 32 step × response_timeout + CLOSE_TIMEOUT + ゆとりを含む。
//! シリアルの同期（設計 §7.3）は後続の PR で行う。
//! 各 step の送信の前に、ドライバの `inter_frame_gap`（Modbus RTU の t3.5）だけ、
//! 前のフレームの最後のバイト（送り終え・受け終えの遅い方）から無音を空ける（§7.3.1）。
//! 送り終えた時刻は `Transport::tx_time` で見積もる（drain が当てにならない通信路のため）。
//! 公開する crate は API → engine → conformance の順に公開する。

pub mod decode;
pub mod transport;

use std::sync::Arc;
use std::time::Duration;

use banto_driver_api::{
    BoxFuture, ConnectContext, FatalError, FrameCheck, FrameConnection, FrameDriver, FrameRule,
    ItemError, PrefixEncoding, PrefixSpec, RawValue, Session, Settings, Step, TagSpec, Value,
    WriteItem, WriteProgress,
};
use tokio::time::{timeout, timeout_at, Instant};
use transport::{Transport, TransportSpec};

pub const MAX_HANDSHAKE_STEPS: usize = 32;
/// broker の停止の上限（2 秒）より短い、致命時の後始末の上限。
pub const CLOSE_TIMEOUT: Duration = Duration::from_millis(100);
/// `inter_frame_gap` の待ちの上限。ドライバの返す値がこれを超えても、待ちは必ずこの長さまでに収める
/// （待ちは応答の期限の外だが、broker の外側の上限 `2T × step 上限 + 1 秒` に収まる長さに限る）。
/// 300 baud 8N1 の t3.5 でも約 117 ms。
pub const MAX_INTER_FRAME_GAP: Duration = Duration::from_millis(500);

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

struct Core {
    conn: Box<dyn FrameConnection>,
    transport: Box<dyn Transport>,
    response_timeout: Duration,
    can_write: bool,
    events: Option<Arc<dyn EngineEvents>>,
    /// 最後に送り終えた・受け終えた時刻の遅い方（フレーム間の無音の起点）。
    last_quiet_from: Option<Instant>,
}

/// 前のフレームの最後のバイトの時刻 `from` から `gap`（`MAX_INTER_FRAME_GAP` で頭打ち）が
/// 過ぎるまで待つ。`gap` が 0 か `from` が無いなら待たない。
async fn wait_quiet(from: Option<Instant>, gap: Duration) {
    let gap = gap.min(MAX_INTER_FRAME_GAP);
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
        let transport = spec.connect(cx.connect_timeout).await?;
        Self::with_transport(driver, settings, transport, cx.response_timeout, events).await
    }

    /// ホストが開いた通信路も、実通信と同じ検証と期限で実行する。
    pub async fn with_transport(
        driver: &dyn FrameDriver,
        settings: &Settings,
        mut transport: Box<dyn Transport>,
        response_timeout: Duration,
        events: Option<Arc<dyn EngineEvents>>,
    ) -> Result<Self, FatalError> {
        let conn = match driver.create_connection(settings) {
            Ok(conn) => conn,
            Err(error) => {
                record_driver_error(&events, "create_connection", &error);
                let _ = timeout(CLOSE_TIMEOUT, transport.close()).await;
                return Err(error);
            }
        };
        let mut core = Core {
            conn,
            transport,
            response_timeout,
            can_write: driver.descriptor().capabilities.write,
            events,
            last_quiet_from: None,
        };
        let result = core.handshake().await;
        match result {
            Ok(()) => Ok(Self { core: Some(core) }),
            Err(error) => {
                core.close().await;
                Err(error)
            }
        }
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
            Err(error) => {
                core.close().await;
                Err(error)
            }
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
                Err(error) => {
                    core.close().await;
                    Err(error)
                }
            }
        })
    }

    fn disconnect(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            if let Some(mut core) = self.core.take() {
                if let Some(step) = core.conn.farewell() {
                    // 別れの応答は待たず、送信（と手前の無音の待ち）にも期限を掛ける。
                    let gap = core.conn.inter_frame_gap();
                    let from = core.last_quiet_from;
                    let _ = timeout(core.response_timeout.min(Duration::from_secs(1)), async {
                        wait_quiet(from, gap).await;
                        core.transport.send(&step.send).await
                    })
                    .await;
                }
                core.close().await;
            }
        })
    }
}

impl Core {
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

    async fn close(&mut self) {
        let _ = timeout(CLOSE_TIMEOUT, self.transport.close()).await;
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
        // フレーム間の無音（設計 §7.3.1）。待ちは応答の期限の外（長さは MAX_INTER_FRAME_GAP 以下）。
        wait_quiet(self.last_quiet_from, self.conn.inter_frame_gap()).await;
        let send_start = Instant::now();
        let deadline = send_start
            .checked_add(self.response_timeout)
            .ok_or_else(|| {
                FatalError::InvalidConfig("応答の期限が時刻の範囲を超えました".into())
            })?;
        timeout_at(deadline, self.transport.send(&step.send))
            .await
            .map_err(|_| FatalError::Timeout)??;
        // 送り終えた時刻の見積もり: send が返った時刻と、開始 + バイト数 × 1 文字の時間の遅い方。
        let sent_at = Instant::now().max(send_start + self.transport.tx_time(step.send.len()));
        self.last_quiet_from = Some(sent_at);
        if matches!(step.expect, FrameRule::None) {
            return Ok(Vec::new());
        }
        let mut buf = Vec::new();
        loop {
            // recv も先頭ノイズの破棄も、同じ期限を延長しない。
            if Instant::now() >= deadline {
                return Err(FatalError::Timeout);
            }
            let before = buf.len();
            let n = timeout_at(deadline, self.transport.recv(&mut buf))
                .await
                .map_err(|_| FatalError::Timeout)??;
            let received_at = Instant::now();
            self.last_quiet_from = Some(
                self.last_quiet_from
                    .map_or(received_at, |t| t.max(received_at)),
            );
            if n == 0 {
                return Err(FatalError::Io("相手が接続を閉じた".into()));
            }
            if buf.len().checked_sub(before) != Some(n) {
                return Err(FatalError::Protocol(
                    "Transport::recv の件数と受信が合いません".into(),
                ));
            }
            if let Some(len) = self.check_complete(step, &mut buf)? {
                if len > step.max_len as usize {
                    return Err(FatalError::Framing("応答が max_len を超えた".into()));
                }
                buf.truncate(len);
                return Ok(buf);
            }
            if buf.len() >= step.max_len as usize {
                return Err(FatalError::Framing(
                    "応答が max_len までに完結しない".into(),
                ));
            }
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
    ) -> Result<Option<usize>, FatalError> {
        match &step.expect {
            FrameRule::Fixed(n) => Ok((buf.len() >= *n as usize).then_some(*n as usize)),
            FrameRule::LengthPrefixed(p) => self.prefixed_len(p, step.max_len, buf),
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
        max_len: u32,
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
        if total < need as i128 || total > i128::from(max_len) {
            return Err(FatalError::Framing(format!(
                "長さ欄が総長 {total} を示す（{need}..={max_len} の外）"
            )));
        }
        let total = total as usize;
        Ok((buf.len() >= total).then_some(total))
    }
}

#[cfg(test)]
mod tests;
