//! #414 段階1: タグのアドレス・データ型が、所属する接続のプロトコルで
//! 読める形かを**保存時に**確かめる。
//!
//! ## なぜ要るか
//!
//! `banto-tags` はタグの `address` を非空しか検証しない（アドレス書式は
//! 収集側の関心、`banto_collect::config` の `build_request` の doc 参照）。
//! そのため Modbus 接続の下に MELSEC 表記の `D3000` を保存できてしまい、
//! 収集を開始した時点で `banto_collect::build_config` が
//! `CollectError::Config` で失敗して、**正常な接続も含めて全体が起動しない**。
//! 保存時に拒否すれば、その場で直せる（どのフィールドが悪いかも分かる）。
//!
//! ## 判定は収集側と同じ解釈器
//!
//! 判定そのものは [`banto_collect::check_tag_address`] に任せ、ここには
//! 解釈ロジックを一切持たない。あの関数は `build_config_from` が使う
//! 解釈器（`parse_read_request`）をそのまま呼ぶので、「保存で通るもの」と
//! 「収集で通るもの」がずれない。このモジュールがやるのは
//! (1) タグ → 収集グループ → 接続を辿ってプロトコルを引くことと、
//! (2) 結果を `BantoError::Validation` の人間可読なフィールドエラーへ
//! 載せ替えること（`field` はワイヤ名: `address`/`dataType`/`protocol`/
//! `plcConnectionId`）だけ。
//!
//! ## 呼ぶ経路（REST と Tauri の両方）
//!
//! - タグの作成・更新: [`ensure_tag_fits_its_connection`]。更新で収集
//!   グループを変える（別プロトコルの接続の下へ移る）場合も、入力の
//!   `collection_group_id` から辿るので同じ関数で拾える。
//! - 接続のプロトコル変更: [`ensure_protocol_change_keeps_tags_readable`]。
//!   配下に新プロトコルで読めないタグがあれば、どのタグかを列挙して拒否する。
//!   プロトコルが変わらない更新（名前・ホストの変更など）では調べない -
//!   段階2 が扱う既存の不正データのせいで、無関係な編集まで保存できなく
//!   なるのを避けるため。
//! - 収集グループの接続変更（グループごと別プロトコルの接続へ移す）:
//!   [`ensure_group_move_keeps_tags_readable`]。接続が変わり、かつ新旧の
//!   プロトコルが違うときだけ、グループ配下の全タグを調べて
//!   `plcConnectionId` で拒否する（文言の形はプロトコル変更と同じ）。
//!
//! いずれも `chronogazer_core::rest` の各ハンドラと、
//! `apps/chronogazer/src-tauri/src/lib.rs` の同名コマンドの**両方**から呼ぶ
//! （片方だけに書くと、もう片方の経路から通ってしまう）。
//!
//! ## 制約（この段階ではやらないこと）
//!
//! - 調べてから書くまでは同じトランザクションではない（検査と書き込みの
//!   間に別の要求が割り込む余地がある）。chronogazer の編集は少人数・
//!   手動が前提で、取りこぼしても段階2（開始時に外して残りを動かす）が
//!   受け止める。
//! - 既に DB にある不正なタグ（この変更より前に保存されたもの）には
//!   触らない。それは段階2 の範囲。

use banto_collect::{check_tag_address, TagAddressField, TagAddressIssue};
use banto_core::{BantoError, FieldError, ListParams};
use banto_tags::{CollectionGroupService, PlcConnectionService, Tag, TagService};

/// プロトコル変更の拒否メッセージに名前を並べるタグの上限。これを超えた分は
/// 「ほか N 件」にまとめる（1 件ずつ全部並べるとトーストが読めなくなる）。
const LISTED_TAGS_LIMIT: usize = 5;

fn wire_field(field: TagAddressField) -> &'static str {
    match field {
        TagAddressField::Address => "address",
        TagAddressField::DataType => "dataType",
    }
}

/// [`TagAddressIssue`] を、タグのフォームにそのまま出せる検証エラーへ。
pub fn issue_to_validation_error(issue: &TagAddressIssue) -> BantoError {
    BantoError::Validation {
        field_errors: vec![FieldError {
            field: wire_field(issue.field()).to_string(),
            message: issue.message(),
        }],
    }
}

/// 純関数: `tags` のうち、`protocol` の接続の下では読めないものとその理由。
/// 入力の順序を保つ。
pub fn unreadable_tags<'a>(protocol: &str, tags: &[&'a Tag]) -> Vec<(&'a Tag, TagAddressIssue)> {
    tags.iter()
        .filter_map(|tag| {
            check_tag_address(protocol, &tag.address, &tag.data_type)
                .err()
                .map(|issue| (*tag, issue))
        })
        .collect()
}

/// 読めなくなるタグの一覧（上限 [`LISTED_TAGS_LIMIT`] 件＋「ほか N 件」）を
/// 「{lead}タグ N 件が読めなくなります: …。先にタグのアドレスを直して
/// ください」の形で `field` に載せる。プロトコル変更とグループの接続変更で
/// 文言の形を揃えるための共通部分。`conflicts` が空なら `None`。
fn unreadable_tags_error(
    field: &str,
    lead: &str,
    conflicts: &[(&Tag, TagAddressIssue)],
) -> Option<BantoError> {
    if conflicts.is_empty() {
        return None;
    }
    let mut listed: Vec<String> = conflicts
        .iter()
        .take(LISTED_TAGS_LIMIT)
        .map(|(tag, _)| format!("「{}」（{}）", tag.name, tag.address))
        .collect();
    if conflicts.len() > LISTED_TAGS_LIMIT {
        listed.push(format!("ほか {} 件", conflicts.len() - LISTED_TAGS_LIMIT));
    }
    Some(BantoError::Validation {
        field_errors: vec![FieldError {
            field: field.to_string(),
            message: format!(
                "{lead}タグ {} 件が読めなくなります: {}。先にタグのアドレスを直してください",
                conflicts.len(),
                listed.join("、")
            ),
        }],
    })
}

/// 純関数: プロトコル変更を拒否する検証エラー（`field: "protocol"`）。
/// `conflicts` が空なら `None`（拒否しない）。
pub fn protocol_change_error(
    new_protocol: &str,
    conflicts: &[(&Tag, TagAddressIssue)],
) -> Option<BantoError> {
    unreadable_tags_error(
        "protocol",
        &format!("プロトコルを {new_protocol} に変更すると、この接続の下の"),
        conflicts,
    )
}

/// 純関数: 収集グループの接続変更を拒否する検証エラー
/// （`field: "plcConnectionId"`）。文言の形は [`protocol_change_error`] と同じ。
/// `conflicts` が空なら `None`（拒否しない）。
pub fn group_move_error(
    new_connection_name: &str,
    new_protocol: &str,
    conflicts: &[(&Tag, TagAddressIssue)],
) -> Option<BantoError> {
    unreadable_tags_error(
        "plcConnectionId",
        &format!("PLC接続を {new_connection_name}（{new_protocol}）に変更すると、このグループの"),
        conflicts,
    )
}

/// タグの作成・更新の前に呼ぶ: 入力のアドレス・データ型が、
/// `collection_group_id` の収集グループが属する接続のプロトコルで読めるか。
///
/// 収集グループや接続が見つからない場合は `Ok(())` を返し、判定を
/// `TagService` 自身の検証（存在しないグループへの参照を人間可読に拒否
/// する）に任せる - ここで `NotFound` を返すと、今までの応答の形が変わる。
pub async fn ensure_tag_fits_its_connection(
    connections: &PlcConnectionService,
    groups: &CollectionGroupService,
    collection_group_id: i64,
    address: &str,
    data_type: &str,
) -> Result<(), BantoError> {
    let group = match groups.get(collection_group_id).await {
        Ok(group) => group,
        Err(BantoError::NotFound { .. }) => return Ok(()),
        Err(err) => return Err(err),
    };
    let connection = match connections.get(group.plc_connection_id).await {
        Ok(connection) => connection,
        Err(BantoError::NotFound { .. }) => return Ok(()),
        Err(err) => return Err(err),
    };
    check_tag_address(&connection.protocol, address, data_type)
        .map_err(|issue| issue_to_validation_error(&issue))
}

/// 接続の更新の前に呼ぶ: プロトコルが変わるなら、この接続の下の全タグ
/// （有効・無効を問わない - 無効なタグも有効にした瞬間に収集を止める）が
/// 新しいプロトコルで読めるか。プロトコルが変わらないなら何も調べない。
///
/// 接続が見つからない場合は `Ok(())`（`PlcConnectionService::update` 自身が
/// `NotFound` を返す）。
pub async fn ensure_protocol_change_keeps_tags_readable(
    connections: &PlcConnectionService,
    groups: &CollectionGroupService,
    tags: &TagService,
    connection_id: i64,
    new_protocol: &str,
) -> Result<(), BantoError> {
    let current = match connections.get(connection_id).await {
        Ok(connection) => connection,
        Err(BantoError::NotFound { .. }) => return Ok(()),
        Err(err) => return Err(err),
    };
    if current.protocol == new_protocol {
        return Ok(());
    }
    let group_ids: Vec<i64> = groups
        .list(ListParams::default())
        .await?
        .rows
        .into_iter()
        .filter(|group| group.plc_connection_id == connection_id)
        .map(|group| group.id)
        .collect();
    if group_ids.is_empty() {
        return Ok(());
    }
    let mut all_tags = tags.list(ListParams::default()).await?.rows;
    all_tags.sort_by_key(|tag| tag.id);
    let under_connection: Vec<&Tag> = all_tags
        .iter()
        .filter(|tag| group_ids.contains(&tag.collection_group_id))
        .collect();
    let conflicts = unreadable_tags(new_protocol, &under_connection);
    match protocol_change_error(new_protocol, &conflicts) {
        Some(err) => Err(err),
        None => Ok(()),
    }
}

/// 収集グループの更新の前に呼ぶ: `plc_connection_id` が変わり、**新しい
/// 接続のプロトコルが古い接続と違う**ときだけ、グループ配下の全タグ
/// （有効・無効を問わない）が新しいプロトコルで読めるか。接続が変わらない、
/// またはプロトコルが同じなら何も調べない（既存の不正データで無関係な
/// 編集を止めないため - [`ensure_protocol_change_keeps_tags_readable`] と
/// 同じ理由）。
///
/// グループ・古い接続・新しい接続のどれかが見つからない場合は `Ok(())` で、
/// `CollectionGroupService::update` 自身の応答（404 / 「指定されたPLC接続が
/// 見つかりません」）に任せる。
pub async fn ensure_group_move_keeps_tags_readable(
    connections: &PlcConnectionService,
    groups: &CollectionGroupService,
    tags: &TagService,
    group_id: i64,
    new_connection_id: i64,
) -> Result<(), BantoError> {
    let group = match groups.get(group_id).await {
        Ok(group) => group,
        Err(BantoError::NotFound { .. }) => return Ok(()),
        Err(err) => return Err(err),
    };
    if group.plc_connection_id == new_connection_id {
        return Ok(());
    }
    let old = match connections.get(group.plc_connection_id).await {
        Ok(connection) => connection,
        Err(BantoError::NotFound { .. }) => return Ok(()),
        Err(err) => return Err(err),
    };
    let new = match connections.get(new_connection_id).await {
        Ok(connection) => connection,
        Err(BantoError::NotFound { .. }) => return Ok(()),
        Err(err) => return Err(err),
    };
    if old.protocol == new.protocol {
        return Ok(());
    }
    let mut all_tags = tags.list(ListParams::default()).await?.rows;
    all_tags.sort_by_key(|tag| tag.id);
    let in_group: Vec<&Tag> = all_tags
        .iter()
        .filter(|tag| tag.collection_group_id == group_id)
        .collect();
    let conflicts = unreadable_tags(&new.protocol, &in_group);
    match group_move_error(&new.name, &new.protocol, &conflicts) {
        Some(err) => Err(err),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrate_memory;
    use banto_tags::{CollectionGroupInput, PlcConnectionInput, TagInput};

    fn tag(id: i64, name: &str, address: &str, data_type: &str) -> Tag {
        Tag {
            id,
            name: name.to_string(),
            collection_group_id: 1,
            address: address.to_string(),
            data_type: data_type.to_string(),
            string_length: None,
            string_encoding: "utf8".to_string(),
            raw_lo: None,
            raw_hi: None,
            eng_lo: None,
            eng_hi: None,
            unit: None,
            decimals: 0,
            threshold_h: None,
            threshold_hh: None,
            threshold_l: None,
            threshold_ll: None,
            enabled: true,
            writable: false,
            tag_kind: "plc".to_string(),
            expression: None,
            retain: false,
            revision: 1,
        }
    }

    fn field_errors(err: BantoError) -> Vec<FieldError> {
        match err {
            BantoError::Validation { field_errors } => field_errors,
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    /// 保存時の判定表（ワイヤのフィールド名まで）。判定そのものは
    /// banto-collect 側の表で総当たりしてあるので、ここはワイヤ名への
    /// 載せ替えと代表例だけ。
    #[test]
    fn issues_map_to_wire_fields() {
        let table: &[(&str, &str, &str, Option<&str>)] = &[
            ("modbus-tcp", "D3000", "i16", Some("address")),
            ("modbus-tcp", "40001", "i16", None),
            ("slmp", "40001", "i16", Some("address")),
            ("slmp", "D3000", "i16", None),
            ("modbus-tcp", "40001.3", "i16", Some("address")),
            ("modbus-tcp", "40001", "nope", Some("dataType")),
        ];
        for &(protocol, address, data_type, expected) in table {
            let got = check_tag_address(protocol, address, data_type)
                .err()
                .map(|issue| {
                    field_errors(issue_to_validation_error(&issue))[0]
                        .field
                        .clone()
                });
            assert_eq!(
                got.as_deref(),
                expected,
                "{protocol} / {address} / {data_type}"
            );
        }
    }

    #[test]
    fn protocol_change_error_lists_the_tags_and_caps_the_list() {
        let tags: Vec<Tag> = (1..=7)
            .map(|i| tag(i, &format!("t{i}"), &format!("D{i}"), "i16"))
            .collect();
        let refs: Vec<&Tag> = tags.iter().collect();

        // SLMP へは全部読める → 拒否しない。
        assert!(unreadable_tags("slmp", &refs).is_empty());
        assert!(protocol_change_error("slmp", &[]).is_none());

        // Modbus へは 7 件とも読めない → 5 件だけ名前を出し、残りは件数。
        let conflicts = unreadable_tags("modbus-tcp", &refs);
        assert_eq!(conflicts.len(), 7);
        let errors = field_errors(protocol_change_error("modbus-tcp", &conflicts).unwrap());
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "protocol");
        let message = &errors[0].message;
        assert!(message.contains("7 件"), "{message}");
        assert!(message.contains("「t1」（D1）"), "{message}");
        assert!(message.contains("「t5」（D5）"), "{message}");
        assert!(!message.contains("「t6」"), "{message}");
        assert!(message.contains("ほか 2 件"), "{message}");
    }

    /// グループの接続変更の文言は、プロトコル変更と同じ形（上限 5 件＋
    /// 「ほか N 件」）で、`field` だけが `plcConnectionId`。
    #[test]
    fn group_move_error_has_the_same_shape_as_protocol_change_error() {
        let tags: Vec<Tag> = (1..=6)
            .map(|i| tag(i, &format!("t{i}"), &format!("4000{i}"), "i16"))
            .collect();
        let refs: Vec<&Tag> = tags.iter().collect();
        assert!(group_move_error("plc2", "modbus-tcp", &[]).is_none());

        let conflicts = unreadable_tags("slmp", &refs);
        assert_eq!(conflicts.len(), 6);
        let errors = field_errors(group_move_error("plc2", "slmp", &conflicts).unwrap());
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "plcConnectionId");
        let message = &errors[0].message;
        assert!(message.contains("plc2（slmp）"), "{message}");
        assert!(message.contains("6 件"), "{message}");
        assert!(message.contains("「t5」（40005）"), "{message}");
        assert!(!message.contains("「t6」"), "{message}");
        assert!(message.contains("ほか 1 件"), "{message}");
    }

    struct Registry {
        connections: PlcConnectionService,
        groups: CollectionGroupService,
        tags: TagService,
    }

    async fn registry() -> Registry {
        let pool = migrate_memory().await.expect("migrate_memory");
        Registry {
            connections: PlcConnectionService::new(pool.clone()),
            groups: CollectionGroupService::new(pool.clone()),
            tags: TagService::new(pool),
        }
    }

    fn connection_input(name: &str, protocol: &str) -> PlcConnectionInput {
        PlcConnectionInput {
            name: name.to_string(),
            protocol: protocol.to_string(),
            host: "192.168.11.200".to_string(),
            port: 502,
            unit_id: 1,
            enabled: true,
            simulation: false,
            word_order: String::new(),
            database: None,
            username: None,
            password: None,
        }
    }

    async fn connection_with_group(registry: &Registry, name: &str, protocol: &str) -> (i64, i64) {
        let conn = registry
            .connections
            .create(connection_input(name, protocol))
            .await
            .unwrap();
        let group = registry
            .groups
            .create(CollectionGroupInput {
                name: format!("{name}-group"),
                plc_connection_id: conn.id,
                period_ms: 1000,
                enabled: true,
                default_writable: true,
                query_sql: None,
            })
            .await
            .unwrap();
        (conn.id, group.id)
    }

    fn tag_input(name: &str, group_id: i64, address: &str) -> TagInput {
        TagInput {
            name: name.to_string(),
            collection_group_id: group_id,
            address: address.to_string(),
            data_type: "i16".to_string(),
            string_length: None,
            string_encoding: "utf8".to_string(),
            raw_lo: None,
            raw_hi: None,
            eng_lo: None,
            eng_hi: None,
            unit: None,
            decimals: 0,
            threshold_h: None,
            threshold_hh: None,
            threshold_l: None,
            threshold_ll: None,
            // 無効なタグも調べる対象（モジュール doc 参照）。
            enabled: false,
            writable: false,
            tag_kind: "plc".to_string(),
            expression: None,
            retain: false,
            expected_revision: None,
        }
    }

    /// 所属グループから接続を辿る: 同じ `D3000` が、SLMP のグループへは
    /// 通り、Modbus のグループへは `address` で拒否される（更新で別
    /// プロトコルのグループへ移す場合も同じ入口で拾える）。
    #[tokio::test]
    async fn a_tag_is_checked_against_its_own_groups_connection() {
        let registry = registry().await;
        let (_, modbus_group) = connection_with_group(&registry, "modbus", "modbus-tcp").await;
        let (_, slmp_group) = connection_with_group(&registry, "slmp", "slmp").await;

        ensure_tag_fits_its_connection(
            &registry.connections,
            &registry.groups,
            slmp_group,
            "D3000",
            "i16",
        )
        .await
        .expect("SLMP の下の D3000 は通る");

        let err = ensure_tag_fits_its_connection(
            &registry.connections,
            &registry.groups,
            modbus_group,
            "D3000",
            "i16",
        )
        .await
        .expect_err("Modbus の下の D3000 は拒否される");
        let errors = field_errors(err);
        assert_eq!(errors[0].field, "address");
        assert!(errors[0].message.contains("Modbus TCP"), "{errors:?}");

        // 存在しないグループは TagService 自身の検証に任せる。
        ensure_tag_fits_its_connection(
            &registry.connections,
            &registry.groups,
            9999,
            "D3000",
            "i16",
        )
        .await
        .expect("存在しないグループはここでは判定しない");
    }

    #[tokio::test]
    async fn a_protocol_change_is_refused_while_a_tag_underneath_would_become_unreadable() {
        let registry = registry().await;
        let (conn, group) = connection_with_group(&registry, "plc", "slmp").await;
        let (_, other_group) = connection_with_group(&registry, "other", "slmp").await;
        registry
            .tags
            .create(tag_input("melsec", group, "D3000"))
            .await
            .unwrap();
        // 別の接続の下のタグは関係しない。
        registry
            .tags
            .create(tag_input("elsewhere", other_group, "D100"))
            .await
            .unwrap();

        // 同じプロトコルのままなら調べない。
        ensure_protocol_change_keeps_tags_readable(
            &registry.connections,
            &registry.groups,
            &registry.tags,
            conn,
            "slmp",
        )
        .await
        .expect("プロトコルが変わらない更新は通る");

        let err = ensure_protocol_change_keeps_tags_readable(
            &registry.connections,
            &registry.groups,
            &registry.tags,
            conn,
            "modbus-tcp",
        )
        .await
        .expect_err("D3000 が Modbus で読めなくなる変更は拒否される");
        let errors = field_errors(err);
        assert_eq!(errors[0].field, "protocol");
        assert!(
            errors[0].message.contains("「melsec」（D3000）"),
            "{errors:?}"
        );
        assert!(!errors[0].message.contains("elsewhere"), "{errors:?}");
        assert!(errors[0].message.contains("1 件"), "{errors:?}");
    }

    #[tokio::test]
    async fn a_protocol_change_passes_when_every_tag_underneath_stays_readable() {
        let registry = registry().await;
        let (conn, group) = connection_with_group(&registry, "plc", "slmp").await;
        // 文字列タグは収集側が読まないので、プロトコル変更の妨げにならない。
        let mut string_tag = tag_input("text", group, "D3000");
        string_tag.data_type = "string".to_string();
        string_tag.string_length = Some(4);
        registry.tags.create(string_tag).await.unwrap();

        ensure_protocol_change_keeps_tags_readable(
            &registry.connections,
            &registry.groups,
            &registry.tags,
            conn,
            "modbus-tcp",
        )
        .await
        .expect("読めなくなるタグが無ければ通る");

        // 接続が無ければ PlcConnectionService::update 自身の NotFound に任せる。
        ensure_protocol_change_keeps_tags_readable(
            &registry.connections,
            &registry.groups,
            &registry.tags,
            9999,
            "modbus-tcp",
        )
        .await
        .expect("存在しない接続はここでは判定しない");
    }
}
