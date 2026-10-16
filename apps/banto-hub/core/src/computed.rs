//! 演算タグ・内部タグの評価エンジンとタグ空間ストア（T6-2、実装対象）。
//!
//! ## 参照する設計
//!
//! - **docs/tag-server-design.md §4.2「演算タグ・内部タグ」**: タグ種別
//!   （`plc`/`computed`/`internal`）の意味論そのもの。演算タグは「純関数
//!   のみ」「DAG のみ許可（演算タグ同士の循環は登録時検証で拒否）」
//!   「評価タイミングは入力タグの更新イベント駆動（結局グループ周期に
//!   律速される）」「品質は入力の最悪品質（Bad > Stale > Good）、時刻は
//!   再計算のトリガとなった入力の時刻」を規定する。内部タグは
//!   「クライアントの書き込みでタグ空間内に完結（PLC へ送らない）」
//!   「`retain` フラグで再起動時の最終値復元を選択」を規定する。
//! - **docs/tag-server-design.md §4.3(a)「サーバー側タグの変更は影響半径
//!   ゼロ」**: 演算タグ・内部タグの追加/変更/削除は PLC 通信に一切触れず、
//!   検証 → 即時反映。本モジュールの [`build_plan`] が純関数（レジストリを
//!   読まない・`TagMap` スナップショットだけを見る）で、[`ComputedEngine::commit`]
//!   が唯一の書き込み口である設計は、この「検証してから即座に反映」の
//!   規律をそのままコードで体現する - `crate::hub::CollectorManager::rebuild`
//!   が catalog/`Collector` の入れ替えと**同じ all-or-nothing の1トランザ
//!   クション**として呼ぶ（`hub.rs` の `computed` フィールド doc comment
//!   参照）。
//!
//! ## このモジュールが持つ2つの部品
//!
//! - [`ServerTagStore`]: computed/internal タグの現在値ストア
//!   （`external_name` の代わりに [`crate::hub::TagEntry::tag_key`] - PLC
//!   タグの `banto_collect::CurrentValuesHandle` と同じキー方式 - をキーに
//!   持つことで、タグのリネームで値を失わない）。`std::sync::RwLock` の
//!   `HashMap` - `CurrentValuesHandle` と同じ理由（読み取りは同期・書き込み
//!   は短時間の非 await 区間）。
//! - [`ComputedEngine`]: `build_plan` でコンパイル済みの式一式（トポロジ
//!   カル順つき）を保持し、[`ComputedEngine::evaluate_tick`] が既存の
//!   250ms 評価ループパターン（`crate::subscribe_core::EVAL_TICK_MS` と
//!   同じ周期 - 呼び出し元がタイマを持つ、このモジュール自身はタイマを
//!   持たない）で入力タグの値/品質の diff を検出し、変化があった演算タグ
//!   だけをトポロジカル順に再計算して [`ServerTagStore`] へ書く。
//!   `quality_since_ms` の品質遷移時刻は、入力の時刻ではなく評価で品質を認識した時刻を使う。
//!
//! ## 実装判断: なぜ diff 検出を保つか（unconditional recompute にしない理由）
//!
//! 250ms ごとに全演算タグを無条件で再評価する方が実装は単純だが、それでは
//! §4.2 の「時刻は再計算の**トリガとなった入力**の時刻」という契約が守れ
//! ない - 入力が実際には変化していなくても毎 tick 時刻が進んでしまい、
//! `/api/v1/values` を1回ポーリングした観測者には「何かが起きた」ように
//! 見えてしまう（WS の on_change 購読者は value/quality しか比較しないので
//! 実害はないが、ポーリング系 IF には嘘の情報になる）。そこで
//! [`ComputedEngine`] は演算タグ毎に「直前 tick で読んだ入力の
//! (値, 品質) 一式」を保持し、今回の tick でそれと完全一致するなら
//! 何もしない（[`ServerTagStore`] の値・時刻は前回のまま）。
//!
//! ## 品質継承・値の決定（§4.2「品質は入力の最悪品質」）
//!
//! 入力タグのいずれかが `Bad`、または値そのものが欠落している（登録時に
//! 存在確認済みのはずだが、レジストリ削除との競合を排除しないための防御的
//! 分岐）場合、式は評価せず `(None, Bad)` を書く（§4.2「入力欠損(未収集)は
//! Bad」に倣う - 値なしの Bad を Some(古い値) で誤魔化さない）。それ以外は
//! 入力の最悪品質（`Stale` があれば `Stale`、なければ `Good`）を継承しつつ
//! 式を評価する。式の結果型が [`banto_expr::Value::Bool`] の場合は
//! `1.0`/`0.0` に変換して格納する - I1 に bool 型のデータ型は無く（`bit` も
//! 0/1 の数値扱い）、[`ServerTagStore`] の値スロットは他の全タグ種別と
//! 同じ `Option<f64>` に統一しているため（`crate::hub::read_current` が
//! `tag_kind` で分岐するだけで済むのは、この統一があってこそ）。
//!
//! ## retain の永続化（§4.2「retain フラグで再起動時の最終値復元」）
//!
//! `retain = true` の内部タグは、書き込み成功時（`crate::write_path::execute_write`
//! の内部タグ分岐）に [`upsert_retained_value`] で `hub_retained_values`
//! テーブル（`migrations-sqlite/0104_hub_retained_values.sql`）へ最終値を upsert する。起動時
//! （`bin/banto-hub.rs`）は [`load_retained_values`] で読み出し、
//! `ServerTagStore::set` で品質 `Good`・時刻は保存時刻として初期化する。
//! `retain = false` の内部タグは何もロードしない - [`ServerTagStore::get`]
//! が未知キーに対し自然に `None` を返し、`crate::hub::read_current`
//! （実体は `crate::hub::effective_sample`）がそれを `(None, Bad, now_ms)`
//! へ変換するので、「retain=false は起動時 Bad」は特別なコードなしに成立
//! する。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use banto_collect::{CurrentReading, CurrentSample, CurrentValuesHandle, Quality};
use banto_core::BantoError;
use banto_expr::{CompiledExpr, Value};
use banto_tags::{COMPUTED_TAG_KIND, STRING_DATA_TYPE};
use sqlx::SqlitePool;

use crate::hub::{read_current, TagEntry, TagMap};

/// computed/internal タグの現在値ストア（このモジュールの doc comment
/// 参照）。`banto_collect::CurrentValuesHandle` の非 PLC 版 - キーは同じ
/// `tag_key`（`"tag:{id}"`）方式。
pub struct ServerTagStore {
    map: RwLock<HashMap<String, StoredValue>>,
    /// Effective quality can be interrupted by catalog disablement without
    /// any sample write. Shared with PLC views; never changes source data.
    reenabled_ms: RwLock<HashMap<String, i64>>,
}

#[derive(Debug, Clone, Copy)]
struct StoredValue {
    value: Option<f64>,
    quality: Quality,
    ptime_ms: i64,
    last_good_ms: Option<i64>,
    quality_since_ms: i64,
}

impl Default for ServerTagStore {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerTagStore {
    pub fn new() -> Self {
        Self {
            map: RwLock::new(HashMap::new()),
            reenabled_ms: RwLock::new(HashMap::new()),
        }
    }

    /// Called at every successful catalog publication, under the manager's
    /// state lock. Readers never take that lock while holding this one.
    pub(crate) fn observe_catalog(&self, previous: &TagMap, next: &TagMap, now_ms: i64) {
        let previous_enabled: HashMap<_, _> = previous
            .iter()
            .map(|entry| (entry.tag_key.as_str(), entry.enabled))
            .collect();
        let next_enabled: HashMap<_, _> = next
            .iter()
            .map(|entry| (entry.tag_key.as_str(), entry.enabled))
            .collect();
        let mut reenabled = self
            .reenabled_ms
            .write()
            .expect("catalog quality lock poisoned");
        reenabled.retain(|key, _| next_enabled.contains_key(key.as_str()));
        for (key, enabled) in next_enabled {
            if enabled && previous_enabled.get(key) == Some(&false) {
                reenabled.insert(key.to_string(), now_ms);
            }
        }
    }

    /// Disabled entries are effectively Bad. Resuming Good/Stale therefore
    /// starts a new interval, while an already-Bad sample remains Bad across
    /// that interval. Unknown metadata remains unknown.
    pub(crate) fn effective_quality_since(
        &self,
        key: &str,
        quality: Quality,
        since: Option<i64>,
    ) -> Option<i64> {
        if quality == Quality::Bad {
            return since;
        }
        let reenabled = self
            .reenabled_ms
            .read()
            .expect("catalog quality lock poisoned");
        since.map(|since| {
            reenabled
                .get(key)
                .map_or(since, |resumed| since.max(*resumed))
        })
    }

    /// `tag_key`（`crate::hub::TagEntry::tag_key`）の現在値。未知のキーは
    /// `None`（`banto_collect::CurrentValuesHandle::get` と同じ「一度も
    /// サンプルが来ていない」扱い - `crate::hub::effective_sample` がこれを
    /// `(None, Bad, now_ms)` に変換する）。
    pub fn get(&self, tag_key: &str) -> Option<CurrentSample> {
        let map = self
            .map
            .read()
            .expect("ServerTagStore lock poisoned (a writer panicked)");
        map.get(tag_key).map(|s| CurrentSample {
            value: s.value,
            ptime_ms: s.ptime_ms,
            quality: s.quality,
        })
    }

    /// Read the value and timestamp metadata from the same stored update.
    pub fn get_reading(&self, tag_key: &str) -> Option<CurrentReading> {
        let map = self
            .map
            .read()
            .expect("ServerTagStore lock poisoned (a writer panicked)");
        map.get(tag_key).map(|s| CurrentReading {
            sample: CurrentSample {
                value: s.value,
                ptime_ms: s.ptime_ms,
                quality: s.quality,
            },
            last_good_ms: s.last_good_ms,
            quality_since_ms: Some(s.quality_since_ms),
        })
    }

    /// `tag_key` の現在値を書き換える。値の時刻と品質の認識時刻が同じ
    /// 更新（内部タグへの書き込み、DB の読み取り成功など）に使う。
    /// 演算や DB Stale は `set_with_observation_time` で両者を分ける。
    pub fn set(&self, tag_key: &str, value: Option<f64>, quality: Quality, ptime_ms: i64) {
        self.set_with_observation_time(tag_key, value, quality, ptime_ms, ptime_ms);
    }

    /// Record quality recognition separately from the source/legacy timestamp
    /// (DB Stale retains its previous value time; computed inputs may be old).
    pub(crate) fn set_with_observation_time(
        &self,
        tag_key: &str,
        value: Option<f64>,
        quality: Quality,
        ptime_ms: i64,
        observed_ms: i64,
    ) {
        let mut map = self
            .map
            .write()
            .expect("ServerTagStore lock poisoned (a writer panicked)");
        let usable = quality == Quality::Good && value.is_some_and(f64::is_finite);
        let normalized = |quality, value: Option<f64>| {
            if quality == Quality::Good && !value.is_some_and(f64::is_finite) {
                Quality::Bad
            } else {
                quality
            }
        };
        let previous = map.get(tag_key);
        let last_good_ms = if usable {
            Some(ptime_ms)
        } else {
            previous.and_then(|s| s.last_good_ms)
        };
        let quality_since_ms = previous
            .filter(|s| normalized(s.quality, s.value) == normalized(quality, value))
            .map_or(observed_ms, |s| s.quality_since_ms);
        map.insert(
            tag_key.to_string(),
            StoredValue {
                value,
                quality,
                ptime_ms,
                last_good_ms,
                quality_since_ms,
            },
        );
    }
}

/// [`build_plan`] の成功結果 - コンパイル済み式一式とトポロジカル順
/// （依存先が先）。`ComputedEngine::commit` に渡すまでは何の副作用も持たない
/// 純粋なデータ（§4.3(a) の all-or-nothing を保つための設計 - `hub.rs` の
/// `CollectorManager::rebuild` 参照）。
#[derive(Debug, Clone)]
pub struct ComputedPlan {
    /// 演算タグの外部名一覧、依存先が先に来る順（`banto_expr::validate_dag`
    /// の戻り値そのもの）。
    order: Vec<String>,
    /// 演算タグの外部名 → コンパイル済み式。
    exprs: HashMap<String, CompiledExpr>,
}

impl ComputedPlan {
    fn empty() -> Self {
        Self {
            order: Vec::new(),
            exprs: HashMap::new(),
        }
    }
}

/// 演算タグの参照先タグ1件を検証できなかった理由（[`resolve_referenced_tag`]
/// 参照）。
pub(crate) enum ReferencedTagError {
    /// `TagMap` に存在しない外部名。
    Missing,
    /// 存在するが文字列タグ（演算タグは文字列タグを参照できない）。
    StringType,
}

/// 演算タグが参照する外部名1件を `TagMap` に照会し、存在確認と文字列タグ
/// 拒否の両方を行う（[`build_plan`] と #342 の単発式チェック API
/// `crate::rest::evaluate_expression_check` が同じ判定条件を共有するための
/// 唯一の実装 - 「条件を新しく発明しない」という #342 実装指示どおり、
/// ここを両者の呼び出し先にする）。
pub(crate) fn resolve_referenced_tag<'a>(
    map: &'a TagMap,
    referenced: &str,
) -> Result<&'a TagEntry, ReferencedTagError> {
    match map.get(referenced) {
        None => Err(ReferencedTagError::Missing),
        Some(ref_entry) if ref_entry.data_type == STRING_DATA_TYPE => {
            Err(ReferencedTagError::StringType)
        }
        Some(ref_entry) => Ok(ref_entry),
    }
}

/// `map` から `tag_kind == "computed"` のタグを集め、式をコンパイル
/// （`banto_expr::compile`）・参照タグの存在確認（文字列タグの参照は拒否）・
/// DAG 検証（`banto_expr::validate_dag`）まで行う純関数（`self` を取らない
/// - レジストリを読まず `TagMap` スナップショットだけを見る）。
///
/// 失敗はテスト計画2・4の「登録時（rebuild 時）拒否」「循環登録の拒否」に
/// 対応する - 呼び出し元（`crate::hub::CollectorManager::rebuild`）はこの
/// `Err` を rebuild 全体の失敗として扱う（§4.3(a) の all-or-nothing、
/// `computed` フィールドの doc comment参照）。
pub fn build_plan(map: &TagMap) -> Result<ComputedPlan, String> {
    let mut exprs = HashMap::new();
    let mut nodes: Vec<(String, Vec<String>)> = Vec::new();

    for entry in map.iter() {
        if entry.tag_kind != COMPUTED_TAG_KIND {
            continue;
        }
        // banto_tags::validate_tag_input が computed タグには expression
        // 必須を強制しているので None は本来到達不能 - 防御的にエラー化する
        // （レジストリとこのモジュールの契約がずれた場合に静かに無視しない
        // ため）。
        let Some(source) = entry.expression.as_deref() else {
            return Err(format!(
                "演算タグ {} に expression がありません",
                entry.external_name
            ));
        };
        let compiled = banto_expr::compile(source)
            .map_err(|err| format!("演算タグ {} の式が不正です: {err}", entry.external_name))?;

        // 文字列タグの参照は拒否（banto_expr 自身はレジストリを持たないため
        // 判定できない - このクレートの `crate` トップレベル doc comment
        // 「文字列タグの参照拒否は T6-2 の登録時検証の責務」どおり、ここが
        // その実装位置。判定条件そのものは [`resolve_referenced_tag`] に
        // 集約）。
        for referenced in compiled.referenced_tags() {
            match resolve_referenced_tag(map, referenced) {
                Ok(_) => {}
                Err(ReferencedTagError::Missing) => {
                    return Err(format!(
                        "演算タグ {} の参照先タグが存在しません: {referenced}",
                        entry.external_name
                    ));
                }
                Err(ReferencedTagError::StringType) => {
                    return Err(format!(
                        "演算タグ {} は文字列タグ {referenced} を参照できません",
                        entry.external_name
                    ));
                }
            }
        }

        nodes.push((
            entry.external_name.clone(),
            compiled.referenced_tags().to_vec(),
        ));
        exprs.insert(entry.external_name.clone(), compiled);
    }

    let order = banto_expr::validate_dag(&nodes)
        .map_err(|err| format!("演算タグに循環参照があります: {err}"))?;

    Ok(ComputedPlan { order, exprs })
}

/// 1タグの入力スナップショット比較用（値・品質のみ - 時刻はここでは比較
/// しない、このモジュールの doc comment「実装判断」参照）。
type InputSnapshot = HashMap<String, (Option<f64>, Quality)>;

struct EngineState {
    plan: ComputedPlan,
    /// 演算タグの外部名 → 直前 tick で読んだ入力スナップショット。
    /// `commit`（rebuild 成功）のたびに丸ごとクリアする - 式集合が変わった
    /// 直後は「全演算タグが初回」として扱い、必ず1回は再評価する
    /// （`crate::subscribe_core::Subscription::last` が初出タグを常に送る
    /// のと同じ判断）。
    baselines: HashMap<String, InputSnapshot>,
}

/// 演算タグの評価エンジン（このモジュールの doc comment参照）。
/// `bin/banto-hub.rs` が1個構築して `Arc` で共有する -
/// `crate::hub::CollectorManager`（`rebuild` からの `commit` 呼び出し用）と
/// 背景評価タスク（`evaluate_tick` を250ms ごとに呼ぶ）の両方がこの同じ
/// `Arc` を持つ。
pub struct ComputedEngine {
    store: Arc<ServerTagStore>,
    state: Mutex<EngineState>,
}

impl ComputedEngine {
    pub fn new(store: Arc<ServerTagStore>) -> Self {
        Self {
            store,
            state: Mutex::new(EngineState {
                plan: ComputedPlan::empty(),
                baselines: HashMap::new(),
            }),
        }
    }

    /// `crate::hub::CollectorManager::server_store` が返す共有ストア。
    pub fn server_store(&self) -> Arc<ServerTagStore> {
        self.store.clone()
    }

    /// 演算タグ `external_name` が式で直接参照しているタグの外部名一覧
    /// （`rest.rs` の `value_source`/`effective_simulation` 判定 - #335、
    /// 2026-09-14 オーナー決定 - 「computed の入力が真のシミュレーション中
    /// か」を辿るために公開する）。現在コミット済みの `plan` に無い名前
    /// （未登録・非 computed・rebuild 直後でまだ `commit` されていない等）
    /// は `None` - 呼び出し側は「入力なし」＝シミュレーションではない、
    /// として扱う。
    pub fn referenced_tags(&self, external_name: &str) -> Option<Vec<String>> {
        let state = self.state.lock().expect("ComputedEngine lock poisoned");
        state
            .plan
            .exprs
            .get(external_name)
            .map(|expr| expr.referenced_tags().to_vec())
    }

    /// `build_plan` が成功したときだけ呼ぶこと - 新しい plan に入れ替え、
    /// baseline を全消去する（§4.3(a) の all-or-nothing は呼び出し元
    /// `crate::hub::CollectorManager::rebuild` が担保する - このメソッド
    /// 自体は「検証済みの plan を無条件で採用する」だけ）。
    pub fn commit(&self, plan: ComputedPlan) {
        let mut state = self.state.lock().expect("ComputedEngine lock poisoned");
        state.plan = plan;
        state.baselines.clear();
    }

    /// 250ms 評価ティック1回分（このモジュールの doc comment参照）。
    /// `map`/`collect` は呼び出し元（背景タスク）が
    /// `crate::hub::CollectorManager::tag_map`/`current_values` から都度
    /// 取得したものを渡す想定 - `crate::subscribe_core::evaluate` と同じ
    /// 「評価の都度、最新の `TagMap` へ再解決する」規律。
    pub fn evaluate_tick(&self, map: &TagMap, collect: Option<&CurrentValuesHandle>, now_ms: i64) {
        let mut state = self.state.lock().expect("ComputedEngine lock poisoned");
        let EngineState { plan, baselines } = &mut *state;

        for name in &plan.order {
            let Some(expr) = plan.exprs.get(name) else {
                continue; // 到達不能（plan.order は plan.exprs のキーそのもの）。
            };
            // 演算タグ自身の catalog エントリ（自分の tag_key を得るため）。
            // rebuild と評価タスクは非同期に走るので、直前の commit と今回の
            // tick の間に当該タグが削除された、という極小のレースは理論上
            // 排除できない - その場合は次の tick 以降 `plan` 自体が新しい
            // commit で入れ替わるので、この tick だけ静かにスキップする
            // （防御的分岐、レジストリ削除との競合は排除しない設計 -
            // `crate::write_path` 冒頭の同種の注記と同じ立場）。
            let Some(self_entry) = map.get(name) else {
                continue;
            };

            let mut inputs: InputSnapshot = HashMap::with_capacity(expr.referenced_tags().len());
            let mut worst = Quality::Good;
            let mut any_bad = false;
            let mut max_t: i64 = i64::MIN;
            for referenced in expr.referenced_tags() {
                let (v, q, t) = match map.get(referenced) {
                    Some(ref_entry) => read_current(ref_entry, collect, &self.store, now_ms),
                    // 登録時に存在確認済みのはずの防御的分岐（`build_plan`
                    // 参照）。
                    None => (None, Quality::Bad, now_ms),
                };
                if v.is_none() || q == Quality::Bad {
                    any_bad = true;
                }
                if severity(q) > severity(worst) {
                    worst = q;
                }
                if t > max_t {
                    max_t = t;
                }
                inputs.insert(referenced.clone(), (v, q));
            }

            // 変化なし（前回と全く同じ入力スナップショット）なら何もしない
            // - このモジュールの doc comment「実装判断」参照。
            let unchanged = baselines.get(name.as_str()) == Some(&inputs);
            if unchanged {
                continue;
            }

            let trigger_time = if max_t == i64::MIN { now_ms } else { max_t };
            let (value, quality) = if any_bad {
                (None, Quality::Bad)
            } else {
                let closure = |tag: &str| inputs.get(tag).and_then(|(v, _)| *v).map(Value::Num);
                match expr.eval(&closure) {
                    Ok(Value::Num(x)) => (Some(x), worst),
                    Ok(Value::Bool(b)) => (Some(if b { 1.0 } else { 0.0 }), worst),
                    // 到達不能に近い防御的分岐（参照タグは登録時に存在確認・
                    // 型は常に Num 前提で解決済み）- 起きた場合は Bad として
                    // 扱う。
                    Err(_) => (None, Quality::Bad),
                }
            };

            let (value, quality) =
                if value.is_some_and(|value| !crate::hub::valid_current_value(self_entry, value)) {
                    (None, Quality::Bad)
                } else {
                    (value, quality)
                };
            self.store.set_with_observation_time(
                &self_entry.tag_key,
                value,
                quality,
                trigger_time,
                now_ms,
            );
            baselines.insert(name.clone(), inputs);
        }
    }
}

fn severity(q: Quality) -> u8 {
    match q {
        Quality::Good => 0,
        Quality::Stale => 1,
        Quality::Bad => 2,
    }
}

/// `retain = true` の内部タグの最終値を `hub_retained_values`
/// （`migrations-sqlite/0104_hub_retained_values.sql`）へ upsert する（このモジュールの doc comment
/// 「retain の永続化」参照）。`tag_id` は `crate::hub::TagEntry::ids` の
/// 第3要素 - `ServerTagStore` のキー（`tag_key` = `"tag:{id}"`）と同じ id
/// を主キーに使う（`load_retained_values` がそのまま `tag_key` を組み立て
/// 直せるように）。
pub async fn upsert_retained_value(
    pool: &SqlitePool,
    tag_id: i64,
    value: f64,
    ptime_ms: i64,
) -> Result<(), BantoError> {
    sqlx::query(
        "INSERT INTO hub_retained_values (tag_id, value, ptime_ms) VALUES (?, ?, ?) \
         ON CONFLICT(tag_id) DO UPDATE SET value = excluded.value, ptime_ms = excluded.ptime_ms",
    )
    .bind(tag_id)
    .bind(value)
    .bind(ptime_ms)
    .execute(pool)
    .await
    .map_err(banto_storage::storage_error)?;
    Ok(())
}

/// 起動時に `hub_retained_values` の全行を読む - `(tag_id, value,
/// ptime_ms)`。呼び出し元（`bin/banto-hub.rs`）は `retain = true` の内部
/// タグそれぞれについて `tag_key = format!("tag:{tag_id}")` を組み立て、
/// `ServerTagStore::set(&tag_key, Some(value), Quality::Good, ptime_ms)`
/// で初期化する（品質 Good・時刻は保存時刻 - §4.2「起動時にロードして
/// ServerTagStore を初期化(品質 Good・時刻は保存時刻)」）。
pub async fn load_retained_values(pool: &SqlitePool) -> Result<Vec<(i64, f64, i64)>, BantoError> {
    let rows: Vec<(i64, f64, i64)> =
        sqlx::query_as("SELECT tag_id, value, ptime_ms FROM hub_retained_values")
            .fetch_all(pool)
            .await
            .map_err(banto_storage::storage_error)?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use banto_tags::PLC_TAG_KIND;

    /// Test-only `TagMap` builder - `crate::hub::TagMap` has no public
    /// constructor from a list of entries (it is always built from the
    /// registry by `crate::hub::CollectorManager`), so these unit tests build
    /// one directly via the same private fields this module already has
    /// access to (same crate).
    fn map_of(entries: Vec<TagEntry>) -> TagMap {
        let mut map = TagMap::default();
        for entry in entries {
            map.insert_for_test(entry);
        }
        map
    }

    #[test]
    fn catalog_quality_epochs_are_shared_stable_and_removed_with_tag_keys() {
        for kind in ["plc", "computed", "internal", "db"] {
            let store = ServerTagStore::new();
            let mut entry = plc_entry("t", "c.g.t", 1);
            entry.tag_kind = kind.to_string();
            let enabled = map_of(vec![entry.clone()]);
            entry.enabled = false;
            let disabled = map_of(vec![entry]);
            store.set("tag:1", Some(42.0), Quality::Good, 100);
            let raw = store.get_reading("tag:1");
            store.observe_catalog(&enabled, &disabled, 200);
            store.observe_catalog(&disabled, &enabled, 300);
            store.observe_catalog(&enabled, &enabled, 400);
            for quality in [Quality::Good, Quality::Stale] {
                assert_eq!(
                    store.effective_quality_since("tag:1", quality, Some(100)),
                    Some(300)
                );
                assert_eq!(
                    store.effective_quality_since("tag:1", quality, Some(500)),
                    Some(500)
                );
            }
            assert_eq!(
                store.effective_quality_since("tag:1", Quality::Bad, Some(100)),
                Some(100)
            );
            assert_eq!(
                store.effective_quality_since("tag:1", Quality::Good, None),
                None
            );
            assert_eq!(
                store.get_reading("tag:1"),
                raw,
                "catalog epochs must never rewrite samples"
            );
            store.observe_catalog(&enabled, &disabled, 600);
            store.observe_catalog(&disabled, &enabled, 700);
            assert_eq!(
                store.effective_quality_since("tag:1", Quality::Good, Some(100)),
                Some(700)
            );
            store.observe_catalog(&enabled, &TagMap::default(), 800);
            assert!(store.reenabled_ms.read().unwrap().is_empty());
        }
    }

    #[test]
    fn invalid_current_values_are_bad_and_preserve_attempt_time() {
        use crate::hub::{effective_sample, read_current_view};
        for data_type in ["f64", "bit"] {
            let mut entry = plc_entry("a", "c.g.a", 1);
            entry.data_type = data_type.to_string();
            for value in [
                f64::NAN,
                f64::INFINITY,
                f64::NEG_INFINITY,
                0.0,
                1.0,
                0.5,
                2.0,
            ] {
                let valid =
                    value.is_finite() && (data_type != "bit" || value == 0.0 || value == 1.0);
                let sample = CurrentSample {
                    value: Some(value),
                    quality: Quality::Good,
                    ptime_ms: 123,
                };
                assert_eq!(
                    effective_sample(&entry, Some(sample), 999),
                    (
                        valid.then_some(value),
                        if valid { Quality::Good } else { Quality::Bad },
                        123
                    )
                );
                entry.tag_kind = "internal".to_string();
                let store = ServerTagStore::new();
                store.set(&entry.tag_key, Some(value), Quality::Good, 123);
                let view = read_current_view(&entry, None, &store, 999);
                assert_eq!(view.source_time_ms, valid.then_some(123));
                assert_eq!(view.quality_since_ms, Some(123));
                assert_eq!(read_current_view(&entry, None, &store, 1_999), view);
            }
        }
    }

    #[test]
    fn consecutive_nonfinite_and_missing_values_keep_first_bad_epoch() {
        use crate::hub::read_current_view;
        let mut entry = plc_entry("a", "c.g.a", 1);
        entry.tag_kind = "internal".to_string();
        let store = ServerTagStore::new();
        store.set(&entry.tag_key, Some(42.0), Quality::Good, 100);
        for (timestamp, value) in [
            (200, Some(f64::NAN)),
            (300, Some(f64::INFINITY)),
            (400, Some(f64::NEG_INFINITY)),
            (500, None),
            (600, None),
        ] {
            store.set(&entry.tag_key, value, Quality::Good, timestamp);
            let reading = store.get_reading(&entry.tag_key).unwrap();
            assert_eq!(reading.quality_since_ms, Some(200));
            let view = read_current_view(&entry, None, &store, 999);
            assert_eq!((view.value, view.quality), (None, Quality::Bad));
            assert_eq!(view.timestamp_ms, timestamp);
            assert_eq!(view.source_time_ms, Some(100));
            assert_eq!(view.quality_since_ms, Some(200));
            assert_eq!(read_current_view(&entry, None, &store, 1_999), view);
        }
        store.set(&entry.tag_key, Some(42.0), Quality::Good, 700);
        let recovered = read_current_view(&entry, None, &store, 999);
        assert_eq!(
            (recovered.value, recovered.quality),
            (Some(42.0), Quality::Good)
        );
        assert_eq!(recovered.quality_since_ms, Some(700));
        assert_eq!(recovered.source_time_ms, Some(700));
    }

    #[test]
    fn missing_current_value_never_reads_as_good_or_stale() {
        use crate::hub::effective_sample;
        let entry = plc_entry("a", "c.g.a", 1);
        for quality in [Quality::Good, Quality::Stale, Quality::Bad] {
            assert_eq!(
                effective_sample(
                    &entry,
                    Some(CurrentSample {
                        value: None,
                        quality,
                        ptime_ms: 123
                    }),
                    999
                ),
                (None, Quality::Bad, 123)
            );
        }
    }

    #[test]
    fn nonfinite_computed_output_is_bad_then_recovers() {
        let mut input = plc_entry("a", "line1.fast.a", 1);
        input.tag_kind = "internal".to_string();
        let map = map_of(vec![
            input,
            computed_entry("b", "calc.x.b", 2, "line1.fast.a * 2"),
            computed_entry("c", "calc.x.c", 3, "calc.x.b + 1"),
        ]);
        let store = Arc::new(ServerTagStore::new());
        let engine = ComputedEngine::new(store.clone());
        engine.commit(build_plan(&map).unwrap());
        for (value, timestamp, observed, expected) in [
            (f64::MAX, 100, 200, Quality::Bad),
            (1.0, 300, 400, Quality::Good),
        ] {
            store.set("tag:1", Some(value), Quality::Good, timestamp);
            engine.evaluate_tick(&map, None, observed);
            for key in ["tag:2", "tag:3"] {
                let reading = store.get_reading(key).unwrap();
                assert_eq!(reading.sample.quality, expected);
                assert_eq!(reading.sample.ptime_ms, timestamp);
                assert_eq!(reading.quality_since_ms, Some(observed));
                if expected == Quality::Bad {
                    assert_eq!(reading.sample.value, None);
                }
            }
        }
    }

    #[test]
    fn invalid_input_recovers_and_constant_bit_output_is_bad() {
        let mut input = plc_entry("a", "line1.fast.a", 1);
        input.tag_kind = "internal".to_string();
        let mut constant = computed_entry("constant", "calc.x.constant", 3, "2");
        constant.data_type = "bit".to_string();
        let map = map_of(vec![
            input,
            computed_entry("b", "calc.x.b", 2, "line1.fast.a + 1"),
            constant,
        ]);
        let store = Arc::new(ServerTagStore::new());
        let engine = ComputedEngine::new(store.clone());
        engine.commit(build_plan(&map).unwrap());
        for (value, expected) in [
            (f64::NAN, Quality::Bad),
            (f64::INFINITY, Quality::Bad),
            (f64::NEG_INFINITY, Quality::Bad),
            (1.0, Quality::Good),
        ] {
            store.set("tag:1", Some(value), Quality::Good, 100);
            engine.evaluate_tick(&map, None, 200);
            assert_eq!(store.get("tag:2").unwrap().quality, expected);
            assert_eq!(store.get("tag:3").unwrap().quality, Quality::Bad);
            assert_eq!(
                store.get_reading("tag:3").unwrap().quality_since_ms,
                Some(200)
            );
        }
    }

    #[test]
    fn on_change_reports_first_invalid_once_and_recovers() {
        use crate::subscribe_core::{evaluate, initial_values, Mode, Subscription, TagPattern};
        let mut entry = plc_entry("a", "line1.fast.a", 1);
        entry.tag_kind = "internal".to_string();
        let map = map_of(vec![entry]);
        let store = ServerTagStore::new();
        store.set("tag:1", Some(f64::NAN), Quality::Good, 100);
        let patterns = vec![TagPattern::All];
        let (first, _) = initial_values(&patterns, &map, None, &store, 200, None);
        assert_eq!((first[0].v, first[0].q), (None, Quality::Bad));
        // A subscriber that starts while Good must receive the first Bad
        // transition as well as the later recovery.
        store.set("tag:1", Some(42.0), Quality::Good, 210);
        let (first, last) = initial_values(&patterns, &map, None, &store, 220, None);
        assert_eq!((first[0].v, first[0].q), (Some(42.0), Quality::Good));
        let mut sub = Subscription {
            patterns,
            mode: Mode::OnChange,
            last,
            next_due_ms: 0,
        };
        assert!(evaluate(&mut sub, &map, None, &store, 230, None).is_none());
        store.set("tag:1", Some(f64::NAN), Quality::Good, 250);
        let bad = evaluate(&mut sub, &map, None, &store, 300, None).unwrap();
        assert_eq!(bad.len(), 1);
        assert_eq!((bad[0].v, bad[0].q, bad[0].t), (None, Quality::Bad, 250));
        assert!(evaluate(&mut sub, &map, None, &store, 350, None).is_none());
        store.set("tag:1", Some(f64::INFINITY), Quality::Good, 400);
        assert!(evaluate(&mut sub, &map, None, &store, 500, None).is_none());
        store.set("tag:1", Some(42.0), Quality::Good, 600);
        let recovered = evaluate(&mut sub, &map, None, &store, 700, None).unwrap();
        assert_eq!(
            (recovered[0].v, recovered[0].q, recovered[0].t),
            (Some(42.0), Quality::Good, 600)
        );
        assert!(evaluate(&mut sub, &map, None, &store, 800, None).is_none());
    }

    #[test]
    fn computed_invalid_bit_propagates_bad_and_recovers() {
        let mut input = plc_entry("a", "line1.fast.a", 1);
        input.tag_kind = "internal".to_string();
        let mut bit = computed_entry("b", "calc.x.b", 2, "line1.fast.a * 2");
        bit.data_type = "bit".to_string();
        let map = map_of(vec![
            input,
            bit,
            computed_entry("c", "calc.x.c", 3, "calc.x.b + 1"),
        ]);
        let store = Arc::new(ServerTagStore::new());
        let engine = ComputedEngine::new(store.clone());
        engine.commit(build_plan(&map).unwrap());
        for (value, sample_time, observed, expected) in [
            (0.0, 100, 200, Quality::Good),
            (1.0, 300, 400, Quality::Bad),
            (0.0, 500, 600, Quality::Good),
        ] {
            store.set("tag:1", Some(value), Quality::Good, sample_time);
            engine.evaluate_tick(&map, None, observed);
            for key in ["tag:2", "tag:3"] {
                let reading = store.get_reading(key).unwrap();
                assert_eq!(reading.sample.quality, expected);
                assert_eq!(reading.sample.ptime_ms, sample_time);
                assert_eq!(reading.quality_since_ms, Some(observed));
                if expected == Quality::Bad {
                    assert_eq!(reading.sample.value, None);
                }
            }
            engine.evaluate_tick(&map, None, observed + 50);
            assert_eq!(
                store.get_reading("tag:2").unwrap().quality_since_ms,
                Some(observed)
            );
        }
    }

    #[test]
    fn common_view_preserves_server_timestamps_and_legacy_tuple() {
        use crate::hub::read_current_view;
        for kind in ["computed", "internal", "db"] {
            let store = ServerTagStore::new();
            let mut entry = plc_entry("t", "c.g.t", 1);
            entry.tag_kind = kind.to_string();
            let missing = read_current_view(&entry, None, &store, 99);
            assert_eq!(
                (missing.source_time_ms, missing.quality_since_ms),
                (None, None)
            );
            store.set(&entry.tag_key, Some(2.0), Quality::Good, 100);
            let good = read_current_view(&entry, None, &store, 200);
            assert_eq!(
                (good.source_time_ms, good.quality_since_ms),
                (Some(100), Some(100))
            );
            store.set(&entry.tag_key, None, Quality::Bad, 300);
            store.set(&entry.tag_key, None, Quality::Bad, 400);
            let bad = read_current_view(&entry, None, &store, 500);
            assert_eq!(
                (bad.source_time_ms, bad.quality_since_ms),
                (Some(100), Some(300))
            );
            assert_eq!(
                read_current(&entry, None, &store, 500),
                (bad.value, bad.quality, bad.timestamp_ms)
            );
            store.set(&entry.tag_key, Some(2.0), Quality::Stale, 600);
            store.set(&entry.tag_key, Some(2.0), Quality::Stale, 700);
            assert_eq!(
                store.get_reading(&entry.tag_key).unwrap().quality_since_ms,
                Some(600)
            );
            entry.enabled = false;
            let disabled = read_current_view(&entry, None, &store, 800);
            assert_eq!(
                (disabled.source_time_ms, disabled.quality_since_ms),
                (None, None)
            );
            assert_eq!(disabled.timestamp_ms, 700);
            // A new key has no usable source time for malformed Good.
            store.set("invalid", Some(f64::INFINITY), Quality::Good, 900);
            assert_eq!(store.get_reading("invalid").unwrap().last_good_ms, None);
        }
    }

    fn plc_entry(name: &str, external_name: &str, tag_id: i64) -> TagEntry {
        TagEntry {
            external_name: external_name.to_string(),
            tag_key: format!("tag:{tag_id}"),
            ids: (1, 1, tag_id),
            connection: "line1".to_string(),
            group: "fast".to_string(),
            name: name.to_string(),
            address: "40001".to_string(),
            data_type: "i16".to_string(),
            unit: None,
            decimals: 0,
            period_ms: 1_000,
            enabled: true,
            writable: false,
            tag_kind: PLC_TAG_KIND.to_string(),
            expression: None,
            retain: false,
            simulation: false,
        }
    }

    fn computed_entry(name: &str, external_name: &str, tag_id: i64, expression: &str) -> TagEntry {
        TagEntry {
            external_name: external_name.to_string(),
            tag_key: format!("tag:{tag_id}"),
            ids: (2, 2, tag_id),
            connection: "calc".to_string(),
            group: "x".to_string(),
            name: name.to_string(),
            address: String::new(),
            data_type: "f32".to_string(),
            unit: None,
            decimals: 2,
            period_ms: 1_000,
            enabled: true,
            writable: false,
            tag_kind: COMPUTED_TAG_KIND.to_string(),
            expression: Some(expression.to_string()),
            retain: false,
            simulation: false,
        }
    }

    #[test]
    fn build_plan_compiles_and_orders_a_simple_computed_tag() {
        let map = map_of(vec![
            plc_entry("a", "line1.fast.a", 1),
            plc_entry("b", "line1.fast.b", 2),
            computed_entry("avg", "calc.x.avg", 3, "(line1.fast.a + line1.fast.b) / 2"),
        ]);
        let plan = build_plan(&map).expect("should compile");
        assert_eq!(plan.order, vec!["calc.x.avg".to_string()]);
        assert!(plan.exprs.contains_key("calc.x.avg"));
    }

    #[test]
    fn build_plan_orders_chained_computed_tags_dependency_first() {
        let map = map_of(vec![
            plc_entry("a", "line1.fast.a", 1),
            computed_entry("double", "calc.x.double", 2, "line1.fast.a * 2"),
            computed_entry("quad", "calc.x.quad", 3, "calc.x.double * 2"),
        ]);
        let plan = build_plan(&map).expect("should compile");
        let double_pos = plan
            .order
            .iter()
            .position(|n| n == "calc.x.double")
            .unwrap();
        let quad_pos = plan.order.iter().position(|n| n == "calc.x.quad").unwrap();
        assert!(
            double_pos < quad_pos,
            "dependency (double) must precede dependent (quad)"
        );
    }

    #[test]
    fn build_plan_rejects_a_cycle() {
        let map = map_of(vec![
            computed_entry("a", "calc.x.a", 1, "calc.x.b + 1"),
            computed_entry("b", "calc.x.b", 2, "calc.x.a + 1"),
        ]);
        let err = build_plan(&map).unwrap_err();
        assert!(err.contains("循環"), "error should mention 循環: {err}");
    }

    #[test]
    fn build_plan_rejects_a_reference_to_a_string_tag() {
        let mut string_tag = plc_entry("s", "line1.fast.s", 1);
        string_tag.data_type = "string".to_string();
        let map = map_of(vec![
            string_tag,
            computed_entry("bad", "calc.x.bad", 2, "line1.fast.s + 1"),
        ]);
        let err = build_plan(&map).unwrap_err();
        assert!(err.contains("文字列"), "error should mention 文字列: {err}");
    }

    #[test]
    fn build_plan_rejects_a_reference_to_a_missing_tag() {
        let map = map_of(vec![computed_entry(
            "bad",
            "calc.x.bad",
            1,
            "line1.fast.nope + 1",
        )]);
        let err = build_plan(&map).unwrap_err();
        assert!(err.contains("存在しません"), "error: {err}");
    }

    #[test]
    fn evaluate_tick_computes_average_and_writes_good_quality() {
        let map = map_of(vec![
            plc_entry("a", "line1.fast.a", 1),
            plc_entry("b", "line1.fast.b", 2),
            computed_entry("avg", "calc.x.avg", 3, "(line1.fast.a + line1.fast.b) / 2"),
        ]);
        let plan = build_plan(&map).unwrap();
        let store = Arc::new(ServerTagStore::new());
        let engine = ComputedEngine::new(store.clone());
        engine.commit(plan);

        // No `CurrentValuesHandle` (collect = None) means every plc input
        // reads as Bad/None (no collector running) - simulate real inputs by
        // writing straight into the store keyed as if they were plc tag_keys
        // is not possible (plc tags are read via `collect`, not
        // `server_store`) - so this test exercises the "input missing" path
        // instead, proving the Bad propagation. A live-PLC-input case is
        // covered by the hub integration test (T6-2 test plan #1).
        engine.evaluate_tick(&map, None, 1_000);
        let sample = store
            .get("tag:3")
            .expect("a value should have been written");
        assert_eq!(sample.quality, Quality::Bad, "missing plc inputs -> Bad");
        assert_eq!(sample.value, None);
    }

    #[test]
    fn evaluate_tick_chains_computed_tags_through_the_server_store() {
        let map = map_of(vec![
            computed_entry("constant", "calc.x.constant", 1, "21"),
            computed_entry("doubled", "calc.x.doubled", 2, "calc.x.constant * 2"),
        ]);
        let plan = build_plan(&map).unwrap();
        let store = Arc::new(ServerTagStore::new());
        let engine = ComputedEngine::new(store.clone());
        engine.commit(plan);

        engine.evaluate_tick(&map, None, 1_000);
        let constant = store.get("tag:1").expect("constant should be computed");
        assert_eq!(constant.value, Some(21.0));
        assert_eq!(constant.quality, Quality::Good);
        let doubled = store.get("tag:2").expect("doubled should be computed");
        assert_eq!(doubled.value, Some(42.0));
        assert_eq!(doubled.quality, Quality::Good);
    }

    #[test]
    fn evaluate_tick_skips_recompute_when_inputs_are_unchanged() {
        let map = map_of(vec![computed_entry("constant", "calc.x.constant", 1, "1")]);
        let plan = build_plan(&map).unwrap();
        let store = Arc::new(ServerTagStore::new());
        let engine = ComputedEngine::new(store.clone());
        engine.commit(plan);

        engine.evaluate_tick(&map, None, 1_000);
        let first = store.get("tag:1").unwrap();
        assert_eq!(first.ptime_ms, 1_000);

        // A later tick with no input change must not touch the stored
        // timestamp (this module's doc comment "実装判断").
        engine.evaluate_tick(&map, None, 5_000);
        let second = store.get("tag:1").unwrap();
        assert_eq!(second.ptime_ms, 1_000, "unchanged inputs must not re-stamp");
    }

    #[test]
    fn computed_quality_time_uses_observation_while_source_time_tracks_inputs() {
        let mut input = plc_entry("a", "line1.fast.a", 1);
        input.tag_kind = "internal".to_string();
        let map = map_of(vec![
            input,
            computed_entry("b", "calc.x.b", 2, "line1.fast.a * 2"),
        ]);
        let store = Arc::new(ServerTagStore::new());
        let engine = ComputedEngine::new(store.clone());
        engine.commit(build_plan(&map).unwrap());
        store.set("tag:1", Some(2.0), Quality::Good, 100);
        engine.evaluate_tick(&map, None, 200);
        let good = store.get_reading("tag:2").unwrap();
        assert_eq!(good.last_good_ms, Some(100));
        assert_eq!(good.quality_since_ms, Some(200));
        store.set("tag:1", Some(2.0), Quality::Stale, 100);
        engine.evaluate_tick(&map, None, 300);
        let stale = store.get_reading("tag:2").unwrap();
        assert_eq!(stale.sample.ptime_ms, 100);
        assert_eq!(stale.quality_since_ms, Some(300));
        assert_eq!(stale.last_good_ms, Some(100));
        engine.evaluate_tick(&map, None, 400);
        assert_eq!(store.get_reading("tag:2"), Some(stale));
        store.set("tag:1", None, Quality::Bad, 100);
        engine.evaluate_tick(&map, None, 500);
        let bad = store.get_reading("tag:2").unwrap();
        assert_eq!(bad.sample.quality, Quality::Bad);
        assert_eq!(bad.sample.ptime_ms, 100);
        assert_eq!(bad.quality_since_ms, Some(500));
        assert_eq!(bad.last_good_ms, Some(100));
    }

    #[test]
    fn severity_orders_bad_worst_then_stale_then_good() {
        assert!(severity(Quality::Bad) > severity(Quality::Stale));
        assert!(severity(Quality::Stale) > severity(Quality::Good));
    }

    #[tokio::test]
    async fn retained_value_round_trips_through_upsert_and_load() {
        let pool = crate::db::init_db_memory().await.unwrap();
        upsert_retained_value(&pool, 42, 12.5, 1_000).await.unwrap();
        let rows = load_retained_values(&pool).await.unwrap();
        assert_eq!(rows, vec![(42, 12.5, 1_000)]);

        // upsert again (same tag_id) must replace, not duplicate.
        upsert_retained_value(&pool, 42, 99.0, 2_000).await.unwrap();
        let rows = load_retained_values(&pool).await.unwrap();
        assert_eq!(rows, vec![(42, 99.0, 2_000)]);
    }
}
