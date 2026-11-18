//! 購読の共有ロジック (docs/tag-server-design.md §5.2「WebSocket（T1）」・
//! §5.4「gRPC（T4）」)。
//!
//! T1 時点では `crate::stream`(WebSocket `/api/v1/stream`)だけがこの
//! ロジックを持っていた。T4 の gRPC `StreamValues` は「WS 購読と同意味論」
//! （設計 §5.4「購読(WS §5.2 と同意味論)」）を守る必要があり、実装指示は
//! 「可能なら評価・diff・resolve のロジックを stream.rs から共通モジュール
//! へ抽出して共有」を求めている。ここへ抽出したのは以下の transport
//! 非依存の部分:
//!
//! - [`TagPattern`]: `subscribe` の `tags` 文字列(ワイルドカード込み)の
//!   パースとマッチ判定
//! - [`resolve`]: パターン集合を現在の `TagMap` に照合する(評価の**都度**
//!   呼ぶ - `config_changed` 相当の再解決を暗黙に行う、モジュール doc
//!   comment 「ワイルドカードは評価時に TagMap へ照合」参照)
//! - [`Mode`]/[`interval_floor_ms`]: on_change/interval の区別と、
//!   interval の下限クランプ
//! - [`Subscription`]/[`initial_values`]/[`evaluate`]: 1購読分の状態
//!   （diff 基準・次回発火時刻)と、初期スナップショット/250ms 評価ティック
//!   1回分の値算出
//!
//! transport 固有のもの(axum の `Message`/`CloseFrame`・バックプレッシャ
//! 切断、tonic の `mpsc`/`Status`)は一切ここに持ち込まない - `crate::stream`
//! と `crate::grpc` はこのモジュールが返す [`ResolvedValue`] を、自分の
//! ワイヤ型（WS の `ValueWire`/gRPC の `TagValue` proto）へ変換するだけ。
//!
//! ## 同期義務(このモジュールが吸収した後に残るもの)
//!
//! `crate::stream`/`crate::grpc` はどちらも評価ループの周期を
//! [`EVAL_TICK_MS`] に固定する(このモジュールがロジックを持つが、
//! `tokio::time::interval` の生成自体は各 transport 側)。この定数を
//! 変える場合は両方の呼び出し元が同じ値を使っていることを確認すること -
//! ロジックの共有はできたが、「評価ループを起動する」という配線までは
//! 共有できていない(各 transport のタスク構造が違う - `crate::stream` は
//! 1コネクション1タスク、`crate::grpc` の `StreamValues` も同様に1呼び出し
//! 1タスクだが、送信キューの型が axum の `Message` と tonic の
//! `Result<ValueBatch, Status>` で異なるため、タスク本体までは統合しない)。
//!
//! ## per-tag read スコープの交差(H10 ③、Option B、
//! docs/h10-3-read-scope-proposal.md §5 S4・§6)
//!
//! [`initial_values`]/[`evaluate`] は `scope: Option<&ApiKeyContext>` を
//! 受け取り、[`resolve`] が返したマッチ集合をさらに
//! `ApiKeyContext::can_read_value` で絞り込む(`None` はフィルタなし -
//! session token 認証、または呼び出し元がまだ ctx を持たない場合)。
//! [`resolve`] 自体・[`interval_floor_ms`] は変更しない(パターンマッチのみ
//! を行う。`interval_floor_ms` は購読元クライアント自身への QoS クランプの
//! 計算でしかなく、他者へのタグ識別子/値の開示を伴わないため、案 B が絞る
//! 対象である「値の読み取り」には当たらない)。`crate::stream`/`crate::grpc`
//! の両方の `StreamValues` 購読解決から共有する。

use std::collections::{HashMap, HashSet};

use banto_collect::{CurrentValuesHandle, Quality};

use crate::api_keys::{ApiKeyContext, HubScopeChecks};
use crate::computed::ServerTagStore;
use crate::hub::{read_current_view_with_history, TagEntry, TagMap};

/// 評価タイマの固定周期(設計 §5.2/§5.4「250ms 評価」)。`crate::stream`/
/// `crate::grpc` の両方がこの値で `tokio::time::interval` を作る。
pub const EVAL_TICK_MS: i64 = 250;

/// 1つの `subscribe`/`StreamValuesRequest` の購読対象パターン(設計
/// §5.2「ワイルドカード `*` は末尾のみ...全タグ購読は `*` 単独」)。
#[derive(Debug, Clone)]
pub enum TagPattern {
    /// 具体名。subscribe 時点で catalog に存在することを検証する
    /// (呼び出し元の責務 - このモジュールは検証しない)。
    Exact(String),
    /// `{connection}.{group}.*`。
    GroupWildcard { connection: String, group: String },
    /// `*` 単独 - 全タグ。
    All,
}

impl TagPattern {
    pub fn parse(raw: &str) -> Result<Self, String> {
        if raw == "*" {
            return Ok(TagPattern::All);
        }
        if let Some(prefix) = raw.strip_suffix(".*") {
            let segments: Vec<&str> = prefix.split('.').collect();
            if segments.len() == 2 && segments.iter().all(|s| !s.is_empty()) {
                return Ok(TagPattern::GroupWildcard {
                    connection: segments[0].to_string(),
                    group: segments[1].to_string(),
                });
            }
            return Err(format!(
                "ワイルドカードは {{connection}}.{{group}}.* か * 単独のみ許可されています: {raw}"
            ));
        }
        if raw.contains('*') {
            return Err(format!("不正なワイルドカード指定です: {raw}"));
        }
        if raw.is_empty() {
            return Err("タグ名が空です".to_string());
        }
        Ok(TagPattern::Exact(raw.to_string()))
    }

    pub fn matches(&self, entry: &TagEntry) -> bool {
        match self {
            TagPattern::Exact(name) => entry.external_name == *name,
            TagPattern::GroupWildcard { connection, group } => {
                &entry.connection == connection && &entry.group == group
            }
            TagPattern::All => true,
        }
    }
}

/// 現在の `TagMap` に対して `patterns` にマッチする全エントリ - **評価の
/// たびに**呼ぶ(このモジュールの doc comment参照)。
pub fn resolve<'a>(patterns: &[TagPattern], map: &'a TagMap) -> Vec<&'a TagEntry> {
    map.iter()
        .filter(|entry| patterns.iter().any(|p| p.matches(entry)))
        .collect()
}

#[derive(Debug, Clone, Copy)]
pub enum Mode {
    OnChange,
    Interval { interval_ms: i64 },
}

/// interval の下限クランプ値(設計 §5.2 要件3、判断は `crate::stream` の
/// モジュール doc comment「interval の下限クランプ」参照): マッチした
/// タグが属するグループの `period_ms` の最小値と、評価ループ自体の周期
/// ([`EVAL_TICK_MS`])の大きい方。マッチが0件ならグループ周期側の制約は
/// なく、評価ループの周期だけが下限になる。
pub fn interval_floor_ms(patterns: &[TagPattern], map: &TagMap) -> i64 {
    resolve(patterns, map)
        .iter()
        .map(|entry| entry.period_ms)
        .min()
        .unwrap_or(EVAL_TICK_MS)
        .max(EVAL_TICK_MS)
}

/// on_change の diff 基準(外部名 → 直近の値/品質/未確認)。単なる型複雑度
/// 回避のエイリアス(clippy::type_complexity)。未確認も入れるのは、評価の
/// 1 周期の間に未確認の Bad → Good → Bad と動いて値・品質が元に戻ったとき、
/// 「未確認」のまま送り直されずに残らないようにするため。
pub type DiffBaseline = HashMap<String, (Option<f64>, Quality, bool)>;

/// 1タグの評価済みの値 - transport 非依存の表現。`crate::stream::ValueWire`
/// (JSON)/gRPC の `TagValue`(proto)へはここから変換する。
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedValue {
    pub tag: String,
    pub v: Option<f64>,
    pub q: Quality,
    pub t: i64,
    /// 未確認: 品質が Good でなく、かつ Hub の今回の起動後にこのタグで一度も
    /// 良好値を受けていない(生の reading の `last_good_ms` が無い)。表示用の
    /// 無効化(`!entry.enabled` で `source_time_ms` が `None` になる)とは独立に、
    /// 生の受信履歴で判定する。
    /// 収集開始後の不良(通信異常)と区別するための表示用で、DB には残さない
    /// (2026-11-18 オーナー決定)。
    pub unverified: bool,
}

/// `read_current_view` を購読の 1 行(値・品質・時刻・未確認)へ射影する。
/// 未確認の定義は [`ResolvedValue::unverified`]。サンプル無しのタグ・値未設定の
/// 内部タグ・一度も良好値を受けずに無効にしたタグは未確認になるが、良好値を
/// 受けた後に無効にしたタグは未確認にならない。不正な bit 値が Good として
/// 記録された場合も生の `last_good_ms` があれば受信はあったとみなす。
fn read_resolved(
    entry: &TagEntry,
    current: Option<&CurrentValuesHandle>,
    server_store: &ServerTagStore,
    now_ms: i64,
) -> (Option<f64>, Quality, i64, bool) {
    let (view, received_good) =
        read_current_view_with_history(entry, current, server_store, now_ms);
    let unverified = view.quality != Quality::Good && !received_good;
    (view.value, view.quality, view.timestamp_ms, unverified)
}

/// 1購読分の状態(diff 基準・次回発火時刻)。`crate::stream`/`crate::grpc`
/// のどちらも「1購読 = 1個の `Subscription`」で持つ - WS は `id` キーの
/// `HashMap` に複数、gRPC の `StreamValues` は1呼び出しにつき1個のみ
/// (proto に複数購読の多重化はない、設計 §5.4 のメッセージ設計どおり)。
#[derive(Debug, Clone)]
pub struct Subscription {
    pub patterns: Vec<TagPattern>,
    pub mode: Mode,
    /// on_change の diff 基準(外部名 → 直近の値/品質)。interval モードの
    /// 購読でも維持はするが参照しない(diff しないため)。
    pub last: DiffBaseline,
    /// interval モードのみ意味を持つ: 次回送信予定の epoch ms。
    pub next_due_ms: i64,
}

/// マッチ集合を per-tag read スコープで絞り込む(H10 ③、Option B -
/// このモジュールの doc comment「per-tag read スコープの交差」参照)。
/// `scope` が `None` なら無フィルタ(session token 認証)。
fn filter_by_scope<'a>(
    matched: Vec<&'a TagEntry>,
    scope: Option<&ApiKeyContext>,
) -> Vec<&'a TagEntry> {
    match scope {
        None => matched,
        Some(ctx) => matched
            .into_iter()
            .filter(|entry| ctx.can_read_value(&entry.external_name))
            .collect(),
    }
}

/// 購読受理直後の初期スナップショットを算出する(設計 §5.2 要件5「接続時に
/// 現在値の初期スナップショットを必ず1回送る」・§5.4「初期スナップショット
/// 必須」)。戻り値の2番目は [`Subscription::last`] の初期値そのもの
/// (diff 基準を初期値で立てる - 呼び出し元は続けてこれを保持する
/// `Subscription` を組み立てる)。
///
/// `scope`(H10 ③): [`resolve`] のマッチ結果をさらに per-tag read
/// スコープで絞る。`None` は無フィルタ(session token 認証、モジュール doc
/// comment参照)。
pub fn initial_values(
    patterns: &[TagPattern],
    map: &TagMap,
    current: Option<&CurrentValuesHandle>,
    server_store: &ServerTagStore,
    now_ms: i64,
    scope: Option<&ApiKeyContext>,
) -> (Vec<ResolvedValue>, DiffBaseline) {
    let matched = filter_by_scope(resolve(patterns, map), scope);
    let mut last = HashMap::with_capacity(matched.len());
    let values = matched
        .into_iter()
        .map(|entry| {
            let (v, q, t, unverified) = read_resolved(entry, current, server_store, now_ms);
            last.insert(entry.external_name.clone(), (v, q, unverified));
            ResolvedValue {
                tag: entry.external_name.clone(),
                v,
                q,
                t,
                unverified,
            }
        })
        .collect();
    (values, last)
}

/// 250ms 評価ティック1回分の評価本体(設計 §5.2/§5.4「250ms 評価」)。
/// `Some(values)` は「この tick で送るべきデータがある」-
/// on_change は変化があった行だけ(空なら `None`、何も送らない)、interval
/// は発火時刻(`next_due_ms`)に達していれば毎回全マッチ行(変化の有無に
/// 関わらず)。
///
/// `scope`(H10 ③): [`initial_values`] と同じ per-tag read スコープ交差。
/// スコープ外に落ちたタグは(ワイルドカード購読が構成変更でマッチしなく
/// なった場合と同様に)単に追跡対象から外れる - on_change の diff 基準
/// (`sub.last`)からも自然に消える([`Subscription::last`] の
/// `still_present` retain と同じ経路)。
pub fn evaluate(
    sub: &mut Subscription,
    map: &TagMap,
    current: Option<&CurrentValuesHandle>,
    server_store: &ServerTagStore,
    now_ms: i64,
    scope: Option<&ApiKeyContext>,
) -> Option<Vec<ResolvedValue>> {
    let matched = filter_by_scope(resolve(&sub.patterns, map), scope);

    match sub.mode {
        Mode::OnChange => {
            let mut changed_values = Vec::new();
            let mut still_present: HashSet<String> = HashSet::with_capacity(matched.len());
            for entry in &matched {
                let (v, q, t, unverified) = read_resolved(entry, current, server_store, now_ms);
                still_present.insert(entry.external_name.clone());

                let changed = match sub.last.get(entry.external_name.as_str()) {
                    Some((prev_v, prev_q, prev_unverified)) => {
                        *prev_v != v || *prev_q != q || *prev_unverified != unverified
                    }
                    // 新規にマッチしたタグ(ワイルドカード購読が構成変更後に
                    // 拾った新タグ等)は初回として必ず1回送る。
                    None => true,
                };
                if changed {
                    sub.last
                        .insert(entry.external_name.clone(), (v, q, unverified));
                    changed_values.push(ResolvedValue {
                        tag: entry.external_name.clone(),
                        v,
                        q,
                        t,
                        unverified,
                    });
                }
            }
            // マッチしなくなったタグの diff 基準は捨てる - 「消えた」通知は
            // ないので、単に追跡をやめるだけ(戻ってきたら新規扱いで再送
            // される)。
            sub.last.retain(|name, _| still_present.contains(name));

            if changed_values.is_empty() {
                None
            } else {
                Some(changed_values)
            }
        }
        Mode::Interval { interval_ms } => {
            if now_ms >= sub.next_due_ms {
                let values = matched
                    .iter()
                    .map(|entry| {
                        let (v, q, t, unverified) =
                            read_resolved(entry, current, server_store, now_ms);
                        ResolvedValue {
                            tag: entry.external_name.clone(),
                            v,
                            q,
                            t,
                            unverified,
                        }
                    })
                    .collect();
                sub.next_due_ms = now_ms + interval_ms;
                Some(values)
            } else {
                None
            }
        }
    }
}

#[cfg(test)]
mod unverified_tests {
    //! 「未確認」(起動後に一度も良好値を受けていない)の判定。
    use super::*;
    use banto_tags::INTERNAL_TAG_KIND;

    fn internal_entry() -> TagEntry {
        TagEntry {
            external_name: "x.y.z".to_string(),
            tag_key: "tag:1".to_string(),
            ids: (1, 1, 1),
            connection: "x".to_string(),
            group: "y".to_string(),
            name: "z".to_string(),
            address: String::new(),
            data_type: "f64".to_string(),
            unit: None,
            decimals: 0,
            period_ms: 1_000,
            enabled: true,
            writable: true,
            tag_kind: INTERNAL_TAG_KIND.to_string(),
            expression: None,
            retain: false,
            simulation: false,
            push: false,
        }
    }

    #[test]
    fn bad_without_any_good_sample_is_unverified() {
        let entry = internal_entry();
        let store = ServerTagStore::new();
        // サンプル無し(値未設定)も、良好値を持たない Bad も未確認。
        assert!(read_resolved(&entry, None, &store, 1_000).3);
        store.set("tag:1", None, Quality::Bad, 900);
        let (_, q, _, unverified) = read_resolved(&entry, None, &store, 1_000);
        assert_eq!(q, Quality::Bad);
        assert!(unverified);
    }

    #[test]
    fn bad_after_a_good_sample_is_not_unverified() {
        let entry = internal_entry();
        let store = ServerTagStore::new();
        store.set("tag:1", Some(1.0), Quality::Good, 800);
        store.set("tag:1", None, Quality::Bad, 900);
        let (_, q, _, unverified) = read_resolved(&entry, None, &store, 1_000);
        assert_eq!(q, Quality::Bad);
        assert!(!unverified);
    }

    #[test]
    fn good_is_not_unverified() {
        let entry = internal_entry();
        let store = ServerTagStore::new();
        store.set("tag:1", Some(1.0), Quality::Good, 900);
        let (v, q, _, unverified) = read_resolved(&entry, None, &store, 1_000);
        assert_eq!((v, q), (Some(1.0), Quality::Good));
        assert!(!unverified);
    }

    #[test]
    fn disabled_after_a_good_sample_is_bad_but_not_unverified() {
        let mut entry = internal_entry();
        let store = ServerTagStore::new();
        store.set("tag:1", Some(1.0), Quality::Good, 900);
        entry.enabled = false;
        let (v, q, _, unverified) = read_resolved(&entry, None, &store, 1_000);
        assert_eq!((v, q), (None, Quality::Bad));
        assert!(!unverified);
    }

    #[test]
    fn disabled_without_any_good_sample_is_unverified() {
        let mut entry = internal_entry();
        let store = ServerTagStore::new();
        entry.enabled = false;
        let (_, q, _, unverified) = read_resolved(&entry, None, &store, 1_000);
        assert_eq!(q, Quality::Bad);
        assert!(unverified);
    }

    #[test]
    fn bad_good_bad_within_one_tick_resends_to_clear_unverified() {
        let entry = internal_entry();
        let mut map = TagMap::default();
        map.insert_for_test(entry);
        let store = ServerTagStore::new();
        let patterns = vec![TagPattern::parse("x.y.z").unwrap()];

        // 初回: サンプル無し(Bad・未確認)。
        let (initial, last) = initial_values(&patterns, &map, None, &store, 1_000, None);
        assert_eq!(initial.len(), 1);
        assert!(initial[0].unverified);
        let mut sub = Subscription {
            patterns,
            mode: Mode::OnChange,
            last,
            next_due_ms: 0,
        };

        // 評価の 1 周期の間に Bad → Good → Bad。v・q は初回と同じに戻るが、
        // 良好値を受けたので未確認は解除される。
        store.set("tag:1", Some(1.0), Quality::Good, 900);
        store.set("tag:1", None, Quality::Bad, 950);

        let sent = evaluate(&mut sub, &map, None, &store, 1_250, None)
            .expect("unverified の変化で送り直される");
        assert_eq!(sent.len(), 1);
        assert_eq!((sent[0].v, sent[0].q), (None, Quality::Bad));
        assert!(!sent[0].unverified);

        // 対照: 変化なしなら何も返らない。
        assert!(evaluate(&mut sub, &map, None, &store, 1_500, None).is_none());
    }
}
