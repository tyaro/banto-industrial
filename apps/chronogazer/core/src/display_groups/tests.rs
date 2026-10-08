//! `crate::display_groups` の単体テスト（検証の総当たり・サービスの CRUD・
//! 楽観ロック・並べ替え・参照されているタグの削除の拒否）。REST の経路は
//! `rest::display_groups` のテスト、Tauri の経路は `src-tauri` のテスト。

use super::*;
use crate::rest::{CollectionGroupPayload, PlcConnectionPayload, TagPayload};
use banto_tags::{CollectionGroupService, PlcConnectionService};

/// `pool` に Modbus 接続 1 つ・収集グループ 1 つを作り、その下にタグ `n` 本。
pub(crate) async fn seed_tags(pool: &SqlitePool, n: usize) -> (TagService, Vec<i64>) {
    let conn = PlcConnectionService::new(pool.clone())
        .create(
            PlcConnectionPayload {
                name: "plc".to_string(),
                protocol: "modbus-tcp".to_string(),
                host: "127.0.0.1".to_string(),
                port: 502,
                unit_id: 1,
                enabled: true,
                word_order: String::new(),
                simulation: None,
            }
            .into_create_input(),
        )
        .await
        .expect("create connection");
    let group = CollectionGroupService::new(pool.clone())
        .create(
            CollectionGroupPayload {
                name: "cg".to_string(),
                plc_connection_id: conn.id,
                period_ms: 1000,
                enabled: true,
            }
            .into(),
        )
        .await
        .expect("create collection group");
    let tags = TagService::new(pool.clone());
    let mut ids = Vec::new();
    for i in 0..n {
        let tag = tags
            .create(tag_payload(&format!("tag{i}"), group.id).into())
            .await
            .expect("create tag");
        ids.push(tag.id);
    }
    (tags, ids)
}

fn tag_payload(name: &str, collection_group_id: i64) -> TagPayload {
    TagPayload {
        name: name.to_string(),
        collection_group_id,
        address: "40001".to_string(),
        data_type: "u16".to_string(),
        raw_lo: None,
        raw_hi: None,
        eng_lo: None,
        eng_hi: None,
        unit: None,
        decimals: 0,
        enabled: true,
    }
}

/// メモリ DB と、その上のタグ `n` 本。
async fn service_with_tags(n: usize) -> (DisplayGroupService, TagService, Vec<i64>) {
    let pool = crate::db::init_db_memory().await.expect("init_db_memory");
    let (tags, ids) = seed_tags(&pool, n).await;
    (DisplayGroupService::new(pool), tags, ids)
}

pub(crate) fn payload(name: &str, kind: &str, tag_ids: &[i64]) -> DisplayGroupPayload {
    DisplayGroupPayload {
        name: name.to_string(),
        sort_order: None,
        kind: kind.to_string(),
        attributes: Map::new(),
        pens: tag_ids
            .iter()
            .map(|&tag_id| PenPayload {
                tag_id,
                color_slot: None,
            })
            .collect(),
        expected_revision: None,
    }
}

fn fields(err: &BantoError) -> Vec<String> {
    match err {
        BantoError::Validation { field_errors } => {
            field_errors.iter().map(|fe| fe.field.clone()).collect()
        }
        other => panic!("expected a validation error, got {other:?}"),
    }
}

fn shape_fields(payload: &DisplayGroupPayload) -> Vec<String> {
    match validate_shape(payload) {
        Ok(_) => Vec::new(),
        Err(errors) => errors.into_iter().map(|fe| fe.field).collect(),
    }
}

type Mutate = fn(&mut DisplayGroupPayload);

/// 形の検証の総当たり（DB を見ない部分）。各行は「この 1 点だけ壊すと、
/// このフィールドだけが誤りになる」。
#[test]
fn shape_validation_table() {
    let ok = payload("g", "trend", &[1, 2]);
    assert!(shape_fields(&ok).is_empty());

    let cases: Vec<(&str, Mutate, Vec<&str>)> = vec![
        ("空の名前", |p| p.name = "   ".into(), vec!["name"]),
        ("長すぎる名前", |p| p.name = "あ".repeat(101), vec!["name"]),
        ("4 種以外の種別", |p| p.kind = "pie".into(), vec!["kind"]),
        ("大文字の種別", |p| p.kind = "Trend".into(), vec!["kind"]),
        ("負の並び順", |p| p.sort_order = Some(-1), vec!["sortOrder"]),
        (
            "9 本のペン",
            |p| {
                p.pens = (1..=9)
                    .map(|tag_id| PenPayload {
                        tag_id,
                        color_slot: None,
                    })
                    .collect()
            },
            vec!["pens"],
        ),
        (
            "同じタグの重複",
            |p| p.pens[1].tag_id = p.pens[0].tag_id,
            vec!["pens[1].tagId"],
        ),
        (
            "色の枠 0",
            |p| p.pens[0].color_slot = Some(0),
            vec!["pens[0].colorSlot"],
        ),
        (
            "色の枠 9",
            |p| p.pens[1].color_slot = Some(9),
            vec!["pens[1].colorSlot"],
        ),
        (
            "トレンドの時間窓が選択肢の外",
            |p| {
                p.attributes.insert("timeWindowSec".into(), json!(123));
            },
            vec!["attributes.timeWindowSec"],
        ),
        (
            "トレンドの時間窓が文字列",
            |p| {
                p.attributes.insert("timeWindowSec".into(), json!("600"));
            },
            vec!["attributes.timeWindowSec"],
        ),
        (
            "閉じた集合に無い属性",
            |p| {
                p.attributes.insert("color".into(), json!("red"));
            },
            vec!["attributes.color"],
        ),
        (
            "デジタルにトレンドの属性",
            |p| {
                p.kind = "digital".into();
                p.attributes.insert("timeWindowSec".into(), json!(600));
            },
            vec!["attributes.timeWindowSec"],
        ),
    ];
    for (label, mutate, expected) in cases {
        let mut p = payload("g", "trend", &[1, 2]);
        mutate(&mut p);
        assert_eq!(shape_fields(&p), expected, "{label}");
    }
}

/// 8 本ちょうど・色の枠の端・4 種すべては通り、属性の既定値が埋まる。
#[test]
fn shape_validation_accepts_the_edges_and_fills_defaults() {
    let mut p = payload("  ライン1  ", "trend", &[1, 2, 3, 4, 5, 6, 7, 8]);
    p.pens[0].color_slot = Some(1);
    p.pens[7].color_slot = Some(8);
    let valid = validate_shape(&p).expect("8 本は通る");
    assert_eq!(valid.name, "ライン1");
    assert_eq!(
        valid.attributes.time_window_sec,
        Some(DEFAULT_TREND_TIME_WINDOW_SEC)
    );
    for sec in TREND_TIME_WINDOWS_SEC {
        let mut p = payload("g", "trend", &[]);
        p.attributes.insert("timeWindowSec".into(), json!(sec));
        assert_eq!(
            validate_shape(&p).unwrap().attributes.time_window_sec,
            Some(sec)
        );
    }
    for kind in DisplayKind::ALL {
        let valid = validate_shape(&payload("g", kind.as_str(), &[])).unwrap();
        assert_eq!(valid.kind, kind);
        if kind != DisplayKind::Trend {
            assert_eq!(valid.attributes, DisplayAttributes::default());
        }
    }
}

/// 応答の JSON をそのまま本文に送り返せる（宣言データとして往復できる）。
#[tokio::test]
async fn the_response_round_trips_as_a_payload() {
    let (svc, _tags, ids) = service_with_tags(2).await;
    let mut p = payload("往復", "trend", &ids);
    p.pens[1].color_slot = Some(5);
    p.attributes.insert("timeWindowSec".into(), json!(1800));
    let created = svc.create(&p).await.expect("create");
    let json = serde_json::to_value(&created).unwrap();
    assert_eq!(
        json,
        json!({
            "id": created.id,
            "name": "往復",
            "sortOrder": 0,
            "kind": "trend",
            "attributes": { "timeWindowSec": 1800 },
            "pens": [
                { "tagId": ids[0], "colorSlot": null },
                { "tagId": ids[1], "colorSlot": 5 }
            ],
            "revision": 1
        })
    );
    let mut back: DisplayGroupPayload = serde_json::from_value(json).unwrap();
    back.expected_revision = Some(created.revision);
    let updated = svc.update(created.id, &back).await.expect("update");
    assert_eq!(updated.revision, created.revision + 1);
    assert_eq!(
        DisplayGroup {
            revision: created.revision,
            ..updated
        },
        created
    );
}

#[tokio::test]
async fn create_get_list_update_delete() {
    let (svc, _tags, ids) = service_with_tags(3).await;
    let a = svc.create(&payload("A", "trend", &ids[..2])).await.unwrap();
    let b = svc.create(&payload("B", "gauge", &ids[2..])).await.unwrap();
    assert_eq!((a.sort_order, b.sort_order), (0, 1), "作成は末尾に足す");
    assert_eq!(svc.get(a.id).await.unwrap(), a);
    let names: Vec<_> = svc
        .list()
        .await
        .unwrap()
        .into_iter()
        .map(|g| g.name)
        .collect();
    assert_eq!(names, ["A", "B"]);

    let mut edit = payload("A2", "bar", &[ids[2], ids[0]]);
    edit.expected_revision = Some(a.revision);
    let updated = svc.update(a.id, &edit).await.unwrap();
    assert_eq!(updated.name, "A2");
    assert_eq!(updated.kind, DisplayKind::Bar);
    assert_eq!(updated.sort_order, 0, "並び順の省略は今の値を保つ");
    assert_eq!(
        updated.pens.iter().map(|p| p.tag_id).collect::<Vec<_>>(),
        [ids[2], ids[0]],
        "ペンは本文の順で置き換わる"
    );
    assert_eq!(updated.attributes, DisplayAttributes::default());

    let deleted = svc.delete(a.id).await.unwrap();
    assert_eq!(deleted.name, "A2");
    assert!(matches!(
        svc.get(a.id).await,
        Err(BantoError::NotFound { .. })
    ));
    assert!(matches!(
        svc.delete(a.id).await,
        Err(BantoError::NotFound { .. })
    ));
    let pens_left: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM display_group_pens WHERE group_id = ?")
            .bind(a.id)
            .fetch_one(&svc.pool)
            .await
            .unwrap();
    assert_eq!(pens_left, 0, "ペンも一緒に消える");
}

/// DB を見る検証: 名前の重複（自分自身は除く）・存在しないタグ・上限 16。
/// 誤りは 1 件目で止めずに全部並べる。
#[tokio::test]
async fn validation_against_the_database() {
    let (svc, _tags, ids) = service_with_tags(1).await;
    let first = svc.create(&payload("A", "trend", &ids)).await.unwrap();

    let err = svc
        .create(&payload("A", "pie", &[ids[0], 99_999]))
        .await
        .expect_err("重複・未知の種別・存在しないタグ");
    assert_eq!(fields(&err), ["kind", "name", "pens[1].tagId"]);

    // 自分自身の名前のままの更新は通る。
    svc.update(first.id, &payload("A", "digital", &ids))
        .await
        .expect("自分の名前は重複ではない");

    for i in 1..MAX_DISPLAY_GROUPS {
        svc.create(&payload(&format!("G{i}"), "digital", &[]))
            .await
            .expect("16 個までは作れる");
    }
    let err = svc
        .create(&payload("G16", "digital", &[]))
        .await
        .expect_err("17 個目");
    assert_eq!(fields(&err), ["displayGroups"]);
    assert_eq!(svc.list().await.unwrap().len(), MAX_DISPLAY_GROUPS);
    // 上限は作成だけ。16 個の状態でも更新はできる。
    svc.update(first.id, &payload("A", "gauge", &ids))
        .await
        .expect("上限に達していても更新はできる");
}

/// 楽観ロック: 古い版の更新は `expectedRevision` で拒否され、保存は変わらない。
#[tokio::test]
async fn a_stale_expected_revision_is_refused() {
    let (svc, _tags, ids) = service_with_tags(1).await;
    let created = svc.create(&payload("A", "trend", &ids)).await.unwrap();
    let mut first = payload("A1", "trend", &ids);
    first.expected_revision = Some(created.revision);
    let saved = svc.update(created.id, &first).await.unwrap();

    let mut stale = payload("A-stale", "digital", &[]);
    stale.expected_revision = Some(created.revision);
    let err = svc.update(created.id, &stale).await.expect_err("古い版");
    assert!(is_revision_conflict(&err));
    assert_eq!(fields(&err), [REVISION_CONFLICT_FIELD]);
    assert_eq!(svc.get(created.id).await.unwrap(), saved);

    assert!(matches!(
        svc.update(99_999, &first).await,
        Err(BantoError::NotFound { .. })
    ));
}

#[tokio::test]
async fn reorder_rewrites_the_sort_order_without_bumping_revisions() {
    let (svc, _tags, _ids) = service_with_tags(0).await;
    let a = svc.create(&payload("A", "digital", &[])).await.unwrap();
    let b = svc.create(&payload("B", "digital", &[])).await.unwrap();
    let c = svc.create(&payload("C", "digital", &[])).await.unwrap();

    let reordered = svc.reorder(&[c.id, a.id, b.id]).await.unwrap();
    assert_eq!(
        reordered
            .iter()
            .map(|g| g.name.as_str())
            .collect::<Vec<_>>(),
        ["C", "A", "B"]
    );
    assert!(reordered.iter().all(|g| g.revision == 1));

    for bad in [
        vec![c.id, a.id],
        vec![c.id, a.id, a.id],
        vec![c.id, a.id, 99_999],
    ] {
        let err = svc.reorder(&bad).await.expect_err("過不足・重複");
        assert_eq!(fields(&err), ["ids"], "{bad:?}");
    }
}

/// §3.7.9 の 8: ペンに割り当てられたタグの削除は参照元を並べて拒否し、
/// 外せば消せる。改名は通る（参照は ID）。
#[tokio::test]
async fn deleting_a_referenced_tag_is_refused_with_the_referrers() {
    let (svc, tags, ids) = service_with_tags(2).await;
    svc.create(&payload("ライン1", "trend", &ids))
        .await
        .unwrap();
    svc.create(&payload("ライン2", "bar", &ids[..1]))
        .await
        .unwrap();

    let err = svc
        .delete_tag_unless_referenced(&tags, ids[0])
        .await
        .expect_err("参照されているタグが消せてしまった");
    let BantoError::Validation { field_errors } = &err else {
        panic!("{err:?}");
    };
    assert_eq!(field_errors.len(), 1);
    assert_eq!(field_errors[0].field, TAG_REFERENCED_FIELD);
    assert!(
        field_errors[0].message.contains("「ライン1」「ライン2」"),
        "{}",
        field_errors[0].message
    );
    assert!(tags.get(ids[0]).await.is_ok(), "タグは残っている");
    assert_eq!(
        svc.referrers_of_tag(ids[0])
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.name)
            .collect::<Vec<_>>(),
        ["ライン1", "ライン2"]
    );

    // 改名は通り、グループは同じタグを指したまま。
    let current = tags.get(ids[0]).await.unwrap();
    let mut rename = tag_payload("改名後", current.collection_group_id);
    rename.address = current.address.clone();
    tags.update(ids[0], rename.into()).await.expect("改名");
    assert_eq!(svc.referrers_of_tag(ids[0]).await.unwrap().len(), 2);

    // ペンから外せば消せる。
    for group in svc.list().await.unwrap() {
        let mut p = payload(&group.name, group.kind.as_str(), &[]);
        p.expected_revision = Some(group.revision);
        svc.update(group.id, &p).await.unwrap();
    }
    svc.delete_tag_unless_referenced(&tags, ids[0])
        .await
        .expect("外せば消せる");
    assert!(matches!(
        tags.get(ids[0]).await,
        Err(BantoError::NotFound { .. })
    ));
    assert!(matches!(
        svc.delete_tag_unless_referenced(&tags, ids[0]).await,
        Err(BantoError::NotFound { .. })
    ));
}

/// 参照元の確認と削除の間にペンが足されない（`BEGIN IMMEDIATE`）: タグの
/// 削除とそのタグを使うグループの作成を同時に走らせても、「タグが消えたのに
/// ペンが残る」状態にならない。複数の接続で同時に書けるよう on-disk の DB。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tag_delete_and_pen_assignment_do_not_interleave() {
    let dir = crate::test_support::TempDir::new();
    let pool = crate::db::init_db(dir.path().join("dg.sqlite3"))
        .await
        .expect("init_db");
    let (tags, _) = seed_tags(&pool, 0).await;
    let group_id: i64 = sqlx::query_scalar("SELECT id FROM collection_groups")
        .fetch_one(&pool)
        .await
        .unwrap();
    let svc = DisplayGroupService::new(pool.clone());
    for round in 0..20 {
        let tag = tags
            .create(tag_payload(&format!("race{round}"), group_id).into())
            .await
            .unwrap();
        let (svc_a, svc_b, tags_a) = (svc.clone(), svc.clone(), tags.clone());
        let name = format!("race{round}");
        let delete =
            tokio::spawn(async move { svc_a.delete_tag_unless_referenced(&tags_a, tag.id).await });
        let create =
            tokio::spawn(async move { svc_b.create(&payload(&name, "trend", &[tag.id])).await });
        let (deleted, created) = (delete.await.unwrap(), create.await.unwrap());
        let tag_exists = tags.get(tag.id).await.is_ok();
        let referenced = !svc.referrers_of_tag(tag.id).await.unwrap().is_empty();
        assert!(
            tag_exists || !referenced,
            "round {round}: タグが消えたのにペンが残った（delete={deleted:?}, create={created:?}）"
        );
        // どちらか一方だけが成功する。
        assert!(deleted.is_ok() != created.is_ok(), "round {round}");
        for g in svc.list().await.unwrap() {
            svc.delete(g.id).await.unwrap();
        }
    }
    pool.close().await;
}
