//! 収集サービス（#383 段階2b / R1-C の C-1）: `banto-collect` の
//! [`Collector`] をこのアプリの設定 DB・データディレクトリに配線する
//! サービス層。
//!
//! chronogazer はこれまで収集エンジンを一切起動していなかった（`banto-collect`
//! は R1-A で依存に足しただけで使用箇所 0）。ここはその**最初の配線**にあたる。
//!
//! # ここまでの範囲（C-1 → C-2）
//!
//! C-1（#406）はサービス層の骨格（`start`/`stop`/`restart`/`state`/読み出し）
//! までで、**誰も [`CollectorService::start`] を呼んでいなかった**。
//!
//! C-2（この変更）で足したのは**操作の口とその前提**だけ:
//!
//! * 起動時の自動開始（[`CollectorService::autostart`]）、
//! * ライフサイクル操作の**待ち時間の上限**（[`COLLECT_OPERATION_TIMEOUT`]）と、
//!   打ち切りを表す [`CollectOutcome::pending`]、
//! * そのために要る [`CollectorState::Starting`]。
//!
//! 残りは変わらず別枠:
//!
//! * 画面・現在値 API・イベント一覧は **C-3**、
//! * シミュレータハーネスと E2E は **C-4**、
//! * Hub 経由で受けている値の保存・合流は **段階3**（`crate::hub` は触らない）。
//!
//! # 扱う対象（段階2b）
//!
//! **SLMP / Modbus TCP の直結のみ**。Hub 接続は設定 KV（`hub.record`）のままの
//! 別建てで、`plc_connections` には何も足していない（マイグレーション無し）。
//!
//! # 状態の表現 — 「収集対象 0 件」は異常ではない
//!
//! [`banto_collect::Collector::start`] は接続 0 件の構成を
//! [`CollectError::Config`] で拒否する。エンジンにとっては正しい（開く
//! スキーマが無い）が、**利用者にとってはエラーではなく「まだ何も登録して
//! いない」**。そこでこのサービスは `Collector::start` を呼ぶ**前に**
//! [`banto_collect::CollectorConfig::tag_count`] を見て、0 件なら
//! [`CollectorState::NoTargets`] という**専用の状態**を返す - #332 / #385 で
//! 確立した「**空とエラーを別の状態にする**」規律
//! （docs/implementation-checklist.md §5 の 1 行目）をここでも守る。
//!
//! **数えるのはタグ**であって、グループでも接続でもない。利用者にとっての
//! 「収集対象」は**タグ 1 本 1 本**で、グループと接続はその入れ物にすぎない
//! （入れ物だけ作って中身がまだ無い、は設定作業の途中として普通に起こる）。
//! [`banto_collect::build_config`] は**タグが 1 本も無い有効グループも計画に
//! 残す**ので、`group_count()` で数えると「有効な接続 1 件・有効なグループ
//! 1 件・**有効タグ 0 件**」という構成が素通りし、読む物が何も無いのに PLC へ
//! 繋ぎに行って tstore まで開いてしまう（#406 レビュー P2）。
//! `tag_count() == 0` は**グループ 0 件・接続 0 件の場合も必ず含む**（タグは
//! グループの中にしか居ない）ので、判定はこの 1 本で足りる - 「起こす条件」を
//! 2 本に割らない（docs/implementation-checklist.md §5）。
//!
//! 状態は 5 つだけ（[`CollectorState`]）: 停止中 / **起動中** / 動作中 /
//! 収集対象なし / 起動失敗（理由付き）。起動失敗の `reason` は
//! [`CollectError`] の文言をそのまま載せる（分類して捨てない）。
//!
//! # 下位の失敗で上位の状態を書き換えない
//!
//! [`CollectorState`] が変わるのは **[`CollectorService`] 自身の
//! start/stop/restart のときだけ**。走り出した後の PLC 断・読み取り失敗・
//! append 失敗は `banto-collect` 側が `Bad` 品質フラグと `collect_events` の
//! イベントに変換して**回り続ける**設計なので（`banto-collect` の
//! `error.rs` / `task.rs` のモジュール doc）、それでこのサービスの状態を
//! 「停止」や「起動失敗」に落とすことはしない（#385 の規律 =
//! docs/implementation-checklist.md §5「状態を別軸に保つ」）。
//!
//! # ライフサイクルは専用タスクが所有する（ロックで守らない）
//!
//! [`Collector`] の**所有権そのもの**と、それに対応する状態遷移は
//! [`CollectorService`] ではなく**1 本の tokio タスク**（[`Lifecycle`]）が
//! 持つ。`start`/`stop`/`restart` と読み出しは、**コマンド（mpsc）+ 応答
//! （oneshot）**でそのタスクに依頼するだけで、呼び出し側は
//! [`Collector`] に指一本触れない。
//!
//! **なぜロックではなくタスクなのか（キャンセル安全性。#406 レビュー P2）**:
//! 以前はサービス側が `AsyncMutex<Option<Collector>>` を `take()` してから
//! `Collector::stop().await` していた。axum のハンドラは接続が切れれば
//! future を drop するので、**`take()` の後・`stop()` の完了前に呼び出し側が
//! 消える**ことが現実に起こる。そうなると `collector` は `None`・状態は
//! `Running` という食い違いが**そのまま残り**、しかも次の `stop()` は
//! 「走っていない」分岐に落ちて状態を直せない。さらに `Collector::stop()` は
//! **接続タスクの join と writer の最終 flush** なので、状態表示だけ直しても
//! 実体の停止は保証できない。ライフサイクル処理をタスク側に置けば、
//! **oneshot の受信側が落ちても送信が失敗するだけで、開始済みの処理は
//! 最後まで進む**（#400 で潰した「飛行中の操作」と同じ層の、キャンセル側）。
//!
//! この形から**ただで**出てくる保証:
//!
//! * タスクはコマンドを**逐次**処理するので、**新しい `start()` は前の
//!   `stop()` が完了するまで進まない**。二重起動の防止も「呼ばれた順に
//!   効く」も、ロックではなく**所有権と 1 本のキュー**が担保する。
//! * 読み出し（[`CollectorService::connection_status`] /
//!   [`CollectorService::current_values`]）も同じキューを通るので、
//!   **ライフサイクル操作の途中の [`Collector`] を覗くことがない** -
//!   返ってくるのは必ず「どれかの操作と操作の間」の姿で、状態と食い違わない。
//!   代償として、**起動処理の最中は読み出しがその完了まで待つ** - ライフ
//!   サイクル操作にだけ上限を付けた（[`COLLECT_OPERATION_TIMEOUT`]）のに対し、
//!   [`CollectorService::connection_status`] /
//!   [`CollectorService::current_values`] は**まだ無上限**。この PR では
//!   どちらも呼び出し口が無いので害は無いが、**C-3 でこれらを画面へ出すときに
//!   同じ上限を付けること**（付けないと、起動が固まっている間ポーリングが
//!   返らなくなる）。
//! * コマンドを**送る前**に呼び出し側が消えた場合は、そもそも何も起きない
//!   （キューに積まれていないので、タスクは知らないまま）。
//!
//! 残るロックは**状態ロック（葉）** [`CollectorContext::state`]
//! （`std::sync::Mutex`）だけで、**書くのはライフサイクルタスクだけ**。
//! `.await` をまたいで保持しないし、ここから他のロックを取らない。
//! ポーリング経路（[`CollectorService::state`]）は**キューを通らない**ので、
//! 起動中（tstore を開いている最中）でも待たされない。
//!
//! # 起動時の自動開始と、「収集を再起動」だけが反映の口であること
//!
//! アプリ（`src-tauri` の `setup()`）と LAN サーバー単体（`banto-serve` の
//! `main()`）は、どちらも起動時に [`CollectorService::autostart`] を spawn
//! する（docs/r1-plan.md の R1-C「起動時に build_config → start」）。
//! **失敗しても起動は止めない** - `crate::hub` の `resume()` と同じ流儀で、
//! 理由は [`CollectorState::StartFailed`] に残るので画面から見える。
//! **収集対象 0 件は失敗ではない**（[`CollectorState::NoTargets`]）。
//!
//! **レジストリ（接続・グループ・タグ）の変更では自動再起動しない。** 反映は
//! **明示的な「収集を再起動」操作**（[`CollectorService::restart`]、
//! `collect_restart`）だけで行う。docs/r1-plan.md の R1-C がそう定めている
//! のに加え、記録計としてはこちらが正しい: レジストリ CRUD のたびに
//! `restart()` を掛けると、**設定を 1 行直すたびに収集が途切れる**。タグの
//! 名前を 1 つ直す、しきい値を 1 つ足す、といった編集の最中に何度も
//! 収集が止まって立ち上がり直し、そのたびに tstore がローテーションして
//! ファイルが刻まれる。編集が一段落したところで利用者が 1 回押す方が、
//! 欠測も断片化も少ない。
//!
//! # 無応答への上限 - 「打ち切り」は「失敗」ではない
//!
//! ライフサイクル操作は [`COLLECT_OPERATION_TIMEOUT`] で**待つのをやめる**。
//! 上限そのものの根拠はその定数の doc を参照。ここで押さえておくのは**言い
//! 分け**（#400 で確立したもの）:
//!
//! * 打ち切ったのは**待ち時間**であって**操作ではない**。ライフサイクルは
//!   専用タスクが所有しているので、**待つのをやめてもタスク側の処理は最後まで
//!   進む**。
//! * したがって [`CollectOutcome::pending`] が `true` のとき、呼び出し側は
//!   「失敗しました」と言ってはいけない。言うべきは「**まだ終わっていない**」。
//! * 「今どうなっているか」は [`CollectorService::state`] が**キューを通らずに**
//!   答えるので、打ち切った側もその後の画面も、そこを見れば追いつける。
//!
//! そのために [`CollectorState::Starting`] がある。上限で打ち切ったあと
//! `state()` が `Stopped` を返すと**嘘になる**（起動処理はタスク側で続いて
//! いる）ので、タスクは**tstore を開く前に**`Starting` を立て、成否が
//! 決まってから `Running` / `NoTargets` / `StartFailed` に遷移する。
//!
//! # 同じ `data.dir` を 2 つのプロセスで開かないこと（未防止の制約）
//!
//! デスクトップアプリと `banto-serve` は**どちらも**起動時に自動開始するので、
//! **両方を同時に起動して同じ `data.dir` を指していると、2 つの収集エンジンが
//! 同じ時系列ファイル群へ書き込む**（同じ設定 DB を共有していれば同じ
//! `data.dir` を解決するので、これは十分に起こりうる）。`banto-tstore` は
//! プロセス間の排他を持たないため、結果は二重書き込み - 同じ時刻の行が
//! 2 回入る、ローテーションが互いのファイルを踏む、といった壊れ方をする。
//!
//! **この PR では防止機構を実装していない**（ロックファイル等は別途）。
//! 運用上の約束として、**同じ `data.dir` に対して収集するプロセスは 1 つだけ**
//! にすること。`banto-serve` はあくまで開発・E2E 用の単体サーバーなので、
//! デスクトップアプリと併走させない。
//!
//! # 終了フックからの停止
//!
//! `src-tauri` の `shutdown_app_state` も**同じ口**（[`CollectorService::stop`]）
//! を通る。あちらは後始末全体に 5 秒の予算があり、超えたら待つのをやめるが、
//! **やめるのは待つ側だけで、タスク側の停止処理（join と最終 flush）は
//! そのまま続く**。プロセスがその直後に終わるので実害は無い - 途中で
//! 切り上げても、失われうるのは最後の未 flush 分だけ（固まる方が悪い）。
//!
//! # 保持期間（`retention.days`）について
//!
//! `crate::settings::StoreSettings` が `data.dir` / `retention.days` を持つが、
//! **このモジュールのコードはファイルを一切削除しない**。期限超過ファイルの
//! 自動削除は docs/recorder-requirements.md §3.4 にある機能だが、**別途
//! 実装する**（誤って削除を先取りしない）。ここは設定値を持つだけ。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use banto_collect::{
    build_config, ClientFactory, CollectError, CollectEvent, Collector, CollectorOptions,
    ConnectionStatus, CurrentValuesHandle, EventSink,
};
use banto_core::BantoError;
use banto_tstore::{Clock, SystemClock};
use serde::Serialize;
use sqlx::SqlitePool;
use tokio::sync::{broadcast, mpsc, oneshot};

/// 収集サービスの状態。**必要最小限の 5 つ**だけ（語彙を増やさない）。
///
/// * [`Self::Stopped`] - 止まっている（まだ一度も起動していない、または
///   停止した）。
/// * [`Self::Starting`] - **起動処理の最中**（構成の組み立てと tstore を
///   開く往復）。C-2 で足した - [`COLLECT_OPERATION_TIMEOUT`] で待つのを
///   やめたあと `Stopped` を返すと嘘になるため（このモジュールの doc
///   「無応答への上限」）。ここから必ず `Running` / `NoTargets` /
///   `StartFailed` のどれかへ抜ける。
/// * [`Self::Running`] - 走っている。`groups`/`tags` は**起動時に実際に
///   採用された**収集対象の数。
/// * [`Self::NoTargets`] - **有効なタグが 1 件も無い**ので起動しなかった
///   （有効な接続やグループだけがあってタグが空、も含む）。**エラーでは
///   ない**（このモジュールの doc 参照）。
/// * [`Self::StartFailed`] - 起動を試みて失敗した。`reason` は
///   [`CollectError`] の文言そのまま。
///
/// **これ以上増やさない。** 停止中を表す `Stopping` は足していない: 停止は
/// 短い（接続タスクは `biased` select で停止を最優先に見ており、`connect()`
/// は別タスク）ので、停止の最中に `Running` のままに見えるのは**安全側の
/// 嘘**（「まだ止まっていないかもしれない」と読める）で済む。`Starting` は
/// そうはいかない - `Stopped` のままだと「もう何も動いていない」という
/// **危険側の嘘**になるので、こちらだけ足した。
///
/// `Serialize` はコマンド／REST で**そのまま**ワイヤに載せるため。
/// `crate::hub` が `HubStatus` を判別共用体のまま画面へ渡しているのと同じ
/// 作法（`{"state":"running","groups":2,"tags":10}`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum CollectorState {
    Stopped,
    Starting,
    Running { groups: usize, tags: usize },
    NoTargets,
    StartFailed { reason: String },
}

impl CollectorState {
    /// 走っているか。`state()` の呼び出し側が毎回 `matches!` を書かずに
    /// 済むだけの薄い述語。
    pub fn is_running(&self) -> bool {
        matches!(self, Self::Running { .. })
    }

    /// 監査ログ用の短い識別子。**`serde` が付けるタグと同じ綴り**（上の
    /// `#[serde(tag = "state", rename_all = "camelCase")]`）なので、監査で
    /// 見た値と画面で見た値が一致する。`crate::hub` の `HubStatus::as_str`
    /// と同じ役割。
    ///
    /// **理由（`StartFailed` の `reason`）も件数も載せない** - 監査の detail に
    /// 入れてよいのは「操作者」と「結果の状態」までで、接続先・ファイルパス
    /// といった内部の詳細を持ち込まないため（`reason` は [`CollectError`] の
    /// 文言そのままで、ホストやパスを含みうる）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::Starting => "starting",
            Self::Running { .. } => "running",
            Self::NoTargets => "noTargets",
            Self::StartFailed { .. } => "startFailed",
        }
    }
}

/// 監査ログ（`crate::audit`）で収集の操作に付ける `resource`。
///
/// **拒否も成功も、REST も Tauri も、必ずこの 1 つの値**。定数にして両方の
/// 経路から参照させているのは、#397 の P2-D で踏んだ穴を繰り返さないため:
/// あのときは Hub の拒否が REST 側 `"hub"` / Tauri 側 `"settings"` に割れて
/// いて、「hub への拒否だけ抽出する」フィルタが Tauri 分を黙って取りこぼして
/// いた。**同じ操作が経路によって別の `resource` にならない**ことを、
/// 申し合わせではなく 1 つの定数で担保する。
///
/// `action` の側は操作ごとに違う（`start` / `stop` / `restart`、拒否は
/// ガードが書く `denied`）ので、既存の語彙（`create`/`delete`/… と同じ
/// 「小文字の動詞」）に合わせて各呼び出し側が書く。
pub const COLLECT_AUDIT_RESOURCE: &str = "collect";

/// ライフサイクル操作（`start`/`stop`/`restart`）1 回の結末。
///
/// **`pending` は「失敗」ではない。** `true` は「[`COLLECT_OPERATION_TIMEOUT`]
/// まで待ったが、まだ終わっていない」という意味で、打ち切ったのは**待ち時間**
/// だけ - ライフサイクルタスク側の処理はそのまま最後まで進む（このモジュールの
/// doc「無応答への上限」/ #400 で確立した言い分け）。呼び出し側は
/// 「失敗しました」ではなく「**まだ終わっていません**」と言い、続きは
/// [`CollectorService::state`] で追うこと。
///
/// `status` は**打ち切った時点**の状態なので、`pending == true` のときはほぼ
/// [`CollectorState::Starting`]（あるいは、停止の最中なら直前の状態）になる。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectOutcome {
    pub status: CollectorState,
    pub pending: bool,
}

impl CollectOutcome {
    /// 操作が終わった。
    fn settled(status: CollectorState) -> Self {
        Self {
            status,
            pending: false,
        }
    }

    /// 上限まで待ったが、まだ終わっていない。
    fn still_working(status: CollectorState) -> Self {
        Self {
            status,
            pending: true,
        }
    }
}

/// ライフサイクルタスクへの依頼。応答はそれぞれの `oneshot` に返す。
///
/// **応答の受信側が落ちても（呼び出し側のキャンセル）、タスクは処理を
/// 最後まで続ける** - `send` が `Err` になるだけ。それがこの設計の要点
/// （このモジュールの doc「ライフサイクルは専用タスクが所有する」）。
enum Command {
    Start(oneshot::Sender<Result<CollectorState, BantoError>>),
    Stop(oneshot::Sender<Result<CollectorState, BantoError>>),
    Restart(oneshot::Sender<Result<CollectorState, BantoError>>),
    /// 読み出しも同じキューを通す（操作の途中の [`Collector`] を覗かない）。
    ConnectionStatus(oneshot::Sender<Option<HashMap<String, ConnectionStatus>>>),
    CurrentValues(oneshot::Sender<Option<CurrentValuesHandle>>),
}

/// コマンドキューの深さ。ライフサイクル操作は人間の操作由来で秒に何度も
/// 来るものではなく、読み出しのポーリングもこの深さで詰まることはない。
/// 満杯になったら `send().await` が待つ（= 背圧）だけで、取りこぼさない。
const COMMAND_QUEUE_DEPTH: usize = 32;

/// ライフサイクル操作（`start`/`stop`/`restart`）で**待つのをやめる**までの
/// 時間。打ち切りの意味は [`CollectOutcome`] の doc を参照（失敗ではない）。
///
/// **なぜ上限が要るのか**: 操作の口を生やした以上、ここは #400 で潰した
/// 「画面が永久に固まる」経路そのものになる。往復の相手はライフサイクル
/// タスクで、そのタスクは `TsWriter::open_with_options`（tstore を開く）を
/// `await` する - `reject` ではなく**無応答**になりうる層なので、上限が
/// 無ければ呼び出し側は永久に返ってこない。
///
/// **なぜ 30 秒か（`crate::hub` の 15/60 秒とは根拠が違う）**: Hub の上限は
/// 「別の機械への HTTP/WS 往復」に対する値だが、こちらが待っているのは
/// **同じプロセスの中のローカル I/O** だけ -
///
/// * `build_config`: 同居している SQLite（レジストリ）の読み取り、
/// * `Collector::start`: `data.dir` の下に tstore の書き手を開く、
/// * `Collector::stop`: 接続タスクの join と最終 flush。
///
/// PLC への接続は**含まれない**（接続は別タスクで、`start` はその完了を
/// 待たない）。健全なローカルディスクならどれもミリ秒で、秒を要する時点で
/// 既に異常である。
///
/// それでも 1 桁秒にしないのは、**`data.dir` が外付け・ネットワークドライブを
/// 指しうる**ため（[`resolve_data_dir`] の doc - 絶対パス指定の主目的が
/// まさにそれ）。応答しない SMB 共有への open は、OS が諦めるまで数十秒
/// 単位で止まることがある。3 秒で切ると「少し遅いだけの共有」を毎回
/// 見限ってしまい、逆に分単位にすると固まっているのと区別が付かない。
/// 30 秒は「遅いが必ず終わるローカル I/O」の上に十分あり、かつ
/// 「利用者がアプリを壊れたと判断する前」に収まる。
///
/// 終了時の後始末（`src-tauri` の `EXIT_CLEANUP_BUDGET` = 5 秒）は**この値
/// より短い**ので、終了経路ではそちらが先に打ち切る - 意図どおり（窓を閉じた
/// 利用者を 30 秒待たせない）。
pub const COLLECT_OPERATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// 収集エンジンを組み立てるのに要る材料と、外から読める状態。
/// **[`CollectorService`] とライフサイクルタスクの両方が `Arc` で持つ**
/// （[`Collector`] 本体だけはタスクの専有）。
struct CollectorContext {
    /// レジストリ（`plc_connections`/`collection_groups`/`tags`）と
    /// `collect_events` を同居させている、このアプリ唯一の SQLite プール。
    /// [`build_config`] の読み取り元であり、[`EventSink`] の書き込み先でもある。
    pool: SqlitePool,
    /// 時系列ファイル（`banto-tstore`）の置き場。設定 `data.dir` を
    /// [`resolve_data_dir`] で解決したもの。
    data_dir: PathBuf,
    /// ストアのローテーションと現在値の Stale 判定が**同じ「今」**を見るための
    /// 時計。本番は [`SystemClock`]。
    clock: Arc<dyn Clock>,
    /// イベントの二系統出力（live broadcast + `collect_events` テーブル）。
    /// **[`Collector`] ではなくこのサービスが所有する**ので、
    /// [`CollectorService::subscribe_events`] で取った受信ハンドルは
    /// start/stop をまたいで生き続ける（banto-hub の `CollectorManager` が
    /// rebuild をまたいで `EventSink` を使い回しているのと同じ理由）。
    events: EventSink,
    options: CollectorOptions,
    /// 接続タスクが再接続に使うクライアントの生成口。本番は
    /// [`banto_collect::default_client_factory`]（SLMP / Modbus TCP の直結）。
    /// テストだけが差し替える。
    factory: ClientFactory,
    /// **状態ロック（葉）**。**書くのは [`Lifecycle`] タスクだけ**なので、
    /// `Collector` の実体と食い違わない。読み取りはどこからでもよい
    /// （[`CollectorService::state`] はキューを通らない）。
    state: Mutex<CollectorState>,
    /// **テスト専用**（製品ビルドにはフィールドごと存在しない）: 起動処理を
    /// 「[`CollectorState::Starting`] を立てた直後・tstore を開く前」で
    /// 止められるゲート。[`COLLECT_OPERATION_TIMEOUT`] の受入条件
    /// （「解決しない起動」で打ち切りが返り、状態が `Starting` のまま残り、
    /// その後タスク側が完了すると追いつく）を、**実時間を待たずに**
    /// 固定するためだけのもの。
    ///
    /// 偽 [`ClientFactory`] では作れない: `Collector::start` が `await` する
    /// のは `TsWriter` を開く往復で、`factory` が呼ばれるのは**その後に
    /// spawn される接続タスクの中**だから（`banto_collect::collector` の
    /// `start_with_client_factory`）。止めたい一点がクライアント生成の手前に
    /// あるので、ここに入れるしかない。
    #[cfg(test)]
    start_gate: Option<Arc<StartGate>>,
}

/// **テスト専用**: [`CollectorContext::start_gate`] の実体。`entered` で
/// 「起動がその一点まで来た」ことを、`release` で「進んでよい」ことを
/// 待ち合わせる。`Notify::notify_one` は待ち手が居なければ permit を
/// 溜めるので、どちらが先でも取りこぼさない（テストモジュールの
/// `StopGate` と同じ作り）。
#[cfg(test)]
#[derive(Default)]
struct StartGate {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

impl CollectorContext {
    fn state(&self) -> CollectorState {
        self.state
            .lock()
            .expect("collector state lock poisoned")
            .clone()
    }

    fn set_state(&self, state: CollectorState) {
        *self.state.lock().expect("collector state lock poisoned") = state;
    }
}

/// [`CollectorService`] の実体。`CollectorService` はこれへの `Arc` 1 本だけを
/// 持つので、`Clone` しても**同じ収集エンジン**を指す（デスクトップの
/// コマンドと LAN の REST が別々のエンジンを立てない）。
struct CollectorInner {
    ctx: Arc<CollectorContext>,
    /// ライフサイクルタスクへのコマンド口。**初回の非同期操作で spawn する**
    /// （[`CollectorService::new`] は `tauri` の `setup` から**ランタイムの
    /// 外**で呼ばれるので、そこで `tokio::spawn` すると panic する）。
    /// `OnceLock` なので二重には立たない。
    commands: OnceLock<mpsc::Sender<Command>>,
}

/// 収集エンジンのサービス層。`src-tauri` の `collect_*` コマンドと
/// `crate::rest` の `/api/collect*` ルーターが共有する - 他のサービスと同じ
/// 「service 層は tauri も axum も知らない」規約。
#[derive(Clone)]
pub struct CollectorService {
    inner: Arc<CollectorInner>,
}

impl CollectorService {
    /// 収集サービスを組み立てる。**この時点では何も起動しない**
    /// （`data_dir` も作らない。実際に開くのは [`Self::start`] の中の
    /// `TsWriter`）。
    ///
    /// `clock` を引数に取らないのは、`src-tauri`（このアプリ自身の新規依存を
    /// 持たない、という invariant）が [`SystemClock`] を名指しできないため。
    /// 本番の時計は 1 つしかないので、差し替えはテスト専用の
    /// `new_for_test` に閉じている。
    pub fn new(pool: SqlitePool, data_dir: PathBuf) -> Self {
        Self::build(
            pool.clone(),
            data_dir,
            Arc::new(SystemClock),
            CollectorOptions::default(),
            banto_collect::default_client_factory(),
        )
    }

    fn build(
        pool: SqlitePool,
        data_dir: PathBuf,
        clock: Arc<dyn Clock>,
        options: CollectorOptions,
        factory: ClientFactory,
    ) -> Self {
        let events = EventSink::new(pool.clone());
        Self::from_context(CollectorContext {
            pool,
            data_dir,
            clock,
            events,
            options,
            factory,
            state: Mutex::new(CollectorState::Stopped),
            #[cfg(test)]
            start_gate: None,
        })
    }

    fn from_context(ctx: CollectorContext) -> Self {
        Self {
            inner: Arc::new(CollectorInner {
                ctx: Arc::new(ctx),
                commands: OnceLock::new(),
            }),
        }
    }

    /// 起動時の自動開始（docs/r1-plan.md の R1-C「起動時に build_config →
    /// start」）。`src-tauri` の `setup()` と `banto-serve` の `main()` が
    /// **共通で通る唯一の口**で、どちらも `spawn` して投げっぱなしにする
    /// （`crate::hub` の `resume()` と同じ形 - 起動を待たせない）。
    ///
    /// **失敗しても起動は止めない**。握り潰しているように見えるが理由は
    /// 捨てていない - [`CollectorState::StartFailed`] に残るので
    /// `collect_status` / `GET /api/collect` から必ず見える。**収集対象 0 件は
    /// 失敗ではない**（[`CollectorState::NoTargets`]）ので何も言わない。
    ///
    /// 打ち切り（[`CollectOutcome::pending`]）も失敗ではないので、
    /// 「まだ終わっていない」とだけ言う。
    ///
    /// **レジストリが変わってもここは二度と呼ばれない** - 反映は明示的な
    /// [`Self::restart`] だけ（理由はこのモジュールの doc）。
    pub async fn autostart(&self) {
        match self.start().await {
            Ok(outcome) if outcome.pending => eprintln!(
                "banto: 起動時の収集の開始が{}秒以内に終わりませんでした（開始処理は続いています。状態は収集の状態表示で確認してください）",
                COLLECT_OPERATION_TIMEOUT.as_secs()
            ),
            Ok(_) => {}
            Err(err) => eprintln!("banto: 起動時の収集の開始に失敗しました: {err}"),
        }
    }

    /// 収集を開始する。
    ///
    /// 1. レジストリから構成を作る（[`build_config`]）。
    /// 2. **有効タグが 0 件なら [`CollectorState::NoTargets`]**（`Ok`）。
    ///    `Collector::start` には渡さない - このモジュール doc 参照。
    /// 3. それ以外は [`Collector`] を起動して [`CollectorState::Running`]。
    ///
    /// **既に走っているときは何もしない**（現在の状態をそのまま返す）。
    /// 二重起動を防いでいるのはロックではなく、ライフサイクルタスクが
    /// コマンドを**逐次**処理すること（このモジュール doc 参照）。
    ///
    /// `Err` を返すのは「起動を試みて失敗した」ときだけ。そのとき状態は
    /// [`CollectorState::StartFailed`] になり、**理由が残る**。
    ///
    /// [`COLLECT_OPERATION_TIMEOUT`] で待つのをやめた場合は `Err` ではなく
    /// [`CollectOutcome::pending`] が立つ（**打ち切りは失敗ではない**）。
    /// そのとき状態は [`CollectorState::Starting`] のままで、起動処理は
    /// タスク側で続いている。
    pub async fn start(&self) -> Result<CollectOutcome, BantoError> {
        self.lifecycle(Command::Start).await
    }

    /// 収集を停止する。**走っていなければ何もしない**（冪等）- 状態にも
    /// 触らないので、[`CollectorState::NoTargets`] や
    /// [`CollectorState::StartFailed`] の理由が `stop()` で消えることはない。
    ///
    /// `Err` は「最終 flush に失敗した」ときだけ返る。その場合でも
    /// **エンジンは確かに止まっている**ので状態は
    /// [`CollectorState::Stopped`] にする（状態は現実を写す）。
    ///
    /// **この `await` を途中でやめても停止は進む**（依頼はもうタスク側にある）。
    /// 終了フックの 5 秒予算が切れたときに起こるのがまさにこれで、待つのを
    /// やめるだけで join と最終 flush は続く。[`COLLECT_OPERATION_TIMEOUT`]
    /// で打ち切った場合も同じで、[`CollectOutcome::pending`] が立つだけ。
    pub async fn stop(&self) -> Result<CollectOutcome, BantoError> {
        self.lifecycle(Command::Stop).await
    }

    /// 停止してから開始する。**間に他の `start`/`stop` を割り込ませない** -
    /// タスク側で「停止 → 開始」を 1 つのコマンドとして処理するので、
    /// `stop().await` → `start().await` と 2 回に分けたときのような隙間が
    /// そもそも無い。
    ///
    /// 停止側の失敗（最終 flush）は**開始を中止する理由にしない** - ログに
    /// 出して開始へ進む。落ちるのは旧ファイルの未 flush 分だけで、それは
    /// 「新しい構成で収集を再開できるか」とは無関係だから（`banto-collect` の
    /// `apply_config` が同じ天秤で同じ側を選んでいる）。
    ///
    /// **レジストリ（接続・グループ・タグ）の変更を収集へ反映する唯一の口**
    /// でもある - CRUD が自動でこれを呼ぶことはしない（理由はこのモジュールの
    /// doc「起動時の自動開始と、「収集を再起動」だけが反映の口であること」）。
    pub async fn restart(&self) -> Result<CollectOutcome, BantoError> {
        self.lifecycle(Command::Restart).await
    }

    /// 現在の状態。**ネットワークもディスクも DB も触らない**し、コマンド
    /// キューも通らないので、起動処理の最中でも待たされない（ポーリングは
    /// これを見る）。
    pub fn state(&self) -> CollectorState {
        self.inner.ctx.state()
    }

    /// 接続ごとの状態のスナップショット。**走っていなければ `None`** -
    /// 空の `HashMap` を返して「接続 0 件」と混同させない
    /// （docs/implementation-checklist.md §5）。
    ///
    /// ライフサイクルタスクに問い合わせるので、**飛行中の start/stop が
    /// 終わってから**answer が返る（操作の途中の [`Collector`] は見えない）。
    pub async fn connection_status(&self) -> Option<HashMap<String, ConnectionStatus>> {
        self.readout(Command::ConnectionStatus).await
    }

    /// 現在値キャッシュのハンドル。**走っていなければ `None`**（同上 -
    /// 「値がまだ無い」キャッシュを返して「0 件」に潰さない）。
    pub async fn current_values(&self) -> Option<CurrentValuesHandle> {
        self.readout(Command::CurrentValues).await
    }

    /// 収集イベントの live 購読。
    ///
    /// ここだけ `Option` ではないのは、[`EventSink`] を**[`Collector`] では
    /// なくこのサービスが所有している**から: 購読者は start/stop をまたいで
    /// 同じ受信ハンドルを使い続けられ、`collection_started` を取りこぼさない。
    /// 「イベントが流れてこない」＝「走っていない」ではないので、走っているか
    /// どうかは [`Self::state`] を見ること。
    pub fn subscribe_events(&self) -> broadcast::Receiver<CollectEvent> {
        self.inner.ctx.events.subscribe()
    }

    // --- ライフサイクルタスクへの依頼 ------------------------------------

    /// ライフサイクルタスクへのコマンド口。**初回の呼び出しで spawn する** -
    /// [`Self::new`] は `tauri` の `setup`（tokio ランタイムの外）から呼ばれる
    /// ので、そこで `tokio::spawn` はできない。ここへ来る経路は全部 `async fn`
    /// の中なので、必ずランタイムの上にいる。
    fn commands(&self) -> &mpsc::Sender<Command> {
        self.inner.commands.get_or_init(|| {
            let (tx, rx) = mpsc::channel(COMMAND_QUEUE_DEPTH);
            let lifecycle = Lifecycle {
                ctx: self.inner.ctx.clone(),
                collector: None,
            };
            tokio::spawn(lifecycle.run(rx));
            tx
        })
    }

    /// `start`/`stop`/`restart` 共通の往復。**[`COLLECT_OPERATION_TIMEOUT`]
    /// で必ず打ち切る**（ここが「操作の口を生やしたのに無応答で固まる」を
    /// 塞ぐ唯一の場所なので、3 つとも同じ 1 本を通す）。
    ///
    /// 打ち切りは `Err` にしない - [`CollectOutcome::pending`] を立てて、
    /// **その時点の [`Self::state`]**（キューを通らない同期読み取り）を
    /// 載せて返す。「失敗した」と「まだ終わっていない」を別の値にしておく
    /// ための分け方（[`CollectOutcome`] の doc）。
    ///
    /// **上限は往復全体に掛かる**ので、稀に「まだキューに積めていない」
    /// 時点で打ち切ることもある。そのとき依頼は行われていないが、キューが
    /// 詰まるのは前の依頼が終わっていないからなので、「まだ終わっていない」
    /// という言い分けはどちらでも正しい（状態も `Self::state` が答える）。
    ///
    /// タスクが居なくなっていたら（= panic した。通常は起こらない）
    /// **黙って成功にしない**。
    async fn lifecycle(
        &self,
        make: fn(oneshot::Sender<Result<CollectorState, BantoError>>) -> Command,
    ) -> Result<CollectOutcome, BantoError> {
        match tokio::time::timeout(COLLECT_OPERATION_TIMEOUT, self.request(make)).await {
            Ok(Some(result)) => result.map(CollectOutcome::settled),
            Ok(None) => Err(BantoError::Other(
                "収集サービスの内部タスクが停止しています（アプリを再起動してください）"
                    .to_string(),
            )),
            Err(_elapsed) => Ok(CollectOutcome::still_working(self.state())),
        }
    }

    /// 読み出し共通の往復。タスクが居ないときは「走っていない」と同じ `None`
    /// になる - 走っていないのは事実（タスクごと消えているので誰も収集して
    /// いない）なので、ここは潰していることにならない。
    async fn readout<T>(&self, make: fn(oneshot::Sender<Option<T>>) -> Command) -> Option<T> {
        self.request(make).await.flatten()
    }

    /// コマンドを 1 つ送って応答を待つ。`None` = タスクが居ない / 応答が
    /// 返らなかった。
    async fn request<T>(&self, make: fn(oneshot::Sender<T>) -> Command) -> Option<T> {
        let (reply_tx, reply_rx) = oneshot::channel();
        // ここでキャンセルされた場合（まだ送れていない）は、タスクは依頼を
        // 知らないまま = 何も起きない。送れた後は、待つのをやめても処理は
        // 最後まで進む。
        self.commands().send(make(reply_tx)).await.ok()?;
        reply_rx.await.ok()
    }

    /// テスト専用の組み立て口: 時計・チューニング・クライアント生成口を
    /// 差し替える。本番の [`Self::new`] を 1 本に保ったまま、テストが
    /// 短いタイムアウトと偽クライアントを使えるようにするためだけのもの。
    #[cfg(test)]
    fn new_for_test(
        pool: SqlitePool,
        data_dir: PathBuf,
        options: CollectorOptions,
        factory: ClientFactory,
    ) -> Self {
        Self::build(pool, data_dir, Arc::new(SystemClock), options, factory)
    }

    /// [`Self::new_for_test`] と同じだが、起動処理を [`StartGate`] で
    /// 止められる（[`CollectorContext::start_gate`] の doc 参照）。
    #[cfg(test)]
    fn new_for_test_gated(
        pool: SqlitePool,
        data_dir: PathBuf,
        options: CollectorOptions,
        factory: ClientFactory,
        gate: Arc<StartGate>,
    ) -> Self {
        let events = EventSink::new(pool.clone());
        Self::from_context(CollectorContext {
            pool,
            data_dir,
            clock: Arc::new(SystemClock),
            events,
            options,
            factory,
            state: Mutex::new(CollectorState::Stopped),
            start_gate: Some(gate),
        })
    }
}

/// 走っている [`Collector`] を**専有する**タスクの中身。
///
/// [`CollectorService`] のどのメソッドもここへコマンドを送るだけなので、
/// `Collector` はこの構造体の外に一度も出ない（`Arc` にも `Mutex` にも
/// 入っていない）。状態を書くのもここだけ。
struct Lifecycle {
    ctx: Arc<CollectorContext>,
    /// `None` = 走っていない。[`Collector::stop`] が `self` を消費するので
    /// `Option` + `take()`。
    collector: Option<Collector>,
}

impl Lifecycle {
    /// コマンドを**逐次**処理する。1 つのコマンドを処理している間、次は
    /// 受け取らない - これが「新しい `start()` が前の `stop()` の完了前に
    /// 進まない」の全てで、そのためのロックは 1 つも要らない。
    ///
    /// [`CollectorService`] が全部 drop されると `Sender` が落ち、この
    /// ループが抜けてタスクが終わる。そのとき走っている [`Collector`] は
    /// `stop()` されずに drop される（接続タスクは切り離される）が、これは
    /// サービスごと捨てられる場面 = プロセス終了時だけで、正規の終了経路は
    /// `shutdown_app_state` が明示的に [`CollectorService::stop`] を通る。
    async fn run(mut self, mut commands: mpsc::Receiver<Command>) {
        while let Some(command) = commands.recv().await {
            match command {
                // `send` の `Err`（呼び出し側が待つのをやめた）は捨てる。
                // **処理そのものはもう終わっている**ので、誰も受け取らなく
                // ても状態と実体は一致している。
                Command::Start(reply) => {
                    let _ = reply.send(self.start().await);
                }
                Command::Stop(reply) => {
                    let _ = reply.send(self.stop().await);
                }
                Command::Restart(reply) => {
                    let _ = reply.send(self.restart().await);
                }
                Command::ConnectionStatus(reply) => {
                    let _ = reply.send(self.collector.as_ref().map(|c| c.status()));
                }
                Command::CurrentValues(reply) => {
                    let _ = reply.send(self.collector.as_ref().map(|c| c.current_values()));
                }
            }
        }
    }

    async fn start(&mut self) -> Result<CollectorState, BantoError> {
        // 二重起動の防止。逐次処理なので「見た直後に誰かが起動していた」は
        // 起こらない。
        if self.collector.is_some() {
            return Ok(self.ctx.state());
        }

        // **tstore を開く前に**「起動中」を立てる。呼び出し側が
        // [`COLLECT_OPERATION_TIMEOUT`] で待つのをやめたあと `state()` が
        // `Stopped` を返すと嘘になる（起動処理はここで続いている）ため -
        // このモジュールの doc「無応答への上限」。ここから必ず
        // `Running` / `NoTargets` / `StartFailed` のどれかへ抜ける。
        self.ctx.set_state(CollectorState::Starting);

        // **テスト専用**の一時停止点。製品ビルドにはこのブロックごと
        // 存在しない（`CollectorContext::start_gate` の doc）。
        #[cfg(test)]
        if let Some(gate) = self.ctx.start_gate.clone() {
            gate.entered.notify_one();
            gate.release.notified().await;
        }

        let config = match build_config(&self.ctx.pool).await {
            Ok(config) => config,
            Err(err) => return Err(self.fail_start(err)),
        };

        // 「収集対象なし」は **`Collector::start` に渡す前に**分岐する。
        // 渡すと `CollectError::Config` になり、本物の構成エラー（アドレスが
        // 解釈できない等）と同じ入れ物に入ってしまう。**数えるのはタグ** -
        // `build_config` はタグが空の有効グループも計画に残すので、
        // `group_count()` では「有効グループ 1・有効タグ 0」を素通りさせて
        // しまう（#406 レビュー P2。このモジュール doc 参照）。
        if config.tag_count() == 0 {
            self.ctx.set_state(CollectorState::NoTargets);
            return Ok(CollectorState::NoTargets);
        }

        let groups = config.group_count();
        let tags = config.tag_count();
        let collector = match Collector::start_with_client_factory(
            config,
            &self.ctx.data_dir,
            self.ctx.clock.clone(),
            self.ctx.events.clone(),
            self.ctx.options,
            self.ctx.factory.clone(),
        )
        .await
        {
            Ok(collector) => collector,
            Err(err) => return Err(self.fail_start(err)),
        };

        // **成功を確かめてから**状態を上げる（先に Running にして失敗時に
        // 降ろす、という順序にしない - docs/implementation-checklist.md §6
        // の「失敗経路での状態の落とし方」）。
        self.collector = Some(collector);
        let state = CollectorState::Running { groups, tags };
        self.ctx.set_state(state.clone());
        Ok(state)
    }

    async fn stop(&mut self) -> Result<CollectorState, BantoError> {
        let Some(collector) = self.collector.take() else {
            // 走っていない: **何もしない**。状態も触らない（`NoTargets` /
            // `StartFailed` の理由を握り潰さないため）。
            return Ok(self.ctx.state());
        };

        // ここから先は誰にも中断されない（呼び出し側が消えてもこのタスクは
        // 生きている）ので、`take()` 済み・状態は `Running` のまま、という
        // 食い違いが残ることはない。
        let result = collector.stop().await;
        // 止まったことは確定なので、flush の成否に関わらず `Stopped` にする。
        self.ctx.set_state(CollectorState::Stopped);
        match result {
            Ok(()) => Ok(CollectorState::Stopped),
            Err(err) => Err(collect_error(err)),
        }
    }

    async fn restart(&mut self) -> Result<CollectorState, BantoError> {
        if let Err(err) = self.stop().await {
            eprintln!(
                "banto: 収集の再起動中、停止側の後始末に失敗しました（開始は続行します）: {err}"
            );
        }
        self.start().await
    }

    /// 起動の失敗を状態に焼き付けて、同じ理由を `Err` として返す。
    /// **[`CollectError`] の文言をそのまま捨てない**。
    fn fail_start(&self, err: CollectError) -> BantoError {
        let reason = err.to_string();
        self.ctx.set_state(CollectorState::StartFailed {
            reason: reason.clone(),
        });
        collect_error_with_reason(err, reason)
    }
}

/// [`CollectError`] をこのアプリの共通エラー型へ。`Registry` は元々
/// [`BantoError`] なのでそのまま戻し（検証エラーの `field_errors` を
/// 文字列に潰さない）、それ以外は文言を保つ。
fn collect_error(err: CollectError) -> BantoError {
    let reason = err.to_string();
    collect_error_with_reason(err, reason)
}

fn collect_error_with_reason(err: CollectError, reason: String) -> BantoError {
    match err {
        CollectError::Registry(inner) => inner,
        _ => BantoError::Other(reason),
    }
}

/// 設定 `data.dir` を実際のディレクトリへ解決する。
///
/// 既定値は banto-hub に倣った相対パス `"./data"` なので、そのまま使うと
/// **プロセスの作業ディレクトリ**（デスクトップアプリでは何であるか分からない
/// 場所）に時系列ファイルを作ってしまう。そこで**相対パスは `base`
/// （アプリのデータディレクトリ）からの相対**として解決する。絶対パスが
/// 設定されていればそれをそのまま使う（運用で外付けドライブを指したい、が
/// この設定の主目的）。
///
/// 空文字・空白だけは「未設定」とみなし、既定の `"./data"` と同じ扱いにする。
pub fn resolve_data_dir(base: &Path, configured: &str) -> PathBuf {
    let trimmed = configured.trim();
    if trimmed.is_empty() {
        return base.join("data");
    }
    let path = PathBuf::from(trimmed);
    if path.is_absolute() {
        path
    } else {
        base.join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::init_db_memory;
    use crate::test_support::TempDir;
    use banto_collect::BackoffConfig;
    use banto_plc::{BoxFuture, PlcClient, PlcError, ReadRequest, ReadResult, TagValue};
    use banto_tags::{
        CollectionGroupInput, CollectionGroupService, PlcConnectionInput, PlcConnectionService,
        TagInput, TagService,
    };
    use banto_tstore::WriterOptions;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;
    use tokio::sync::Notify;

    /// 速い・決定的なチューニング。`banto-collect` の `tests/integration.rs`
    /// の `fast_options` と同じ意図（**テストの中で実時間を待たない**ための
    /// もので、待ち合わせのための sleep はどのテストにも書いていない）。
    fn fast_options() -> CollectorOptions {
        CollectorOptions {
            backoff: BackoffConfig {
                base: Duration::from_millis(20),
                max: Duration::from_millis(100),
            },
            connect_timeout: Duration::from_millis(100),
            response_timeout: Duration::from_millis(100),
            writer_options: WriterOptions::default(),
        }
    }

    /// 接続しに行かない偽クライアント。**実 PLC を模さない**（一巡の確認は
    /// C-4 のハーネスの仕事）が、「収集対象があるときに `Collector` が確かに
    /// 立ち上がる」を実ネットワーク無しで押さえるために使う。
    struct OfflineClient;

    impl PlcClient for OfflineClient {
        fn connect(&mut self) -> BoxFuture<'_, Result<(), PlcError>> {
            Box::pin(async { Ok(()) })
        }

        fn read_batch<'a>(
            &'a mut self,
            requests: &'a [ReadRequest],
        ) -> BoxFuture<'a, Result<Vec<ReadResult>, PlcError>> {
            Box::pin(async move {
                Ok(requests
                    .iter()
                    .map(|_| ReadResult::Value(TagValue::F64(1.0)))
                    .collect())
            })
        }

        fn disconnect(&mut self) -> BoxFuture<'_, ()> {
            Box::pin(async {})
        }
    }

    fn offline_factory() -> ClientFactory {
        Arc::new(|_spec| Box::new(OfflineClient) as Box<dyn PlcClient>)
    }

    /// 停止の途中で**確実に**止まってくれるゲート。接続タスクの graceful
    /// exit は `client.disconnect().await` を通るので、そこを塞ぐと
    /// [`Collector::stop`]（接続タスクの join → 最終 flush）が中で止まる。
    ///
    /// **1 回だけ**効く（`armed`）: 同じテストの中で立て直した 2 本目の
    /// エンジンの後始末まで塞ぐと、テストが終われなくなるため。
    ///
    /// 実時間は一切待たない - 「入った」も「開けた」も [`Notify`] で
    /// 待ち合わせる（`notify_one` は待ち手が居なければ permit を溜めるので、
    /// どちらが先でも取りこぼさない）。
    struct StopGate {
        entered: Notify,
        release: Notify,
        armed: AtomicBool,
    }

    impl StopGate {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                entered: Notify::new(),
                release: Notify::new(),
                armed: AtomicBool::new(true),
            })
        }

        /// 偽クライアントの `disconnect` から呼ばれる。
        async fn pass(&self) {
            if self.armed.swap(false, Ordering::SeqCst) {
                self.entered.notify_one();
                self.release.notified().await;
            }
        }

        /// 停止処理が「実体の停止」の途中まで進んだことを待つ。
        async fn wait_entered(&self) {
            self.entered.notified().await;
        }

        fn release(&self) {
            self.release.notify_one();
        }
    }

    /// [`OfflineClient`] と同じだが、切断だけ [`StopGate`] を通る。
    struct GatedClient {
        gate: Arc<StopGate>,
    }

    impl PlcClient for GatedClient {
        fn connect(&mut self) -> BoxFuture<'_, Result<(), PlcError>> {
            Box::pin(async { Ok(()) })
        }

        fn read_batch<'a>(
            &'a mut self,
            requests: &'a [ReadRequest],
        ) -> BoxFuture<'a, Result<Vec<ReadResult>, PlcError>> {
            Box::pin(async move {
                Ok(requests
                    .iter()
                    .map(|_| ReadResult::Value(TagValue::F64(1.0)))
                    .collect())
            })
        }

        fn disconnect(&mut self) -> BoxFuture<'_, ()> {
            let gate = self.gate.clone();
            Box::pin(async move { gate.pass().await })
        }
    }

    fn gated_factory(gate: &Arc<StopGate>) -> ClientFactory {
        let gate = gate.clone();
        Arc::new(move |_spec| Box::new(GatedClient { gate: gate.clone() }) as Box<dyn PlcClient>)
    }

    /// 「[`COLLECT_OPERATION_TIMEOUT`] に掛からずに終わった」ことを確かめて、
    /// 結果の状態だけ取り出す。打ち切りを扱わないテスト（この下のほとんど）が
    /// **`pending` を見落とさない**ようにするための一手間 - 素通しにすると、
    /// 上限が誤って効いている回帰を「状態が違う」という遠い形でしか
    /// 検出できない。
    fn settled(outcome: CollectOutcome) -> CollectorState {
        assert!(
            !outcome.pending,
            "上限に掛からない前提のテストで打ち切られた: {outcome:?}"
        );
        outcome.status
    }

    /// いま溜まっているイベントの種類を全部取り出す（待たない）。
    fn drain(rx: &mut broadcast::Receiver<CollectEvent>) -> Vec<banto_collect::EventKind> {
        let mut kinds = Vec::new();
        while let Ok(event) = rx.try_recv() {
            kinds.push(event.kind);
        }
        kinds
    }

    fn kind_count(kinds: &[banto_collect::EventKind], kind: banto_collect::EventKind) -> usize {
        kinds.iter().filter(|k| **k == kind).count()
    }

    fn count_kind(
        rx: &mut broadcast::Receiver<CollectEvent>,
        kind: banto_collect::EventKind,
    ) -> usize {
        kind_count(&drain(rx), kind)
    }

    async fn service(dir: &TempDir) -> (SqlitePool, CollectorService) {
        let pool = init_db_memory().await.expect("init_db_memory");
        let svc = CollectorService::new_for_test(
            pool.clone(),
            dir.path().join("data"),
            fast_options(),
            offline_factory(),
        );
        (pool, svc)
    }

    /// [`service`] と同じだが、切断を [`StopGate`] で塞げるサービス。
    async fn gated_service(dir: &TempDir) -> (SqlitePool, CollectorService, Arc<StopGate>) {
        let pool = init_db_memory().await.expect("init_db_memory");
        let gate = StopGate::new();
        let svc = CollectorService::new_for_test(
            pool.clone(),
            dir.path().join("data"),
            fast_options(),
            gated_factory(&gate),
        );
        (pool, svc, gate)
    }

    /// 指定した種類のイベントが来るまで待つ。**実時間を待たない**
    /// （broadcast の受信で待ち合わせるだけ）。
    async fn wait_for(rx: &mut broadcast::Receiver<CollectEvent>, kind: banto_collect::EventKind) {
        loop {
            let event = rx.recv().await.expect("イベント購読が切れた");
            if event.kind == kind {
                return;
            }
        }
    }

    /// 有効な接続 1 と、その下の**有効グループ 1**をレジストリに入れる
    /// （**タグは作らない**）。グループ id を返すので、テストが自分で
    /// 「タグ 0 件」「無効タグだけ」といった構成を組める。
    async fn seed_enabled_group(pool: &SqlitePool) -> i64 {
        let conn = PlcConnectionService::new(pool.clone())
            .create(PlcConnectionInput {
                name: "PLC1".to_string(),
                protocol: "modbus-tcp".to_string(),
                // 127.0.0.1:1 は何も listen していない予約ポート。
                // `offline_factory` を使うのでそもそも誰も dial しない。
                host: "127.0.0.1".to_string(),
                port: 1,
                unit_id: 1,
                enabled: true,
                simulation: false,
                word_order: "low_high".to_string(),
                database: None,
                username: None,
                password: None,
            })
            .await
            .expect("create plc connection");
        let group = CollectionGroupService::new(pool.clone())
            .create(CollectionGroupInput {
                name: "G1".to_string(),
                plc_connection_id: conn.id,
                period_ms: 1000,
                enabled: true,
                default_writable: true,
                query_sql: None,
            })
            .await
            .expect("create collection group");
        group.id
    }

    /// [`seed_enabled_group`] のグループにタグを 1 本足す。
    /// `address` を呼び出し側が決められるので、「解釈できないアドレス」で
    /// 構成組み立ての失敗も作れる。`enabled` で無効タグも作れる。
    async fn seed_tag(pool: &SqlitePool, group_id: i64, address: &str, enabled: bool) {
        TagService::new(pool.clone())
            .create(TagInput {
                name: "T1".to_string(),
                collection_group_id: group_id,
                address: address.to_string(),
                data_type: "i16".to_string(),
                string_length: None,
                string_encoding: "utf8".to_string(),
                raw_lo: None,
                raw_hi: None,
                eng_lo: None,
                eng_hi: None,
                unit: None,
                decimals: 0,
                threshold_h: None,
                threshold_hh: None,
                threshold_l: None,
                threshold_ll: None,
                enabled,
                writable: false,
                tag_kind: "plc".to_string(),
                expression: None,
                retain: false,
                expected_revision: None,
            })
            .await
            .expect("create tag");
    }

    /// 有効な接続 1 / グループ 1 / **有効タグ 1** - 「走る」構成。
    async fn seed_one_tag(pool: &SqlitePool, address: &str) {
        let group_id = seed_enabled_group(pool).await;
        seed_tag(pool, group_id, address, true).await;
    }

    /// **この PR の一番大事な受入条件**: 有効な収集対象が 1 件も無いときは
    /// 「収集対象なし」であって、**エラーではない**。
    ///
    /// 反証（回帰の検出）: `Lifecycle::start` の `tag_count() == 0` 分岐を
    /// 消して `Collector::start` の `CollectError::Config` をそのまま返す実装に
    /// 戻すと、`start()` が `Err` を返すのでこのテストは `expect` で落ちる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn no_enabled_targets_is_a_state_of_its_own_not_an_error() {
        let dir = TempDir::new();
        let (_pool, svc) = service(&dir).await;

        let state = settled(
            svc.start()
                .await
                .expect("収集対象 0 件は「エラー」ではない（Ok が返る）"),
        );
        assert_eq!(state, CollectorState::NoTargets);
        assert_eq!(svc.state(), CollectorState::NoTargets);
        // 「走っていない」ことが読み出し側からも分かる（空に潰さない）。
        assert!(svc.connection_status().await.is_none());
        assert!(svc.current_values().await.is_none());
    }

    /// #406 レビュー P2: **接続とグループは有効なのに、タグが 1 本も
    /// 登録されていない**構成。`build_config` はタグが空の有効グループも計画に
    /// 残す（`GroupPlan` の `tags`/`requests` が空になるだけ）ので
    /// `group_count() == 1` / `tag_count() == 0` になり、`group_count` で
    /// 判定していた頃は **`Running { groups: 1, tags: 0 }` になって PLC へ
    /// 繋ぎに行き、tstore まで開いていた**。収集する物は 1 つも無い。
    ///
    /// 反証（回帰の検出）: 判定を `config.group_count() == 0` に戻すと
    /// `NoTargets` ではなく `Running { groups: 1, tags: 0 }` が返り、
    /// `connection_status()` も `Some` になるのでこのテストは落ちる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_enabled_group_with_no_tag_at_all_is_no_targets() {
        let dir = TempDir::new();
        let (pool, svc) = service(&dir).await;
        seed_enabled_group(&pool).await;

        let state = settled(svc.start().await.expect("有効タグ 0 件はエラーではない"));
        assert_eq!(state, CollectorState::NoTargets);
        assert_eq!(svc.state(), CollectorState::NoTargets);
        // **収集エンジンを起動していない** = PLC へ繋ぎにも行っていない。
        assert!(
            svc.connection_status().await.is_none(),
            "収集対象が無いのにエンジンが立っている"
        );
        assert!(svc.current_values().await.is_none());
    }

    /// 同上の、**タグは登録されているが全部無効**な場合。`build_config` は
    /// 無効タグを落とすので、レジストリに行はあっても計画のタグは 0 件になる。
    ///
    /// 反証（回帰の検出）: 上と同じ - `group_count()` 判定に戻すと
    /// `Running { groups: 1, tags: 0 }` になって落ちる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_enabled_group_whose_tags_are_all_disabled_is_no_targets() {
        let dir = TempDir::new();
        let (pool, svc) = service(&dir).await;
        let group_id = seed_enabled_group(&pool).await;
        seed_tag(&pool, group_id, "40001", false).await;

        let state = settled(svc.start().await.expect("有効タグ 0 件はエラーではない"));
        assert_eq!(state, CollectorState::NoTargets);
        assert_eq!(svc.state(), CollectorState::NoTargets);
        assert!(
            svc.connection_status().await.is_none(),
            "収集対象が無いのにエンジンが立っている"
        );
        assert!(svc.current_values().await.is_none());
    }

    /// 停止中に `stop()` を呼んでも壊れない（冪等）。あわせて、**`stop()` が
    /// 直前の理由（ここでは `NoTargets`）を握り潰さない**ことも固定する。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stop_is_idempotent_and_keeps_the_previous_reason() {
        let dir = TempDir::new();
        let (_pool, svc) = service(&dir).await;

        assert_eq!(
            settled(svc.stop().await.expect("停止中の stop")),
            CollectorState::Stopped
        );
        assert_eq!(svc.state(), CollectorState::Stopped);

        svc.start().await.expect("start");
        assert_eq!(svc.state(), CollectorState::NoTargets);

        // 走っていないので「何もしない」= NoTargets のまま。
        assert_eq!(
            settled(svc.stop().await.expect("走っていない stop")),
            CollectorState::NoTargets
        );
        assert_eq!(
            settled(svc.stop().await.expect("2 回目の stop")),
            CollectorState::NoTargets
        );
    }

    /// `start()` に失敗したら**理由が状態に残る**。解釈できないアドレスの
    /// タグを 1 本入れて `build_config` を失敗させる（`banto-tags` は
    /// アドレスの書式を検証しない - 書式は I2/I3b の担当）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_failed_start_keeps_its_reason_in_the_state() {
        let dir = TempDir::new();
        let (pool, svc) = service(&dir).await;
        seed_one_tag(&pool, "これはアドレスではない").await;

        let err = svc.start().await.expect_err("構成が組み立てられない");
        let message = err.to_string();

        match svc.state() {
            CollectorState::StartFailed { reason } => {
                assert!(
                    !reason.trim().is_empty(),
                    "理由が空になっている: {reason:?}"
                );
                assert!(
                    message.contains(&reason) || reason.contains("収集設定エラー"),
                    "状態の理由と返ったエラーが食い違っている: state={reason:?} err={message:?}"
                );
            }
            other => panic!("StartFailed を期待したが {other:?}"),
        }
        assert!(svc.connection_status().await.is_none());
    }

    /// 収集対象があるときは本当に起動し、読み出しが「走っている」形になる。
    /// 二重 `start()` が 2 つ目のエンジンを立てないことも同時に固定する。
    ///
    /// **数え方**: 接続ごとの `ConnectionStatus` は接続タスクが**自分の
    /// タイミングで**書き込むので、起動直後の件数を数えるのは時間に依存する
    /// （それを待つのは「実時間待ちのテスト」になる）。代わりに
    /// `collection_started` イベントの本数を数える - これは
    /// `Collector::start` が**戻る前に**必ず 1 回出すので、2 本目のエンジンが
    /// 立っていれば必ず 2 件になる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn start_twice_runs_exactly_one_engine() {
        let dir = TempDir::new();
        let (pool, svc) = service(&dir).await;
        seed_one_tag(&pool, "40001").await;
        let mut rx = svc.subscribe_events();

        let first = settled(svc.start().await.expect("1 回目の start"));
        assert_eq!(first, CollectorState::Running { groups: 1, tags: 1 });
        assert!(svc.state().is_running());
        assert!(
            svc.connection_status().await.is_some(),
            "走っているので Some（`None` = 走っていない、と読み分けられる）"
        );
        assert!(svc.current_values().await.is_some());

        let second = settled(svc.start().await.expect("2 回目の start"));
        assert_eq!(second, first, "二重起動はせず、同じ状態を返す");
        assert_eq!(
            count_kind(&mut rx, banto_collect::EventKind::CollectionStarted),
            1,
            "2 回目の start がもう 1 つエンジンを立てていない"
        );

        svc.stop().await.expect("stop");
        assert_eq!(svc.state(), CollectorState::Stopped);
        assert!(svc.connection_status().await.is_none());
    }

    /// `start` と `stop` を**同時に**投げても、ライフサイクルタスクが逐次
    /// 処理するので「半分だけ起動した」状態は残らない - 終わったあとは必ず
    /// 「走っている（`Running`）」か「止まっている（`Stopped`）」の
    /// どちらかで、しかも状態と読み出しが一致する。
    ///
    /// 実時間を待たない（`join` するだけ。`sleep` も `tokio::time` の進行も
    /// 使っていない）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn start_and_stop_racing_leave_a_consistent_state() {
        let dir = TempDir::new();
        let (pool, svc) = service(&dir).await;
        seed_one_tag(&pool, "40001").await;

        let starter = {
            let svc = svc.clone();
            tokio::spawn(async move { svc.start().await })
        };
        let stopper = {
            let svc = svc.clone();
            tokio::spawn(async move { svc.stop().await })
        };
        starter.await.expect("start task").expect("start");
        stopper.await.expect("stop task").expect("stop");

        let state = svc.state();
        let running = svc.connection_status().await.is_some();
        assert_eq!(
            state.is_running(),
            running,
            "状態（{state:?}）と読み出し（走っている={running}）が食い違っている"
        );
        assert!(
            matches!(
                state,
                CollectorState::Running { .. } | CollectorState::Stopped
            ),
            "中途半端な状態が残っている: {state:?}"
        );

        // 後始末（走っていれば止める。止まっていれば何もしない）。
        svc.stop().await.expect("後始末の stop");
    }

    /// `restart()` は止めてから起動する。走っていない状態から呼んでも
    /// 単なる起動として成立し、走っている状態から呼ぶと**止めてから**
    /// 立て直す（= 前のエンジンが残らない）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn restart_stops_the_old_engine_before_starting_a_new_one() {
        let dir = TempDir::new();
        let (pool, svc) = service(&dir).await;
        seed_one_tag(&pool, "40001").await;
        let mut rx = svc.subscribe_events();

        // 走っていない状態からの restart = ただの起動（停止は何もしない）。
        let state = settled(svc.restart().await.expect("restart"));
        assert_eq!(state, CollectorState::Running { groups: 1, tags: 1 });

        let again = settled(svc.restart().await.expect("2 回目の restart"));
        assert_eq!(again, CollectorState::Running { groups: 1, tags: 1 });

        let events = drain(&mut rx);
        assert_eq!(
            kind_count(&events, banto_collect::EventKind::CollectionStarted),
            2,
            "restart 2 回で起動は 2 回: {events:?}"
        );
        assert_eq!(
            kind_count(&events, banto_collect::EventKind::CollectionStopped),
            1,
            "2 回目の restart は古いエンジンを確かに止めてから立て直した: {events:?}"
        );

        svc.stop().await.expect("stop");
    }

    // --- 停止の途中キャンセル（#406 レビュー P2） ------------------------

    /// **この修正の一番大事な受入条件**: `stop()` の呼び出し側が
    /// 停止処理の途中で消えても、**停止は実体まで完了し、状態と食い違わない**。
    ///
    /// 旧実装（サービス側で `AsyncMutex<Option<Collector>>` を `take()` して
    /// から `collector.stop().await`）では、この瞬間にキャンセルされると
    /// `collector = None` / 状態 = `Running` のまま**永久に**固定され、
    /// もう一度 `stop()` を呼んでも「走っていない」分岐に落ちて直せなかった。
    /// さらに接続タスクの join も最終 flush も行われないままだった。
    ///
    /// 実時間は待たない（ゲートは [`Notify`]、完了待ちは読み出しのキュー）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelling_stop_still_finishes_the_stop() {
        let dir = TempDir::new();
        let (pool, svc, gate) = gated_service(&dir).await;
        seed_one_tag(&pool, "40001").await;
        let mut rx = svc.subscribe_events();

        svc.start().await.expect("start");
        // 接続タスクが `Connected` になるまで待つ。そこまで行かないと
        // graceful exit が `disconnect` を通らず、ゲートに入らない。
        wait_for(&mut rx, banto_collect::EventKind::PlcConnected).await;

        let stopper = {
            let svc = svc.clone();
            tokio::spawn(async move { svc.stop().await })
        };
        // 「停止が実体の停止の途中まで進んだ」ことを確かめてから切る
        // （切るのが早すぎると、そもそも何も始まっていない）。
        gate.wait_entered().await;
        stopper.abort();
        assert!(
            stopper.await.unwrap_err().is_cancelled(),
            "呼び出し側は確かにキャンセルされた"
        );

        gate.release();

        // 読み出しはライフサイクルタスクのキューを通るので、**飛行中の停止が
        // 終わってから**返る - sleep で待つ必要がない。
        assert!(
            svc.connection_status().await.is_none(),
            "キャンセル後も停止は実体まで完了している"
        );
        assert!(svc.current_values().await.is_none());
        assert_eq!(
            svc.state(),
            CollectorState::Stopped,
            "状態と実体が一致している"
        );

        let kinds = drain(&mut rx);
        assert!(
            kinds.contains(&banto_collect::EventKind::CollectionStopped),
            "最終 flush まで進んだ（`collection_stopped` は stop の最後に出る）: {kinds:?}"
        );

        // キャンセルの後にもう一度呼んでも壊れない（冪等のまま）。
        assert_eq!(
            settled(svc.stop().await.expect("キャンセル後の stop")),
            CollectorState::Stopped
        );
    }

    /// キャンセルされた `stop()` の**後に投げた `start()` は、その停止が
    /// 完了するまで進まない**（オーナー指定）。
    ///
    /// 証拠はイベントの順序: `collection_stopped` は旧エンジンの最終 flush の
    /// **後**に出るので、2 本目の `collection_started` がその後ろに来ていれば、
    /// 新しい起動は確かに前の停止の完了を待っている。
    ///
    /// `start` の依頼を**キューに積んでからゲートを開ける**ために、ここだけ
    /// 内部の [`Command`] を直接送っている（`svc.start()` を別タスクで
    /// 走らせる書き方だと、依頼が積まれる前にゲートを開けてしまう競争になり、
    /// テストが「たまたま順番どおり」でも通ってしまう）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_start_queued_after_a_cancelled_stop_waits_for_it() {
        let dir = TempDir::new();
        let (pool, svc, gate) = gated_service(&dir).await;
        seed_one_tag(&pool, "40001").await;
        let mut rx = svc.subscribe_events();

        svc.start().await.expect("start");
        wait_for(&mut rx, banto_collect::EventKind::PlcConnected).await;

        let stopper = {
            let svc = svc.clone();
            tokio::spawn(async move { svc.stop().await })
        };
        gate.wait_entered().await;
        stopper.abort();
        let _ = stopper.await;

        // 停止はまだゲートの中。ここで start をキューへ積む。
        let (reply_tx, reply_rx) = oneshot::channel();
        svc.commands()
            .send(Command::Start(reply_tx))
            .await
            .map_err(|_| ())
            .expect("start をキューに積む");
        gate.release();
        let state = reply_rx
            .await
            .expect("start の応答")
            .expect("キャンセル後の start");
        assert_eq!(state, CollectorState::Running { groups: 1, tags: 1 });

        let kinds = drain(&mut rx);
        let stopped = kinds
            .iter()
            .position(|k| *k == banto_collect::EventKind::CollectionStopped)
            .unwrap_or_else(|| panic!("前の停止が完了していない: {kinds:?}"));
        let started = kinds
            .iter()
            .position(|k| *k == banto_collect::EventKind::CollectionStarted)
            .unwrap_or_else(|| panic!("新しい起動が見当たらない: {kinds:?}"));
        assert!(
            stopped < started,
            "新しい start が、前の stop の完了を待たずに進んだ: {kinds:?}"
        );

        svc.stop().await.expect("後始末の stop");
    }

    /// `subscribe_events()` は start/stop をまたいで生き続ける
    /// （`EventSink` を `Collector` ではなくサービスが持っているため）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_event_subscription_survives_start_and_stop() {
        let dir = TempDir::new();
        let (pool, svc) = service(&dir).await;
        seed_one_tag(&pool, "40001").await;

        let mut rx = svc.subscribe_events();
        svc.start().await.expect("start");
        svc.stop().await.expect("stop");

        let kinds = drain(&mut rx);
        assert!(
            kinds.contains(&banto_collect::EventKind::CollectionStarted),
            "購読は start 前に取ったので collection_started が届く: {kinds:?}"
        );
        assert!(
            kinds.contains(&banto_collect::EventKind::CollectionStopped),
            "同じ受信ハンドルで stop も見える: {kinds:?}"
        );
    }

    // --- 無応答への上限と `Starting`（C-2） -------------------------------

    /// **この PR の一番大事な受入条件**: 起動が解決しないとき、呼び出し側は
    /// [`COLLECT_OPERATION_TIMEOUT`] で**待つのをやめる**が、
    ///
    /// * それは**失敗ではない**（`Err` にならず `pending` が立つ）、
    /// * 状態は [`CollectorState::Starting`] のままで **`Stopped` と嘘を
    ///   つかない**、
    /// * **その後タスク側が完了すると状態が追いつく**。
    ///
    /// 実時間は 1 ミリ秒も待たない: 起動は [`StartGate`] で止め、上限は
    /// [`tokio::time::pause`] の仮想時計が自動で進めて消費する。
    ///
    /// **時計を止めるのは足場を組み終えてから**（`start_paused = true` では
    /// ない）。sqlx の接続確立は別スレッドで進むので、仮想時計のままだと
    /// ランタイムがアイドルになった瞬間に時間が飛び、プールの取得上限
    /// （30 秒）が**接続が返る前に**切れて `init_db_memory` 自体が失敗する。
    ///
    /// 反証（回帰の検出）: `Lifecycle::start` の `set_state(Starting)` を
    /// 消すと、打ち切り時点の状態が `Stopped` のままになって 2 番目の
    /// `assert_eq!` が落ちる。`CollectorService::lifecycle` の
    /// `tokio::time::timeout` を外すと、`starter` が永久に返らずテストが
    /// 終わらない（= 上限が効いていないことがそのまま現れる）。
    ///
    /// 上限を消費し終えたら**時計を実時間へ戻す**。戻さずに仮想時計のまま
    /// 進めると、アイドルのたびに時間が飛んで sqlx のプール保守が
    /// 「10 分アイドル」の接続を刈り、`sqlite::memory:` の DB ごと消えてしまう。
    #[tokio::test]
    async fn an_unresponsive_start_is_abandoned_not_failed_and_the_state_catches_up() {
        let dir = TempDir::new();
        let pool = init_db_memory().await.expect("init_db_memory");
        let gate = Arc::new(StartGate::default());
        let svc = CollectorService::new_for_test_gated(
            pool.clone(),
            dir.path().join("data"),
            fast_options(),
            offline_factory(),
            gate.clone(),
        );

        tokio::time::pause();

        let starter = {
            let svc = svc.clone();
            tokio::spawn(async move { svc.start().await })
        };
        // 起動が「`Starting` を立てた直後」まで来たことを確かめてから待つ
        // （来る前に上限が過ぎると、テストは通っても何も確かめていない）。
        gate.entered.notified().await;
        assert_eq!(
            svc.state(),
            CollectorState::Starting,
            "tstore を開く前に「起動中」が立っていない"
        );

        let outcome = starter
            .await
            .expect("start task")
            .expect("打ち切りは `Err` にしない");
        assert!(
            outcome.pending,
            "上限で待つのをやめたことが呼び出し側に伝わっていない: {outcome:?}"
        );
        assert_eq!(
            outcome.status,
            CollectorState::Starting,
            "打ち切った時点の状態を載せていない"
        );
        assert_eq!(
            svc.state(),
            CollectorState::Starting,
            "打ち切っただけなのに「止まっている」と嘘をついている"
        );

        tokio::time::resume();

        // タスク側の起動処理はまだ続いている。開けてやると、状態が追いつく。
        gate.release.notify_one();
        assert!(
            svc.connection_status().await.is_none(),
            "収集対象が無い構成なのでエンジンは立たない"
        );
        assert_eq!(
            svc.state(),
            CollectorState::NoTargets,
            "タスク側が決着した後も `Starting` のまま取り残されている"
        );
    }

    // --- 起動時の自動開始（C-2） ------------------------------------------

    /// 自動開始は「収集対象があれば本当に走り出す」。`src-tauri` の
    /// `setup()` と `banto-serve` の `main()` が呼ぶのはこの 1 本だけなので、
    /// ここを押さえれば両経路の中身が同じであることも同時に決まる
    /// （呼び出し側の配線そのものは、`crate::hub` の `resume()` と同様に
    /// 目視確認の範囲 - どちらのエントリポイントも自動テストの外にある）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn autostart_starts_collecting_when_there_are_targets() {
        let dir = TempDir::new();
        let (pool, svc) = service(&dir).await;
        seed_one_tag(&pool, "40001").await;

        svc.autostart().await;

        assert_eq!(svc.state(), CollectorState::Running { groups: 1, tags: 1 });
        assert!(svc.connection_status().await.is_some());

        svc.stop().await.expect("後始末の stop");
    }

    /// 自動開始は**起動を止めない**。収集対象 0 件（失敗ですらない）でも、
    /// 構成が壊れていて起動できなくても、`autostart()` は panic せずに戻り、
    /// **理由は状態に残る**。
    ///
    /// 反証（回帰の検出）: `autostart()` が `start()` の `Err` を
    /// `expect`/`?` で外へ出す実装に変えると、2 つ目のケースで panic して
    /// 落ちる（= アプリの起動が収集の失敗で止まることを意味する）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn autostart_never_stops_the_app_and_keeps_the_reason() {
        let no_targets_dir = TempDir::new();
        let (_pool, svc) = service(&no_targets_dir).await;
        svc.autostart().await;
        assert_eq!(
            svc.state(),
            CollectorState::NoTargets,
            "収集対象 0 件は失敗ではない"
        );

        let failing_dir = TempDir::new();
        let (pool, failing) = service(&failing_dir).await;
        seed_one_tag(&pool, "これはアドレスではない").await;
        failing.autostart().await;
        match failing.state() {
            CollectorState::StartFailed { reason } => {
                assert!(!reason.trim().is_empty(), "理由が空になっている");
            }
            other => panic!("StartFailed を期待したが {other:?}"),
        }
    }

    // --- data.dir の解決 --------------------------------------------------

    #[test]
    fn relative_data_dir_resolves_against_the_app_data_dir() {
        let base = Path::new("C:/appdata");
        assert_eq!(
            resolve_data_dir(base, "./data"),
            base.join("./data"),
            "既定の相対パスは作業ディレクトリではなくデータディレクトリ基準"
        );
    }

    #[test]
    fn absolute_data_dir_is_used_as_is() {
        let base = Path::new("C:/appdata");
        let absolute = if cfg!(windows) { "D:/ts" } else { "/var/ts" };
        assert_eq!(resolve_data_dir(base, absolute), PathBuf::from(absolute));
    }

    #[test]
    fn blank_data_dir_falls_back_to_the_default_location() {
        let base = Path::new("C:/appdata");
        assert_eq!(resolve_data_dir(base, "   "), base.join("data"));
    }
}
