//! タグ1件の `value_source`/`effective_simulation` 判定（#335、2026-09-14
//! オーナー決定、2026-09-15 追補「外部出力を PLC への出力と勘違いしていた」）。
//!
//! 元々は `crate::rest` 専用のプライベート関数だったが、2026-09-15 オーナー
//! 指示で MQTT publish（`crate::mqtt`）のペイロードにも同じ `value_source`
//! を乗せることになったため、REST/MQTT の両方から呼べる共有モジュールへ
//! 切り出した（WS/gRPC は wire 形式の制約でこのラベルを持たない - 元々の
//! 設計、このスライスでは変更しない）。
//!
//! 判定規則:
//! - `plc`: 有効・収集中・(接続単位の simulation 設定 または run が
//!   `AllSimulation`) なら `effective_simulation = true`、`value_source =
//!   "simulation"`。それ以外は `"real"`。
//! - `computed`: 収集中・(run が `AllSimulation` または、式が参照する入力
//!   タグのいずれかが真にシミュレーション中) なら `effective_simulation =
//!   true`、`value_source = "derived_simulation"`（情報ラベル - 値の抑止は
//!   しない）。それ以外の既定は `"computed"`。
//! - `internal`: 常に `"internal"`。
//! - `db`（2026-09-06、外部 DB 連携 S2）: 常に `"db"`。

use std::collections::HashSet;

use crate::computed::ComputedEngine;
use crate::controller::{CollectionState, CollectionStatus, RunMode};
use crate::hub::{TagEntry, TagMap};

/// タグ1件の `effective_simulation`。[`value_source_for_tag`]のdoc comment
/// 参照。computed 自体は PLC 接続を持たないので、入力側の判定へ委譲する
/// （[`effective_simulation_for_tag_inner`]の computed 分岐）。
pub(crate) fn effective_simulation_for_tag(
    entry: &TagEntry,
    runtime: &CollectionStatus,
    map: &TagMap,
    computed: &ComputedEngine,
) -> bool {
    effective_simulation_for_tag_inner(entry, runtime, map, computed, &mut HashSet::new())
}

/// [`effective_simulation_for_tag`]の実体。computed → 入力タグの再帰を
/// `visited` で防御する - `banto_expr::validate_dag`が登録時に循環を拒否
/// しているので理論上到達しないが、`map`（`TagMap`スナップショット）と
/// `computed`（`ComputedEngine`の現在の plan）は別々に読むため、rebuild の
/// 合間に読めば理論上ずれ得る - そのずれが循環に見えても無限再帰にしない
/// ための防御。
fn effective_simulation_for_tag_inner(
    entry: &TagEntry,
    runtime: &CollectionStatus,
    map: &TagMap,
    computed: &ComputedEngine,
    visited: &mut HashSet<String>,
) -> bool {
    match entry.tag_kind.as_str() {
        banto_tags::PLC_TAG_KIND => {
            entry.enabled
                && runtime.state == CollectionState::Running
                && (entry.simulation || runtime.mode == RunMode::AllSimulation)
        }
        banto_tags::COMPUTED_TAG_KIND => {
            if runtime.state != CollectionState::Running {
                return false;
            }
            if runtime.mode == RunMode::AllSimulation {
                return true;
            }
            if !visited.insert(entry.external_name.clone()) {
                return false;
            }
            computed
                .referenced_tags(&entry.external_name)
                .map(|inputs| {
                    inputs.iter().any(|name| {
                        map.get(name)
                            .map(|input| {
                                effective_simulation_for_tag_inner(
                                    input, runtime, map, computed, visited,
                                )
                            })
                            .unwrap_or(false)
                    })
                })
                .unwrap_or(false)
        }
        _ => false,
    }
}

/// タグ1件の `value_source`。computed は既定 `"computed"` -
/// [`effective_simulation_for_tag`]が true になる場合（AllSimulation 運転中、
/// または入力のいずれかが真にシミュレーション中）だけ情報ラベル
/// `"derived_simulation"` に切り替わる（隠す・抑止する意味は持たない -
/// 値そのものは常時返す、呼び出し元のどこにもフィルタは無い）。
pub(crate) fn value_source_for_tag(
    entry: &TagEntry,
    runtime: &CollectionStatus,
    map: &TagMap,
    computed: &ComputedEngine,
) -> &'static str {
    match entry.tag_kind.as_str() {
        banto_tags::PLC_TAG_KIND if effective_simulation_for_tag(entry, runtime, map, computed) => {
            "simulation"
        }
        banto_tags::PLC_TAG_KIND => "real",
        banto_tags::COMPUTED_TAG_KIND
            if effective_simulation_for_tag(entry, runtime, map, computed) =>
        {
            "derived_simulation"
        }
        banto_tags::COMPUTED_TAG_KIND => "computed",
        banto_tags::INTERNAL_TAG_KIND => "internal",
        // 外部 DB 連携 §6-11（2026-09-06 オーナー決定「足す」）: 外部 DB
        // 由来の値は実機でもシミュレーションでも内部書き込みでもないので
        // 独自ラベルを持つ。banto-tagclient SDK は未知ラベルを
        // `Unknown(raw)` で保持するので、旧 SDK との互換も保たれる。
        banto_tags::DB_TAG_KIND => "db",
        // Tag registration validates tag_kind, but keep the wire contract
        // fail-safe if a future kind is introduced without this DTO update.
        _ => "internal",
    }
}
