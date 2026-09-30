//! banto-tagclient SDK（設計 §5.1「Hub からの値の取得は banto-tagclient
//! SDK」）の薄いラッパー。**サイドカーに 1 本だけ**クライアントを持ち、
//! 全 sink group の対象タグの**和集合**を external name で購読して、外部名で
//! 引ける最新スナップショット（[`ValueView`]）を全グループへ配る。
//!
//! ## なぜグループごとに 1 本ではないのか
//!
//! Hub 側の購読は 250ms の評価ループ（`subscribe_core.rs` の
//! `EVAL_TICK_MS`）で回るので、購読を分けるとその評価が本数分だけ増える。
//! sink group は同じタグを重複して選べる（グループ A と B が同じタグを
//! 別テーブルへ書く）ため、和集合にすると Hub 側の負荷は「実際に使う
//! タグの本数」で頭打ちになる。SDK 側も `BindingRequest` の `external_name` 重複を
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
//! ## 購読の鍵は external name（リネームの扱い）
//!
//! SDK の Binding は購読も書き込みも `external_name` で行い、Hub の安定 ID
//! （`StableTagId`）は使わない（2026-09-30 オーナー決定、
//! `docs/scada-design.md` §9.6）。ID は削除→同名再作成や CSV 再取り込みで
//! 変わるが、名前は Hub の公開契約（WS subscribe 等）そのものだから。
//! 安定 ID は DB 行（`ts, tag_id, external_name, value, quality`）用に
//! [`crate::hub_api::SinkConfigTag`] へ残るだけで、購読には使わない。
//!
//! したがってタグをリネームすると、購読中の旧名は catalog に無くなる。SDK は
//! **1 件でも unresolved があると世代全体を `binding_unresolved` で落とす**
//! （`worker.rs` `run_generation_inner`）ので、全タグを 1 本にまとめている
//! Sink が何もしなければ、100 タグ中 1 タグの rename で全タグの記録が
//! 設定再取得（既定 30 秒）まで止まってしまう。そこで Sink 側で catalog を
//! 引いて**解決できる名前だけを SDK へ渡す**（ChronoGazer の `plan_bindings`
//! と同じ。[`plan_bindings`]）。rename されたタグだけが unresolved になり、
//! **残りのタグの記録は続く**。
//!
//! unresolved の名前は周期的（`SidecarOptions::replan_interval`）に catalog を
//! 引き直して計画を作り直すので、同名再作成や、設定再取得後に設定へ入った
//! 新名で復帰する（tag-server-design.md §4.1「リネームは破壊的変更」。rename
//! 後の行は新しい名前になる - §5.3）。購読中に subscribed タグが rename された
//! 場合は、SDK が世代を落として catalog から再試行し続けるので、
//! [`Subscription::needs_replan`] で検出して直ちに計画を作り直し、残りの
//! タグで復帰させる。

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use banto_tagclient::{
    BindingRequest, CatalogSnapshot, Endpoint, ErrorKind, RestClient, SecretApiKey,
    TagClientConnectionState, TagClientHandle,
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

/// 購読の計画（[`plan_bindings`] の結果）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BindingPlan {
    /// catalog で解決でき、SDK へ渡す external name（ソート済み）。
    pub resolved: Vec<String>,
    /// 今は購読できない external name（ソート済み）。catalog に無い名前
    /// （rename / 削除 / 権限で見えない）と、購読プロトコルに載せられない
    /// 綴り（空・空白のみ・カンマを含む）を**まとめて**入れる。
    ///
    /// ChronoGazer は前者を `unresolved`、後者を `unsupported` と別バケツに
    /// 分けるが、Sink の利用者はどちらも「この名前は記録されない」という
    /// 同じ結果しか見ないので分けない（ログにも同じ列挙で出す）。
    pub unresolved: Vec<String>,
}

/// 空・空白のみ・カンマを含む名前は SDK の `start()` が
/// `InvalidTagSelection` で**購読全体を**拒否する（購読要求はタグ名を
/// カンマ区切りで並べるため）。ChronoGazer の `is_unsupported_tag_name` と
/// 同じ規則。
fn is_unsupported_tag_name(name: &str) -> bool {
    name.trim().is_empty() || name.contains(',')
}

/// 購読したい名前を catalog と突き合わせ、SDK へ渡せる名前（`resolved`）と
/// 今は渡せない名前（`unresolved`）に分ける**純関数**。
///
/// - SDK は 1 件でも unresolved があると世代全体を落とすので、Sink 側で
///   解決できる名前だけを渡す（ChronoGazer の `plan_bindings` と同じ）。
/// - 購読プロトコルに載せられない綴り（[`is_unsupported_tag_name`]）は
///   catalog を引く前に `unresolved` へ落とす（1 件混ざると全体が死ぬため）。
/// - `desired` は [`normalize_names`] 済みが前提だが、重複は SDK が
///   `external_name` 重複として拒否するので、念のためここでも畳む。
/// - 両方ソート済みで返す（差分判定・ログの順序を安定させる）。
pub fn plan_bindings(desired: &[String], catalog: &CatalogSnapshot) -> BindingPlan {
    let known: BTreeSet<&str> = catalog
        .tags
        .iter()
        .map(|tag| tag.external_name.as_str())
        .collect();
    let mut resolved = BTreeSet::new();
    let mut unresolved = BTreeSet::new();
    for name in desired {
        if !is_unsupported_tag_name(name) && known.contains(name.as_str()) {
            resolved.insert(name.clone());
        } else {
            unresolved.insert(name.clone());
        }
    }
    BindingPlan {
        resolved: resolved.into_iter().collect(),
        unresolved: unresolved.into_iter().collect(),
    }
}

/// 購読 1 世代（SDK ハンドル + 変換タスク）。設定変更で対象タグの集合が
/// 変わったとき、SDK の worker が終了したとき、または計画を作り直す
/// 必要が出たとき（[`Self::needs_replan`]、`unresolved` が残っている間の
/// 周期的な再計画）に作り直す。
///
/// `resolved` が空（全部 unresolved）のときは SDK ハンドルを持たない
/// （`handle` / `pump` が `None`）。それでも `Some(Subscription)` として
/// 残すのは、「全部 unresolved」の状態を「購読したいタグが無い」と区別して
/// 再計画の契機を失わないため。
pub struct Subscription {
    handle: Option<TagClientHandle>,
    pump: Option<JoinHandle<()>>,
    dead: Arc<AtomicBool>,
    /// SDK が `BindingUnresolved` を報告した（購読中に subscribed タグが
    /// rename された等）。true なら直ちに計画を作り直す。
    replan: Arc<AtomicBool>,
    /// 差分判定用の、購読したい名前の全体（順序正規化済み）。
    desired: Vec<String>,
    /// SDK へ渡した名前（`desired` のうち catalog で解決できたもの）。
    resolved: Vec<String>,
    /// `desired` のうち今は購読できない名前。
    unresolved: Vec<String>,
}

impl Subscription {
    /// 購読を開始する。`desired` は重複除去・ソート済みであること
    /// （[`normalize_names`]）。空なら `None`（購読するものが無い）。
    ///
    /// catalog を 1 度取得して [`plan_bindings`] で分け、**`resolved` だけ**を
    /// SDK へ渡す。catalog を取得できなければ `Err`（呼び出し側の
    /// バックオフに乗せる）。
    pub async fn start(
        config: &SidecarConfig,
        desired: Vec<String>,
        view_tx: watch::Sender<Arc<ValueView>>,
    ) -> Result<Option<Self>, banto_tagclient::Error> {
        if desired.is_empty() {
            let _ = view_tx.send(Arc::new(ValueView::default()));
            return Ok(None);
        }
        let endpoint = Endpoint::new(&config.hub_url)?;
        let secret = SecretApiKey::new(config.api_key.clone())?;
        let rest = RestClient::new(endpoint, secret)?;
        let catalog = rest.fetch_catalog().await?;
        let BindingPlan {
            resolved,
            unresolved,
        } = plan_bindings(&desired, &catalog);
        let dead = Arc::new(AtomicBool::new(false));
        let replan = Arc::new(AtomicBool::new(false));
        if resolved.is_empty() {
            // SDK は空の購読を拒否する。行は作らない（live = false）。
            let _ = view_tx.send(Arc::new(ValueView::default()));
            return Ok(Some(Self {
                handle: None,
                pump: None,
                dead,
                replan,
                desired,
                resolved,
                unresolved,
            }));
        }
        let requests: Vec<BindingRequest> = resolved
            .iter()
            .map(|name| BindingRequest {
                binding_key: name.clone(),
                external_name: name.clone(),
            })
            .collect();
        let handle = rest.start(requests)?;
        let state_rx = handle.state_watch();
        let pump = tokio::spawn(pump_values(state_rx, view_tx, dead.clone(), replan.clone()));
        Ok(Some(Self {
            handle: Some(handle),
            pump: Some(pump),
            dead,
            replan,
            desired,
            resolved,
            unresolved,
        }))
    }

    /// SDK の worker が終了した（`Unauthorized` などの終端エラー）か。
    /// true になったら呼び出し側は [`Self::stop`] してバックオフののち
    /// 作り直す（設計 §5.6「Hub の再起動にも追従」- SDK 自身の再接続で
    /// 直らない終端だけがここへ来る）。
    pub fn is_dead(&self) -> bool {
        self.dead.load(Ordering::Relaxed)
    }

    /// SDK が `BindingUnresolved` を報告したか。true なら呼び出し側は
    /// バックオフなしで計画を作り直す（SDK は世代を落として catalog から
    /// 再試行し続けるだけなので、残りのタグで復帰させるには Sink 側で
    /// 解決できる名前だけの購読へ張り直すしかない）。
    pub fn needs_replan(&self) -> bool {
        self.replan.load(Ordering::Relaxed)
    }

    /// 購読したい名前の全体（差分判定用）。
    pub fn desired(&self) -> &[String] {
        &self.desired
    }

    /// 実際に SDK へ渡した名前。
    pub fn resolved(&self) -> &[String] {
        &self.resolved
    }

    /// 今は購読できない名前。
    pub fn unresolved(&self) -> &[String] {
        &self.unresolved
    }

    /// worker を停止して join し、変換タスクも畳む。
    pub async fn stop(mut self) {
        if let Some(handle) = self.handle.take() {
            // 停止時のエラー（`Unauthorized` で終わっていた世代など）は
            // ここでは意味を持たない - 呼び出し側は作り直すだけ。
            let _ = handle.shutdown().await;
        }
        if let Some(pump) = self.pump.take() {
            pump.abort();
        }
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        // `stop` を通らずに落ちた場合の保険。`TagClientHandle::drop` 自身が
        // worker を abort するので、ここでは変換タスクだけ畳めばよい。
        if let Some(pump) = self.pump.take() {
            pump.abort();
        }
    }
}

/// SDK の状態 watch を [`ValueView`] の watch へ変換し続けるタスク。
/// 「SDK ハンドルは世代ごとに作り直すが、プロデューサが持つ受信端は
/// 作り直さない」ための一枚。
async fn pump_values(
    mut state_rx: watch::Receiver<banto_tagclient::TagClientState>,
    view_tx: watch::Sender<Arc<ValueView>>,
    dead: Arc<AtomicBool>,
    replan: Arc<AtomicBool>,
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
            if state.last_error() == Some(ErrorKind::BindingUnresolved) {
                replan.store(true, Ordering::Relaxed);
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

/// 重複除去 + ソート。差分判定（`==` 比較）が順序に左右されないように
/// する。
pub fn normalize_names(mut names: Vec<String>) -> Vec<String> {
    names.sort();
    names.dedup();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_names_sorts_and_deduplicates() {
        let names = normalize_names(vec![
            "line2.g.b".to_owned(),
            "line1.g.z".to_owned(),
            "line2.g.b".to_owned(),
            "line1.g.a".to_owned(),
        ]);
        assert_eq!(names, vec!["line1.g.a", "line1.g.z", "line2.g.b"]);
    }

    fn owned(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    fn catalog_json(names: &[&str]) -> String {
        let tags: Vec<String> = names
            .iter()
            .enumerate()
            .map(|(index, name)| {
                format!(
                    r#"{{"external_name":"{name}","tag_key":"tag:{index}","ids":[1,1,{index}],
                    "connection":"c","group":"g","name":"{name}","address":"a","data_type":"f64",
                    "unit":null,"decimals":0,"period_ms":100,"enabled":true,"writable":false,
                    "tag_kind":"plc","expression":null,"retain":false,"simulation":false,
                    "configured_simulation":false,"effective_simulation":false,
                    "value_source":"real"}}"#
                )
            })
            .collect();
        format!(
            r#"{{"revision":1,"run_id":1,"collection_mode":"configured","tags":[{}]}}"#,
            tags.join(",")
        )
    }

    fn catalog(names: &[&str]) -> CatalogSnapshot {
        serde_json::from_str(&catalog_json(names)).unwrap()
    }

    #[test]
    fn plan_bindings_splits_resolved_and_unresolved() {
        let plan = plan_bindings(&owned(&["a", "gone", "b"]), &catalog(&["a", "b", "c"]));
        assert_eq!(plan.resolved, owned(&["a", "b"]));
        assert_eq!(plan.unresolved, owned(&["gone"]));
    }

    #[test]
    fn plan_bindings_folds_duplicates() {
        let plan = plan_bindings(&owned(&["a", "a", "gone", "gone"]), &catalog(&["a"]));
        assert_eq!(plan.resolved, owned(&["a"]));
        assert_eq!(plan.unresolved, owned(&["gone"]));
    }

    #[test]
    fn plan_bindings_returns_both_lists_sorted() {
        let plan = plan_bindings(
            &owned(&["z", "y.gone", "b", "a.gone", "a"]),
            &catalog(&["a", "b", "z"]),
        );
        assert_eq!(plan.resolved, owned(&["a", "b", "z"]));
        assert_eq!(plan.unresolved, owned(&["a.gone", "y.gone"]));
    }

    #[test]
    fn plan_bindings_drops_names_the_subscription_protocol_rejects() {
        // catalog に同じ綴りがあっても、SDK の start() が全体を拒否する
        // 綴り（空・空白のみ・カンマ入り）は購読へ渡さない。
        let plan = plan_bindings(
            &owned(&["", "  ", "a,b", "ok"]),
            &catalog(&["", "  ", "a,b", "ok"]),
        );
        assert_eq!(plan.resolved, owned(&["ok"]));
        assert_eq!(plan.unresolved, owned(&["", "  ", "a,b"]));
    }

    #[test]
    fn plan_bindings_on_an_empty_desired_is_empty() {
        let plan = plan_bindings(&[], &catalog(&["a"]));
        assert_eq!(plan, BindingPlan::default());
    }

    /// `GET /api/v1/tags` に `catalog` を返し続ける最小の HTTP モック
    /// （それ以外のリクエスト = SDK の WS 接続などは 404 で切る）。
    /// 受けたリクエスト行を `requests` へ積む。
    async fn spawn_catalog_server(
        catalog: String,
    ) -> (
        SidecarConfig,
        Arc<std::sync::Mutex<Vec<String>>>,
        JoinHandle<()>,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let hub_url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = requests.clone();
        let server = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let mut request = Vec::new();
                let mut buffer = [0_u8; 1024];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    match stream.read(&mut buffer).await {
                        Ok(0) | Err(_) => break,
                        Ok(count) => request.extend_from_slice(&buffer[..count]),
                    }
                }
                let request = String::from_utf8_lossy(&request).into_owned();
                let line = request.lines().next().unwrap_or_default().to_owned();
                seen.lock().unwrap().push(line.clone());
                let response = if line.starts_with("GET /api/v1/tags ") {
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{catalog}",
                        catalog.len()
                    )
                } else {
                    "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .to_owned()
                };
                let _ = stream.write_all(response.as_bytes()).await;
            }
        });
        let config = SidecarConfig {
            hub_url,
            api_key: "bh_test.secret".to_owned(),
            config_refresh: std::time::Duration::from_secs(30),
            status_push: std::time::Duration::from_secs(10),
            queue_max_rows: 1000,
            flush_interval: std::time::Duration::from_millis(100),
            batch_size: 100,
            shutdown_flush: std::time::Duration::from_secs(1),
        };
        (config, requests, server)
    }

    #[tokio::test]
    async fn start_subscribes_only_to_names_the_catalog_resolves() {
        let (config, requests, server) = spawn_catalog_server(catalog_json(&["a"])).await;
        let (view_tx, _view_rx) = watch::channel(Arc::new(ValueView::default()));
        let subscription = Subscription::start(&config, owned(&["a", "gone"]), view_tx)
            .await
            .unwrap()
            .expect("解決できる名前があれば購読は作られる");
        assert_eq!(subscription.desired(), owned(&["a", "gone"]).as_slice());
        assert_eq!(subscription.resolved(), owned(&["a"]).as_slice());
        assert_eq!(subscription.unresolved(), owned(&["gone"]).as_slice());
        assert!(!subscription.needs_replan());
        assert!(requests
            .lock()
            .unwrap()
            .iter()
            .any(|line| line.starts_with("GET /api/v1/tags ")));
        tokio::time::timeout(std::time::Duration::from_secs(5), subscription.stop())
            .await
            .expect("stop は WS 接続の失敗待ちで詰まらない");
        server.abort();
    }

    #[tokio::test]
    async fn start_with_everything_unresolved_keeps_a_handle_less_subscription() {
        let (config, requests, server) = spawn_catalog_server(catalog_json(&["other"])).await;
        let (view_tx, view_rx) = watch::channel(Arc::new(ValueView {
            live: true,
            values: HashMap::new(),
        }));
        let subscription = Subscription::start(&config, owned(&["gone1", "gone2"]), view_tx)
            .await
            .unwrap()
            .expect("全部 unresolved でも Some（再計画の契機を残す）");
        assert!(subscription.resolved().is_empty());
        assert_eq!(
            subscription.unresolved(),
            owned(&["gone1", "gone2"]).as_slice()
        );
        assert!(!subscription.needs_replan());
        assert!(!subscription.is_dead());
        assert!(!view_rx.borrow().live, "行は作らない");
        // SDK を起動していないので、catalog 取得の 1 回だけ。
        assert_eq!(requests.lock().unwrap().len(), 1);
        subscription.stop().await;
        server.abort();
    }

    #[tokio::test]
    async fn start_fails_when_the_catalog_cannot_be_fetched() {
        let (config, _requests, server) = spawn_catalog_server(catalog_json(&["a"])).await;
        server.abort();
        let _ = server.await;
        // 落ちたサーバーのポートへ繋ぐ = 接続拒否。
        let (view_tx, _view_rx) = watch::channel(Arc::new(ValueView::default()));
        assert!(Subscription::start(&config, owned(&["a"]), view_tx)
            .await
            .is_err());
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
