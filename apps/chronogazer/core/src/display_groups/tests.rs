//! `crate::display_groups` の単体テスト（検証の総当たり・サービスの CRUD・
//! 楽観ロック・並べ替え・参照されているタグの削除の拒否）。REST の経路は
//! `rest::display_groups` のテスト、Tauri の経路は `src-tauri` のテスト。

use super::*;
use crate::rest::{CollectionGroupPayload, TagPayload};
use banto_tags::{CollectionGroupService, PlcConnectionService};

/// `pool` に Modbus 接続 1 つ・収集グループ 1 つを作り、その下にタグ `n` 本。
pub(crate) async fn seed_tags(pool: &SqlitePool, n: usize) -> (TagService, Vec<i64>) {
    let conn = PlcConnectionService::new(pool.clone())
        .create(banto_tags::PlcConnectionInput {
            name: "plc".to_string(),
            protocol: "modbus-tcp".to_string(),
            host: "127.0.0.1".to_string(),
            port: 502,
            unit_id: 1,
            enabled: true,
            word_order: String::new(),
            simulation: false,
            cpu_type: None,
            database: None,
            username: None,
            secrets: Default::default(),
            settings: None,
        })
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

/// タグの本文。JSON から組むのは、`TagPayload` に省略できる項目が増えても
/// （#525 のしきい値・`expectedRevision` など）このテストを直さずに済むように。
fn tag_payload(name: &str, collection_group_id: i64) -> TagPayload {
    serde_json::from_value(json!({
        "name": name,
        "collectionGroupId": collection_group_id,
        "address": "40001",
        "dataType": "u16",
    }))
    .expect("TagPayload")
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
        // 更新でも使い回せるよう明示の null（キーなしは更新で拒否される）。
        bundle: Some(None),
        sort_order: None,
        kind: kind.to_string(),
        attributes: Map::new(),
        pens: tag_ids
            .iter()
            .map(|&tag_id| PenPayload::tag(tag_id, None))
            .collect(),
        expected_revision: None,
        hub_tag_group: None,
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
            "17 本のペン",
            |p| {
                p.pens = (1..=17)
                    .map(|tag_id| PenPayload::tag(tag_id, None))
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
            "色の枠 17",
            |p| p.pens[1].color_slot = Some(17),
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

/// 16 本ちょうど・色の枠の端（1 と 16）・4 種すべては通り、属性の既定値が埋まる。
#[test]
fn shape_validation_accepts_the_edges_and_fills_defaults() {
    let mut p = payload("  ライン1  ", "trend", &(1..=16).collect::<Vec<i64>>());
    p.pens[0].color_slot = Some(1);
    p.pens[15].color_slot = Some(16);
    let valid = validate_shape(&p).expect("16 本は通る");
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
            "bundle": null,
            "sortOrder": 0,
            "kind": "trend",
            "attributes": { "timeWindowSec": 1800 },
            "pens": [
                { "tagId": ids[0], "colorSlot": null, "label": null, "unit": null, "decimals": null, "lineStyle": null, "lineWidth": null, "marker": null, "markerSize": null, "axis": null, "yMin": null, "yMax": null },
                { "tagId": ids[1], "colorSlot": 5, "label": null, "unit": null, "decimals": null, "lineStyle": null, "lineWidth": null, "marker": null, "markerSize": null, "axis": null, "yMin": null, "yMax": null }
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

/// 16 本・色の枠 16 まで DB に保存でき、読み戻せる（0104 の CHECK の広げ方。
/// 17 本目・色の枠 17 は検証で断られるので、ここでは境界の保存だけを見る）。
#[tokio::test]
async fn sixteen_pens_with_color_slot_sixteen_round_trip_through_the_db() {
    let (svc, _tags, ids) = service_with_tags(16).await;
    let mut p = payload("16 本", "trend", &ids);
    p.pens[15].color_slot = Some(16);
    p.pens[8].color_slot = Some(9);
    let created = svc.create(&p).await.expect("16 本は作れる");
    let got = svc.get(created.id).await.unwrap();
    assert_eq!(got.pens.len(), 16);
    assert_eq!(got.pens[15].color_slot, Some(16));
    assert_eq!(got.pens[8].color_slot, Some(9));
    assert_eq!(got.pens.iter().map(|p| p.tag_id).collect::<Vec<_>>(), ids);
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
            .expect("36 個までは作れる");
    }
    let err = svc
        .create(&payload("G36", "digital", &[]))
        .await
        .expect_err("37 個目");
    assert_eq!(fields(&err), ["displayGroups"]);
    assert_eq!(svc.list().await.unwrap().len(), MAX_DISPLAY_GROUPS);
    // 上限は作成だけ。36 個の状態でも更新はできる。
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

/// on-disk（WAL）の DB と、その上のタグ `n` 本。複数の接続で同時に読み書き
/// するテスト用（メモリ DB は接続を跨いだ割り込みを再現できない）。
async fn on_disk_service(
    dir: &crate::test_support::TempDir,
    n: usize,
) -> (SqlitePool, DisplayGroupService, TagService, Vec<i64>) {
    let pool = crate::db::init_db(dir.path().join("dg.sqlite3"))
        .await
        .expect("init_db");
    let (tags, ids) = seed_tags(&pool, n).await;
    (pool.clone(), DisplayGroupService::new(pool), tags, ids)
}

fn read_pause() -> (
    ReadPause,
    std::sync::Arc<tokio::sync::Notify>,
    std::sync::Arc<tokio::sync::Notify>,
) {
    let reached = std::sync::Arc::new(tokio::sync::Notify::new());
    let resume = std::sync::Arc::new(tokio::sync::Notify::new());
    (
        ReadPause {
            reached: reached.clone(),
            resume: resume.clone(),
        },
        reached,
        resume,
    )
}

/// オーナーレビュー P2-1: `list`/`get` はグループの行とペンを 1 つのスナップ
/// ショットで読む。2 つの SELECT の間で止め（[`ReadPause`]）、その間に別の接続
/// から名前・種別・ペンを変える更新を commit しても、応答は「更新前の全体」で、
/// 古い名前・版に新しいペンが混ざらない。
///
/// 反証（2026-10-08 実施）: `list_paused`/`get_paused` の `self.pool.begin()` を
/// `self.pool.acquire()`（素の接続）に戻すと、両方の `assert_eq!` が落ちる
/// （名前は「前」、ペンは「後」）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn list_and_get_read_rows_and_pens_from_one_snapshot() {
    let dir = crate::test_support::TempDir::new();
    let (pool, svc, _tags, ids) = on_disk_service(&dir, 2).await;
    let before = svc
        .create(&payload("前", "trend", &ids[..1]))
        .await
        .unwrap();

    for use_get in [false, true] {
        let current = svc.get(before.id).await.unwrap();
        let (pause, reached, resume) = read_pause();
        let reader = svc.clone();
        let id = before.id;
        let read = tokio::spawn(async move {
            if use_get {
                reader.get_paused(id, Some(&pause)).await.map(|g| vec![g])
            } else {
                reader.list_paused(Some(&pause)).await
            }
        });
        reached.notified().await;

        // 読み取りが 2 つの SELECT の間にいる間に、別の接続で更新を commit する。
        let mut edit = payload("後", "gauge", &ids[1..]);
        edit.expected_revision = Some(current.revision);
        let after = svc
            .update(id, &edit)
            .await
            .expect("読み取りの最中でも更新できる");
        resume.notify_one();

        let seen = read.await.unwrap().expect("read");
        assert_eq!(seen, vec![current.clone()], "use_get={use_get}");
        assert_eq!(svc.get(id).await.unwrap(), after);

        // 次の周のために元へ戻す。
        let mut back = payload("前", "trend", &ids[..1]);
        back.expected_revision = Some(after.revision);
        svc.update(id, &back).await.unwrap();
    }
    pool.close().await;
}

/// Codex レビュー P2-3: 更新（PUT）の本文の `sortOrder` は無視する。前に読んだ
/// 応答（`sortOrder` 入り）をそのまま送り返しても、その間の並べ替えは取り消され
/// ない。
///
/// 反証（2026-10-08 実施）: `update` の UPDATE 文を本文の `sortOrder` で書く形
/// （`sort_order = COALESCE(?, sort_order)`、修正前と同じ）に戻すと、最後の並びが `["A", "B"]` になって落ちる。
#[tokio::test]
async fn update_ignores_sort_order_so_a_reorder_is_not_undone() {
    let (svc, _tags, _ids) = service_with_tags(0).await;
    let a = svc.create(&payload("A", "digital", &[])).await.unwrap();
    let b = svc.create(&payload("B", "digital", &[])).await.unwrap();
    let fetched = serde_json::to_value(svc.get(a.id).await.unwrap()).unwrap();
    assert_eq!(fetched["sortOrder"], 0);

    svc.reorder(&[b.id, a.id]).await.unwrap();

    // 前に読んだ A（sortOrder 0）を、名前だけ変えて送り返す。
    let mut body: DisplayGroupPayload = serde_json::from_value(fetched).unwrap();
    body.name = "A2".into();
    body.expected_revision = Some(a.revision);
    let updated = svc.update(a.id, &body).await.unwrap();
    assert_eq!(updated.sort_order, 1, "並べ替え後の位置のまま");
    let names: Vec<_> = svc
        .list()
        .await
        .unwrap()
        .into_iter()
        .map(|g| g.name)
        .collect();
    assert_eq!(names, ["B", "A2"]);
}

/// Codex レビュー P2-4: 作成で `sortOrder` を省略したときの末尾が
/// [`MAX_SORT_ORDER`] を超えるなら、並びを保ったまま 0 から振り直す。DB の
/// CHECK も範囲外を拒否する。
///
/// 反証（2026-10-08 実施）: `end_sort_order` の振り直しを消して「最大 + 1」を
/// そのまま返すと、2 つ目の作成が CHECK 違反（storage エラー）で落ちる。
#[tokio::test]
async fn an_implicit_end_position_never_exceeds_the_limit() {
    let (svc, _tags, _ids) = service_with_tags(0).await;
    let mut first = payload("先頭", "digital", &[]);
    first.sort_order = Some(0);
    svc.create(&first).await.unwrap();
    let mut last = payload("端", "digital", &[]);
    last.sort_order = Some(MAX_SORT_ORDER);
    svc.create(&last).await.unwrap();

    let appended = svc
        .create(&payload("追加", "digital", &[]))
        .await
        .expect("末尾が溢れても作れる");
    let groups = svc.list().await.unwrap();
    assert_eq!(
        groups
            .iter()
            .map(|g| (g.name.as_str(), g.sort_order))
            .collect::<Vec<_>>(),
        [("先頭", 0), ("端", 1), ("追加", 2)]
    );
    assert_eq!(appended.sort_order, 2);

    let err = sqlx::query(
        "INSERT INTO display_groups (name, sort_order, kind) VALUES ('範囲外', 10000, 'trend')",
    )
    .execute(&svc.pool)
    .await
    .expect_err("CHECK が範囲外を拒否する");
    assert!(err.to_string().contains("CHECK"), "{err}");
}

// --- #1179 PR5: Hub のタグをペンにする一括保存 ---------------------------------

/// hub 接続を受け付けるレジストリのサービス一式（本番と同じ）。
fn registrar(pool: &SqlitePool) -> HubTagRegistrar {
    let drivers = crate::test_support::drivers();
    HubTagRegistrar {
        plc_connections: PlcConnectionService::with_registry(pool.clone(), drivers.clone())
            .accept_hub_connections(),
        collection_groups: CollectionGroupService::with_registry(pool.clone(), drivers.clone())
            .accept_hub_connections(),
        tags: TagService::with_registry(pool.clone(), drivers).accept_hub_connections(),
    }
}

fn hub_pen(external_name: &str, name: &str, data_type: &str) -> PenPayload {
    PenPayload {
        tag_id: None,
        hub_tag: Some(HubTagRow {
            external_name: external_name.to_owned(),
            name: name.to_owned(),
            data_type: data_type.to_owned(),
            unit: None,
            decimals: None,
        }),
        ..PenPayload::default()
    }
}

fn new_hub_group(name: &str) -> Option<HubTagGroupPayload> {
    Some(HubTagGroupPayload {
        collection_group_id: None,
        new_group: Some(NewHubGroup {
            name: name.to_owned(),
            period_ms: 1000,
        }),
    })
}

/// (タグ, 接続, 収集グループ, 表示グループ) の行数。巻き戻りの確認に使う。
async fn row_counts(pool: &SqlitePool) -> (i64, i64, i64, i64) {
    sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM tags), (SELECT COUNT(*) FROM plc_connections),          (SELECT COUNT(*) FROM collection_groups), (SELECT COUNT(*) FROM display_groups)",
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

/// 未登録の Hub のタグ 2 本 + 登録済みの記録項目 1 本で作ると、Hub のタグが
/// 記録項目になり（hub 接続・収集グループも暗黙に作る）、ペンは解決した id を指す。
/// 応答は `registeredTags` を持ち、GET と同じく本文に送り返せる。更新で既存の
/// hub の収集グループ（`collectionGroupId`）に足すこともできる。
#[tokio::test]
async fn hub_tags_are_registered_and_assigned_in_one_save() {
    let (svc, _tags, ids) = service_with_tags(1).await;
    let reg = registrar(&svc.pool);
    let mut body = payload("L1", "trend", &[ids[0]]);
    body.pens.insert(0, hub_pen("L.G.a", "a", "f32"));
    body.pens.push(hub_pen(" L.G.b ", "b", "bit"));
    body.hub_tag_group = new_hub_group("hub-g");

    let saved = svc
        .create_with_hub_tags(&reg, &body)
        .await
        .expect("一括保存");
    let registration = saved.registration.clone().expect("登録した");
    assert!(registration.connection_created && registration.group_created);
    let by_address: HashMap<&str, i64> = saved
        .registered_tags()
        .iter()
        .map(|tag| (tag.address.as_str(), tag.id))
        .collect();
    assert_eq!(by_address.len(), 2);
    let pen_ids: Vec<i64> = saved.group.pens.iter().map(|pen| pen.tag_id).collect();
    assert_eq!(
        pen_ids,
        vec![by_address["L.G.a"], ids[0], by_address["L.G.b"]]
    );
    assert_eq!(
        crate::hub_tags::registered_external_names(&svc.pool)
            .await
            .unwrap(),
        vec!["L.G.a".to_owned(), "L.G.b".to_owned()]
    );

    // 応答: `registeredTags` を持ち、そのまま本文に送り返せる（未知の項目は無視）。
    let json = serde_json::to_value(saved.clone().into_response()).unwrap();
    assert_eq!(json["registeredTags"].as_array().unwrap().len(), 2);
    assert_eq!(json["id"], saved.group.id);
    let back: DisplayGroupPayload = serde_json::from_value(json).unwrap();
    assert_eq!(back.pens.len(), 3);

    // 更新: 既存の hub の収集グループに 1 本足す。
    let group_id = registration.collection_group.unwrap().id;
    let mut edit = payload("L1", "trend", &pen_ids);
    edit.pens.push(hub_pen("L.G.c", "c", "i16"));
    edit.expected_revision = Some(saved.group.revision);
    edit.hub_tag_group = Some(HubTagGroupPayload {
        collection_group_id: Some(group_id),
        new_group: None,
    });
    let updated = svc
        .update_with_hub_tags(&reg, saved.group.id, &edit)
        .await
        .expect("更新の一括保存");
    let added = &updated.registration.as_ref().expect("登録した").tags;
    assert_eq!(added.len(), 1);
    assert!(!updated.registration.as_ref().unwrap().group_created);
    assert_eq!(updated.group.pens[3].tag_id, added[0].id);
    assert_eq!(updated.group.revision, saved.group.revision + 1);
}

/// 登録済みの外部名の Hub のタグは、新しく登録せずに既存の記録項目を参照する
/// （二重登録しない）。登録が無いので `hubTagGroup` も要らず、応答に
/// `registeredTags` は出ない。
#[tokio::test]
async fn a_registered_hub_tag_is_referenced_not_registered_again() {
    let (svc, _tags, _ids) = service_with_tags(0).await;
    let reg = registrar(&svc.pool);
    let mut first = payload("L1", "trend", &[]);
    first.pens.push(hub_pen("L.G.a", "a", "f32"));
    first.hub_tag_group = new_hub_group("hub-g");
    let first = svc.create_with_hub_tags(&reg, &first).await.unwrap();
    let tag_id = first.group.pens[0].tag_id;
    let before = row_counts(&svc.pool).await;

    let mut second = payload("L2", "bar", &[]);
    second.pens.push(hub_pen("  L.G.a", "別名", "f32"));
    let second = svc
        .create_with_hub_tags(&reg, &second)
        .await
        .expect("登録済みは参照だけ");
    assert!(second.registration.is_none());
    assert_eq!(second.group.pens[0].tag_id, tag_id);
    let after = row_counts(&svc.pool).await;
    assert_eq!((after.0, after.1, after.2), (before.0, before.1, before.2));
    let json = serde_json::to_value(second.into_response()).unwrap();
    assert!(json.get("registeredTags").is_none(), "{json}");

    // 同じ記録項目を tagId と hubTag で 2 回指すのは、後ろのペンの hubTag の誤り。
    let mut twice = payload("L3", "bar", &[tag_id]);
    twice.pens.push(hub_pen("L.G.a", "a", "f32"));
    let err = svc.create_with_hub_tags(&reg, &twice).await.unwrap_err();
    assert_eq!(fields(&err), ["pens[1].hubTag.externalName"]);
}

/// 未登録の Hub のタグがあるのに登録先が無い（または両方ある）と `hubTagGroup` の
/// 誤りで、何も書かない。
#[tokio::test]
async fn an_unregistered_hub_tag_needs_exactly_one_target() {
    let (svc, _tags, _ids) = service_with_tags(0).await;
    let reg = registrar(&svc.pool);
    let before = row_counts(&svc.pool).await;
    let mut body = payload("L1", "trend", &[]);
    body.pens.push(hub_pen("L.G.a", "a", "f32"));
    let err = svc.create_with_hub_tags(&reg, &body).await.unwrap_err();
    assert_eq!(fields(&err), ["hubTagGroup"]);

    body.hub_tag_group = Some(HubTagGroupPayload {
        collection_group_id: Some(1),
        new_group: Some(NewHubGroup {
            name: "g".to_owned(),
            period_ms: 1000,
        }),
    });
    let err = svc.create_with_hub_tags(&reg, &body).await.unwrap_err();
    assert_eq!(fields(&err), ["hubTagGroup"]);

    // 登録しない経路（`create`）は登録先があっても登録しない。
    body.hub_tag_group = new_hub_group("g");
    let err = svc.create(&body).await.unwrap_err();
    assert_eq!(fields(&err), ["hubTagGroup"]);
    assert_eq!(row_counts(&svc.pool).await, before);
}

/// **巻き戻り**: 登録は通るが表示グループの検証で落ちる（名前の重複・ペンが
/// 17 本）と、登録した記録項目も、暗黙に作った hub 接続・収集グループも残らない。
///
/// 反証（2026-11-18 実施）: `save_on` の中で登録の後に `conn` を一度 commit する
/// （登録を別のトランザクションにする）と、このテストの `row_counts` の比較が落ちる。
#[tokio::test]
async fn a_display_group_failure_after_registration_rolls_back_the_registration() {
    let (svc, _tags, _ids) = service_with_tags(0).await;
    let reg = registrar(&svc.pool);
    svc.create(&payload("L1", "trend", &[])).await.unwrap();
    let before = row_counts(&svc.pool).await;

    let mut clash = payload("L1", "trend", &[]);
    clash.pens.push(hub_pen("L.G.a", "a", "f32"));
    clash.hub_tag_group = new_hub_group("hub-g");
    let err = svc.create_with_hub_tags(&reg, &clash).await.unwrap_err();
    assert_eq!(fields(&err), ["name"]);
    assert_eq!(row_counts(&svc.pool).await, before);
    assert!(crate::hub_tags::registered_external_names(&svc.pool)
        .await
        .unwrap()
        .is_empty());

    let mut seventeen = payload("L2", "trend", &[]);
    seventeen.pens = (0..17)
        .map(|i| hub_pen(&format!("L.G.{i}"), &format!("t{i}"), "f32"))
        .collect();
    seventeen.hub_tag_group = new_hub_group("hub-g");
    let err = svc
        .create_with_hub_tags(&reg, &seventeen)
        .await
        .unwrap_err();
    assert_eq!(fields(&err), ["pens"]);
    assert_eq!(row_counts(&svc.pool).await, before);
}

/// 登録の行の誤りはペンの位置（`pens[i].hubTag.*`）、登録先の誤りは
/// `hubTagGroup.*` に付く。形の誤り（名前）も一緒に返す。
#[tokio::test]
async fn registration_errors_are_reported_at_the_pen_and_the_target() {
    let (svc, _tags, ids) = service_with_tags(1).await;
    let reg = registrar(&svc.pool);
    let before = row_counts(&svc.pool).await;

    let mut body = payload("  ", "trend", &[ids[0]]);
    body.pens.push(hub_pen("L.G.text", "text", "string"));
    body.hub_tag_group = new_hub_group("hub-g");
    let err = svc.create_with_hub_tags(&reg, &body).await.unwrap_err();
    // `stringLength` は banto-tags が文字列型に言う指摘（「Hub から追加」と同じ）。
    assert_eq!(
        fields(&err),
        [
            "name",
            "pens[1].hubTag.dataType",
            "pens[1].hubTag.stringLength"
        ]
    );

    body.name = "L1".to_owned();
    body.pens[1] = hub_pen("L.G.a", "a", "f32");
    body.hub_tag_group = Some(HubTagGroupPayload {
        collection_group_id: None,
        new_group: Some(NewHubGroup {
            name: "hub-g".to_owned(),
            period_ms: 1234,
        }),
    });
    let err = svc.create_with_hub_tags(&reg, &body).await.unwrap_err();
    assert_eq!(fields(&err), ["hubTagGroup.newGroup.periodMs"]);

    // PLC 接続の収集グループは登録先にできない。
    let plc_group: i64 = sqlx::query_scalar("SELECT id FROM collection_groups LIMIT 1")
        .fetch_one(&svc.pool)
        .await
        .unwrap();
    body.hub_tag_group = Some(HubTagGroupPayload {
        collection_group_id: Some(plc_group),
        new_group: None,
    });
    let err = svc.create_with_hub_tags(&reg, &body).await.unwrap_err();
    assert_eq!(fields(&err), ["hubTagGroup.collectionGroupId"]);
    assert_eq!(row_counts(&svc.pool).await, before);
}

/// 更新の `expectedRevision` が古ければ、Hub のタグを登録する前に断る。
#[tokio::test]
async fn a_stale_revision_registers_nothing() {
    let (svc, _tags, _ids) = service_with_tags(0).await;
    let reg = registrar(&svc.pool);
    let created = svc.create(&payload("L1", "trend", &[])).await.unwrap();
    let before = row_counts(&svc.pool).await;
    let mut edit = payload("L1", "trend", &[]);
    edit.pens.push(hub_pen("L.G.a", "a", "f32"));
    edit.hub_tag_group = new_hub_group("hub-g");
    edit.expected_revision = Some(created.revision + 1);
    let err = svc
        .update_with_hub_tags(&reg, created.id, &edit)
        .await
        .unwrap_err();
    assert!(is_revision_conflict(&err), "{err:?}");
    assert_eq!(row_counts(&svc.pool).await, before);

    let err = svc
        .update_with_hub_tags(&reg, created.id + 100, &edit)
        .await
        .unwrap_err();
    assert!(matches!(err, BantoError::NotFound { .. }), "{err:?}");
    assert_eq!(row_counts(&svc.pool).await, before);
}

/// 同じ外部名を 2 本のペンが指すのは後ろのペンの誤り（何も登録しない）。
#[tokio::test]
async fn the_same_hub_tag_twice_is_an_error_at_the_later_pen() {
    let (svc, _tags, _ids) = service_with_tags(0).await;
    let reg = registrar(&svc.pool);
    let before = row_counts(&svc.pool).await;
    let mut body = payload("L1", "trend", &[]);
    body.pens.push(hub_pen("L.G.a", "a", "f32"));
    body.pens.push(hub_pen("L.G.b", "b", "f32"));
    body.pens.push(hub_pen(" L.G.a", "a2", "f32"));
    body.hub_tag_group = new_hub_group("hub-g");
    let err = svc.create_with_hub_tags(&reg, &body).await.unwrap_err();
    assert_eq!(fields(&err), ["pens[2].hubTag.externalName"]);
    assert_eq!(row_counts(&svc.pool).await, before);
}

/// ペンは `tagId` か `hubTag` のちょうど 1 つ。両方・どちらも無しは `pens[i]`。
#[tokio::test]
async fn a_pen_needs_exactly_one_of_tag_id_and_hub_tag() {
    let (svc, _tags, ids) = service_with_tags(1).await;
    let reg = registrar(&svc.pool);
    let mut both = payload("L1", "trend", &[]);
    let mut pen = hub_pen("L.G.a", "a", "f32");
    pen.tag_id = Some(ids[0]);
    both.pens.push(pen);
    both.hub_tag_group = new_hub_group("hub-g");
    assert_eq!(shape_fields(&both), ["pens[0]"]);
    let err = svc.create_with_hub_tags(&reg, &both).await.unwrap_err();
    assert_eq!(fields(&err), ["pens[0]"]);

    let neither: DisplayGroupPayload = serde_json::from_value(json!({
        "name": "L1", "kind": "trend", "pens": [{ "tagId": ids[0] }, { "colorSlot": 2 }]
    }))
    .unwrap();
    assert_eq!(shape_fields(&neither), ["pens[1]"]);
    let err = svc.create_with_hub_tags(&reg, &neither).await.unwrap_err();
    assert_eq!(fields(&err), ["pens[1]"]);
    assert!(svc.list().await.unwrap().is_empty());
}

/// 束は作成・更新で往復し、空白は除かれ、空は束なしに戻り、更新（PUT）が書いて
/// 版を進める（#1207・#1231 PR 5b）。
#[tokio::test]
async fn bundle_round_trips_trims_and_clears_through_create_and_update() {
    let (svc, _tags, ids) = service_with_tags(1).await;
    let mut p = payload("束あり", "trend", &ids);
    p.bundle = Some(Some("  ライン A  ".to_string()));
    let created = svc.create(&p).await.expect("create");
    assert_eq!(created.bundle.as_deref(), Some("ライン A"));
    assert_eq!(
        svc.get(created.id).await.expect("get").bundle.as_deref(),
        Some("ライン A")
    );
    assert_eq!(
        svc.list().await.expect("list")[0].bundle.as_deref(),
        Some("ライン A")
    );

    p.bundle = Some(Some("ライン B".to_string()));
    p.expected_revision = Some(created.revision);
    let updated = svc.update(created.id, &p).await.expect("update");
    assert_eq!(updated.bundle.as_deref(), Some("ライン B"));
    assert_eq!(updated.revision, created.revision + 1);

    p.bundle = Some(Some("   ".to_string()));
    p.expected_revision = Some(updated.revision);
    let cleared = svc.update(created.id, &p).await.expect("clear");
    assert_eq!(cleared.bundle, None);
}

/// 更新で `bundle` のキーが無い本文は拒否し、保存済みの束は変わらない。
/// `null` は束から外す。作成ではキーなしでよい。
#[tokio::test]
async fn update_requires_bundle_key_but_create_does_not() {
    let (svc, _tags, ids) = service_with_tags(1).await;
    let mut p = payload("束あり", "trend", &ids);
    p.bundle = None;
    let created = svc.create(&p).await.expect("create without bundle");
    assert_eq!(created.bundle, None);

    p.bundle = Some(Some("ライン A".to_string()));
    let with = svc.update(created.id, &p).await.expect("set bundle");

    p.bundle = None;
    p.name = "改名".to_string();
    let err = svc.update(created.id, &p).await.expect_err("refused");
    assert_eq!(fields(&err), vec!["bundle".to_string()]);
    let kept = svc.get(created.id).await.expect("get");
    assert_eq!(kept.bundle.as_deref(), Some("ライン A"));
    assert_eq!(kept.name, "束あり");
    assert_eq!(kept.revision, with.revision);

    // JSON のキーなしと null の区別。
    let absent: DisplayGroupPayload =
        serde_json::from_value(json!({ "name": "x", "kind": "trend" })).expect("absent");
    assert_eq!(absent.bundle, None);
    let null: DisplayGroupPayload =
        serde_json::from_value(json!({ "name": "x", "kind": "trend", "bundle": null }))
            .expect("null");
    assert_eq!(null.bundle, Some(None));

    p.bundle = Some(None);
    let cleared = svc.update(created.id, &p).await.expect("clear");
    assert_eq!(cleared.bundle, None);
}

/// 「束なし」は束に入れないグループの表示名なので、束の名前には使えない。
#[tokio::test]
async fn bundle_name_no_bundle_label_is_reserved() {
    let (svc, _tags, ids) = service_with_tags(1).await;
    let mut p = payload("束なし予約", "trend", &ids);
    p.bundle = Some(Some(format!("  {NO_BUNDLE_LABEL} ")));
    let err = svc.create(&p).await.expect_err("reserved");
    assert_eq!(fields(&err), vec!["bundle".to_string()]);
}

#[tokio::test]
async fn bundle_is_limited_to_fifty_chars() {
    let (svc, _tags, ids) = service_with_tags(1).await;
    let mut p = payload("束の長さ", "trend", &ids);
    p.bundle = Some(Some("あ".repeat(51)));
    let err = svc.create(&p).await.expect_err("51 chars");
    assert_eq!(fields(&err), vec!["bundle".to_string()]);
    p.bundle = Some(Some("あ".repeat(50)));
    let created = svc.create(&p).await.expect("50 chars");
    assert_eq!(created.bundle.map(|b| b.chars().count()), Some(50));
}

// ---- ペンの属性（#1207・#1231 PR 6a） ----

fn pen_json(extra: Value) -> PenPayload {
    let mut v = json!({ "tagId": 1 });
    v.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    serde_json::from_value(v).expect("pen payload")
}

fn pen_fields(extra: Value) -> Vec<String> {
    let mut p = payload("g", "trend", &[]);
    p.pens = vec![pen_json(extra)];
    shape_fields(&p)
}

fn valid_pen(extra: Value) -> Pen {
    let mut p = payload("g", "trend", &[]);
    p.pens = vec![pen_json(extra)];
    validate_shape(&p).expect("valid").pens.remove(0)
}

/// 受ける端・断る端（各項目）。
#[test]
fn pen_attribute_edges() {
    let long_ok = "あ".repeat(64);
    let long_bad = "あ".repeat(65);
    for ok in [
        json!({ "label": long_ok }),
        json!({ "unit": "a".repeat(16) }),
        json!({ "decimals": 0 }),
        json!({ "decimals": 6 }),
        json!({ "lineWidth": 1 }),
        json!({ "lineWidth": 1.5 }),
        json!({ "lineWidth": 4 }),
        json!({ "markerSize": 2 }),
        json!({ "markerSize": 8 }),
        json!({ "yMin": -5.5, "yMax": 5.5 }),
        json!({ "yMin": 3 }),
        json!({ "yMax": -3 }),
        json!({ "lineStyle": "dashDotDot", "marker": "diamond", "axis": "right" }),
    ] {
        assert!(pen_fields(ok.clone()).is_empty(), "{ok}");
    }
    for (bad, field) in [
        (json!({ "label": long_bad }), "label"),
        (json!({ "label": "a\nb" }), "label"),
        (json!({ "unit": "a".repeat(17) }), "unit"),
        (json!({ "unit": "a\u{7}b" }), "unit"),
        (json!({ "decimals": -1 }), "decimals"),
        (json!({ "decimals": 7 }), "decimals"),
        (json!({ "lineWidth": 0.5 }), "lineWidth"),
        (json!({ "lineWidth": 4.5 }), "lineWidth"),
        (json!({ "lineWidth": 1.2 }), "lineWidth"),
        (json!({ "markerSize": 1 }), "markerSize"),
        (json!({ "markerSize": 9 }), "markerSize"),
        (json!({ "yMin": 1, "yMax": 1 }), "yMax"),
        (json!({ "yMin": 2, "yMax": 1 }), "yMax"),
        (json!({ "lineStyle": "dotted" }), "lineStyle"),
        (json!({ "lineStyle": "Solid" }), "lineStyle"),
        (json!({ "marker": "star" }), "marker"),
        (json!({ "axis": "top" }), "axis"),
    ] {
        assert_eq!(
            pen_fields(bad.clone()),
            vec![format!("pens[0].{field}")],
            "{bad}"
        );
    }
}

/// 前後の空白は落とし、空・空白だけは null。
#[test]
fn pen_label_and_unit_are_trimmed_and_blank_is_null() {
    let pen = valid_pen(json!({ "label": "  温度 ", "unit": "   " }));
    assert_eq!(pen.label.as_deref(), Some("温度"));
    assert_eq!(pen.unit, None);
    let pen = valid_pen(json!({ "label": "" }));
    assert_eq!(pen.label, None);
    // 空白を落としたあとで 64 文字ちょうどなら通る。
    let padded = format!("  {}  ", "あ".repeat(64));
    assert_eq!(
        valid_pen(json!({ "label": padded }))
            .label
            .map(|l| l.chars().count()),
        Some(64)
    );
}

/// 全項目ありで保存 → 読み直しで同じ値。キー無しのペンは全部 null で、応答は全キーを出す。
#[tokio::test]
async fn pen_attributes_round_trip_through_the_db() {
    let (svc, _tags, ids) = service_with_tags(2).await;
    let mut p = payload("属性", "trend", &[]);
    p.pens = vec![
        pen_json(json!({
            "tagId": ids[0], "colorSlot": 3, "label": "入口温度", "unit": "℃", "decimals": 2,
            "lineStyle": "dashDot", "lineWidth": 2.5, "marker": "triangle", "markerSize": 6,
            "axis": "right", "yMin": -10.5, "yMax": 99.5
        })),
        pen_json(json!({ "tagId": ids[1] })),
    ];
    let created = svc.create(&p).await.expect("create");
    let got = svc.get(created.id).await.unwrap();
    assert_eq!(got, created);
    let listed = svc.list().await.unwrap();
    assert_eq!(listed[0], created);
    let json = serde_json::to_value(&got.pens).unwrap();
    assert_eq!(
        json[0],
        json!({
            "tagId": ids[0], "colorSlot": 3, "label": "入口温度", "unit": "℃", "decimals": 2,
            "lineStyle": "dashDot", "lineWidth": 2.5, "marker": "triangle", "markerSize": 6,
            "axis": "right", "yMin": -10.5, "yMax": 99.5
        })
    );
    assert_eq!(
        json[1],
        json!({
            "tagId": ids[1], "colorSlot": null, "label": null, "unit": null, "decimals": null,
            "lineStyle": null, "lineWidth": null, "marker": null, "markerSize": null,
            "axis": null, "yMin": null, "yMax": null
        })
    );
    // 応答をそのまま本文に戻して更新しても属性が変わらない。
    let mut back: DisplayGroupPayload =
        serde_json::from_value(serde_json::to_value(&got).unwrap()).unwrap();
    back.expected_revision = Some(got.revision);
    let updated = svc.update(got.id, &back).await.expect("update");
    assert_eq!(updated.pens, got.pens);
    // 本文のキー無しは null に戻る（ペンの並びは丸ごと置き換え）。
    let mut bare = payload("属性", "trend", &ids);
    bare.expected_revision = Some(updated.revision);
    let cleared = svc.update(got.id, &bare).await.expect("update");
    assert!(cleared.pens.iter().all(|pen| pen.label.is_none()
        && pen.line_style.is_none()
        && pen.line_width.is_none()
        && pen.y_max.is_none()));
}

/// migration 0106 が当たり、CHECK が最後の砦として働く（NULL は通る）。
#[tokio::test]
async fn migration_0106_adds_pen_columns_with_checks() {
    let (svc, _tags, ids) = service_with_tags(1).await;
    let created = svc
        .create(&payload("m", "trend", &ids))
        .await
        .expect("create");
    let pool = svc.pool.clone();
    let columns: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('display_group_pens')")
            .fetch_all(&pool)
            .await
            .unwrap();
    for name in [
        "label",
        "unit",
        "decimals",
        "line_style",
        "line_width",
        "marker",
        "marker_size",
        "axis",
        "y_min",
        "y_max",
    ] {
        assert!(columns.iter().any(|c| c == name), "{name}");
    }
    for set in [
        "line_width = 0.5",
        "line_width = 5",
        "decimals = 7",
        "marker_size = 1",
        "line_style = 'dotted'",
        "marker = 'star'",
        "axis = 'top'",
        "label = ''",
        "y_min = 5, y_max = 5",
    ] {
        let res = sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE display_group_pens SET {set} WHERE group_id = ?"
        )))
        .bind(created.id)
        .execute(&pool)
        .await;
        assert!(res.is_err(), "{set} は CHECK で断られる");
    }
    sqlx::query("UPDATE display_group_pens SET y_min = 1, y_max = 2, line_width = 4, axis = 'right' WHERE group_id = ?")
        .bind(created.id)
        .execute(&pool)
        .await
        .expect("端は通る");
}
