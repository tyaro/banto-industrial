//! サイドカー（S5、未実装）が `PUT /api/sink/status` で5秒ごとに push する
//! 運転状態を、プロセス内メモリだけに保持する店（設計 §5.2「Hub はメモリに
//! 保持し、`GET /api/status`の sink 節と状態画面に出す」）。
//!
//! サイドカーは状態を持たない（設計 §5.1）ため、この情報も**永続化しない** -
//! Hub を再起動すれば直後は「まだ一度も push が無い」= `unknown`から始まる。
//! `crate::db_source::status`（S2 の DB Source 側運転状態、同じくプロセス内
//! メモリのみ）と対になる存在。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

/// 最後の push からこの時間（ミリ秒）以上経過していたら、サイドカーの
/// 状態は`unknown`（サービス停止の疑い、設計 §5.2「15秒以上pushが無ければ
/// unknown」）。一度も push が無い場合も同じく`unknown`。
pub const SINK_SIDECAR_STALE_AFTER_MS: i64 = 15_000;

/// `PUT /api/sink/status`のボディが運ぶ1グループ分の運転状態
/// （設計 §5.2）。`state`の許容値は[`ALLOWED_SINK_GROUP_STATES`]。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SinkGroupStatusPush {
    pub id: i64,
    pub state: String,
    pub queued: u64,
    pub dropped: u64,
    pub last_flush_at: Option<i64>,
    pub last_error: Option<String>,
}

/// [`SinkGroupStatusPush::state`]の許容値（設計 §6 実装指示: "running" |
/// "backoff" | "error" | "disabled"）。
pub const ALLOWED_SINK_GROUP_STATES: &[&str] = &["running", "backoff", "error", "disabled"];

pub fn is_valid_sink_group_state(state: &str) -> bool {
    ALLOWED_SINK_GROUP_STATES.contains(&state)
}

#[derive(Default)]
struct Inner {
    groups: HashMap<i64, SinkGroupStatusPush>,
    /// 直近の`PUT /api/sink/status`呼び出し全体の受信時刻（Hub 側の時計） -
    /// サイドカー自体が生きているかどうかの判定
    /// （[`SinkStatusStore::snapshot`]の`sidecarState`）に使う。個々の
    /// グループの`lastFlushAt`（push のペイロード内、サイドカー側の時計）
    /// とは別物 - グループ単位の受信時刻は保持しない（設計 §5.2 が求める
    /// 鮮度判定はサイドカー全体の1本だけ）。
    last_received_at_ms: Option<i64>,
}

/// `GET /api/status`/`GET /api/v1/status`の`sink`節用スナップショット。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SinkStatusSnapshot {
    /// `"online"`（[`SINK_SIDECAR_STALE_AFTER_MS`]以内に push があった）か
    /// `"unknown"`（無い、または一度も push が無い）。
    pub sidecar_state: &'static str,
    pub last_seen_at: Option<i64>,
    pub groups: Vec<SinkGroupStatusPush>,
}

/// サイドカーの運転状態を保持するプロセス内メモリの店（本モジュールの
/// doc comment参照）。`Arc`で包んで REST の複数ルーター（`/api/sink/status`
/// が書き込み、`/api/status`・`/api/v1/status`が読み出し）に配る -
/// `crate::write_control::WriteControl`等、他の共有状態と同じ規律。
pub struct SinkStatusStore {
    inner: Mutex<Inner>,
}

impl SinkStatusStore {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner::default()),
        })
    }

    /// `PUT /api/sink/status`の1呼び出し分を記録する。呼び出し元
    /// （`crate::rest`）が既知の sink group id 集合との突き合わせ
    /// （未知の id を422で拒否）を済ませてから呼ぶ - 渡された全グループの
    /// セットを全量スナップショットとして受け取り、`inner.groups`を置換する
    /// （push に含まれない既存グループは削除される）。
    pub fn record(&self, groups: Vec<SinkGroupStatusPush>, received_at_ms: i64) {
        let mut inner = self.inner.lock().expect("SinkStatusStore mutex poisoned");
        inner.groups = groups.into_iter().map(|g| (g.id, g)).collect();
        inner.last_received_at_ms = Some(received_at_ms);
    }

    /// `now_ms`時点のスナップショット - `now_ms`は呼び出し元
    /// （`crate::rest::compute_status`）が注入可能な`Clock`
    /// （`CollectorManager::clock`）から渡すので、テストは実時計を待たずに
    /// 15秒経過後の`"unknown"`遷移を検証できる。
    pub fn snapshot(&self, now_ms: i64) -> SinkStatusSnapshot {
        let inner = self.inner.lock().expect("SinkStatusStore mutex poisoned");
        let sidecar_state = match inner.last_received_at_ms {
            Some(last) if now_ms.saturating_sub(last) < SINK_SIDECAR_STALE_AFTER_MS => "online",
            _ => "unknown",
        };
        let mut groups: Vec<SinkGroupStatusPush> = inner.groups.values().cloned().collect();
        groups.sort_by_key(|g| g.id);
        SinkStatusSnapshot {
            sidecar_state,
            last_seen_at: inner.last_received_at_ms,
            groups,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push(id: i64) -> SinkGroupStatusPush {
        SinkGroupStatusPush {
            id,
            state: "running".to_string(),
            queued: 0,
            dropped: 0,
            last_flush_at: Some(1_000),
            last_error: None,
        }
    }

    #[test]
    fn snapshot_is_unknown_before_any_push() {
        let store = SinkStatusStore::new();
        let snapshot = store.snapshot(1_000_000);
        assert_eq!(snapshot.sidecar_state, "unknown");
        assert_eq!(snapshot.last_seen_at, None);
        assert!(snapshot.groups.is_empty());
    }

    #[test]
    fn snapshot_is_online_right_after_a_push() {
        let store = SinkStatusStore::new();
        store.record(vec![push(1)], 1_000_000);
        let snapshot = store.snapshot(1_000_000);
        assert_eq!(snapshot.sidecar_state, "online");
        assert_eq!(snapshot.groups.len(), 1);
    }

    #[test]
    fn snapshot_becomes_unknown_after_the_stale_window() {
        let store = SinkStatusStore::new();
        store.record(vec![push(1)], 1_000_000);
        let snapshot = store.snapshot(1_000_000 + SINK_SIDECAR_STALE_AFTER_MS);
        assert_eq!(snapshot.sidecar_state, "unknown");
        // グループ自体の最後の push 内容は消えない - 単に鮮度の判定が
        // 変わるだけ。
        assert_eq!(snapshot.groups.len(), 1);
    }

    #[test]
    fn record_overwrites_the_same_group_id() {
        let store = SinkStatusStore::new();
        store.record(vec![push(1)], 1_000);
        let mut updated = push(1);
        updated.queued = 42;
        store.record(vec![updated], 2_000);
        let snapshot = store.snapshot(2_000);
        assert_eq!(snapshot.groups.len(), 1);
        assert_eq!(snapshot.groups[0].queued, 42);
    }

    #[test]
    fn record_replaces_all_groups_not_in_the_new_push() {
        let store = SinkStatusStore::new();
        store.record(vec![push(1), push(2)], 1_000);
        assert_eq!(store.snapshot(1_000).groups.len(), 2);

        // push に group 1 だけが含まれる場合、group 2 は削除される
        store.record(vec![push(1)], 2_000);
        let snapshot = store.snapshot(2_000);
        assert_eq!(snapshot.groups.len(), 1);
        assert_eq!(snapshot.groups[0].id, 1);
    }

    #[test]
    fn is_valid_sink_group_state_accepts_the_four_allowed_values() {
        for state in ALLOWED_SINK_GROUP_STATES {
            assert!(is_valid_sink_group_state(state));
        }
        assert!(!is_valid_sink_group_state("stopped"));
    }
}
