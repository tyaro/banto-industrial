//! Hub 接続（#332 relay-wright 分）: `banto-hub-bootstrap` をこのアプリの
//! 設定ストア・OS キーリングに配線するサービス層。
//!
//! relay-wright はこれまで banto-hub に接続するコードを一切持っていなかった
//! （PLC 直結の独立アプリで、設定画面の「接続」は自アプリの LAN 公開設定）。
//! したがってここは「既存接続の認証自動化」ではなく **Hub 接続機能の新規
//! 実装**にあたる。
//!
//! # 役割分担
//!
//! * 発行規則・6 状態・スコープのホワイトリストは
//!   [`banto_hub_bootstrap`] 側にある（アプリ非依存）。このモジュールは
//!   「どこに保存するか」だけを与える。
//! * 平文の API キーは **OS キーリングだけ**に入る（[`KeyStore`]）。設定 KV
//!   には [`HubRecord`] の JSON（接続先・キー ID・キー名・キーリングの
//!   アカウント名・選択タグ）しか書かない - この型にキー欄が無いことが、
//!   「設定 DB に平文が無い」ことの根拠になる。
//! * 購読（`TagClientHandle`/`start`）はここでは行わない。選んだタグを
//!   トレンド等のデータ源へ繋ぐのは別 issue の仕事。
//!
//! # 同期 trait と非同期設定ストアの橋渡し
//!
//! [`BootstrapState`] は意図的に同期 trait（crate 側の設計、`KeyStore` が
//! 元々ブロッキングな OS 呼び出しであることに合わせている）だが、
//! [`SettingsService`] は sqlx で非同期。そこで [`SettingsMirror`] という
//! 小さなインメモリの写しを置き、[`HubService`] が bootstrapper を呼ぶ
//! **前に設定から hydrate し、後に変更があれば flush する**。
//! `block_on`/`block_in_place` は使わない（tauri のコマンドは既に非同期
//! ランタイム上で走っているため、同じランタイムで待つと panic する）。

use std::sync::{Arc, Mutex};

use banto_core::BantoError;
use banto_hub_bootstrap::{
    error::{Error as BootstrapError, ErrorKind as BootstrapErrorKind},
    BootstrapState, Bootstrapper, HubConnection, HubRecord, HubStatus, KeyStore,
};
use banto_tagclient::CatalogTag;
use serde::Serialize;

use crate::settings::SettingsService;

/// `banto-hub` が発行するキー名の先頭に付くアプリ識別子。
pub const APP_ID: &str = "relay-wright";

/// 設定 KV のキー。`hub.record` は [`HubRecord`] の JSON、
/// `hub.installation_id` はこのインストールの一意 ID。
const KEY_HUB_RECORD: &str = "hub.record";
const KEY_HUB_INSTALLATION_ID: &str = "hub.installation_id";

/// 画面に出す 1 タグ分（`GET /api/v1/tags` の catalog 行のうち、選択 UI に
/// 必要な列だけ）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HubTagView {
    pub external_name: String,
    pub name: String,
    pub data_type: String,
    pub unit: Option<String>,
    pub tag_kind: String,
}

impl From<&CatalogTag> for HubTagView {
    fn from(tag: &CatalogTag) -> Self {
        Self {
            external_name: tag.external_name.clone(),
            name: tag.name.clone(),
            data_type: tag.data_type.clone(),
            unit: tag.unit.clone(),
            tag_kind: tag.tag_kind.clone(),
        }
    }
}

/// Hub 接続画面が 1 回の往復で必要とするものすべて。`status` は
/// `banto-hub-bootstrap` の 6 状態の判別共用体をそのまま載せる
/// （`{"state":"connected","tagCount":0}` 等）。
///
/// `tags` は catalog を**実際に読めたときだけ** `Some`。読めなかったときに
/// 空配列を返すと「タグ 0 件で接続済み」と区別が付かなくなるため、
/// `null` と空配列を意図的に別物にしている（受入条件）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HubView {
    pub status: HubStatus,
    pub endpoint: Option<String>,
    pub key_name: Option<String>,
    pub selected_tags: Vec<String>,
    pub tags: Option<Vec<HubTagView>>,
}

impl HubView {
    fn new(status: HubStatus, record: Option<&HubRecord>, tags: Option<Vec<HubTagView>>) -> Self {
        Self {
            status,
            endpoint: record.map(|record| record.endpoint.clone()),
            key_name: record.and_then(|record| record.key_name.clone()),
            selected_tags: record
                .map(|record| record.selected_tags.clone())
                .unwrap_or_default(),
            tags,
        }
    }
}

/// [`BootstrapState`] のインメモリ実装。[`HubService`] が設定ストアとの
/// 間を往復させる（モジュール doc の「橋渡し」節）。
#[derive(Debug, Default)]
struct SettingsMirror {
    inner: Mutex<MirrorInner>,
}

#[derive(Debug, Default)]
struct MirrorInner {
    record: Option<HubRecord>,
    dirty: bool,
}

impl SettingsMirror {
    /// 設定から読んだ値で写しを置き換える（dirty はここでクリアする）。
    fn reset(&self, record: Option<HubRecord>) {
        let mut inner = self.inner.lock().expect("hub mirror poisoned");
        inner.record = record;
        inner.dirty = false;
    }

    /// 変更があれば「書き戻すべき値」を返す（`Some(None)` = 消去）。
    fn take_dirty(&self) -> Option<Option<HubRecord>> {
        let mut inner = self.inner.lock().expect("hub mirror poisoned");
        if !inner.dirty {
            return None;
        }
        inner.dirty = false;
        Some(inner.record.clone())
    }

    fn current(&self) -> Option<HubRecord> {
        self.inner
            .lock()
            .expect("hub mirror poisoned")
            .record
            .clone()
    }
}

impl BootstrapState for SettingsMirror {
    fn load(&self) -> Result<Option<HubRecord>, BootstrapError> {
        Ok(self.current())
    }

    fn save(&self, record: &HubRecord) -> Result<(), BootstrapError> {
        let mut inner = self.inner.lock().expect("hub mirror poisoned");
        inner.record = Some(record.clone());
        inner.dirty = true;
        Ok(())
    }

    fn clear(&self) -> Result<(), BootstrapError> {
        let mut inner = self.inner.lock().expect("hub mirror poisoned");
        inner.record = None;
        inner.dirty = true;
        Ok(())
    }
}

/// OS キーリングが無い実行形態（`relay-wright-serve`: Tauri 非依存の開発・E2E
/// 用サーバー）向けの [`KeyStore`]。
///
/// 読み出しは「エントリ無し」を返し、書き込みは**失敗する** - 平文の API
/// キーを設定 DB やファイルに落とす代替経路を作らないため。この実行形態
/// でも「到達不能な Hub へ接続を試みて `Unreachable` になる」「ロック
/// ダウン済みで `NeedsPairing` になる」といった、キーを保存しない範囲の
/// 動作はそのまま確認できる。
#[derive(Debug, Default)]
pub struct UnavailableKeyStore;

impl KeyStore for UnavailableKeyStore {
    fn get(&self, _account: &str) -> Result<Option<String>, BootstrapError> {
        Ok(None)
    }

    fn set(&self, _account: &str, _secret: &str) -> Result<(), BootstrapError> {
        Err(BootstrapError::with_detail(
            BootstrapErrorKind::KeyStore,
            "この実行形態では OS キーリングを利用できません（デスクトップアプリから設定してください）",
        ))
    }

    fn delete(&self, _account: &str) -> Result<(), BootstrapError> {
        Ok(())
    }
}

/// `banto-hub-bootstrap` のエラーを、このアプリの境界型へ。
///
/// `detail` は crate 側で「鍵・エンドポイントのパスを含めない」と規定されて
/// いるものだけが入る（[`KeyStore`] 実装が返す「キーリングが使えない理由」
/// など）ので、そのまま画面に出してよい。
fn to_banto_error(error: BootstrapError) -> BantoError {
    let detail = error.detail().unwrap_or_default().to_string();
    match error.kind() {
        BootstrapErrorKind::InvalidEndpoint => BantoError::Validation {
            field_errors: vec![banto_core::FieldError {
                field: "endpoint".to_string(),
                message: "接続先は http://ホスト:ポート の形式で入力してください".to_string(),
            }],
        },
        BootstrapErrorKind::InvalidKey => BantoError::Validation {
            field_errors: vec![banto_core::FieldError {
                field: "key".to_string(),
                message: "APIキーの形式が正しくありません".to_string(),
            }],
        },
        BootstrapErrorKind::ForbiddenScope => BantoError::Other(format!(
            "このスコープの自動発行は許可されていません: {detail}"
        )),
        BootstrapErrorKind::KeyStore => BantoError::Other(if detail.is_empty() {
            "OSキーリングを利用できません".to_string()
        } else {
            detail
        }),
        BootstrapErrorKind::State => {
            BantoError::Storage(format!("Hub接続設定の保存に失敗しました: {detail}"))
        }
        BootstrapErrorKind::NotConfigured => {
            BantoError::Other("Hubへの接続がまだ設定されていません".to_string())
        }
        // `ErrorKind` は `#[non_exhaustive]`。将来 crate 側に分類が増えた
        // ときも、既知の分類の文言を取り違えるより「不明」として出す。
        _ => BantoError::Other(format!("Hubへの接続に失敗しました: {error}")),
    }
}

/// Hub 接続のサービス層。`src-tauri` の `hub_*` コマンドと
/// `crate::rest` の `/api/hub/*` ルーターが共有する（他のサービスと同じ
/// 「service 層は tauri も axum も知らない」規約）。
#[derive(Clone)]
pub struct HubService {
    settings: SettingsService,
    mirror: Arc<SettingsMirror>,
    bootstrapper: Arc<Bootstrapper>,
}

impl HubService {
    /// このインストールの `installation_id` を設定から読み（無ければ生成
    /// して保存し）、bootstrapper を組み立てる。
    ///
    /// `keys` は実行形態ごとに差し替える: デスクトップ（`src-tauri`）は OS
    /// キーリング、`relay-wright-serve` は [`UnavailableKeyStore`]。
    pub async fn new(
        settings: SettingsService,
        keys: Arc<dyn KeyStore>,
    ) -> Result<Self, BantoError> {
        let installation_id = match settings.get(KEY_HUB_INSTALLATION_ID).await? {
            Some(value) if !value.trim().is_empty() => value,
            _ => {
                let generated = generate_installation_id();
                settings.set(KEY_HUB_INSTALLATION_ID, &generated).await?;
                generated
            }
        };
        let mirror = Arc::new(SettingsMirror::default());
        let bootstrapper = Arc::new(Bootstrapper::new(
            APP_ID,
            installation_id,
            keys,
            Arc::clone(&mirror) as Arc<dyn BootstrapState>,
        ));
        Ok(Self {
            settings,
            mirror,
            bootstrapper,
        })
    }

    /// 保存済みの設定で現在の状態を返す。**発行は絶対に行わない**
    /// （設定画面を開いただけでキーが増えないように）。
    pub async fn status(&self) -> Result<HubView, BantoError> {
        self.hydrate().await?;
        let record = self.mirror.current();
        if record.is_none() {
            return Ok(HubView::new(HubStatus::NotConfigured, None, None));
        }
        let connection = self
            .bootstrapper
            .refresh_catalog()
            .await
            .map_err(to_banto_error)?;
        self.flush().await?;
        Ok(self.view(connection))
    }

    /// 接続（保存済みキーがあれば再利用、無ければ試運転中のみ自己発行）。
    pub async fn connect(&self, endpoint: &str) -> Result<HubView, BantoError> {
        self.hydrate().await?;
        let connection = self
            .bootstrapper
            .connect(endpoint.trim())
            .await
            .map_err(to_banto_error)?;
        self.flush().await?;
        Ok(self.view(connection))
    }

    /// ロックダウン済み Hub 向けの手動連携。平文はキーリングにだけ入る。
    pub async fn adopt_manual_key(
        &self,
        endpoint: &str,
        key: String,
    ) -> Result<HubView, BantoError> {
        self.hydrate().await?;
        let connection = self
            .bootstrapper
            .adopt_manual_key(endpoint.trim(), key)
            .await
            .map_err(to_banto_error)?;
        self.flush().await?;
        Ok(self.view(connection))
    }

    /// タグ一覧の再取得。
    pub async fn refresh_catalog(&self) -> Result<HubView, BantoError> {
        self.hydrate().await?;
        let connection = self
            .bootstrapper
            .refresh_catalog()
            .await
            .map_err(to_banto_error)?;
        self.flush().await?;
        Ok(self.view(connection))
    }

    /// 選択タグの保存。空でも保存できる（受入条件）。
    pub async fn set_selected_tags(&self, tags: Vec<String>) -> Result<(), BantoError> {
        self.hydrate().await?;
        self.bootstrapper
            .set_selected_tags(tags)
            .map_err(to_banto_error)?;
        self.flush().await
    }

    /// 切断: ローカルの設定とキーリングだけを消す。Hub 側のキーは失効
    /// させない（他のインストールを巻き込まないため - crate 側の
    /// `disconnect` の doc comment参照）。
    pub async fn disconnect(&self) -> Result<HubView, BantoError> {
        self.hydrate().await?;
        self.bootstrapper.disconnect().map_err(to_banto_error)?;
        self.flush().await?;
        Ok(HubView::new(HubStatus::NotConfigured, None, None))
    }

    fn view(&self, connection: HubConnection) -> HubView {
        let record = self.mirror.current();
        let tags = connection
            .catalog
            .as_ref()
            .map(|catalog| catalog.tags.iter().map(HubTagView::from).collect());
        HubView::new(connection.status, record.as_ref(), tags)
    }

    /// 設定ストア → インメモリの写し。
    async fn hydrate(&self) -> Result<(), BantoError> {
        let raw = self.settings.get(KEY_HUB_RECORD).await?;
        let record = match raw {
            Some(value) if !value.trim().is_empty() => {
                match serde_json::from_str(&value) {
                    Ok(record) => Some(record),
                    Err(err) => {
                        // 壊れた行で起動不能にはしない - 未設定として扱い、
                        // 次の接続で上書きされる。
                        eprintln!("banto: Hub接続設定の読み取りに失敗しました（未設定として扱います）: {err}");
                        None
                    }
                }
            }
            _ => None,
        };
        self.mirror.reset(record);
        Ok(())
    }

    /// インメモリの写し → 設定ストア（変更があったときだけ）。
    ///
    /// `SettingsService` に削除 API は無いので、消去は空文字列の upsert で
    /// 表す（[`Self::hydrate`] が空文字列を「未設定」として読む）。
    async fn flush(&self) -> Result<(), BantoError> {
        let Some(change) = self.mirror.take_dirty() else {
            return Ok(());
        };
        let value = match change {
            Some(record) => serde_json::to_string(&record).map_err(|err| {
                BantoError::Storage(format!("Hub接続設定のシリアライズに失敗しました: {err}"))
            })?,
            None => String::new(),
        };
        self.settings.set(KEY_HUB_RECORD, &value).await
    }
}

/// UUID v4 形式のインストール ID。
///
/// `uuid` クレートはこのワークスペースの直接依存に無く、これ 1 箇所の
/// ために増やす理由も無いので、既にワークスペース依存にある `getrandom`
/// （`apps/banto-hub/core::api_keys` が API キーの生成に使っているのと同じ
/// もの。新しい依存ノードは増えない）で 128 ビットを取り、version 4 /
/// variant 1 のビットを立てて正規形に整形する。
fn generate_installation_id() -> String {
    let mut bytes = [0u8; 16];
    // getrandom の失敗（OS 側の乱数源が壊れている等）は復旧不能なので
    // `expect` で落とす - `apps/banto-hub/core::api_keys` と同じ扱い。
    getrandom::fill(&mut bytes).expect("システム乱数生成器の呼び出しに失敗しました");
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::init_db_memory;

    async fn service() -> (SettingsService, HubService) {
        let pool = init_db_memory().await.expect("init_db_memory");
        let settings = SettingsService::new(pool);
        let hub = HubService::new(settings.clone(), Arc::new(UnavailableKeyStore))
            .await
            .expect("HubService::new");
        (settings, hub)
    }

    #[tokio::test]
    async fn installation_id_is_generated_once_and_reused() {
        let (settings, _hub) = service().await;
        let first = settings
            .get(KEY_HUB_INSTALLATION_ID)
            .await
            .unwrap()
            .expect("installation id must be persisted");
        assert_eq!(first.len(), 36, "canonical UUID form");
        assert_eq!(&first[14..15], "4", "version 4");

        let again = HubService::new(settings.clone(), Arc::new(UnavailableKeyStore))
            .await
            .unwrap();
        drop(again);
        assert_eq!(
            settings.get(KEY_HUB_INSTALLATION_ID).await.unwrap(),
            Some(first)
        );
    }

    #[tokio::test]
    async fn generated_ids_differ() {
        assert_ne!(generate_installation_id(), generate_installation_id());
    }

    #[tokio::test]
    async fn status_is_not_configured_before_any_connect() {
        let (settings, hub) = service().await;
        let view = hub.status().await.unwrap();
        assert_eq!(view.status, HubStatus::NotConfigured);
        assert!(view.endpoint.is_none());
        assert!(view.tags.is_none(), "an unread catalog is null, not empty");
        assert_eq!(settings.get(KEY_HUB_RECORD).await.unwrap(), None);
    }

    #[tokio::test]
    async fn an_unreachable_endpoint_leaves_the_app_unconfigured() {
        // Bind then drop so the port is well-formed but closed.
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);

        let (settings, hub) = service().await;
        let view = hub.connect(&endpoint).await.unwrap();
        assert_eq!(view.status.as_str(), "unreachable");
        assert!(view.tags.is_none());
        assert_eq!(
            settings.get(KEY_HUB_RECORD).await.unwrap(),
            None,
            "a failed connect must not persist a record"
        );
    }

    #[tokio::test]
    async fn an_invalid_endpoint_is_a_field_validation_error() {
        let (_settings, hub) = service().await;
        match hub.connect("https://example.test").await {
            Err(BantoError::Validation { field_errors }) => {
                assert_eq!(field_errors[0].field, "endpoint");
            }
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn the_record_round_trips_through_the_settings_kv_without_a_key() {
        let (settings, hub) = service().await;
        // Seed a record the way a successful connect would have.
        let record = HubRecord {
            endpoint: "http://127.0.0.1:3100".to_owned(),
            installation_id: "inst".to_owned(),
            key_id: Some(3),
            key_name: Some("relay-wright-inst-1000".to_owned()),
            keyring_account: "hub:127.0.0.1:3100:inst".to_owned(),
            selected_tags: vec!["line1.fast.temp01".to_owned()],
        };
        settings
            .set(KEY_HUB_RECORD, &serde_json::to_string(&record).unwrap())
            .await
            .unwrap();

        hub.set_selected_tags(vec!["line1.fast.temp02".to_owned()])
            .await
            .unwrap();

        let stored = settings.get(KEY_HUB_RECORD).await.unwrap().unwrap();
        assert!(
            !stored.contains("\"key\""),
            "no plaintext key may be stored: {stored}"
        );
        let read_back: HubRecord = serde_json::from_str(&stored).unwrap();
        assert_eq!(
            read_back.selected_tags,
            vec!["line1.fast.temp02".to_owned()]
        );
        assert_eq!(read_back.key_id, Some(3));
    }

    #[tokio::test]
    async fn an_empty_selection_is_savable_and_disconnect_clears_the_record() {
        let (settings, hub) = service().await;
        let record = HubRecord {
            endpoint: "http://127.0.0.1:3100".to_owned(),
            installation_id: "inst".to_owned(),
            key_id: Some(3),
            key_name: None,
            keyring_account: "hub:127.0.0.1:3100:inst".to_owned(),
            selected_tags: vec!["line1.fast.temp01".to_owned()],
        };
        settings
            .set(KEY_HUB_RECORD, &serde_json::to_string(&record).unwrap())
            .await
            .unwrap();

        hub.set_selected_tags(Vec::new()).await.unwrap();
        let stored: HubRecord =
            serde_json::from_str(&settings.get(KEY_HUB_RECORD).await.unwrap().unwrap()).unwrap();
        assert!(stored.selected_tags.is_empty());

        let view = hub.disconnect().await.unwrap();
        assert_eq!(view.status, HubStatus::NotConfigured);
        assert_eq!(
            settings.get(KEY_HUB_RECORD).await.unwrap().as_deref(),
            Some(""),
            "clearing is an empty upsert (SettingsService has no delete)"
        );
        assert_eq!(hub.status().await.unwrap().status, HubStatus::NotConfigured);
    }

    #[tokio::test]
    async fn a_corrupt_record_reads_as_unconfigured_instead_of_failing() {
        let (settings, hub) = service().await;
        settings.set(KEY_HUB_RECORD, "{not json").await.unwrap();
        assert_eq!(hub.status().await.unwrap().status, HubStatus::NotConfigured);
    }

    #[tokio::test]
    async fn the_unavailable_key_store_refuses_to_write_but_reads_as_empty() {
        let store = UnavailableKeyStore;
        assert_eq!(store.get("hub:x:1:y").unwrap(), None);
        assert_eq!(
            store.set("hub:x:1:y", "secret").unwrap_err().kind(),
            BootstrapErrorKind::KeyStore
        );
        store.delete("hub:x:1:y").unwrap();
    }
}
