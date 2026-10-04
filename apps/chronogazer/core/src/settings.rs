//! App settings storage (spec §12.1 `SettingsProvider` role): the generic
//! `key`/`value` store and the typed views over it.
//!
//! ## 中身はほぼ banto のもの（I2a、2026-10-04）
//!
//! [`SettingsService`] と、その上の型付きの設定（LAN 公開 [`ServerSettings`]・
//! 認証モード [`AuthSettings`]・監査ログの保持 [`AuditSettings`]）は
//! `banto_admin_services::settings` をそのまま re-export している（以前は
//! admin-template からのコピーを持っていた。独自実装は banto に寄せる、
//! 2026-10-01 オーナー方針）。テーブル（`settings`）の定義はこの app の
//! `migrations-sqlite/`（admin-template の byte 等価コピー、`crate::db`）が
//! 持つ - banto conventions §11「テーブルはアプリが持つ」。
//!
//! このモジュールに自前で残しているのは、ChronoGazer 固有の設定キーの
//! **型付きラッパ**だけ:
//!
//! - [`StoreSettings`]（`data.dir`・`retention.days`）- [`store_config`] /
//!   [`set_store_config`]。banto の汎用 [`SettingsService::get`] /
//!   [`SettingsService::set_many`] の上に乗る。
//! - `hub.*` は `crate::hub` が同じく汎用の `get`/`set` で読み書きする。
//!
//! ## 閲覧公開（`server.viewer_public`）を使う（2026-10-04 オーナー決定）
//!
//! banto の [`ServerSettings`] の `viewer_public`（閲覧公開、ADR-0012）を、
//! admin-template と同じ形で使う（同日の I2a で「画面に出さない」と決めた
//! のを、I2b で変更した）。手元の端末はログイン不要モード、LAN の相手には
//! 閲覧だけ、という banto の使い方ができる:
//!
//! - `crate::rest::api_router` が `GrantSpec::public_viewer` を登録し、
//!   `POST /api/auth/grant/publicViewer` が ON の間だけ固定の viewer の
//!   セッションを発行する（条件はリクエストのたびにこの設定を読む）。
//! - 書き込みは `src-tauri` の `server_apply`（LAN 設定の「保存して適用」）。
//!   OFF にしたら保存の直後に発行済みの閲覧者のトークンを失効させる
//!   （ADR-0017 の順序）。
//! - 「認証無効 + LAN 有効」は閲覧公開が ON のときだけ許される
//!   （[`auth_server_combination_allowed`]、banto #288。保存時は banto の
//!   [`SettingsService::set_server_config`] / `set_auth_config` が、起動時の
//!   自動開始は `src-tauri` がこの述語で判定する）。
//! - 既定は OFF（banto の [`ServerSettings`] の既定）。

use banto_core::BantoError;
use serde::{Deserialize, Serialize};

pub use banto_admin_services::settings::*;

/// #383 段階2b / R1-C: 収集ランタイムの保存先と保持期間
/// （`apps/banto-hub/core/src/settings.rs` の `StoreSettings` と同じキー名）。
const KEY_DATA_DIR: &str = "data.dir";
const KEY_RETENTION_DAYS: &str = "retention.days";

/// 時系列ファイルの既定の置き場（#383 段階2b / R1-C）。banto-hub の
/// `StoreSettings` に倣った相対パス。**相対パスの解決先**は
/// [`crate::collect::resolve_data_dir`] が決める（作業ディレクトリではなく
/// アプリのデータディレクトリ基準 - デスクトップアプリの作業ディレクトリは
/// 何であるか分からないため）。
const DEFAULT_DATA_DIR: &str = "./data";

/// 時系列データの既定の保持期間（日）。**90 日**
/// （docs/recorder-requirements.md §3.4「保持期間既定 90 日（設定可）」）。
///
/// banto-hub の既定は 7 日だが、あれは**Hub のローカル記録**の話なので
/// 真似しない。ChronoGazer は記録計そのものであり、要件の 90 日が正。
const DEFAULT_RETENTION_DAYS: i64 = 90;

/// 保存されている保持期間の読み方。banto の `AuditSettings` と同じ約束
/// （banto 側の同名の関数は非公開なので、ここに写している）: 未設定は
/// `default`、数値として読めない値も `default`、読めた値は `0` 以下を
/// 「無制限」（`None`）に正規化する - とくに `"0"` は「利用者が無制限を
/// 選んだ」なので `default` に戻してはいけない。
fn parse_retention(raw: Option<String>, default: Option<i64>) -> Option<i64> {
    match raw {
        Some(value) => value
            .parse::<i64>()
            .map(|days| if days > 0 { Some(days) } else { None })
            .unwrap_or(default),
        None => default,
    }
}

/// 収集ランタイムの保存設定（#383 段階2b / R1-C、キーは
/// `data.dir` / `retention.days`）。`apps/banto-hub/core/src/settings.rs` の
/// 同名の型が手本。
///
/// * `data_dir`: 時系列ファイル（`banto-tstore`）の置き場。既定は banto-hub に
///   倣って `"./data"`。**相対パスの解決先**は
///   [`crate::collect::resolve_data_dir`]（アプリのデータディレクトリ基準）。
/// * `retention_days`: 保持期間。既定 **90 日**
///   （docs/recorder-requirements.md §3.4）。`None` は「無制限」
///   （[`AuditSettings`] と同じ約束 - [`parse_retention`]）。
///
/// # このコードはファイルを一切削除しない
///
/// 期限超過ファイルの自動削除（要件 §3.4）は**この PR（C-1）では実装して
/// いない**。`retention_days` は**設定値を持つだけ**で、chronogazer のどの
/// コードもこれを読んで削除を行わない - 誤って削除を先取りしないための
/// 明示的な線引きで、削除は別途実装する。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreSettings {
    pub data_dir: String,
    pub retention_days: Option<i64>,
}

impl Default for StoreSettings {
    fn default() -> Self {
        Self {
            data_dir: DEFAULT_DATA_DIR.to_string(),
            retention_days: Some(DEFAULT_RETENTION_DAYS),
        }
    }
}

/// 収集ランタイムの保存設定を読む（#383 段階2b / R1-C）。未設定のキーは
/// [`StoreSettings::default`] にフォールバックする。`retention_days` の
/// 読み方は [`parse_retention`]（`0` 以下は「無制限」= `None`）。
pub async fn store_config(settings: &SettingsService) -> Result<StoreSettings, BantoError> {
    let defaults = StoreSettings::default();
    let data_dir = settings
        .get(KEY_DATA_DIR)
        .await?
        .unwrap_or(defaults.data_dir);
    let retention_days = parse_retention(
        settings.get(KEY_RETENTION_DAYS).await?,
        defaults.retention_days,
    );
    Ok(StoreSettings {
        data_dir,
        retention_days,
    })
}

/// 収集ランタイムの保存設定を保存する。`None`（無制限）は `"0"` として
/// 書き戻す - banto の `SettingsService::set_audit_config` と同じ約束。2 つの
/// キーは 1 つのトランザクションで書く（[`SettingsService::set_many`]）。
pub async fn set_store_config(
    settings: &SettingsService,
    config: &StoreSettings,
) -> Result<(), BantoError> {
    let retention_days = config.retention_days.unwrap_or(0).to_string();
    settings
        .set_many(&[
            (KEY_DATA_DIR, config.data_dir.as_str()),
            (KEY_RETENTION_DAYS, retention_days.as_str()),
        ])
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{migrate_memory, Db};

    async fn service() -> SettingsService {
        let pool = migrate_memory().await.expect("migrate_memory");
        SettingsService::new(Db::Sqlite(pool))
    }

    /// 既定は `./data`・**90 日**（docs/recorder-requirements.md §3.4）。
    /// banto-hub の 7 日を真似していないことを固定する。
    #[tokio::test]
    async fn store_config_defaults_when_unset() {
        let svc = service().await;
        let config = store_config(&svc).await.unwrap();
        assert_eq!(config, StoreSettings::default());
        assert_eq!(config.data_dir, "./data");
        assert_eq!(config.retention_days, Some(90));
    }

    #[tokio::test]
    async fn store_config_round_trips_through_set() {
        let svc = service().await;
        let config = StoreSettings {
            data_dir: "D:/chronogazer-data".to_string(),
            retention_days: Some(365),
        };
        set_store_config(&svc, &config).await.unwrap();
        assert_eq!(store_config(&svc).await.unwrap(), config);
    }

    #[tokio::test]
    async fn store_config_none_round_trips_as_unlimited() {
        let svc = service().await;
        set_store_config(
            &svc,
            &StoreSettings {
                data_dir: "./data".to_string(),
                retention_days: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(store_config(&svc).await.unwrap().retention_days, None);
    }

    /// 数値として読めない値は既定に戻り、`"0"` は「無制限」のまま（既定に
    /// 戻さない）。
    #[tokio::test]
    async fn store_config_corrupt_value_falls_back_but_zero_stays_unlimited() {
        let svc = service().await;
        svc.set(KEY_RETENTION_DAYS, "not-a-number").await.unwrap();
        assert_eq!(store_config(&svc).await.unwrap().retention_days, Some(90));
        svc.set(KEY_RETENTION_DAYS, "0").await.unwrap();
        assert_eq!(store_config(&svc).await.unwrap().retention_days, None);
    }

    /// 閲覧公開（`server.viewer_public`）は既定で OFF（モジュール doc
    /// 「閲覧公開を使う」）。banto の `ServerSettings` の既定がそうで
    /// あることを、この app のスキーマの上で固定する。
    #[tokio::test]
    async fn viewer_public_is_off_by_default() {
        let svc = service().await;
        assert!(!svc.server_config().await.unwrap().viewer_public);
    }
}
