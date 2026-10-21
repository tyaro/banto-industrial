//! 設定ファイル `banto-hub-sink.toml`（Hub の URL と admin API キー）。
//! それ以外の設定はすべて Hub 側。
//!
//! ## 置き場（#630、2026-10-17 オーナー決定。破壊的変更）
//!
//! 置き場は **`%ProgramData%\BantoHub\sink\banto-hub-sink.toml` だけ**（ProgramData は
//! 既知のフォルダから取り、`BANTO_HUB_ROOT` には追従しない）。置き場は
//! `banto-hub-elev prepare-sink-config` が SYSTEM・Administrators だけ・所有者
//! Administrators の保護された DACL で作る。sink は**置き場とファイルが保護されていなければ
//! 起動しない**（理由を 1 行ずつ出し、`prepare-sink-config` を案内する）。
//!
//! - LocalSystem のサービスは環境変数の上書きを見ない。コンソールの `run`（開発・利用者として
//!   動かす形）は [`ENV_CONFIG_PATH`]（`BANTO_HUB_SINK_CONFIG`、絶対パス）を見るが、指した
//!   ファイルにも同じ保護の確かめをかける。環境変数で指していても、旧い置き場に toml が
//!   あれば止まる（移行を強制するため）。
//! - 旧い置き場（ルート直下の `banto-hub-sink.toml`・exe の隣）は読まない。そこにファイルが
//!   あれば、移し方とキーの失効・再発行を案内して起動しない（旧いファイルを自動で消したり
//!   動かしたりはしない）。
//!
//! 決め方・確かめ方・読み方は [`place`]。採用したパスは起動時に 1 行ログする
//! （[`load_config`]。パスのみ・内容は絶対に出さない）。
//!
//! ### 以前の探索順（参考。#630 で廃止）
//!
//! I2（2026-09-07）以降は「環境変数 → `%ProgramData%\BantoHub\`（利用者権限で書ける）→
//! exe の隣」の 3 段だった。どの段も一般の利用者が読めたため廃止した。
//!
//! ## このファイルに置くもの / 置かないもの
//!
//! 置くのは「Hub にたどり着くための情報」と「このプロセスのメモリ・I/O の
//! 上限」だけ。**どのタグをどのテーブルへ、どの周期で書くか**は 1 つも
//! ここに無い - それは Hub の `hub_sink_groups`（S4）が持ち、
//! `GET /api/sink/config` で取得する（設計 §5.1「サイドカーは状態を
//! 持たない」）。
//!
//! ## 書式（すべての項目・既定値つき）
//!
//! ```toml
//! # 必須。Hub のループバック URL（§6-15: パスワードが平文で流れるため
//! # Hub と同一マシンでの運用が前提）。
//! hub_url = "http://127.0.0.1:8722"
//! # 必須。Hub で発行した API キー。スコープは `admin` と `read` の両方
//! # （下記「API キーに必要なスコープ」参照）。**このファイルに平文で
//! # 置かれる** - v1 は MQTT のパスワードと同じ前提（§2.2・§6-3）なので、
//! # 置き場の ACL で守る（上の「置き場」）。
//! api_key = "bh_xxxxxxxx.yyyyyyyy"
//!
//! # 以下はすべて任意（既定値を記載）。
//! config_refresh_secs = 30   # GET /api/sink/config の間隔（5〜3600）
//! status_push_secs = 5       # PUT /api/sink/status の間隔（1〜14）
//! queue_max_rows = 10000     # グループごとのキュー上限（100〜10000000）
//! flush_interval_ms = 1000   # flush の周期（100〜60000）
//! batch_size = 500           # 1 INSERT の行数（1〜10000、キュー上限以下）
//! shutdown_flush_secs = 5    # 停止時に残キューを流す上限（0〜60）
//! ```
//!
//! ## 未知キーを拒否する理由
//!
//! [`FileConfig`] は `#[serde(deny_unknown_fields)]`。綴り間違い
//! （`api-key` / `apikey` / `hub_uri` …）が「既定値で静かに動く」形で
//! 通ってしまうと、現場では「設定したのに効かない」という最悪の失敗の
//! 仕方をする。24/365 で無人運転するサイドカーなので、起動時に落として
//! 直させる方が安全。
//!
//! ## `api_key` の扱い
//!
//! [`SidecarConfig`] の `Debug` は手書きで、`api_key` は常に
//! `"<redacted>"` になる（`derive(Debug)` を**使ってはいけない** -
//! 構造体ごとダンプする1行のログでキーが漏れる）。値そのものは
//! [`crate::hub_api`] が `Authorization: Bearer` に、
//! [`crate::values`] が `banto_tagclient::SecretApiKey` に渡すためだけに
//! 読み出す。
//!
//! ## API キーに必要なスコープ（実装上の注意）
//!
//! 設計 §5.2 は「サイドカー専用に発行した **admin** キー」と書いているが、
//! `banto_hub_core::api_keys` の `admin` スコープは read/write と**直交**で、
//! `admin` だけのキーでは `/api/v1/{tags,values,stream}`（SDK の購読経路）が
//! 403 になる。したがってサイドカーのキーは **`admin` と `read` の両方**を
//! 持つ必要がある（`["admin", "read"]` で発行する）。この 1 本を
//! `api_key` に書く。DB ブリッジ（[`crate::source`]、R2a-3b）を動かすときは、同じキーに
//! ブリッジの `push` 接続ごとの `ingest:#{接続 id}` も足す（`admin` + `read` +
//! `ingest:#12` …。docs/banto-hub-ingest-design.md §10.3）。スコープの無い接続のブリッジは
//! 動かさない。`config_refresh_secs` はブリッジの設定（`GET /api/ingest/bridge-config`）の
//! 取得の間隔にも使う（項目は増やさない）。

pub mod place;

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

pub use place::RunMode;

/// 設定ファイル名（置き場の中の名前。旧い置き場の確かめにも同じ名前を使う）。
pub const DEFAULT_CONFIG_FILE_NAME: &str = "banto-hub-sink.toml";

/// `%ProgramData%` の下の製品のフォルダ名（Hub のルートと同じ名前。置き場はその下の
/// [`place::SINK_DIR`]）。
const PROGRAM_DATA_SUBDIR: &str = "BantoHub";

/// 設定ファイルのパスを上書きする環境変数（開発・検証用。**コンソールの `run` だけが
/// 見る**。LocalSystem のサービスは見ない - モジュール doc「置き場」）。指したファイルにも
/// 既定の置き場と同じ保護の確かめをかける。
pub const ENV_CONFIG_PATH: &str = "BANTO_HUB_SINK_CONFIG";

/// 置き場とファイルを整える昇格のコマンド（案内に出す）。
pub const PREPARE_COMMAND: &str = "banto-hub-elev.exe prepare-sink-config";

// --- 既定値（設計 §5.4 の「既定 10,000 行 / 1,000ms / 500 行」と
// --- §5.2 の「5 秒ごとに push」・§5.6 の「最大 5 秒だけ flush」） ----------

const DEFAULT_CONFIG_REFRESH_SECS: u64 = 30;
const DEFAULT_STATUS_PUSH_SECS: u64 = 5;
const DEFAULT_QUEUE_MAX_ROWS: usize = 10_000;
const DEFAULT_FLUSH_INTERVAL_MS: u64 = 1_000;
const DEFAULT_BATCH_SIZE: usize = 500;
const DEFAULT_SHUTDOWN_FLUSH_SECS: u64 = 5;

// --- 許容範囲。上限は「明らかな設定ミス」を弾くためのもので、性能上の
// --- 根拠がある値ではない（下限の方が意味を持つ: 短すぎる周期で Hub や
// --- DB を叩き続ける設定を防ぐ）。

const MIN_CONFIG_REFRESH_SECS: u64 = 5;
const MAX_CONFIG_REFRESH_SECS: u64 = 3_600;
const MIN_STATUS_PUSH_SECS: u64 = 1;
/// Hub は 15 秒 push が無ければ `unknown` と表示する
/// （`banto_hub_core::sink::SINK_SIDECAR_STALE_AFTER_MS`）ので、それ以上
/// 空ける設定は「常に unknown」を意味してしまう - 上限をそれ未満に固定する。
const MAX_STATUS_PUSH_SECS: u64 = 14;
const MIN_QUEUE_MAX_ROWS: usize = 100;
const MAX_QUEUE_MAX_ROWS: usize = 10_000_000;
const MIN_FLUSH_INTERVAL_MS: u64 = 100;
const MAX_FLUSH_INTERVAL_MS: u64 = 60_000;
const MIN_BATCH_SIZE: usize = 1;
/// PostgreSQL の bind パラメータ上限は 65535。1 行 5 パラメータなので
/// 13,107 行が理論上限 - 余裕を見て 10,000 行で頭打ちにする
/// （[`crate::sql`] のモジュール doc 参照）。
const MAX_BATCH_SIZE: usize = 10_000;
const MAX_SHUTDOWN_FLUSH_SECS: u64 = 60;

/// 設定ファイルの読み込み・検証の失敗。どの派生も**値を含めない**か、
/// 含めても秘密ではない項目だけ（`api_key` の中身は決して載せない。パス・SID・理由だけ）。
/// 利用者に出すときは [`ConfigError::lines`] を 1 行ずつ出す。
#[derive(Debug)]
pub enum ConfigError {
    /// ファイルが読めない（[`load_config_from`] だけ。保護の確かめをしない検証用の口）。
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    /// TOML として壊れている / 未知のキーがある / 型が違う / UTF-8 でない。
    Parse { path: PathBuf, message: String },
    /// 値の範囲・形式が不正。
    Invalid {
        field: &'static str,
        message: String,
    },
    /// 旧い置き場（ルート直下・exe の隣）に設定ファイルがある（#630。読まずに起動しない）。
    /// `new_place` は移し先（ProgramData を取れなければ `None`）。
    LegacyFound {
        paths: Vec<PathBuf>,
        new_place: Option<PathBuf>,
    },
    /// 置き場（フォルダ）が無い。`env` はコンソールの `run` で環境変数が指したものか。
    PlaceMissing { dir: PathBuf, env: bool },
    /// 置き場に設定ファイルが無い。
    FileMissing { path: PathBuf, env: bool },
    /// 置き場か設定ファイルが保護されていない（理由ごとに 1 行。中身は読んでいない）。
    Unprotected {
        dir: PathBuf,
        dir_problems: Vec<place::Problem>,
        file: PathBuf,
        file_problems: Vec<place::Problem>,
        env: bool,
    },
    /// 置き場か設定ファイルを開けなかった（アクセスが拒否された）。
    AccessDenied { path: PathBuf },
    /// 設定ファイルが別のプロセスで書き込みに開かれている（共有違反。読み取りだけを共有して
    /// 開くため）。
    FileInUse { path: PathBuf },
    /// 保護されているかを確かめられなかった（開けない・読めないなど）。
    Unverifiable { path: PathBuf, reason: String },
    /// 環境変数 [`ENV_CONFIG_PATH`] の値が使えない。
    EnvInvalid {
        value: PathBuf,
        reason: &'static str,
    },
    /// 既知のフォルダの ProgramData を取れなかった。
    ProgramDataUnavailable(String),
    /// この OS では保護を確かめられない（banto-hub-sink は Windows 専用）。
    UnsupportedPlatform,
}

/// 「整えてから、もう一度起動」の案内（既定の置き場）。
fn prepare_line() -> String {
    format!(
        "管理者の PowerShell で {PREPARE_COMMAND}（Hub のインストール先にあります）を実行して、置き場と設定ファイルの所有者・ACL を整えてから、もう一度起動してください"
    )
}

/// 環境変数で指したファイルの案内（`prepare-sink-config` は既定の置き場しか整えない）。
fn env_line() -> String {
    format!(
        "環境変数 {ENV_CONFIG_PATH} で指したファイルにも同じ保護が要ります（置き場とファイルの所有者が SYSTEM か Administrators で、ほかの利用者にアクセスを許さない・リンクでない）"
    )
}

/// キーの失効・再発行の案内。
const REISSUE_LINE: &str = "Hub の管理画面の「API キー」でこのキーを失効し、admin と read のスコープで新しいキーを発行して書き換えてください";

impl ConfigError {
    /// 利用者に出す行（1 行 1 理由。頭の `banto-hub-sink: ` は呼ぶ側が付ける）。
    pub fn lines(&self) -> Vec<String> {
        let start = "起動を止めます";
        match self {
            Self::Read { path, source } => vec![format!(
                "設定ファイル {} を読めませんでした: {source}",
                path.display()
            )],
            Self::Parse { path, message } => vec![format!(
                "設定ファイル {} の解析に失敗しました: {message}",
                path.display()
            )],
            Self::Invalid { field, message } => vec![format!("設定 {field} が不正です: {message}")],
            Self::LegacyFound { paths, new_place } => {
                let mut lines: Vec<String> = paths
                    .iter()
                    .map(|path| {
                        format!(
                            "旧い置き場に設定ファイル {} があります。この版は旧い置き場を読みません",
                            path.display()
                        )
                    })
                    .collect();
                let new_place = new_place.as_ref().map_or_else(
                    || format!(r"%ProgramData%\BantoHub\sink\{DEFAULT_CONFIG_FILE_NAME}"),
                    |path| path.display().to_string(),
                );
                lines.push(format!(
                    "新しい置き場 {new_place} へ移してください: {PREPARE_COMMAND} を実行してから、管理者のメモ帳で新しい置き場の中の {DEFAULT_CONFIG_FILE_NAME}.example をコピーして {DEFAULT_CONFIG_FILE_NAME} を作り、hub_url などを写し、もう一度 {PREPARE_COMMAND} を実行します（旧いファイルを外から移動しないでください。旧い ACL が付いてきます）"
                ));
                lines.push(format!(
                    "旧いファイルにはキーが平文で入っていて、ほかの利用者から読めた可能性があります。{REISSUE_LINE}。そのあと旧いファイルを消してください"
                ));
                lines.push(format!("旧いファイルが残っている間は{start}"));
                lines
            }
            Self::PlaceMissing { dir, env: false } => vec![
                format!("設定ファイルの置き場 {} がありません", dir.display()),
                format!(
                    "管理者の PowerShell で {PREPARE_COMMAND} を実行して置き場を作り、中の {DEFAULT_CONFIG_FILE_NAME}.example をコピーして {DEFAULT_CONFIG_FILE_NAME} を作ってください。{start}"
                ),
            ],
            Self::PlaceMissing { dir, env: true } => vec![format!(
                "環境変数 {ENV_CONFIG_PATH} が指す設定ファイルの置き場 {} がありません。{start}",
                dir.display()
            )],
            Self::FileMissing { path, env: false } => vec![
                format!("設定ファイル {} がありません", path.display()),
                format!(
                    "管理者のメモ帳で、置き場の中の {DEFAULT_CONFIG_FILE_NAME}.example をコピーして {DEFAULT_CONFIG_FILE_NAME} を作り、hub_url と api_key を書いてから、{PREPARE_COMMAND} を実行してください。{start}"
                ),
            ],
            Self::FileMissing { path, env: true } => vec![format!(
                "環境変数 {ENV_CONFIG_PATH} が指す設定ファイル {} がありません。{start}",
                path.display()
            )],
            Self::Unprotected {
                dir,
                dir_problems,
                file,
                file_problems,
                env,
            } => {
                let mut lines = Vec::new();
                for problem in dir_problems {
                    lines.push(format!(
                        "置き場 {} は保護されていません: {}",
                        dir.display(),
                        problem.text()
                    ));
                }
                for problem in file_problems {
                    lines.push(format!(
                        "設定ファイル {} は保護されていません: {}",
                        file.display(),
                        problem.text()
                    ));
                }
                if file_problems.iter().any(place::Problem::may_have_leaked)
                    || dir_problems.iter().any(place::Problem::may_have_leaked)
                {
                    lines.push(format!(
                        "このファイルのキーは、ほかの利用者から読めた可能性があります。{REISSUE_LINE}"
                    ));
                }
                lines.push(if *env { env_line() } else { prepare_line() });
                lines.push(format!("中身は読んでいません。{start}"));
                lines
            }
            Self::FileInUse { path } => vec![format!(
                "設定ファイル {} は別のプロセス（エディタなど）で書き込みに開かれています。閉じてから、もう一度起動してください。{start}",
                path.display()
            )],
            Self::AccessDenied { path } => vec![format!(
                "{} を開けませんでした（アクセスが拒否されました）。設定ファイルの置き場は SYSTEM と Administrators だけが開けます。サービスとして起動するか、管理者として実行してください。{start}",
                path.display()
            )],
            Self::Unverifiable { path, reason } => vec![format!(
                "{} が保護されているかを確かめられませんでした: {reason}。{start}",
                path.display()
            )],
            Self::EnvInvalid { value, reason } => vec![format!(
                "環境変数 {ENV_CONFIG_PATH} の値 {} は使えません: {reason}（設定ファイルを絶対パスで指定してください）。{start}",
                value.display()
            )],
            Self::ProgramDataUnavailable(reason) => vec![format!(
                "ProgramData の場所を取れませんでした: {reason}。{start}"
            )],
            Self::UnsupportedPlatform => vec![format!(
                "この OS では設定ファイルが保護されているかを確かめられません（banto-hub-sink は Windows 専用です）。{start}"
            )],
        }
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.lines().join("\n"))
    }
}

impl std::error::Error for ConfigError {}

/// toml の生の形。既定値の補完はここで行い、範囲検証は
/// [`SidecarConfig::from_file_config`] が行う（serde のエラーと業務的な
/// 検証エラーを混ぜないため）。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    hub_url: String,
    api_key: String,
    #[serde(default = "default_config_refresh_secs")]
    config_refresh_secs: u64,
    #[serde(default = "default_status_push_secs")]
    status_push_secs: u64,
    #[serde(default = "default_queue_max_rows")]
    queue_max_rows: usize,
    #[serde(default = "default_flush_interval_ms")]
    flush_interval_ms: u64,
    #[serde(default = "default_batch_size")]
    batch_size: usize,
    #[serde(default = "default_shutdown_flush_secs")]
    shutdown_flush_secs: u64,
}

fn default_config_refresh_secs() -> u64 {
    DEFAULT_CONFIG_REFRESH_SECS
}
fn default_status_push_secs() -> u64 {
    DEFAULT_STATUS_PUSH_SECS
}
fn default_queue_max_rows() -> usize {
    DEFAULT_QUEUE_MAX_ROWS
}
fn default_flush_interval_ms() -> u64 {
    DEFAULT_FLUSH_INTERVAL_MS
}
fn default_batch_size() -> usize {
    DEFAULT_BATCH_SIZE
}
fn default_shutdown_flush_secs() -> u64 {
    DEFAULT_SHUTDOWN_FLUSH_SECS
}

/// 検証済みの設定。`Debug` は手書き（このモジュールの doc comment参照）。
#[derive(Clone)]
pub struct SidecarConfig {
    /// `http://127.0.0.1:<port>`。`banto_tagclient::Endpoint` が受け付ける
    /// 形（http のみ・クエリ/フラグメント/userinfo なし）であることを
    /// 読み込み時に確認済み。
    pub hub_url: String,
    /// `admin` + `read` スコープの API キー（このモジュールの doc comment
    /// 「API キーに必要なスコープ」参照）。**ログ禁止**。
    pub api_key: String,
    pub config_refresh: Duration,
    pub status_push: Duration,
    pub queue_max_rows: usize,
    pub flush_interval: Duration,
    pub batch_size: usize,
    pub shutdown_flush: Duration,
}

impl fmt::Debug for SidecarConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SidecarConfig")
            .field("hub_url", &self.hub_url)
            .field("api_key", &"<redacted>")
            .field("config_refresh", &self.config_refresh)
            .field("status_push", &self.status_push)
            .field("queue_max_rows", &self.queue_max_rows)
            .field("flush_interval", &self.flush_interval)
            .field("batch_size", &self.batch_size)
            .field("shutdown_flush", &self.shutdown_flush)
            .finish()
    }
}

impl SidecarConfig {
    /// TOML 文字列から読む（ファイル I/O 抜き - 単体テストと
    /// [`load_config`] の共通経路）。
    pub fn from_toml_str(raw: &str, path: &Path) -> Result<Self, ConfigError> {
        let file: FileConfig = toml::from_str(raw).map_err(|err| ConfigError::Parse {
            path: path.to_path_buf(),
            message: err.to_string(),
        })?;
        Self::from_file_config(file)
    }

    fn from_file_config(file: FileConfig) -> Result<Self, ConfigError> {
        let hub_url = file.hub_url.trim().to_string();
        // `Endpoint::new` は http 限定・userinfo/クエリ/フラグメント拒否と
        // いう、このサイドカーが必要とする検証をそのまま持っている
        // （SDK が実際に使う型なので、ここで通れば購読も通る）。
        banto_tagclient::Endpoint::new(&hub_url).map_err(|_| ConfigError::Invalid {
            field: "hub_url",
            message:
                "http://<host>:<port> の形で指定してください（https・クエリ・ユーザー情報は不可）"
                    .to_string(),
        })?;

        let api_key = file.api_key.trim().to_string();
        if api_key.is_empty() {
            return Err(ConfigError::Invalid {
                field: "api_key",
                message: "空です。Hub で admin と read のスコープを持つ API キーを発行して設定してください".to_string(),
            });
        }
        // SDK の `SecretApiKey` と同じ受け入れ条件（bearer ヘッダに載る
        // 可視 ASCII のみ）。ここで弾いておけば購読開始時に
        // `InvalidSecret` で落ちることがない。
        if !api_key.bytes().all(|byte| (0x21..=0x7e).contains(&byte)) {
            return Err(ConfigError::Invalid {
                field: "api_key",
                message: "空白や制御文字を含んでいます（可視 ASCII のみ使用できます）".to_string(),
            });
        }

        range_u64(
            "config_refresh_secs",
            file.config_refresh_secs,
            MIN_CONFIG_REFRESH_SECS,
            MAX_CONFIG_REFRESH_SECS,
        )?;
        range_u64(
            "status_push_secs",
            file.status_push_secs,
            MIN_STATUS_PUSH_SECS,
            MAX_STATUS_PUSH_SECS,
        )?;
        range_usize(
            "queue_max_rows",
            file.queue_max_rows,
            MIN_QUEUE_MAX_ROWS,
            MAX_QUEUE_MAX_ROWS,
        )?;
        range_u64(
            "flush_interval_ms",
            file.flush_interval_ms,
            MIN_FLUSH_INTERVAL_MS,
            MAX_FLUSH_INTERVAL_MS,
        )?;
        range_usize(
            "batch_size",
            file.batch_size,
            MIN_BATCH_SIZE,
            MAX_BATCH_SIZE,
        )?;
        range_u64(
            "shutdown_flush_secs",
            file.shutdown_flush_secs,
            0,
            MAX_SHUTDOWN_FLUSH_SECS,
        )?;
        if file.batch_size > file.queue_max_rows {
            return Err(ConfigError::Invalid {
                field: "batch_size",
                message: format!(
                    "queue_max_rows（{}）より大きくできません",
                    file.queue_max_rows
                ),
            });
        }

        Ok(Self {
            hub_url,
            api_key,
            config_refresh: Duration::from_secs(file.config_refresh_secs),
            status_push: Duration::from_secs(file.status_push_secs),
            queue_max_rows: file.queue_max_rows,
            flush_interval: Duration::from_millis(file.flush_interval_ms),
            batch_size: file.batch_size,
            shutdown_flush: Duration::from_secs(file.shutdown_flush_secs),
        })
    }

    /// `hub_url` のホストがループバックか（設計 §6-15「パスワードが平文で
    /// 流れるためループバック運用」）。false のときは起動時に 1 回だけ
    /// `warn` する - 拒否はしない（LAN 越しの構成を選ぶのは利用者の判断で、
    /// 設計はそれを「明記する」とだけ決めている）。
    pub fn hub_is_loopback(&self) -> bool {
        match banto_tagclient::Endpoint::new(&self.hub_url) {
            Ok(endpoint) => matches!(
                endpoint.host(),
                Some("127.0.0.1") | Some("localhost") | Some("[::1]") | Some("::1")
            ),
            Err(_) => false,
        }
    }
}

fn range_u64(field: &'static str, value: u64, min: u64, max: u64) -> Result<(), ConfigError> {
    if value < min || value > max {
        return Err(ConfigError::Invalid {
            field,
            message: format!("{min}〜{max} の範囲で指定してください（指定値: {value}）"),
        });
    }
    Ok(())
}

fn range_usize(
    field: &'static str,
    value: usize,
    min: usize,
    max: usize,
) -> Result<(), ConfigError> {
    if value < min || value > max {
        return Err(ConfigError::Invalid {
            field,
            message: format!("{min}〜{max} の範囲で指定してください（指定値: {value}）"),
        });
    }
    Ok(())
}

/// 設定を読んで検証する（モジュール doc「置き場」）。置き場・旧い置き場を決め
/// （[`place::decide_paths`]）、置き場とファイルが保護されていることを確かめてから、確かめた
/// ハンドルのまま読む（[`place::read_protected`]）。採用したパスは起動時に 1 行ログする
/// （[`crate::log::log_line`]。パスのみ・内容は一切出さない）。
///
/// `mode` が [`RunMode::Service`] なら環境変数 [`ENV_CONFIG_PATH`] を見ない。非 Windows では
/// 保護を確かめられないので [`ConfigError::UnsupportedPlatform`]。
pub fn load_config(mode: RunMode) -> Result<SidecarConfig, ConfigError> {
    #[cfg(windows)]
    {
        let env_value = match mode {
            RunMode::Service => None,
            RunMode::Console => std::env::var_os(ENV_CONFIG_PATH),
        };
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(Path::to_path_buf));
        let paths = place::decide_paths(
            mode,
            env_value,
            place::win::program_data(),
            exe_dir.as_deref(),
        )?;
        let (path, raw) = place::read_protected(&place::win::WindowsPlaceOps, &paths)?;
        let how = match paths.source {
            place::Source::Default => String::new(),
            place::Source::Env => format!("（環境変数 {ENV_CONFIG_PATH} の指定）"),
        };
        crate::log::log_line(&format!(
            "banto-hub-sink: 設定ファイル {}{how} を使用します",
            path.display()
        ));
        SidecarConfig::from_toml_str(&raw, &path)
    }
    #[cfg(not(windows))]
    {
        let _ = mode;
        Err(ConfigError::UnsupportedPlatform)
    }
}

/// 明示したパスから、**保護の確かめをせずに**読む（統合テスト・検証用の口。製品の起動は
/// 使わない - [`load_config`]）。
pub fn load_config_from(path: &Path) -> Result<SidecarConfig, ConfigError> {
    let raw = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    SidecarConfig::from_toml_str(&raw, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path() -> PathBuf {
        PathBuf::from("banto-hub-sink.toml")
    }

    const MINIMAL: &str = r#"
hub_url = "http://127.0.0.1:8722"
api_key = "bh_test.secret"
"#;

    #[test]
    fn minimal_file_fills_in_every_default() {
        let config = SidecarConfig::from_toml_str(MINIMAL, &path()).expect("minimal config");
        assert_eq!(config.hub_url, "http://127.0.0.1:8722");
        assert_eq!(config.config_refresh, Duration::from_secs(30));
        assert_eq!(config.status_push, Duration::from_secs(5));
        assert_eq!(config.queue_max_rows, 10_000);
        assert_eq!(config.flush_interval, Duration::from_millis(1_000));
        assert_eq!(config.batch_size, 500);
        assert_eq!(config.shutdown_flush, Duration::from_secs(5));
        assert!(config.hub_is_loopback());
    }

    #[test]
    fn every_value_can_be_overridden() {
        let raw = r#"
hub_url = "http://10.0.0.5:8722"
api_key = "bh_x.y"
config_refresh_secs = 60
status_push_secs = 2
queue_max_rows = 500
flush_interval_ms = 250
batch_size = 100
shutdown_flush_secs = 0
"#;
        let config = SidecarConfig::from_toml_str(raw, &path()).expect("full config");
        assert_eq!(config.config_refresh, Duration::from_secs(60));
        assert_eq!(config.status_push, Duration::from_secs(2));
        assert_eq!(config.queue_max_rows, 500);
        assert_eq!(config.flush_interval, Duration::from_millis(250));
        assert_eq!(config.batch_size, 100);
        assert_eq!(config.shutdown_flush, Duration::from_secs(0));
        // LAN 越しの Hub はループバックではない（起動時に warn する対象）。
        assert!(!config.hub_is_loopback());
    }

    /// 綴り間違いが既定値で静かに通らないこと（このモジュールの doc
    /// comment「未知キーを拒否する理由」）。
    #[test]
    fn unknown_key_is_rejected() {
        let raw = format!("{MINIMAL}\nbatch_sizes = 100\n");
        let err = SidecarConfig::from_toml_str(&raw, &path()).expect_err("unknown key");
        assert!(matches!(err, ConfigError::Parse { .. }), "{err:?}");
        assert!(err.to_string().contains("batch_sizes"), "{err}");
    }

    #[test]
    fn missing_required_keys_are_rejected() {
        let err = SidecarConfig::from_toml_str("api_key = \"bh_x\"\n", &path())
            .expect_err("missing hub_url");
        assert!(matches!(err, ConfigError::Parse { .. }), "{err:?}");
    }

    #[test]
    fn hub_url_must_be_a_plain_http_endpoint() {
        for bad in [
            "https://127.0.0.1:8722",
            "http://user:pw@127.0.0.1:8722",
            "http://127.0.0.1:8722/?x=1",
            "127.0.0.1:8722",
            "",
        ] {
            let raw = format!("hub_url = \"{bad}\"\napi_key = \"bh_x\"\n");
            match SidecarConfig::from_toml_str(&raw, &path()) {
                Err(ConfigError::Invalid {
                    field: "hub_url", ..
                }) => {}
                other => panic!("{bad} should be rejected as hub_url: {other:?}"),
            }
        }
    }

    #[test]
    fn api_key_must_be_present_and_header_safe() {
        let raw = "hub_url = \"http://127.0.0.1:8722\"\napi_key = \"\"\n";
        let err = SidecarConfig::from_toml_str(raw, &path()).expect_err("empty key");
        assert!(
            matches!(
                err,
                ConfigError::Invalid {
                    field: "api_key",
                    ..
                }
            ),
            "{err:?}"
        );

        let raw = "hub_url = \"http://127.0.0.1:8722\"\napi_key = \"bh with space\"\n";
        let err = SidecarConfig::from_toml_str(raw, &path()).expect_err("space in key");
        assert!(
            matches!(
                err,
                ConfigError::Invalid {
                    field: "api_key",
                    ..
                }
            ),
            "{err:?}"
        );
    }

    #[test]
    fn out_of_range_values_are_rejected_with_the_field_name() {
        let cases: &[(&str, &str)] = &[
            ("config_refresh_secs = 1", "config_refresh_secs"),
            ("config_refresh_secs = 100000", "config_refresh_secs"),
            ("status_push_secs = 0", "status_push_secs"),
            // Hub の 15 秒 unknown 判定より短くなければならない。
            ("status_push_secs = 15", "status_push_secs"),
            ("queue_max_rows = 10", "queue_max_rows"),
            ("flush_interval_ms = 10", "flush_interval_ms"),
            ("flush_interval_ms = 60001", "flush_interval_ms"),
            ("batch_size = 0", "batch_size"),
            ("batch_size = 20000", "batch_size"),
            ("shutdown_flush_secs = 61", "shutdown_flush_secs"),
        ];
        for (line, field) in cases {
            let raw = format!("{MINIMAL}\n{line}\n");
            match SidecarConfig::from_toml_str(&raw, &path()) {
                Err(ConfigError::Invalid { field: got, .. }) => assert_eq!(&got, field, "{line}"),
                other => panic!("{line} should be rejected: {other:?}"),
            }
        }
    }

    #[test]
    fn batch_size_cannot_exceed_the_queue_size() {
        let raw = format!("{MINIMAL}\nqueue_max_rows = 100\nbatch_size = 101\n");
        let err = SidecarConfig::from_toml_str(&raw, &path()).expect_err("batch > queue");
        assert!(
            matches!(
                err,
                ConfigError::Invalid {
                    field: "batch_size",
                    ..
                }
            ),
            "{err:?}"
        );
    }

    /// 構造体ごとダンプする 1 行のログでキーが漏れないこと。
    #[test]
    fn debug_never_prints_the_api_key() {
        let config = SidecarConfig::from_toml_str(MINIMAL, &path()).expect("config");
        let rendered = format!("{config:?}");
        assert!(!rendered.contains("bh_test.secret"), "{rendered}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
    }

    /// エラー文言自体にもキーを載せない。
    #[test]
    fn error_text_never_contains_the_api_key() {
        let raw = "hub_url = \"nope\"\napi_key = \"bh_test.secret\"\n";
        let err = SidecarConfig::from_toml_str(raw, &path()).expect_err("bad url");
        assert!(!err.to_string().contains("bh_test.secret"), "{err}");
    }
}
