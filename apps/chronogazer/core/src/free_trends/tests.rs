//! `crate::free_trends` の単体テスト（検証・上限・版・権限の判定・タグの削除の拒否）。REST の
//! 権限の表は `rest/free_trends_tests.rs`、Tauri の経路は `src-tauri` のテスト。

use super::*;
use crate::display_groups::tests::seed_tags;
use crate::display_groups::{DisplayGroupService, LineStyle};
use banto_tags::TagService;
use serde_json::json;

fn payload(name: &str, tag_ids: &[i64]) -> FreeTrendPayload {
    FreeTrendPayload {
        name: name.to_string(),
        time_window_sec: None,
        pens: tag_ids
            .iter()
            .map(|&id| FreeTrendPenPayload::tag(id))
            .collect(),
        expected_revision: None,
    }
}

/// 試験で使う名前のアカウントと、その `users.id`（`localDesktop` は予約名と同じ名前の古いアカウント）。
/// ID は [`service_with_tags`] がこの値で入れる。
const SEEDED_USERS: [(&str, i64); 4] = [
    ("alice", 101),
    ("bob", 102),
    ("eve", 103),
    ("localDesktop", 104),
];

/// 認証の時点の ID を持つアカウントの主体（REST の stamp・Tauri の `UserIdentity.id` と同じ）。
/// 名前は [`SEEDED_USERS`] の ID に写す（無い名前は `users` に無い ID）。
fn account(username: &str, role: Role) -> FreeTrendActor {
    let user_id = SEEDED_USERS
        .iter()
        .find(|(name, _)| *name == username)
        .map_or(999, |(_, id)| *id);
    account_with_id(username, user_id, role)
}

fn account_with_id(username: &str, user_id: i64, role: Role) -> FreeTrendActor {
    FreeTrendActor::Account {
        user_id,
        username: username.to_string(),
        role,
    }
}

/// `users` にアカウントを足して ID を返す（役割は主体の側で持つので、ここでは既定のまま）。
async fn seed_user(pool: &SqlitePool, username: &str) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO users (username, password_hash, display_name, account_uid)          VALUES (?, 'x', ?, ?) RETURNING id",
    )
    .bind(username)
    .bind(username)
    .bind(format!("uid-{username}"))
    .fetch_one(pool)
    .await
    .expect("seed user")
}

/// `users` に ID を決めてアカウントを足す。
async fn seed_user_with_id(pool: &SqlitePool, username: &str, id: i64) {
    sqlx::query(
        "INSERT INTO users (id, username, password_hash, display_name, account_uid)          VALUES (?, ?, 'x', ?, ?)",
    )
    .bind(id)
    .bind(username)
    .bind(username)
    .bind(format!("uid-{username}-{id}"))
    .execute(pool)
    .await
    .expect("seed user");
}

fn fields(err: BantoError) -> Vec<String> {
    match err {
        BantoError::Validation { field_errors } => {
            field_errors.into_iter().map(|e| e.field).collect()
        }
        other => panic!("検証エラーではない: {other:?}"),
    }
}

async fn service_with_tags(n: usize) -> (FreeTrendService, TagService, Vec<i64>, SqlitePool) {
    let pool = crate::db::init_db_memory().await.expect("init_db_memory");
    let (tags, ids) = seed_tags(&pool, n).await;
    for (username, id) in SEEDED_USERS {
        seed_user_with_id(&pool, username, id).await;
    }
    (FreeTrendService::new(pool.clone()), tags, ids, pool)
}

#[test]
fn shape_checks_name_window_pens_and_reports_all_errors() {
    // 64 文字はよい、65 文字は誤り。前後の空白は除く。
    let ok = validate_shape(&payload(&format!("  {}  ", "あ".repeat(64)), &[1])).unwrap();
    assert_eq!(ok.name.chars().count(), 64);
    assert_eq!(ok.time_window_sec, DEFAULT_TREND_TIME_WINDOW_SEC);
    assert_eq!(
        validate_shape(&payload(&"あ".repeat(65), &[])).unwrap_err()[0].field,
        "name"
    );
    assert_eq!(
        validate_shape(&payload("   ", &[])).unwrap_err()[0].field,
        "name"
    );

    // 時間窓は表示グループと同じ候補から 1 つ。数でないものも `field_errors`。
    for (value, good) in [
        (json!(60), true),
        (json!(3600), true),
        (Value::Null, true),
        (json!(120), false),
        (json!("600"), false),
    ] {
        let mut p = payload("t", &[]);
        p.time_window_sec = Some(value.clone());
        assert_eq!(validate_shape(&p).is_ok(), good, "{value}");
        if !good {
            assert_eq!(validate_shape(&p).unwrap_err()[0].field, "timeWindowSec");
        }
    }

    // ペン 16 本はよい、17 本は誤り。
    let ids: Vec<i64> = (1..=17).collect();
    assert!(validate_shape(&payload("t", &ids[..16])).is_ok());
    assert_eq!(
        validate_shape(&payload("t", &ids)).unwrap_err()[0].field,
        "pens"
    );

    // 重複・tagId 無し・色の枠・属性の範囲（表示グループと同じ検証）を全部並べる。
    let mut p = payload("", &[1, 1]);
    p.pens.push(FreeTrendPenPayload::default());
    p.pens[0].color_slot = Some(17);
    p.pens[0].line_width = Some(5.0);
    p.pens[0].limit_h = Some(10.0);
    p.pens[0].limit_hh = Some(5.0);
    p.pens[1].bit_band_height = Some(40);
    let got: Vec<String> = validate_shape(&p)
        .unwrap_err()
        .into_iter()
        .map(|e| e.field)
        .collect();
    for want in [
        "name",
        "pens[0].colorSlot",
        "pens[0].lineWidth",
        "pens[0].limitHh",
        "pens[1].tagId",
        "pens[1].bitBandHeight",
        "pens[2].tagId",
    ] {
        assert!(got.iter().any(|f| f == want), "{want} が無い: {got:?}");
    }
}

#[test]
fn pen_payload_ignores_hub_tag_and_requires_tag_id() {
    // `hubTag` は本文の型に無い（読み捨てる）ので、tagId の無いペンとして断る。
    let p: FreeTrendPayload = serde_json::from_value(json!({
        "name": "t",
        "pens": [{ "hubTag": { "name": "x" } }],
    }))
    .unwrap();
    assert_eq!(validate_shape(&p).unwrap_err()[0].field, "pens[0].tagId");
}

#[test]
fn actor_mapping_uses_the_session_origin_not_the_name() {
    // grant の無いセッションはアカウント（名前が予約名でも）。
    assert_eq!(
        FreeTrendActor::from_session("localDesktop", "viewer", None, Some(104)),
        account("localDesktop", Role::Viewer)
    );
    assert_eq!(
        FreeTrendActor::from_session("localDesktop", "editor", Some("localDesktop"), None),
        FreeTrendActor::LocalDesktop { role: Role::Editor }
    );
    assert_eq!(
        FreeTrendActor::from_session("public", "viewer", Some("publicViewer"), None),
        FreeTrendActor::ReadOnly
    );
    assert_eq!(
        FreeTrendActor::from_session("someone", "superuser", None, Some(1)),
        FreeTrendActor::ReadOnly
    );
    assert!(account("a", Role::Viewer).can_create());
    assert!(FreeTrendActor::LocalDesktop { role: Role::Viewer }.can_create());
    assert!(!FreeTrendActor::ReadOnly.can_create());
}

/// オーナーの P2（2026-11-20）: REST のアカウントの主体は stamp の ID をそのまま使い、名前から引き
/// 直さない。stamp の無いアカウントのセッションは名前で補わず開くだけ。
#[test]
fn from_session_takes_the_stamp_account_id_and_refuses_a_session_without_one() {
    // 名前が別のアカウントの名前でも、ID は stamp の物。
    assert_eq!(
        FreeTrendActor::from_session("alice", "viewer", None, Some(7)),
        account_with_id("alice", 7, Role::Viewer)
    );
    assert_eq!(
        FreeTrendActor::from_session("alice", "editor", None, Some(101)),
        account_with_id("alice", 101, Role::Editor)
    );
    let unstamped = FreeTrendActor::from_session("alice", "admin", None, None);
    assert_eq!(unstamped, FreeTrendActor::ReadOnly);
    assert!(!unstamped.can_create());
    // grant のセッションは stamp を見ない（`localDesktop` は ID なしで書ける）。
    assert_eq!(
        FreeTrendActor::from_session("localDesktop", "viewer", Some("localDesktop"), Some(3)),
        FreeTrendActor::LocalDesktop { role: Role::Viewer }
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn crud_round_trips_every_pen_attribute_and_checks_the_db() {
    let (service, _tags, ids, _pool) = service_with_tags(2).await;
    let alice = account("alice", Role::Viewer);

    let mut p = payload("炉", &ids);
    p.time_window_sec = Some(json!(1800));
    p.pens[0] = serde_json::from_value(json!({
        "tagId": ids[0], "colorSlot": 9, "label": "温度", "unit": "℃", "decimals": 2,
        "lineStyle": "dashDot", "lineWidth": 2.5, "marker": "diamond", "markerSize": 6,
        "axis": "right", "yMin": 0.0, "yMax": 100.0, "limitLines": true,
        "limitHh": 90.0, "limitH": 80.0, "limitL": 10.0, "limitLl": 5.0,
        "bitStyle": "band", "bitBandHeight": 20,
    }))
    .unwrap();
    let created = service.create(&alice, &p).await.expect("create");
    assert_eq!(created.revision, 1);
    assert_eq!(created.time_window_sec, 1800);
    assert_eq!(created.created_by, "alice");
    assert_eq!(created.updated_by, "alice");
    assert!(created.can_edit);
    let pen = &created.pens[0];
    assert_eq!(pen.color_slot, Some(9));
    assert_eq!(pen.line_style, Some(LineStyle::DashDot));
    assert_eq!(pen.limit_ll, Some(5.0));
    assert_eq!(pen.bit_band_height, Some(20));
    // 2 本目は全部既定（キー無し = null）で、応答は全部のキーを出す。
    let second = serde_json::to_value(&created.pens[1]).unwrap();
    assert_eq!(second.as_object().unwrap().len(), 19);
    assert!(second["label"].is_null() && second["bitStyle"].is_null());
    assert_eq!(service.get(&alice, created.id).await.unwrap(), created);

    // 名前の重複・存在しないタグ。
    let err = service
        .create(&alice, &payload("炉", &[]))
        .await
        .unwrap_err();
    assert_eq!(fields(err), vec!["name"]);
    let err = service
        .create(&alice, &payload("別", &[9999]))
        .await
        .unwrap_err();
    assert_eq!(fields(err), vec!["pens[0].tagId"]);

    // 更新（版を進め、ペンは丸ごと置き換え）→ 古い版は 409 の形 → 404。
    let mut edit = payload("炉 2", &ids[1..]);
    edit.expected_revision = Some(created.revision);
    let updated = service.update(&alice, created.id, &edit).await.unwrap();
    assert_eq!(updated.revision, 2);
    assert_eq!(updated.pens.len(), 1);
    assert_eq!(updated.pens[0].tag_id, ids[1]);
    let err = service.update(&alice, created.id, &edit).await.unwrap_err();
    assert!(crate::revision::is_revision_conflict(&err), "{err:?}");
    assert!(matches!(
        service.update(&alice, 9999, &edit).await,
        Err(BantoError::NotFound { .. })
    ));
    // 別の行と同じ名前への変更は誤り。
    let other = service.create(&alice, &payload("他", &[])).await.unwrap();
    let mut clash = payload("他", &[]);
    clash.expected_revision = Some(updated.revision);
    assert_eq!(
        fields(
            service
                .update(&alice, created.id, &clash)
                .await
                .unwrap_err()
        ),
        vec!["name"]
    );

    // 一覧は名前順。削除でペンも消える。
    let names: Vec<String> = service
        .list(&alice)
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.name)
        .collect();
    assert_eq!(names, vec!["他", "炉 2"]);
    service.delete(&alice, created.id).await.unwrap();
    service.delete(&alice, other.id).await.unwrap();
    assert!(service.list(&alice).await.unwrap().is_empty());
    let pens: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM free_trend_pens")
        .fetch_one(&service.pool)
        .await
        .unwrap();
    assert_eq!(pens, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ownership_decides_who_can_change_and_can_edit_follows_it() {
    let (service, _tags, ids, _pool) = service_with_tags(1).await;
    let alice = account("alice", Role::Viewer);
    let bob = account("bob", Role::Viewer);
    let editor = account("eve", Role::Editor);
    let local = FreeTrendActor::LocalDesktop { role: Role::Viewer };
    // 予約名と同じ名前の古いアカウント（grant の作った物を自分の物にできてはいけない）。
    let impostor = account("localDesktop", Role::Viewer);

    let mine = service.create(&alice, &payload("A", &ids)).await.unwrap();
    // 他人（viewer）は開けるが変えられない。
    let seen = service.get(&bob, mine.id).await.unwrap();
    assert!(!seen.can_edit);
    let edit = payload("A改", &ids);
    assert!(matches!(
        service.update(&bob, mine.id, &edit).await,
        Err(BantoError::Forbidden)
    ));
    assert!(matches!(
        service.delete(&bob, mine.id).await,
        Err(BantoError::Forbidden)
    ));
    // 権限の判定は版の確かめより先（他人には 409 ではなく 403）。
    let mut stale = edit.clone();
    stale.expected_revision = Some(99);
    assert!(matches!(
        service.update(&bob, mine.id, &stale).await,
        Err(BantoError::Forbidden)
    ));
    // editor は他人の物も変えられ、最後に変えた人が残る。
    assert!(service.get(&editor, mine.id).await.unwrap().can_edit);
    let changed = service.update(&editor, mine.id, &edit).await.unwrap();
    assert_eq!(changed.created_by, "alice");
    assert_eq!(changed.updated_by, "eve");
    // 作った人は自分の物を変えられる（editor が変えた後も）。
    let mut again = payload("A改2", &ids);
    again.expected_revision = Some(changed.revision);
    service.update(&alice, mine.id, &again).await.unwrap();

    // 開くだけの主体は作れない・変えられない・canEdit は false。
    assert!(matches!(
        service
            .create(&FreeTrendActor::ReadOnly, &payload("R", &[]))
            .await,
        Err(BantoError::Forbidden)
    ));
    assert!(
        !service
            .get(&FreeTrendActor::ReadOnly, mine.id)
            .await
            .unwrap()
            .can_edit
    );
    assert!(matches!(
        service.delete(&FreeTrendActor::ReadOnly, mine.id).await,
        Err(BantoError::Forbidden)
    ));

    // この PC からの接続は作れて、作った人は localDesktop。別の localDesktop のセッションも
    // 同じ主体として変えられる。同じ名前のアカウントは別人。
    let by_local = service.create(&local, &payload("L", &[])).await.unwrap();
    assert_eq!(by_local.created_by, "localDesktop");
    assert!(by_local.can_edit);
    assert!(!service.get(&impostor, by_local.id).await.unwrap().can_edit);
    assert!(matches!(
        service.delete(&impostor, by_local.id).await,
        Err(BantoError::Forbidden)
    ));
    assert!(!service.get(&local, mine.id).await.unwrap().can_edit);
    let other_local = FreeTrendActor::LocalDesktop { role: Role::Viewer };
    service.delete(&other_local, by_local.id).await.unwrap();
    // その逆: アカウント「localDesktop」の作った物を localDesktop の grant は変えられない。
    let by_impostor = service.create(&impostor, &payload("I", &[])).await.unwrap();
    assert!(!service.get(&local, by_impostor.id).await.unwrap().can_edit);
    // editor の役割の localDesktop は他人の物も変えられる。
    assert!(
        service
            .get(
                &FreeTrendActor::LocalDesktop { role: Role::Editor },
                mine.id
            )
            .await
            .unwrap()
            .can_edit
    );
}

/// オーナーの P2（2026-11-20）: 作った人はアカウントの ID で見分ける。viewer の alice が作る →
/// alice を消す → 同じ名前で作り直す。新しい alice は名前が同じでも別人（変更・削除は `Forbidden`、
/// `canEdit` は false）、editor は変えられる。消した ID の主体（認証の後に消されたアカウント）は書けない。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_recreated_account_with_the_same_name_is_not_the_owner() {
    let (service, _tags, ids, pool) = service_with_tags(1).await;
    let alice = account("alice", Role::Viewer);
    let mine = service.create(&alice, &payload("A", &ids)).await.unwrap();
    let old_id: i64 = sqlx::query_scalar("SELECT id FROM users WHERE username = 'alice'")
        .fetch_one(&pool)
        .await
        .unwrap();
    let stored: (String, Option<i64>, String) = sqlx::query_as(
        "SELECT created_by_kind, created_by_user_id, created_by FROM free_trends WHERE id = ?",
    )
    .bind(mine.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        stored,
        ("account".to_string(), Some(old_id), "alice".to_string())
    );
    assert!(service.get(&alice, mine.id).await.unwrap().can_edit);

    // 消した直後: 消した ID の主体は書けない（作った物の ID と一致していても）。
    sqlx::query("DELETE FROM users WHERE id = ?")
        .bind(old_id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(!service.get(&alice, mine.id).await.unwrap().can_edit);
    assert!(matches!(
        service.create(&alice, &payload("B", &[])).await,
        Err(BantoError::Forbidden)
    ));

    // 同じ名前で作り直す: ID は再利用されない（AUTOINCREMENT）。
    let new_id = seed_user(&pool, "alice").await;
    assert_ne!(new_id, old_id);
    let new_alice = account_with_id("alice", new_id, Role::Viewer);
    let edit = payload("A改", &ids);
    for actor in [alice.clone(), new_alice.clone()] {
        assert!(!service.get(&actor, mine.id).await.unwrap().can_edit);
        assert!(!service.list(&actor).await.unwrap()[0].can_edit);
        assert!(matches!(
            service.update(&actor, mine.id, &edit).await,
            Err(BantoError::Forbidden)
        ));
        assert!(matches!(
            service.delete(&actor, mine.id).await,
            Err(BantoError::Forbidden)
        ));
    }
    // 新しい alice の作った物は新しい alice の物。
    let theirs = service
        .create(&new_alice, &payload("N", &[]))
        .await
        .unwrap();
    assert!(theirs.can_edit);
    // editor は消えたアカウントの物も変えられ、表示用の名前は当時のまま残る。
    let editor = account("eve", Role::Editor);
    assert!(service.get(&editor, mine.id).await.unwrap().can_edit);
    let changed = service.update(&editor, mine.id, &edit).await.unwrap();
    assert_eq!(changed.created_by, "alice");
    service.delete(&editor, mine.id).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_201st_free_trend_is_refused() {
    let (service, _tags, _ids, pool) = service_with_tags(0).await;
    for i in 0..MAX_FREE_TRENDS {
        sqlx::query(
            "INSERT INTO free_trends \
             (name, created_by_kind, created_by_user_id, created_by, updated_by) \
             VALUES (?, 'account', 101, 'alice', 'alice')",
        )
        .bind(format!("t{i}"))
        .execute(&pool)
        .await
        .unwrap();
    }
    let alice = account("alice", Role::Viewer);
    let err = service
        .create(&alice, &payload("t200", &[]))
        .await
        .unwrap_err();
    assert_eq!(fields(err), vec![LIMIT_FIELD]);
    // 上限でも更新はできる（作成だけを数える）。
    let id: i64 = sqlx::query_scalar("SELECT id FROM free_trends WHERE name = 't0'")
        .fetch_one(&pool)
        .await
        .unwrap();
    service
        .update(&alice, id, &payload("t0 改", &[]))
        .await
        .expect("update at the limit");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tag_used_by_a_free_trend_cannot_be_deleted() {
    let (service, tags, ids, pool) = service_with_tags(2).await;
    let groups = DisplayGroupService::new(pool.clone());
    let alice = account("alice", Role::Viewer);
    let trend = service.create(&alice, &payload("炉", &ids)).await.unwrap();

    let err = groups
        .delete_tag_unless_referenced(&tags, ids[0])
        .await
        .unwrap_err();
    let BantoError::Validation { field_errors } = err else {
        panic!("検証エラーではない");
    };
    assert_eq!(field_errors.len(), 1);
    assert_eq!(field_errors[0].field, TAG_REFERENCED_FIELD);
    assert!(
        field_errors[0].message.contains("「炉」"),
        "{}",
        field_errors[0].message
    );

    // 表示グループにも割り当てると、両方の誤りを並べる。
    groups
        .create(&crate::display_groups::tests::payload(
            "G",
            "trend",
            &ids[..1],
        ))
        .await
        .unwrap();
    let err = groups
        .delete_tag_unless_referenced(&tags, ids[0])
        .await
        .unwrap_err();
    assert_eq!(
        fields(err),
        vec![
            crate::display_groups::TAG_REFERENCED_FIELD.to_string(),
            TAG_REFERENCED_FIELD.to_string()
        ]
    );

    // ペンから外せば消せる。
    let mut edit = payload("炉", &ids[..1]);
    edit.expected_revision = Some(trend.revision);
    service.update(&alice, trend.id, &edit).await.unwrap();
    groups
        .delete_tag_unless_referenced(&tags, ids[1])
        .await
        .expect("参照の無いタグは消せる");
}
