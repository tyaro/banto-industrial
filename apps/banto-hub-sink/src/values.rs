//! banto-tagclient SDK（設計 §5.1「Hub からの値の取得は banto-tagclient
//! SDK」）の薄いラッパー。**サイドカーに 1 本だけ**クライアントを持ち、
//! 全 sink group の対象タグの**和集合**を安定 ID で購読して、外部名で
//! 引ける最新スナップショット（[`ValueView`]）を全グループへ配る。
//!
//! ## なぜグループごとに 1 本ではないのか
//!
//! Hub 側の購読は 250ms の評価ループ（`subscribe_core.rs` の
//! `EVAL_TICK_MS`）で回るので、購読を分けるとその評価が本数分だけ増える。
//! sink group は同じタグを重複して選べる（グループ A と B が同じタグを
//! 別テーブルへ書く）ため、和集合にすると Hub 側の負荷は「実際に使う
//! タグの本数」で頭打ちになる。SDK 側も `BindingRequest` の安定 ID 重複を
//! 拒否する（`validate_start_requests`）ので、和集合化は必須でもある。
//!
//! ## 値の流れと SDK の性質（`docs/banto-tagclient-design.md` §4.5）
//!
//! - 購読は **on-change 配信**で、値も quality も変わらなければ WS フレーム
//!   は 1 つも来ない。初期値は SDK が Live 到達時に REST スナップショットで
//!   埋める（publish gate）ので、`interval` モードのグループは静止した
//!   環境でも毎周期きちんと行を作れる。
//! - SDK が publish する `ValuesSnapshot` は**購読タグ全件**（REST
//!   スナップショットに新しい WS 値を重ねたもの）なので、[`ValueView`] は
//!   毎回まるごと差し替えてよい。差分（どのタグが変わったか）は
//!   `on_change` モードのプロデューサ側が自分で取る
//!   （[`crate::group::OnChangeGate`]）。
//! - Live でない間（Hub 停止・再接続・rebinding 中）は `current()` が
//!   `None` になる。そのとき [`ValueView::live`] は false で、**行は 1 つも
//!   作らない**（設計の実装指示「SDK が切断されていれば行は作られない」）。
//!
//! ## リネームの追従
//!
//! SDK は安定 ID で再解決するので、タグをリネームしても購読は途切れない
//! （`config_changed` → rebinding）。一方このモジュールが配る
//! [`ValueView`] のキーは**外部名**なので、リネーム直後は
//! `GET /api/sink/config` が返した古い外部名で引けなくなる。次の設定取得
//! （既定 30 秒）で解消する - その間そのタグの行が落ちるのは
//! 「rename 後の行は新しい名前になる」（§5.3）の範囲内の挙動として許容
//! する。外部名ではなく安定 ID で引けるようにするには SDK が解決後の
//! 外部名を公開する必要があり、そちらは SDK の API 拡張になるため v1 では
//! 採らない。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use banto_tagclient::{
    BindingRequest, Endpoint, RestClient, SecretApiKey, StableTagId, TagClientConnectionState,
    TagClientHandle,
};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::config::SidecarConfig;

/// 1 タグの最新値（`banto_tagclient::ValueEntry` から必要な 3 つだけ）。
#[derive(Clone, Debug, PartialEq)]
pub struct ValueSample {
    pub value: Option<f64>,
    /// wire の quality 文字列（`good`/`bad`/`stale`、未知はその値）。
    pub quality: String,
    /// 値の `ptime`（epoch ミリ秒）。
    pub ptime_ms: i64,
}

/// 購読タグ全件の最新スナップショット（外部名で引く）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ValueView {
    /// SDK が Live で、スナップショットを持っているか。false の間は
    /// 行を作らない。
    pub live: bool,
    pub values: HashMap<String, ValueSample>,
}

impl ValueView {
    pub fn get(&self, external_name: &str) -> Option<&ValueSample> {
        if !self.live {
            return None;
        }
        self.values.get(external_name)
    }
}

/// 購読 1 世代（SDK ハンドル + 変換タスク）。設定変更で対象タグの集合が
/// 変わったとき、または SDK の worker が終了したときに作り直す。
pub struct Subscription {
    handle: Option<TagClientHandle>,
    pump: JoinHandle<()>,
    dead: Arc<AtomicBool>,
    /// 差分判定用の購読集合（順序正規化済み）。
    subscribed: Vec<StableTagId>,
}

impl Subscription {
    /// 購読を開始する。`stable_ids` は重複除去・ソート済みであること
    /// （[`normalize_ids`]）。空なら `None`（SDK は空の購読を拒否する）。
    pub fn start(
        config: &SidecarConfig,
        stable_ids: Vec<StableTagId>,
        view_tx: watch::Sender<Arc<ValueView>>,
    ) -> Result<Option<Self>, banto_tagclient::Error> {
        if stable_ids.is_empty() {
            let _ = view_tx.send(Arc::new(ValueView::default()));
            return Ok(None);
        }
        let endpoint = Endpoint::new(&config.hub_url)?;
        let secret = SecretApiKey::new(config.api_key.clone())?;
        let rest = RestClient::new(endpoint, secret)?;
        let requests: Vec<BindingRequest> = stable_ids
            .iter()
            .map(|id| BindingRequest {
                binding_key: binding_key(*id),
                stable_id: *id,
            })
            .collect();
        let handle = rest.start(requests)?;
        let state_rx = handle.state_watch();
        let dead = Arc::new(AtomicBool::new(false));
        let pump = tokio::spawn(pump_values(state_rx, view_tx, dead.clone()));
        Ok(Some(Self {
            handle: Some(handle),
            pump,
            dead,
            subscribed: stable_ids,
        }))
    }

    /// SDK の worker が終了した（`Unauthorized` などの終端エラー）か。
    /// true になったら呼び出し側は [`Self::stop`] してバックオフののち
    /// 作り直す（設計 §5.6「Hub の再起動にも追従」- SDK 自身の再接続で
    /// 直らない終端だけがここへ来る）。
    pub fn is_dead(&self) -> bool {
        self.dead.load(Ordering::Relaxed)
    }

    /// 現在の購読集合（差分判定用）。
    pub fn subscribed(&self) -> &[StableTagId] {
        &self.subscribed
    }

    /// worker を停止して join し、変換タスクも畳む。
    pub async fn stop(mut self) {
        if let Some(handle) = self.handle.take() {
            // 停止時のエラー（`Unauthorized` で終わっていた世代など）は
            // ここでは意味を持たない - 呼び出し側は作り直すだけ。
            let _ = handle.shutdown().await;
        }
        self.pump.abort();
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        // `stop` を通らずに落ちた場合の保険。`TagClientHandle::drop` 自身が
        // worker を abort するので、ここでは変換タスクだけ畳めばよい。
        self.pump.abort();
    }
}

/// SDK の状態 watch を [`ValueView`] の watch へ変換し続けるタスク。
/// 「SDK ハンドルは世代ごとに作り直すが、プロデューサが持つ受信端は
/// 作り直さない」ための一枚。
async fn pump_values(
    mut state_rx: watch::Receiver<banto_tagclient::TagClientState>,
    view_tx: watch::Sender<Arc<ValueView>>,
    dead: Arc<AtomicBool>,
) {
    let mut saw_active = false;
    loop {
        {
            let state = state_rx.borrow_and_update();
            if state.connection_state() != TagClientConnectionState::Stopped {
                saw_active = true;
            } else if saw_active {
                // worker が終了した（`run_supervisor` が `Unauthorized` /
                // 終端エラーで抜けた、または shutdown された）。
                dead.store(true, Ordering::Relaxed);
            }
            let view = match state.current() {
                Some(snapshot) => ValueView {
                    live: true,
                    values: snapshot
                        .values
                        .iter()
                        .map(|entry| {
                            (
                                entry.tag.clone(),
                                ValueSample {
                                    value: entry.v,
                                    quality: entry.q.as_str().to_string(),
                                    ptime_ms: entry.t,
                                },
                            )
                        })
                        .collect(),
                },
                None => ValueView::default(),
            };
            view_tx.send_replace(Arc::new(view));
        }
        if state_rx.changed().await.is_err() {
            // 送信端（SDK ハンドル）が落ちた = この世代は終わり。
            dead.store(true, Ordering::Relaxed);
            view_tx.send_replace(Arc::new(ValueView::default()));
            return;
        }
    }
}

/// SDK の `binding_key`（安定 ID の 3 つ組をそのまま文字列に）。SDK は
/// キーの重複を拒否するので、一意であればよい。
fn binding_key(id: StableTagId) -> String {
    format!("{}:{}:{}", id.connection_id, id.group_id, id.tag_id)
}

/// 重複除去 + ソート。差分判定（`==` 比較）が順序に左右されないように
/// する。
pub fn normalize_ids(mut ids: Vec<StableTagId>) -> Vec<StableTagId> {
    ids.sort_by_key(|id| (id.connection_id, id.group_id, id.tag_id));
    ids.dedup_by_key(|id| (id.connection_id, id.group_id, id.tag_id));
    ids
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_ids_sorts_and_deduplicates() {
        let ids = normalize_ids(vec![
            StableTagId::new(2, 1, 1),
            StableTagId::new(1, 1, 9),
            StableTagId::new(2, 1, 1),
            StableTagId::new(1, 1, 2),
        ]);
        assert_eq!(
            ids,
            vec![
                StableTagId::new(1, 1, 2),
                StableTagId::new(1, 1, 9),
                StableTagId::new(2, 1, 1),
            ]
        );
    }

    #[test]
    fn binding_keys_are_unique_per_stable_id() {
        assert_eq!(binding_key(StableTagId::new(1, 2, 3)), "1:2:3");
        assert_ne!(
            binding_key(StableTagId::new(1, 2, 3)),
            binding_key(StableTagId::new(1, 2, 4))
        );
    }

    #[test]
    fn a_view_that_is_not_live_never_yields_a_sample() {
        let mut view = ValueView {
            live: false,
            values: HashMap::new(),
        };
        view.values.insert(
            "line1.fast.temp01".to_string(),
            ValueSample {
                value: Some(1.0),
                quality: "good".to_string(),
                ptime_ms: 10,
            },
        );
        assert!(view.get("line1.fast.temp01").is_none());
        view.live = true;
        assert!(view.get("line1.fast.temp01").is_some());
    }
}
