//! 設定を変える操作の本体（#1161 PR5a、docs/chronogazer/chronogazer-agent-api-design.md
//! §5.4「処理を一つにする」）。
//!
//! REST のハンドラ（`crate::rest`）と MCP のツール（#1161 PR5b）は、どちらも
//! ここの関数を呼ぶ。1 つの操作は次の順で、関数の中で全部行う。
//!
//! 1. 権限の確かめ（[`Actor::can_configure`]、収集の操作は
//!    [`Actor::can_operate_collect`]）。足りなければ `denied` を監査して
//!    `BantoError::Forbidden`。
//! 2. 前検査（接続のプロトコルの制限、タグのしきい値の拒否など）。
//! 3. サービスの呼び出し。
//! 4. 成功の監査（[`Actor::record`]）。
//! 5. 接続・収集グループ・記録項目を変えたら `HubService::registry_changed`。
//!
//! これで「REST だけ前検査がある」「MCP だけ `registry_changed` を忘れる」と
//! いったずれが起きない。順序・監査の値（action・resource・entity_id・
//! detail・result）は PR5a より前の REST のハンドラと同じで、違うのは呼び出し元が
//! 渡す [`Via`] から来る `origin` と、`denied` の `detail` だけ。
//!
//! ## 呼び出し元（[`Via`]）
//!
//! - REST: `Via::Rest { method, path }`。`origin: "rest"`、`denied` の
//!   `detail` は `{method, path}`（`path` はルートの型、例
//!   `/api/plc-connections/{id}`。banto の `require_role_at_least` と同じ形）。
//! - MCP: `Via::Mcp { tool }`。`origin: "mcp"`、`denied` の `detail` は
//!   `{tool}`（ツール名、例 `create_connection`）。MCP には method・path が
//!   無く、ツール名が「何をしようとして断られたか」を一意に表すため。
//!
//! 収集の操作（start/stop/restart）は、REST ではルーターの層
//! （`require_actor_at_least`）が先に床を確かめているので、関数の中の確かめは
//! REST では必ず通る（二重に `denied` を残すことはない）。MCP には層が無いので、
//! 関数の中の確かめが床になる。
//!
//! ## 読み取り
//!
//! 読み取りは前検査も監査も無いので、MCP のツールも各サービス（下の構造体の
//! フィールド）を直接呼べばよい。ただし接続先は秘密の参照を外して返す必要が
//! あるので、[`RegistryOps::list_connections`]・[`RegistryOps::get_connection`]
//! を通す（REST の `GET /api/plc-connections*` も同じ関数を呼ぶ）。読み取りの
//! 権限（`read`・`config`）は呼び出し元が確かめる（REST はルーターの層、MCP は
//! ツールのスコープ）。
//!
//! ## デスクトップ版
//!
//! デスクトップ版（`src-tauri`）のコマンドは、今は同じ処理を自前で並べている
//! （主体の型が違い、`origin: "tauri"`）。REST と挙動を揃える約束
//! （`crate::rest` の doc）は変わらない。

use std::sync::Arc;

use banto_core::{BantoError, ListParams};
use banto_tags::{CollectionGroup, CollectionGroupService, PlcConnectionService, Tag, TagService};
use serde_json::{json, Value};

use crate::audit::AuditLogService;
use crate::collect::{CollectOutcome, CollectorService, COLLECT_AUDIT_RESOURCE};
use crate::display_groups::{
    audit_detail as display_group_audit_detail, DisplayGroup, DisplayGroupPayload,
    DisplayGroupSaveResponse, DisplayGroupSaved, DisplayGroupService,
    AUDIT_RESOURCE as DISPLAY_GROUPS_RESOURCE,
};
use crate::free_trends::{
    audit_detail as free_trend_audit_detail, FreeTrend, FreeTrendPayload, FreeTrendService,
    AUDIT_RESOURCE as FREE_TRENDS_RESOURCE,
};
use crate::hub::HubService;
use crate::hub_tags::{registration_audit_detail, HubTagRegistrar};
use crate::rest::actor::Actor;
use crate::rest::{
    plc_connection_audit_detail, reject_disallowed_connection_protocol, update_plc_connection,
    CollectionGroupPayload, PlcConnectionPayload, PlcConnectionResponse, TagPayload,
};
use crate::tag_address::update_tag_checked;
use crate::tag_thresholds::{
    audit_detail as threshold_audit_detail, TagThresholdService, TagThresholds,
    TagThresholdsPayload, AUDIT_RESOURCE as TAG_THRESHOLDS_RESOURCE,
};

/// REST の `origin`。
pub(crate) const ORIGIN_REST: &str = "rest";
/// MCP の `origin`（HUB の MCP と同じ値）。
pub(crate) const ORIGIN_MCP: &str = "mcp";

/// 操作の呼び出し元（モジュール doc「呼び出し元」）。監査の `origin` と、
/// `denied` の `detail` を決める。
#[derive(Debug, Clone, Copy)]
pub(crate) enum Via<'a> {
    /// REST。`path` はルートの型（`/api/tags/{id}` など）か、要求の path。
    Rest { method: &'a str, path: &'a str },
    /// MCP（#1161 PR5b、`crate::mcp`）。`tool` はツール名。
    Mcp { tool: &'a str },
}

impl Via<'_> {
    /// 監査の `origin`。
    pub(crate) fn origin(&self) -> &'static str {
        match self {
            Self::Rest { .. } => ORIGIN_REST,
            Self::Mcp { .. } => ORIGIN_MCP,
        }
    }

    /// `denied` の `detail`: REST は `{method, path}`、MCP は `{tool}`。
    pub(crate) fn denied_detail(&self) -> Value {
        match self {
            Self::Rest { method, path } => json!({ "method": method, "path": path }),
            Self::Mcp { tool } => json!({ "tool": tool }),
        }
    }
}

/// 設定を変えられるか（editor 以上 / `config`）を確かめる。足りなければ
/// `denied` を監査して `BantoError::Forbidden`。
pub(crate) async fn require_configure(
    actor: &Actor,
    audit: &AuditLogService,
    via: &Via<'_>,
    resource: &str,
) -> Result<(), BantoError> {
    if actor.can_configure() {
        return Ok(());
    }
    actor.record_denied(audit, via, resource).await;
    Err(BantoError::Forbidden)
}

/// 収集を操作できるか（`COLLECT_OPERATION_ROLE` 以上 / `config`）を確かめる。
async fn require_operate_collect(
    actor: &Actor,
    audit: &AuditLogService,
    via: &Via<'_>,
) -> Result<(), BantoError> {
    if actor.can_operate_collect() {
        return Ok(());
    }
    actor
        .record_denied(audit, via, COLLECT_AUDIT_RESOURCE)
        .await;
    Err(BantoError::Forbidden)
}

const PLC_CONNECTIONS_RESOURCE: &str = "plc_connections";
const COLLECTION_GROUPS_RESOURCE: &str = "collection_groups";
const TAGS_RESOURCE: &str = "tags";

/// 接続先・収集グループ・記録項目の操作（REST の `/api/plc-connections/*`・
/// `/api/collection-groups/*`・`/api/tags/*`）。
#[derive(Clone)]
pub(crate) struct RegistryOps {
    pub(crate) plc_connections: PlcConnectionService,
    pub(crate) collection_groups: CollectionGroupService,
    pub(crate) tags: TagService,
    /// #393: タグの削除の前に、表示グループのペンからの参照を確かめる。
    pub(crate) display_groups: DisplayGroupService,
    /// #383 段階3 P4: 書き込みの後に `HubService::registry_changed` を呼ぶ
    /// （登録済みの hub タグの集合が変わったら購読に反映する）。
    pub(crate) hub: HubService,
    pub(crate) audit: AuditLogService,
    /// R2b-3a-3: 接続のプロトコルの制限と `/api/config-exclusions` の判定。
    pub(crate) drivers: Arc<banto_driver_api::DriverRegistry>,
}

impl RegistryOps {
    /// 接続先の一覧（秘密の参照は返さない）。
    pub(crate) async fn list_connections(&self) -> Result<Vec<PlcConnectionResponse>, BantoError> {
        Ok(self
            .plc_connections
            .list(ListParams::default())
            .await?
            .rows
            .into_iter()
            .map(PlcConnectionResponse::from)
            .collect())
    }

    /// 接続先を 1 件（秘密の参照は返さない）。
    pub(crate) async fn get_connection(
        &self,
        id: i64,
    ) -> Result<PlcConnectionResponse, BantoError> {
        Ok(PlcConnectionResponse::from(
            self.plc_connections.get(id).await?,
        ))
    }

    /// 接続先の作成（REST `POST /api/plc-connections`、MCP `create_connection`）。
    pub(crate) async fn create_connection(
        &self,
        actor: &Actor,
        via: &Via<'_>,
        input: PlcConnectionPayload,
    ) -> Result<PlcConnectionResponse, BantoError> {
        require_configure(actor, &self.audit, via, PLC_CONNECTIONS_RESOURCE).await?;
        reject_disallowed_connection_protocol(&self.drivers, &input.protocol)?;
        let created = self
            .plc_connections
            .create_settings(input.into_create_input())
            .await?;
        actor
            .record(
                &self.audit,
                via.origin(),
                "create",
                PLC_CONNECTIONS_RESOURCE,
                Some(&created.id.to_string()),
                Some(plc_connection_audit_detail(&created)),
            )
            .await;
        self.hub.registry_changed().await;
        Ok(PlcConnectionResponse::from(created))
    }

    /// 接続先の更新（REST `PUT /api/plc-connections/{id}`、MCP `update_connection`）。
    pub(crate) async fn update_connection(
        &self,
        actor: &Actor,
        via: &Via<'_>,
        id: i64,
        input: PlcConnectionPayload,
    ) -> Result<PlcConnectionResponse, BantoError> {
        require_configure(actor, &self.audit, via, PLC_CONNECTIONS_RESOURCE).await?;
        reject_disallowed_connection_protocol(&self.drivers, &input.protocol)?;
        let updated = update_plc_connection(&self.plc_connections, id, input).await?;
        actor
            .record(
                &self.audit,
                via.origin(),
                "update",
                PLC_CONNECTIONS_RESOURCE,
                Some(&id.to_string()),
                Some(plc_connection_audit_detail(&updated)),
            )
            .await;
        self.hub.registry_changed().await;
        Ok(PlcConnectionResponse::from(updated))
    }

    /// 接続先の削除（REST `DELETE /api/plc-connections/{id}`、MCP `delete_connection`）。
    pub(crate) async fn delete_connection(
        &self,
        actor: &Actor,
        via: &Via<'_>,
        id: i64,
    ) -> Result<(), BantoError> {
        require_configure(actor, &self.audit, via, PLC_CONNECTIONS_RESOURCE).await?;
        self.plc_connections.delete(id).await?;
        actor
            .record(
                &self.audit,
                via.origin(),
                "delete",
                PLC_CONNECTIONS_RESOURCE,
                Some(&id.to_string()),
                None,
            )
            .await;
        self.hub.registry_changed().await;
        Ok(())
    }

    /// 収集グループの作成（REST `POST /api/collection-groups`、MCP `create_group`）。
    pub(crate) async fn create_group(
        &self,
        actor: &Actor,
        via: &Via<'_>,
        input: CollectionGroupPayload,
    ) -> Result<CollectionGroup, BantoError> {
        require_configure(actor, &self.audit, via, COLLECTION_GROUPS_RESOURCE).await?;
        let created = self.collection_groups.create(input.into()).await?;
        actor
            .record(
                &self.audit,
                via.origin(),
                "create",
                COLLECTION_GROUPS_RESOURCE,
                Some(&created.id.to_string()),
                Some(json!({ "name": created.name, "enabled": created.enabled })),
            )
            .await;
        self.hub.registry_changed().await;
        Ok(created)
    }

    /// 収集グループの更新（REST `PUT /api/collection-groups/{id}`、MCP `update_group`）。
    pub(crate) async fn update_group(
        &self,
        actor: &Actor,
        via: &Via<'_>,
        id: i64,
        input: CollectionGroupPayload,
    ) -> Result<CollectionGroup, BantoError> {
        require_configure(actor, &self.audit, via, COLLECTION_GROUPS_RESOURCE).await?;
        let updated = self.collection_groups.update(id, input.into()).await?;
        actor
            .record(
                &self.audit,
                via.origin(),
                "update",
                COLLECTION_GROUPS_RESOURCE,
                Some(&id.to_string()),
                Some(json!({ "name": updated.name, "enabled": updated.enabled })),
            )
            .await;
        self.hub.registry_changed().await;
        Ok(updated)
    }

    /// 収集グループの削除（REST `DELETE /api/collection-groups/{id}`、MCP `delete_group`）。
    pub(crate) async fn delete_group(
        &self,
        actor: &Actor,
        via: &Via<'_>,
        id: i64,
    ) -> Result<(), BantoError> {
        require_configure(actor, &self.audit, via, COLLECTION_GROUPS_RESOURCE).await?;
        self.collection_groups.delete(id).await?;
        actor
            .record(
                &self.audit,
                via.origin(),
                "delete",
                COLLECTION_GROUPS_RESOURCE,
                Some(&id.to_string()),
                None,
            )
            .await;
        self.hub.registry_changed().await;
        Ok(())
    }

    /// 記録項目の作成（REST `POST /api/tags`、MCP `create_tag`）。しきい値を
    /// 値付きで送ってきたら断る（#532、`TagPayload::reject_thresholds`）。
    pub(crate) async fn create_tag(
        &self,
        actor: &Actor,
        via: &Via<'_>,
        input: TagPayload,
    ) -> Result<Tag, BantoError> {
        require_configure(actor, &self.audit, via, TAGS_RESOURCE).await?;
        input.reject_thresholds()?;
        let created = self.tags.create(input.into()).await?;
        actor
            .record(
                &self.audit,
                via.origin(),
                "create",
                TAGS_RESOURCE,
                Some(&created.id.to_string()),
                Some(json!({ "name": created.name, "enabled": created.enabled })),
            )
            .await;
        self.hub.registry_changed().await;
        Ok(created)
    }

    /// 記録項目の更新（REST `PUT /api/tags/{id}`、MCP `update_tag`）。版の
    /// 食い違いは `crate::revision::is_revision_conflict` で見分けられる
    /// `BantoError`（REST は `409`）。
    pub(crate) async fn update_tag(
        &self,
        actor: &Actor,
        via: &Via<'_>,
        id: i64,
        input: TagPayload,
    ) -> Result<Tag, BantoError> {
        require_configure(actor, &self.audit, via, TAGS_RESOURCE).await?;
        input.reject_thresholds()?;
        let updated = update_tag_checked(&self.tags, id, input.into()).await?;
        actor
            .record(
                &self.audit,
                via.origin(),
                "update",
                TAGS_RESOURCE,
                Some(&id.to_string()),
                Some(json!({ "name": updated.name, "enabled": updated.enabled })),
            )
            .await;
        self.hub.registry_changed().await;
        Ok(updated)
    }

    /// 記録項目の削除（REST `DELETE /api/tags/{id}`、MCP `delete_tag`）。
    /// 表示グループのペンに割り当てられたタグは、参照元を並べて断る（#393）。
    pub(crate) async fn delete_tag(
        &self,
        actor: &Actor,
        via: &Via<'_>,
        id: i64,
    ) -> Result<(), BantoError> {
        require_configure(actor, &self.audit, via, TAGS_RESOURCE).await?;
        self.display_groups
            .delete_tag_unless_referenced(&self.tags, id)
            .await?;
        actor
            .record(
                &self.audit,
                via.origin(),
                "delete",
                TAGS_RESOURCE,
                Some(&id.to_string()),
                None,
            )
            .await;
        self.hub.registry_changed().await;
        Ok(())
    }
}

/// 表示グループの操作（REST `/api/display-groups/*`）。表示グループ自体は収集の
/// 対象ではないので `registry_changed` は呼ばない（PR5a より前の REST と同じ）。
/// ただし作成・更新でペンの Hub のタグを記録項目として登録したとき（#1179 PR5、
/// `crate::display_groups` のモジュール doc）は、`POST /api/tags/from-hub` と同じく
/// 呼ぶ。
#[derive(Clone)]
pub(crate) struct DisplayGroupOps {
    pub(crate) display_groups: DisplayGroupService,
    /// #1179 PR5: ペンの Hub のタグを同じトランザクションで登録するサービス一式。
    pub(crate) registrar: HubTagRegistrar,
    /// #1179 PR5: 登録したら購読に反映し、移行前の選択の消去を予約する。
    pub(crate) hub: HubService,
    pub(crate) audit: AuditLogService,
}

impl DisplayGroupOps {
    /// 作成（REST `POST /api/display-groups`、MCP `create_display_group`）。
    pub(crate) async fn create(
        &self,
        actor: &Actor,
        via: &Via<'_>,
        payload: &DisplayGroupPayload,
    ) -> Result<DisplayGroupSaveResponse, BantoError> {
        require_configure(actor, &self.audit, via, DISPLAY_GROUPS_RESOURCE).await?;
        let saved = self
            .display_groups
            .create_with_hub_tags(&self.registrar, payload)
            .await?;
        self.after_save(actor, via, "create", &saved).await;
        Ok(saved.into_response())
    }

    /// 更新（REST `PUT /api/display-groups/{id}`、MCP `update_display_group`）。
    pub(crate) async fn update(
        &self,
        actor: &Actor,
        via: &Via<'_>,
        id: i64,
        payload: &DisplayGroupPayload,
    ) -> Result<DisplayGroupSaveResponse, BantoError> {
        require_configure(actor, &self.audit, via, DISPLAY_GROUPS_RESOURCE).await?;
        let saved = self
            .display_groups
            .update_with_hub_tags(&self.registrar, id, payload)
            .await?;
        self.after_save(actor, via, "update", &saved).await;
        Ok(saved.into_response())
    }

    /// 作成・更新の後処理（Tauri の `display_groups_save_after` と同じ順）: 登録が
    /// あれば先に記録項目の `create` の監査（`POST /api/tags/from-hub` と同じ
    /// `detail`）→ 表示グループの監査 → 登録があれば `registry_changed` と移行前の
    /// 選択の消去の予約（待たない）。
    async fn after_save(
        &self,
        actor: &Actor,
        via: &Via<'_>,
        action: &str,
        saved: &DisplayGroupSaved,
    ) {
        if let Some(registration) = &saved.registration {
            actor
                .record(
                    &self.audit,
                    via.origin(),
                    "create",
                    TAGS_RESOURCE,
                    None,
                    Some(registration_audit_detail(registration)),
                )
                .await;
        }
        actor
            .record(
                &self.audit,
                via.origin(),
                action,
                DISPLAY_GROUPS_RESOURCE,
                Some(&saved.group.id.to_string()),
                Some(display_group_audit_detail(&saved.group)),
            )
            .await;
        if saved.registration.is_some() {
            self.hub.registry_changed().await;
            self.hub.schedule_legacy_selection_clear();
        }
    }

    /// 削除（REST `DELETE /api/display-groups/{id}`、MCP `delete_display_group`）。
    pub(crate) async fn delete(
        &self,
        actor: &Actor,
        via: &Via<'_>,
        id: i64,
    ) -> Result<(), BantoError> {
        require_configure(actor, &self.audit, via, DISPLAY_GROUPS_RESOURCE).await?;
        let deleted = self.display_groups.delete(id).await?;
        actor
            .record(
                &self.audit,
                via.origin(),
                "delete",
                DISPLAY_GROUPS_RESOURCE,
                Some(&id.to_string()),
                Some(json!({ "name": deleted.name })),
            )
            .await;
        Ok(())
    }

    /// 並べ替え（REST `PUT /api/display-groups/order`、MCP `reorder_display_groups`）。
    pub(crate) async fn reorder(
        &self,
        actor: &Actor,
        via: &Via<'_>,
        ids: &[i64],
    ) -> Result<Vec<DisplayGroup>, BantoError> {
        require_configure(actor, &self.audit, via, DISPLAY_GROUPS_RESOURCE).await?;
        let groups = self.display_groups.reorder(ids).await?;
        actor
            .record(
                &self.audit,
                via.origin(),
                "reorder",
                DISPLAY_GROUPS_RESOURCE,
                None,
                Some(json!({ "ids": ids })),
            )
            .await;
        Ok(groups)
    }
}

/// しきい値の操作（REST `/api/tag-thresholds/*`）。
#[derive(Clone)]
pub(crate) struct ThresholdOps {
    pub(crate) tag_thresholds: TagThresholdService,
    pub(crate) audit: AuditLogService,
}

impl ThresholdOps {
    /// 全項目の置き換え（REST `PUT /api/tag-thresholds/{tagId}`、MCP `set_threshold`）。
    pub(crate) async fn update(
        &self,
        actor: &Actor,
        via: &Via<'_>,
        tag_id: i64,
        payload: &TagThresholdsPayload,
    ) -> Result<TagThresholds, BantoError> {
        require_configure(actor, &self.audit, via, TAG_THRESHOLDS_RESOURCE).await?;
        let updated = self.tag_thresholds.update(tag_id, payload).await?;
        actor
            .record(
                &self.audit,
                via.origin(),
                "update",
                TAG_THRESHOLDS_RESOURCE,
                Some(&tag_id.to_string()),
                Some(threshold_audit_detail(&updated)),
            )
            .await;
        Ok(updated.thresholds)
    }
}

/// 収集の操作（REST `/api/collect/{start,stop,restart}`）。
#[derive(Clone)]
pub(crate) struct CollectOps {
    pub(crate) collect: CollectorService,
    pub(crate) audit: AuditLogService,
}

/// 収集の操作の種類。監査の `action` になる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CollectOperation {
    Start,
    Stop,
    Restart,
}

impl CollectOperation {
    fn action(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Restart => "restart",
        }
    }
}

impl CollectOps {
    /// 収集の開始・停止・再起動（REST `POST /api/collect/{start,stop,restart}`、
    /// MCP `start_collection`・`stop_collection`・`restart_collection`）。
    ///
    /// 監査の `resource` は `COLLECT_AUDIT_RESOURCE`（Tauri の `collect_*` と同じ
    /// 定数）、`detail` は**結果の状態**だけ（`collectState`・`pending`）。接続先・
    /// ファイルパス・起動失敗の理由といった内部の詳細は入れない
    /// （`CollectorState::as_str` の doc）。打ち切り（`pending`）は失敗ではない
    /// ので `result` は `"ok"`。
    pub(crate) async fn run(
        &self,
        actor: &Actor,
        via: &Via<'_>,
        operation: CollectOperation,
    ) -> Result<CollectOutcome, BantoError> {
        require_operate_collect(actor, &self.audit, via).await?;
        let outcome = match operation {
            CollectOperation::Start => self.collect.start().await?,
            CollectOperation::Stop => self.collect.stop().await?,
            CollectOperation::Restart => self.collect.restart().await?,
        };
        actor
            .record(
                &self.audit,
                via.origin(),
                operation.action(),
                COLLECT_AUDIT_RESOURCE,
                None,
                Some(
                    json!({ "collectState": outcome.status.as_str(), "pending": outcome.pending }),
                ),
            )
            .await;
        Ok(outcome)
    }
}

/// 自由トレンドの操作（REST `/api/free-trends/*`、#1207・#1231 PR 8a）。権限は表示グループと
/// 違い、作成は viewer 以上のアカウントと `localDesktop`、変更・削除は作った人か editor 以上で、
/// ログイン不要モード・閲覧公開・API キーは書けない（`crate::free_trends` のモジュール doc
/// 「権限」）。断ったときは `denied` を監査して `Forbidden`。MCP のツールは無い（§9 Q7）。
#[derive(Clone)]
pub(crate) struct FreeTrendOps {
    pub(crate) free_trends: FreeTrendService,
    pub(crate) audit: AuditLogService,
}

impl FreeTrendOps {
    /// サービスの結果が `Forbidden` なら `denied` を監査する（作った人の確認はサービスの
    /// トランザクションの中で行うので、拒否はその結果で知る）。
    async fn denied_on_forbidden<T>(
        &self,
        actor: &Actor,
        via: &Via<'_>,
        result: Result<T, BantoError>,
    ) -> Result<T, BantoError> {
        if matches!(result, Err(BantoError::Forbidden)) {
            actor
                .record_denied(&self.audit, via, FREE_TRENDS_RESOURCE)
                .await;
        }
        result
    }

    /// 作成（REST `POST /api/free-trends`）。
    pub(crate) async fn create(
        &self,
        actor: &Actor,
        via: &Via<'_>,
        payload: &FreeTrendPayload,
    ) -> Result<FreeTrend, BantoError> {
        let principal = actor.free_trend_actor();
        let result = self.free_trends.create(&principal, payload).await;
        let created = self.denied_on_forbidden(actor, via, result).await?;
        actor
            .record(
                &self.audit,
                via.origin(),
                "create",
                FREE_TRENDS_RESOURCE,
                Some(&created.id.to_string()),
                Some(free_trend_audit_detail(&created)),
            )
            .await;
        Ok(created)
    }

    /// 更新（REST `PUT /api/free-trends/{id}`）。
    pub(crate) async fn update(
        &self,
        actor: &Actor,
        via: &Via<'_>,
        id: i64,
        payload: &FreeTrendPayload,
    ) -> Result<FreeTrend, BantoError> {
        let principal = actor.free_trend_actor();
        let result = self.free_trends.update(&principal, id, payload).await;
        let updated = self.denied_on_forbidden(actor, via, result).await?;
        actor
            .record(
                &self.audit,
                via.origin(),
                "update",
                FREE_TRENDS_RESOURCE,
                Some(&id.to_string()),
                Some(free_trend_audit_detail(&updated)),
            )
            .await;
        Ok(updated)
    }

    /// 削除（REST `DELETE /api/free-trends/{id}`）。
    pub(crate) async fn delete(
        &self,
        actor: &Actor,
        via: &Via<'_>,
        id: i64,
    ) -> Result<(), BantoError> {
        let principal = actor.free_trend_actor();
        let result = self.free_trends.delete(&principal, id).await;
        let deleted = self.denied_on_forbidden(actor, via, result).await?;
        actor
            .record(
                &self.audit,
                via.origin(),
                "delete",
                FREE_TRENDS_RESOURCE,
                Some(&id.to_string()),
                Some(json!({ "name": deleted.name })),
            )
            .await;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
