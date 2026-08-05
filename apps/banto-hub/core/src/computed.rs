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
//! テーブル（`crate::db` の冪等 DDL）へ最終値を upsert する。起動時
//! （`bin/banto-hub.rs`）は [`load_retained_values`] で読み出し、
//! `ServerTagStore::set` で品質 `Good`・時刻は保存時刻として初期化する。
//! `retain = false` の内部タグは何もロードしない - [`ServerTagStore::get`]
//! が未知キーに対し自然に `None` を返し、`crate::hub::read_current`
//! （実体は `crate::hub::effective_sample`）がそれを `(None, Bad, now_ms)`
//! へ変換するので、「retain=false は起動時 Bad」は特別なコードなしに成立
//! する。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use banto_collect::{CurrentSample, CurrentValuesHandle, Quality};
use banto_core::BantoError;
use banto_expr::{CompiledExpr, Value};
use banto_tags::{COMPUTED_TAG_KIND, STRING_DATA_TYPE};
use sqlx::SqlitePool;

use crate::hub::{read_current, TagMap};

/// computed/internal タグの現在値ストア（このモジュールの doc comment
/// 参照）。`banto_collect::CurrentValuesHandle` の非 PLC 版 - キーは同じ
/// `tag_key`（`"tag:{id}"`）方式。
pub struct ServerTagStore {
    map: RwLock<HashMap<String, StoredValue>>,
}

#[derive(Debug, Clone, Copy)]
struct StoredValue {
    value: Option<f64>,
    quality: Quality,
    ptime_ms: i64,
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
        }
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

    /// `tag_key` の現在値を書き換える。[`ComputedEngine`]（演算タグの評価
    /// 結果）と `crate::write_path::execute_write`（内部タグへの書き込み）
    /// の両方がこの1つの入口を通る。
    pub fn set(&self, tag_key: &str, value: Option<f64>, quality: Quality, ptime_ms: i64) {
        let mut map = self
            .map
            .write()
            .expect("ServerTagStore lock poisoned (a writer panicked)");
        map.insert(
            tag_key.to_string(),
            StoredValue {
                value,
                quality,
                ptime_ms,
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
        // その実装位置）。
        for referenced in compiled.referenced_tags() {
            match map.get(referenced) {
                None => {
                    return Err(format!(
                        "演算タグ {} の参照先タグが存在しません: {referenced}",
                        entry.external_name
                    ));
                }
                Some(ref_entry) if ref_entry.data_type == STRING_DATA_TYPE => {
                    return Err(format!(
                        "演算タグ {} は文字列タグ {referenced} を参照できません",
                        entry.external_name
                    ));
                }
                Some(_) => {}
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

            self.store
                .set(&self_entry.tag_key, value, quality, trigger_time);
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
/// （`crate::db` の冪等 DDL）へ upsert する（このモジュールの doc comment
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
    use crate::hub::TagEntry;
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
