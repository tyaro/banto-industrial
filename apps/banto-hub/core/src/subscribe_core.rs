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

use std::collections::{HashMap, HashSet};

use banto_collect::{CurrentValuesHandle, Quality};

use crate::computed::ServerTagStore;
use crate::hub::{read_current, TagEntry, TagMap};

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

/// on_change の diff 基準(外部名 → 直近の値/品質)。単なる型複雑度回避の
/// エイリアス(clippy::type_complexity)。
pub type DiffBaseline = HashMap<String, (Option<f64>, Quality)>;

/// 1タグの評価済みの値 - transport 非依存の表現。`crate::stream::ValueWire`
/// (JSON)/gRPC の `TagValue`(proto)へはここから変換する。
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedValue {
    pub tag: String,
    pub v: Option<f64>,
    pub q: Quality,
    pub t: i64,
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

/// 購読受理直後の初期スナップショットを算出する(設計 §5.2 要件5「接続時に
/// 現在値の初期スナップショットを必ず1回送る」・§5.4「初期スナップショット
/// 必須」)。戻り値の2番目は [`Subscription::last`] の初期値そのもの
/// (diff 基準を初期値で立てる - 呼び出し元は続けてこれを保持する
/// `Subscription` を組み立てる)。
pub fn initial_values(
    patterns: &[TagPattern],
    map: &TagMap,
    current: Option<&CurrentValuesHandle>,
    server_store: &ServerTagStore,
    now_ms: i64,
) -> (Vec<ResolvedValue>, DiffBaseline) {
    let matched = resolve(patterns, map);
    let mut last = HashMap::with_capacity(matched.len());
    let values = matched
        .into_iter()
        .map(|entry| {
            let (v, q, t) = read_current(entry, current, server_store, now_ms);
            last.insert(entry.external_name.clone(), (v, q));
            ResolvedValue {
                tag: entry.external_name.clone(),
                v,
                q,
                t,
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
pub fn evaluate(
    sub: &mut Subscription,
    map: &TagMap,
    current: Option<&CurrentValuesHandle>,
    server_store: &ServerTagStore,
    now_ms: i64,
) -> Option<Vec<ResolvedValue>> {
    let matched = resolve(&sub.patterns, map);

    match sub.mode {
        Mode::OnChange => {
            let mut changed_values = Vec::new();
            let mut still_present: HashSet<String> = HashSet::with_capacity(matched.len());
            for entry in &matched {
                let (v, q, t) = read_current(entry, current, server_store, now_ms);
                still_present.insert(entry.external_name.clone());

                let changed = match sub.last.get(entry.external_name.as_str()) {
                    Some((prev_v, prev_q)) => *prev_v != v || *prev_q != q,
                    // 新規にマッチしたタグ(ワイルドカード購読が構成変更後に
                    // 拾った新タグ等)は初回として必ず1回送る。
                    None => true,
                };
                if changed {
                    sub.last.insert(entry.external_name.clone(), (v, q));
                    changed_values.push(ResolvedValue {
                        tag: entry.external_name.clone(),
                        v,
                        q,
                        t,
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
                        let (v, q, t) = read_current(entry, current, server_store, now_ms);
                        ResolvedValue {
                            tag: entry.external_name.clone(),
                            v,
                            q,
                            t,
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
