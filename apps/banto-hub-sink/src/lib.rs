//! `banto-hub-sink`（外部 DB 連携 S5、docs/banto-hub-external-db-design.md
//! §5）: banto-hub のタグ空間を購読し、値を外部 PostgreSQL へ long スキーマ
//! （§5.3）でバッチ INSERT する**別プロセスのサイドカー**。
//!
//! ## なぜ Hub の中ではなく外なのか（§5.1、2026-09-06 オーナー決定）
//!
//! 原則は「**タグ空間を定義する側は Hub の中、消費するだけの側は外**」。
//! DB Source（#228、`banto_hub_core::db_source`）はタグ値を定義するので
//! Hub 内、Sink は消費者なので外。Sink のエンジンは「キューと遅い SQL を
//! 持つ唯一の重い消費者」であり、メモリ枯渇やランタイム閉塞といった物理的
//! な巻き込みは同一プロセスでは防げない - キューが膨らんでもこのプロセス内
//! で閉じ、Hub の収集・購読配信には波及しない（§5.4 末尾）というのが分割の
//! 目的そのもの。
//!
//! **設定・UI・監視は Hub が持つ**（§5.2）。このプロセスは状態を持たない:
//! 起動時に Hub から設定を取得 → 購読 → INSERT を繰り返すだけで、落ちれば
//! SCM が再起動して設定を取り直す。`banto-hub-sink.toml`（[`config`]。#630 で置き場を
//! SYSTEM・Administrators だけの `%ProgramData%\BantoHub\sink\` に限り、保護されて
//! いなければ起動しない）が持つのは Hub の URL と API キーだけで、
//! それ以外の設定（どのタグをどのテーブルへ、どの周期で）はすべて Hub 側の
//! `hub_sink_groups`（`banto_hub_core::sink`、S4）にある。
//!
//! ## 構成（データの流れ）
//!
//! ```text
//!   banto-hub                         banto-hub-sink                外部 PostgreSQL
//!   ─────────                         ──────────────                ───────────────
//!   GET /api/sink/config  ──────────▶ [hub_api] 設定取得（30秒ごと）
//!                                       │ 差分 → group/connection の start/stop
//!   /api/v1/{tags,values,stream} ◀───▶ [values] banto-tagclient SDK（1本）
//!                                       │ 最新スナップショット（watch）
//!                                     [group] interval 採取 / on_change スロットル
//!                                       │ 1タグ1行
//!                                     [queue] グループごとの bounded queue
//!                                       │ flush_interval_ms / batch_size
//!                                     [flush] 接続ごとの flusher ──────────▶ INSERT（1トランザクション）
//!                                       │
//!   PUT /api/sink/status  ◀─────────── [status] 5秒ごとの状態 push
//! ```
//!
//! - [`config`][]: 保護された置き場 `%ProgramData%\BantoHub\sink\` の toml（置き場と
//!   ファイルを辿らずに確かめてから読む。未知キーは拒否、範囲検証、`api_key` は `Debug` にも
//!   出さない）。
//! - [`hub_api`][]: `GET /api/sink/config` / `PUT /api/sink/status`（admin
//!   スコープの API キー + ループバック運用、§6-15）。
//! - [`values`][]: banto-tagclient の `TagClientHandle` を1本だけ持ち、全
//!   グループの対象タグの**和集合**を安定 ID で購読して、外部名で引ける
//!   最新スナップショット（[`values::ValueView`]）へ変換して配る。
//! - [`group`][]: 1 sink group の実行時状態（対象タグ・キュー・運転状態）と、
//!   `interval`（周期採取）/ `on_change`（変化時 + 最短発行間隔）の
//!   プロデューサ。
//! - [`queue`][]: 行の bounded queue。満杯なら**最も古い行を捨てて**
//!   `dropped` を増やす（§5.4・§6-8）。commit されるまで行はキューから
//!   出ない（at-least-once）。
//! - [`sql`][]: テーブル識別子の再検証・引用と、multi-row INSERT /
//!   `SELECT ... LIMIT 0` の SQL 組み立て（bind パラメータのみ、値の文字列
//!   連結は一切しない）。
//! - [`flush`][]: DB 接続ごとの `PgPool` と flusher タスク。テーブル検査・
//!   バックオフ（1s → 30s、DB Source と同じ定数）を持つ。
//! - [`status`][]: グループ単位の運転状態（`running`/`backoff`/`error`）と
//!   Hub への push 本体。
//! - [`source`][]: **DB ブリッジ**（R2a-3b、docs/banto-hub-ingest-design.md §10.3）。
//!   Hub のブリッジの `push` 接続ごとに PostgreSQL へ問い合わせ、取り込み口の
//!   WebSocket（`/api/v1/ingest/{id}/stream`）へ値を送る。設定は
//!   `GET /api/ingest/bridge-config`。値の変換と品質の梯子は Hub 内の DB Source と同じ
//!   共有 crate `banto-db-source`。上のエンジン（購読 → INSERT）とは別のタスク群で、
//!   設定の取得の失敗も互いに独立。
//! - [`run`][]: 上記を束ねる実行ループ（設定の差分適用・状態 push・SDK の
//!   監督・停止時の flush）。テスト可能にするため `main.rs` ではなくこの
//!   lib 側に置いてある。
//! - [`service`][]（Windows のみ）: banto-hub と同じ共有 crate
//!   `banto-service-control` のランナーと登録の手順（`install`/`uninstall`/
//!   `run-service`、#392 B3-1）に加え、
//!   `banto-hub-elev.exe`（banto-hub 側の UAC 昇格ヘルパー）を子プロセスで
//!   呼ぶだけの `grant-service-acl`（S6 レビュー指摘の follow-up、
//!   `service` のモジュール doc「サービス ACL の付与」節参照）。
//!
//! ## 資格情報の扱い（§2.2・§6-3・§6-15）
//!
//! - `banto-hub-sink.toml` の `api_key` は**平文**で置かれる（Hub の MQTT
//!   パスワードと同じ v1 の前提）。`GET /api/sink/config` は DB 接続の
//!   パスワードを平文で返すため、**Hub と同一マシンのループバック接続**が
//!   運用前提。キーを守る境界は置き場の ACL（SYSTEM・Administrators だけ。#630）。
//!   Hub が止まっている間に一般の利用者がループバックのポートを先に取ると、キーを
//!   送ってしまう穴はこの ACL では塞がらない（#644）。
//! - DB ブリッジ（[`source`]）は同じキーに `ingest:#{id}` のスコープを足して使う
//!   （`admin` + `read` + `ingest:#12` …）。`GET /api/ingest/bridge-config` も読む DB の
//!   パスワードを平文で返す（同じループバックの前提）。スコープの無い接続は動かさない。
//! - API キーと DB パスワードは**ログにも Hub への status push にも
//!   出さない**。sqlx のエラーは [`status::sanitize_db_error`] で
//!   カテゴリ + DB 自身のメッセージだけに縮め、さらに
//!   [`status::Redactor`] が既知の秘密文字列を機械的に伏せる（多層防御）。
//!
//! ## ログ
//!
//! `println!`/`eprintln!` の薄いラッパー（[`log`]）。banto-hub の
//! `banto_hub_core::hub_log` と同じ方針で、サービスモードの間だけ同じ内容を
//! ファイルへミラーする（コンソールが無いサービスでは標準出力が誰にも
//! 見えないため）。`warn` は**初回とバックオフ段階・状態が変わったときだけ**
//! （§5.5「MQTT と同じ抑制」）。サービスログファイルの置き場は
//! `%ProgramData%\BantoHub\logs\banto-hub-sink-service.log`（[`log`]の
//! モジュール doc「サービスログファイルの置き場」節参照、I4 追従）。

pub mod config;
pub mod flush;
pub mod group;
pub mod hub_api;
pub mod log;
pub mod queue;
pub mod run;
#[cfg(windows)]
pub mod service;
pub mod source;
pub mod sql;
pub mod status;
pub mod values;

pub use config::{load_config, RunMode, SidecarConfig, DEFAULT_CONFIG_FILE_NAME, ENV_CONFIG_PATH};
pub use run::{run, SidecarOptions, StopRequest};
