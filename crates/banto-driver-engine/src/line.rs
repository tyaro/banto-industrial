//! 線の表（設計 §7.3）: シリアルの線（COM / tty のポート、変換器の `host:port`）ごとの状態を、
//! `FrameSession` の寿命をまたいでプロセスに 1 つ持つ。
//!
//! TCP（Modbus TCP・SLMP）はソケットを閉じれば遅れた応答は届かないので、ここを通らない。
//! シリアルは線も機器も元のままなので、時間切れ・`Framing`・取り消しの後に「同期が取れたと
//! 証明できるまでデータの要求を送らない」。そのために線は次を覚える:
//!
//! - 開いたポート（致命の後も閉じない。閉じるのは broker が接続を止めた・設定が変わったとき）。
//! - 同期の状態（[`SyncState`]）。開いた直後は `Fresh`、致命・取り消しの後は `Unsynced`。
//! - 応えの返っていない要求の署名（[`Signature`]）の集合。「応答待ち」はこの集合が空でないこと
//!   （送る前に足し、検証の通った完結でだけ外す）。
//!
//! 状態は同期の `std::sync::Mutex` に置き、`await` をまたがずに書く（§7.3 の 7）。
//! 「同期済み」に戻る道は、完結して検証の通った応答（[`InFlightGuard::complete`]）と
//! 同期の手順（`FrameSession` の `sync_line`）の 2 つだけ。それ以外の終わり方
//! （致命・取り消し・panic）では番人の `Drop` が線を `Unsynced` にし、署名は残る。
//!
//! 待っている署名には寿命がある（2026-10-23 オーナー決定）: 署名は送る直前の時刻を持ち、線は
//! 最後にバイトを受けた時刻（`last_rx`）を持つ。「その要求を送った後、線から何も届かない時間」
//! （`now - max(送った時刻, last_rx)`）が寿命（`crate::stale_signature_ttl`）に達した署名は、
//! 同期の手順が集合を読むときに忘れる（[`Line::forget_stale`]）。機器が長く落ちている（電源断）
//! と、broker が張り直すたびに同期の試行の署名が足され、やがてドライバが区別できる要求を選べず、
//! 機器が戻っても二度と同期できなくなるため。機器が何かを送り続けている（ごみでも）間は忘れない。
//! 状態は `Unsynced` のまま（同期の手順は必ず走る）。
//!
//! 線の貸し出し（[`LineLease`]）は 1 度に 1 つ。貸し出しが drop されると（セッションの破棄・
//! future の取り消し）、ポートは表に戻る。`close_on_drop` が立っていれば戻さずに閉じる
//! （切断・`Io` の致命・§7.3 の 6 の隔離）。
//!
//! 線は最後に借りた持ち主の印（[`LineOwner`]。broker の接続の task ごとに 1 つ）を覚える。
//! task の終わりの解放（[`LineTable::close_by`]）は印が合うときだけ閉じる: Hub は設定が変わると
//! 古い task の終わりを待たずに新しい task を作るので、古い task の解放が新しい task の借りた
//! 線（待っている署名）を消してはならない。
//!
//! 1 回きりのダイヤル（Hub の接続テスト。[`LineOwner::one_shot`]）は、別の持ち主の印の線を
//! 貸し出し中でなくても借りない（「使用中」で断る）。broker の task が致命の後のバックオフの
//! 間に表へ返した線（`Unsynced`・待っている署名あり）を借りて印を書き換え、終わりに閉じると、
//! task の待っている署名が消えて次のダイヤルが `Fresh` から始まる（§7.3 の保証が崩れる）。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::Duration;

use banto_driver_api::{BoxFuture, FatalError, Signature};
use tokio::time::Instant;

use crate::transport::{SerialPortName, Transport, TransportSpec};

/// 線の鍵。`Serial` はポート名だけ（線の設定が変わっても同じ線）、`SerialOverTcp` は `host:port`。
/// TCP は線の表を通らないので鍵を持たない。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LineKey {
    Serial(SerialPortName),
    SerialOverTcp { host: String, port: u16 },
}

impl LineKey {
    /// `spec` の線の鍵。`Tcp` は `None`。
    pub fn of(spec: &TransportSpec) -> Option<Self> {
        match spec {
            TransportSpec::Serial { port, .. } => Some(Self::Serial(port.clone())),
            TransportSpec::SerialOverTcp { host, port } => Some(Self::SerialOverTcp {
                host: host.clone(),
                port: *port,
            }),
            _ => None,
        }
    }
}

impl std::fmt::Display for LineKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Serial(port) => write!(f, "{port}"),
            Self::SerialOverTcp { host, port } => write!(f, "{host}:{port}"),
        }
    }
}

/// 線の持ち主の印。broker は接続の task ごとに 1 つ作り、借りるときに線に記録し、task の
/// 終わりに [`LineTable::close_by`] で「自分が最後に借りた線」だけを閉じる。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LineOwner {
    id: u64,
    /// 1 回きりのダイヤル（broker の task の外）の印か。
    one_shot: bool,
}

impl LineOwner {
    /// 新しい印（プロセスの中で一意）。broker の接続の task が使う。
    pub fn new() -> Self {
        Self {
            id: Self::next_id(),
            one_shot: false,
        }
    }

    /// 1 回きりのダイヤル（Hub の接続テストなど、broker の task の外）の新しい印。この印で
    /// 借りるときは、別の持ち主の印の線を貸し出し中でなくても借りない（[`LineTable`] の冒頭）。
    pub fn one_shot() -> Self {
        Self {
            id: Self::next_id(),
            one_shot: true,
        }
    }

    /// [`one_shot`](Self::one_shot) で作った印か。
    pub fn is_one_shot(&self) -> bool {
        self.one_shot
    }

    fn next_id() -> u64 {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        NEXT.fetch_add(1, Ordering::Relaxed)
    }
}

impl Default for LineOwner {
    fn default() -> Self {
        Self::new()
    }
}

/// 線の同期の状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncState {
    /// 開いたばかりで、まだ何も送っていない。
    Fresh,
    /// 同期している（データの要求を送ってよい）。
    Synced,
    /// 致命・取り消しの後。同期の手順が通るまでデータの要求を送らない。
    Unsynced,
}

/// 応えの返っていない要求。
#[derive(Debug, Clone)]
struct Pending {
    signature: Signature,
    /// 送る直前に記録した時刻（[`Line::begin`]）。寿命の起点。
    since: Instant,
}

struct LineInner {
    /// 開いているポート。貸し出し中と閉じた後は `None`。
    transport: Option<Box<dyn Transport>>,
    /// 開いたときの接続先（線の設定が変わったかを見る）。
    spec: Option<TransportSpec>,
    leased: bool,
    /// 最後に借りた（予約した）持ち主の印。印を持たない貸し出し（試験）は `None` のまま。
    owner: Option<LineOwner>,
    state: SyncState,
    outstanding: Vec<Pending>,
    /// 線から最後にバイトを受けた時刻（`quiet_from` と違い、送り終えの見積もりは含まない）。
    /// 待っている署名の寿命の起点の 1 つ（モジュールの doc）。
    last_rx: Option<Instant>,
    /// §7.3 の 6: 同期の手順を持たないドライバが致命の後に借りようとした。
    /// 閉じる（[`LineTable::close`]）まで、この線は開かない。
    quarantined: bool,
    /// 線が最後に静かになり始める時刻（送り終えの見積もり・受けたバイトの時刻の遅い方。
    /// フレーム間の無音 §7.3.1 の起点）。セッションをまたいで引き継ぎ、借りた直後のセッションは
    /// ここから無音を測る（致命の直後に張り直しても、前のフレームや送信待ちに残った途中の
    /// バイトの直後に同期の要求を続けない。Codex P2、2026-10-23）。値は遅い方にだけ動かす。
    /// ポートを閉じて開き直しても消さない: `SerialOverTcp` は変換器の側に送信待ちが残りうるし、
    /// 機器の応答が線の上にまだありうる。残しても遅れは高々 `inter_frame_gap`（≤ 500 ms）。
    quiet_from: Option<Instant>,
}

/// 1 本の線。
pub struct Line {
    key: LineKey,
    inner: Mutex<LineInner>,
}

impl Line {
    fn lock(&self) -> MutexGuard<'_, LineInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 線の鍵。
    pub fn key(&self) -> &LineKey {
        &self.key
    }

    /// 今の同期の状態。
    pub fn state(&self) -> SyncState {
        self.lock().state
    }

    /// 線が最後に静かになり始める時刻（§7.3.1。[`LineInner`] の `quiet_from`）。
    pub(crate) fn quiet_from(&self) -> Option<Instant> {
        self.lock().quiet_from
    }

    /// 線が静かになり始める時刻を `at` 以降に寄せる（早めない）。
    pub(crate) fn note_quiet(&self, at: Instant) {
        let mut inner = self.lock();
        inner.quiet_from = Some(inner.quiet_from.map_or(at, |t| t.max(at)));
    }

    /// 線からバイトを受けた（`at` 以降に寄せる。早めない）。
    pub(crate) fn note_rx(&self, at: Instant) {
        let mut inner = self.lock();
        inner.last_rx = Some(inner.last_rx.map_or(at, |t| t.max(at)));
    }

    /// 線から最後にバイトを受けた時刻。
    pub fn last_rx(&self) -> Option<Instant> {
        self.lock().last_rx
    }

    /// 応えの返っていない要求の署名の写し。
    pub fn outstanding(&self) -> Vec<Signature> {
        self.lock()
            .outstanding
            .iter()
            .map(|p| p.signature.clone())
            .collect()
    }

    /// 寿命（モジュールの doc）の過ぎた署名を忘れ、その数を返す。「送った後、線から何も届かない
    /// 時間」が `ttl` に達した署名が対象（`now - max(送った時刻, last_rx) >= ttl`）。
    /// 状態は変えない（`Unsynced` のまま。同期の手順は必ず走る）。
    pub(crate) fn forget_stale(&self, now: Instant, ttl: Duration) -> usize {
        let mut inner = self.lock();
        let last_rx = inner.last_rx;
        let before = inner.outstanding.len();
        inner.outstanding.retain(|p| {
            let from = last_rx.map_or(p.since, |rx| rx.max(p.since));
            now.saturating_duration_since(from) < ttl
        });
        before - inner.outstanding.len()
    }

    /// ポートが開いているか（貸し出し中も含む）。
    pub fn is_open(&self) -> bool {
        let inner = self.lock();
        inner.transport.is_some() || inner.leased
    }

    /// 送る前に呼ぶ: 署名を「応えの返っていない要求」に足す（同期の手順を持たないドライバは
    /// 署名を持たないので `None`。致命の後の扱いは §7.3 の 6）。
    /// 返る番人を [`InFlightGuard::complete`] しないまま drop すると線は `Unsynced` になる。
    pub(crate) fn begin(self: &Arc<Self>, signature: Option<Signature>) -> InFlightGuard {
        if let Some(signature) = &signature {
            self.lock().outstanding.push(Pending {
                signature: signature.clone(),
                since: Instant::now(),
            });
        }
        InFlightGuard {
            line: Arc::clone(self),
            signature,
            armed: true,
        }
    }

    /// 応答が完結し検証が通った（同期の要求なら、同期の手順が通った）。
    fn mark_synced(&self, signature: Option<&Signature>) {
        let mut inner = self.lock();
        if let Some(i) = signature.and_then(|signature| {
            inner
                .outstanding
                .iter()
                .position(|p| &p.signature == signature)
        }) {
            inner.outstanding.remove(i);
        }
        inner.state = SyncState::Synced;
    }

    /// 同期の手順が通った（§7.3 の 2）: 半二重・主局 1 つの線では、古い要求の応答が来るなら
    /// 同期の応答より前に来ているので、待っている署名はすべて外す。残すと致命のたびに集合が
    /// 増え、ドライバが区別できる要求を選べなくなって線が永久に同期できなくなる。
    fn mark_resynced(&self) {
        let mut inner = self.lock();
        inner.outstanding.clear();
        inner.state = SyncState::Synced;
    }

    fn mark_unsynced(&self) {
        self.lock().state = SyncState::Unsynced;
    }

    /// 同期の手順の途中で、待っている要求への古い応答が届いて捨てた: その要求はもう待っていない。
    pub(crate) fn forget_outstanding(&self, signature: &Signature) {
        let mut inner = self.lock();
        if let Some(i) = inner
            .outstanding
            .iter()
            .position(|p| &p.signature == signature)
        {
            inner.outstanding.remove(i);
        }
    }
}

/// step ごとの番人（§7.3 の 7）。正常に完結したら [`complete`](Self::complete)。
/// それ以外の終わり方（致命・取り消し・panic）では `Drop` が線を `Unsynced` にする。
/// 署名は `begin` で記録済みなので残る。
pub(crate) struct InFlightGuard {
    line: Arc<Line>,
    signature: Option<Signature>,
    armed: bool,
}

impl InFlightGuard {
    /// 応答が完結し、検証が通った。署名を外して `Synced` に戻す。
    pub(crate) fn complete(mut self) {
        self.armed = false;
        self.line.mark_synced(self.signature.as_ref());
    }

    /// 応答を待たない step を送り終えた。線の状態は変えずに外す（応答の検証が無いので
    /// `Synced` にはしない）。番人は署名なしで作ったものに限る。
    pub(crate) fn disarm(mut self) {
        debug_assert!(self.signature.is_none(), "署名つきの番人は disarm しない");
        self.armed = false;
    }

    /// 同期の要求の応答が届いた。待っている署名をすべて外して `Synced` に戻す。
    pub(crate) fn complete_sync(mut self) {
        self.armed = false;
        self.line.mark_resynced();
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        if self.armed {
            self.line.mark_unsynced();
        }
    }
}

/// 線の貸し出し。`Transport` としてはポートに委譲する。drop で表に戻す（または閉じる）。
pub struct LineLease {
    line: Arc<Line>,
    transport: Option<Box<dyn Transport>>,
    close_on_drop: bool,
}

impl LineLease {
    pub fn line(&self) -> &Arc<Line> {
        &self.line
    }

    /// drop で表に戻さず閉じる（切断・`Io` の致命）。線の状態（署名）は残る。
    pub fn close_on_drop(&mut self) {
        self.close_on_drop = true;
    }

    /// 切断（broker が接続を止めた）: ポートを閉じ、線を開いた直後の状態（`Fresh`、署名なし）に
    /// 戻す（§7.3 の 5。待っている署名は分からなくなる）。`close` の上限は呼び手が掛ける。
    pub async fn close_port(&mut self) {
        self.close_on_drop = true;
        {
            let mut inner = self.line.lock();
            inner.state = SyncState::Fresh;
            inner.outstanding.clear();
            inner.spec = None;
        }
        if let Some(mut transport) = self.transport.take() {
            transport.close().await;
        }
    }

    fn transport(&mut self) -> &mut dyn Transport {
        self.transport
            .as_deref_mut()
            .expect("貸し出しのポートは close_port の後に使わない")
    }
}

impl Drop for LineLease {
    fn drop(&mut self) {
        let mut inner = self.line.lock();
        inner.leased = false;
        if self.close_on_drop || inner.quarantined {
            // SerialTransport の Drop が送信待ちを捨ててポートを閉じる。
            drop(self.transport.take());
            inner.transport = None;
        } else {
            inner.transport = self.transport.take();
        }
    }
}

impl Transport for LineLease {
    fn tx_time(&self, len: usize) -> Duration {
        self.transport
            .as_deref()
            .map_or(Duration::ZERO, |t| t.tx_time(len))
    }
    fn send<'a>(&'a mut self, bytes: &'a [u8]) -> BoxFuture<'a, Result<(), FatalError>> {
        self.transport().send(bytes)
    }
    fn recv<'a>(&'a mut self, buf: &'a mut Vec<u8>) -> BoxFuture<'a, Result<usize, FatalError>> {
        self.transport().recv(buf)
    }
    fn discard_input(&mut self) -> BoxFuture<'_, ()> {
        self.transport().discard_input()
    }
    fn close(&mut self) -> BoxFuture<'_, ()> {
        // 致命の後の `Core::close` から来る。ポートは閉じずに表へ戻す（drop で）。
        Box::pin(async {})
    }
}

/// プロセスに 1 つの線の表（[`LineTable::global`]）。試験は `new` で独立の表を作る。
#[derive(Default)]
pub struct LineTable {
    lines: Mutex<HashMap<LineKey, Arc<Line>>>,
    /// 試験だけの差し込み口: [`LineTable::lease`] が表のロックを取る直前に 1 回だけ呼ぶ
    /// （その瞬間に旧 task の `close_by` を挟む回帰テスト用）。
    #[cfg(test)]
    pub(crate) before_lease_lock: Mutex<Option<LeaseHook>>,
    /// 試験だけの差し込み口: `close` / `close_by` が表と線のロックを放した直後（取り出したポートを
    /// drop する前）に 1 回だけ呼ぶ。この時点で古い線のポートが取り出し済みであることを確かめる。
    #[cfg(test)]
    pub(crate) after_close_unlock: Mutex<Option<CloseHook>>,
}

/// [`LineTable`] の試験だけの差し込み口の型。
#[cfg(test)]
pub(crate) type CloseHook = Box<dyn FnOnce(&Line) + Send>;
#[cfg(test)]
pub(crate) type LeaseHook = Box<dyn FnOnce(&LineTable) + Send>;

static GLOBAL: LazyLock<LineTable> = LazyLock::new(LineTable::default);

/// 線を借りたときに、`FrameSession` がハンドシェイクの前に取るべき手順。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LeaseStart {
    /// 開いたばかり。同期の手順を持つドライバは同期する（§7.3 の 5）。持たなければそのまま使う。
    Fresh,
    /// 前のセッションが正常に完結した状態のまま。そのまま使う。
    Synced,
    /// 致命・取り消しの後。同期の手順を通す。持たないドライバは隔離する（§7.3 の 6）。
    Unsynced,
}

impl LineTable {
    /// プロセスに 1 つの表。
    pub fn global() -> &'static LineTable {
        &GLOBAL
    }

    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<LineKey, Arc<Line>>> {
        self.lines.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 鍵の線（無ければ `None`）。
    pub fn line(&self, key: &LineKey) -> Option<Arc<Line>> {
        self.lock().get(key).cloned()
    }

    /// 線を借りる。閉じていれば `open` で開く（表のロックの外で待つ。その間は予約して
    /// 2 つ目の貸し出しを断る）。開いたまま残っていれば（致命の後）そのポートを渡す。
    /// 線の設定が変わっていれば古いポートを閉じて開き直す。`owner` は予約と同時に線に記録する
    /// （[`Self::close_by`] が見る）。`owner` が 1 回きりの印（[`LineOwner::one_shot`]）なら、
    /// 別の持ち主の印の線は貸し出し中でなくても借りずに「使用中」で断る（線の印・状態・待っている
    /// 署名は変えない）。
    pub(crate) async fn lease<F>(
        &self,
        spec: &TransportSpec,
        owner: Option<LineOwner>,
        open: impl FnOnce() -> F,
    ) -> Result<(LineLease, LeaseStart), FatalError>
    where
        F: std::future::Future<Output = Result<Box<dyn Transport>, FatalError>>,
    {
        let key = LineKey::of(spec)
            .ok_or_else(|| FatalError::InvalidConfig("TCP の接続は線の表を通りません".into()))?;
        #[cfg(test)]
        {
            let hook = self
                .before_lease_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take();
            if let Some(hook) = hook {
                hook(self);
            }
        }
        // 表からの取り出し（無ければ作る）と予約（使用中・隔離・設定変更の判定、`leased` と
        // 持ち主の印の記録）は、表のロックを持ったまま線のロックを取って一体で行う。ロックの順は
        // `close` / `close_by` と同じ「表 → 線」。表のロックを先に放すと、その隙に旧 task の
        // `close_by` が（まだ貸し出し中でない）線を表から外し、隔離の印も立てないので、この
        // セッションが表から外れた線を予約して使い、時間切れの署名がセッションを捨てると失われる
        // （オーナーのレビュー P2、2026-10-23）。どちらのロックも `await` をまたがない。
        let (line, reused) = {
            let mut lines = self.lock();
            let line = Arc::clone(lines.entry(key.clone()).or_insert_with(|| {
                Arc::new(Line {
                    key: key.clone(),
                    inner: Mutex::new(LineInner {
                        transport: None,
                        spec: None,
                        leased: false,
                        owner: None,
                        state: SyncState::Fresh,
                        outstanding: Vec::new(),
                        last_rx: None,
                        quarantined: false,
                        quiet_from: None,
                    }),
                })
            }));
            let reused = {
                let mut inner = line.lock();
                if inner.leased {
                    return Err(FatalError::Protocol(format!(
                        "シリアルの線 {key} は別のセッションが使用中です"
                    )));
                }
                if owner.is_some_and(|o| o.is_one_shot())
                    && inner.owner.is_some_and(|current| Some(current) != owner)
                {
                    // 1 回きりのダイヤルは、broker の task が返した線（待っている署名）を奪わない。
                    return Err(FatalError::Protocol(format!(
                        "シリアルの線 {key} は別の接続が使用中です"
                    )));
                }
                if inner.quarantined {
                    return Err(quarantine_error(&key));
                }
                if inner.spec.as_ref() != Some(spec) {
                    // 線の設定が変わった。開いたままのポートは閉じる（表に戻さない）。
                    drop(inner.transport.take());
                    // 開き直したポートは同期済みを引き継がない（同期の手順を持つドライバは同期から）。
                    // 未同期と待っている署名は残す: 機器と線は同じなので古い要求の応答がまだ届きうる。
                    // 溜まった署名は、同期が通れば空にする。
                    if inner.state == SyncState::Synced {
                        inner.state = SyncState::Fresh;
                    }
                }
                inner.leased = true; // 予約。開く間に 2 つ目を断る。
                if owner.is_some() {
                    inner.owner = owner;
                }
                inner.transport.take()
            };
            drop(lines);
            (line, reused)
        };
        // ここから先、`open().await` の最中に `close` / `close_by` が来ても、線は貸し出し中
        // （予約済み）なので `close_line` が隔離の印を立て、貸し出しの drop でポートを閉じる。
        // 予約の番人: `open` が `Err` を返したとき、または `open` の待ちの最中に future が
        // drop された（broker の connect の上限での abort など）とき、予約を戻す。
        let reservation = Reservation { line: &line };
        let transport = match reused {
            Some(t) => t,
            None => open().await?,
        };
        reservation.disarm();
        let start = {
            let mut inner = line.lock();
            inner.spec = Some(spec.clone());
            match inner.state {
                SyncState::Fresh => LeaseStart::Fresh,
                SyncState::Synced => LeaseStart::Synced,
                SyncState::Unsynced => LeaseStart::Unsynced,
            }
        };
        Ok((
            LineLease {
                line,
                transport: Some(transport),
                close_on_drop: false,
            },
            start,
        ))
    }

    /// §7.3 の 6: 同期の手順を持たないドライバが致命の後に借りた。ポートを閉じて、
    /// [`close`](Self::close)（接続の停止）まで開かない。呼び手は貸し出しを捨てる。
    pub(crate) fn quarantine(&self, lease: &mut LineLease) -> FatalError {
        lease.close_on_drop = true;
        let mut inner = lease.line.lock();
        inner.quarantined = true;
        inner.state = SyncState::Unsynced;
        quarantine_error(&lease.line.key)
    }

    /// broker が接続を止めた・設定が変わった: 線を表から外す（貸し出し中でなければポートも
    /// 閉じる。貸し出し中なら、その貸し出しの drop で閉じる）。待っている署名は消える
    /// （§7.3 の 5。次に開くときは `Fresh` で、同期の手順を持つドライバは同期から始める）。
    pub fn close(&self, key: &LineKey) -> bool {
        self.close_where(key, None)
    }

    /// broker の接続の task の終わり: 最後に借りたのが `owner` の線（または、まだ誰も印を
    /// 付けていない線）だけを [`close`](Self::close) する。別の（新しい）task が借りた・開いた
    /// 線は閉じない（`false`）。
    pub fn close_by(&self, key: &LineKey, owner: LineOwner) -> bool {
        self.close_where(key, Some(owner))
    }

    /// 表から外すことと、線の中のポートの取り出し・隔離の印は、表 → 線のロックを持ったまま
    /// 一体で行う（`lease` と同じ順）。分けると、その隙に同じ鍵で `lease` した task が新しい線で
    /// 同じポートを開きにいき、古いポートがまだ開いているので COM は「使用中」で失敗し、
    /// `SerialOverTcp` は変換器へ一瞬 2 本つながる（Codex P2）。取り出したポートの drop（同期の
    /// close）は、両方のロックを放した後で行う。
    fn close_where(&self, key: &LineKey, owner: Option<LineOwner>) -> bool {
        let (_closed_line, taken) = {
            let mut lines = self.lock();
            let Some(line) = lines.get(key) else {
                return false;
            };
            let line = Arc::clone(line);
            let mut inner = line.lock();
            if let Some(owner) = owner {
                if inner.owner.is_some_and(|current| current != owner) {
                    return false;
                }
            }
            lines.remove(key);
            let transport = inner.transport.take();
            if inner.leased {
                inner.quarantined = true;
            }
            drop(inner);
            (line, transport)
        };
        #[cfg(test)]
        {
            let hook = self
                .after_close_unlock
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take();
            if let Some(hook) = hook {
                hook(&_closed_line);
            }
        }
        drop(taken);
        true
    }

    /// 開いている線の数（試験・診断）。
    pub fn open_count(&self) -> usize {
        self.lock().values().filter(|l| l.is_open()).count()
    }
}

/// [`LineTable::lease`] の予約（`leased = true`）を、貸し出しを作る前に抜けたら戻す番人。
/// 番人が持つのは予約した線そのもの。その線が [`LineTable::close`] / [`LineTable::close_by`]
/// で表から外れていても、外れた線の予約を戻すだけで、表の新しい線には触らない。
struct Reservation<'a> {
    line: &'a Line,
}

impl Reservation<'_> {
    /// 貸し出しを作った。予約は `LineLease` の drop が戻す。
    fn disarm(self) {
        std::mem::forget(self);
    }
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        self.line.lock().leased = false;
    }
}

fn quarantine_error(key: &LineKey) -> FatalError {
    FatalError::InvalidConfig(format!(
        "シリアルの線 {key} は、時間切れの後の同期の手順を持たないドライバでは使い直せません。\
         接続を止めて開き直してください"
    ))
}
