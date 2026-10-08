//! `crate::tag_thresholds` の単体テスト（検証・CRUD・楽観ロック・`BEGIN IMMEDIATE`・
//! タグの削除との連動・収集に渡す形）。REST の経路は `rest::tag_thresholds` の
//! テスト、Tauri の経路は `src-tauri` のテスト。

use super::*;
use crate::display_groups::tests::seed_tags;
use crate::display_groups::DisplayGroupService;
use crate::revision::is_revision_conflict;
use banto_tags::TagService;

async fn service_with_tags(n: usize) -> (TagThresholdService, TagService, Vec<i64>, SqlitePool) {
    let pool = crate::db::init_db_memory().await.expect("init_db_memory");
    let (tags, ids) = seed_tags(&pool, n).await;
    (TagThresholdService::new(pool.clone()), tags, ids, pool)
}

fn payload(
    ll: Option<f64>,
    l: Option<f64>,
    h: Option<f64>,
    hh: Option<f64>,
    expected_revision: Option<i64>,
) -> TagThresholdsPayload {
    TagThresholdsPayload {
        threshold_ll: ll,
        threshold_l: l,
        threshold_h: h,
        threshold_hh: hh,
        expected_revision,
    }
}

fn fields(err: &BantoError) -> Vec<String> {
    match err {
        BantoError::Validation { field_errors } => {
            field_errors.iter().map(|fe| fe.field.clone()).collect()
        }
        other => panic!("検証エラーではない: {other:?}"),
    }
}

/// 検証の総当たり（純関数）: 大小関係は設定されたものだけ比べ、崩れた側の欄に
/// 出る（banto-tags と同じ規則・同じフィールド名）。文字列タグには付けられず、
/// 非有限の数は断る。全部空は通る（設定なし）。
#[test]
fn validation_table() {
    let ok = [
        payload(None, None, None, None, None),
        payload(Some(0.0), Some(10.0), Some(80.0), Some(90.0), None),
        payload(None, None, Some(80.0), None, None),
        payload(Some(5.0), None, None, Some(5.0), None),
        payload(Some(1.0), Some(1.0), Some(1.0), Some(1.0), None),
    ];
    for p in ok {
        assert!(validate_thresholds_for("u16", &p).is_ok(), "{p:?}");
    }
    let bad: [(TagThresholdsPayload, &str, Vec<&str>); 5] = [
        (
            payload(None, Some(50.0), Some(10.0), None, None),
            "u16",
            vec!["thresholdH"],
        ),
        (
            payload(Some(9.0), None, None, Some(1.0), None),
            "u16",
            vec!["thresholdHh"],
        ),
        (
            payload(None, None, Some(80.0), None, None),
            "string",
            vec!["thresholdH"],
        ),
        (
            payload(Some(f64::NAN), None, Some(f64::INFINITY), None, None),
            "f32",
            vec!["thresholdLl", "thresholdH"],
        ),
        (
            payload(Some(1.0), Some(2.0), None, None, None),
            "string",
            vec!["thresholdLl", "thresholdL"],
        ),
    ];
    for (p, data_type, expected) in bad {
        let err = validate_thresholds_for(data_type, &p).expect_err("通ってしまった");
        assert_eq!(fields(&err), expected, "{p:?} / {data_type}");
    }
}

/// 既定は設定なし: 行が無いタグは 4 つとも `None`・版 0。一覧には出ない。
/// タグが無ければ `NotFound`（読み取りも書き込みも）。
#[tokio::test]
async fn unset_tags_read_as_no_thresholds_and_missing_tags_are_not_found() {
    let (svc, _tags, ids, _pool) = service_with_tags(1).await;
    assert_eq!(svc.get(ids[0]).await.unwrap(), TagThresholds::unset(ids[0]));
    assert!(svc.list().await.unwrap().is_empty());
    assert!(matches!(
        svc.get(9999).await,
        Err(BantoError::NotFound { .. })
    ));
    assert!(matches!(
        svc.update(9999, &payload(None, None, Some(1.0), None, None))
            .await,
        Err(BantoError::NotFound { .. })
    ));
}

/// 保存・読み直し・全項目置換・楽観ロック。版は行が無いとき 0、保存のたびに +1。
/// 古い版の保存は `expectedRevision` の検証エラー（REST は 409）で、何も変えない。
/// 全部消した保存も行を残して版を進める（消して作り直すと版が 0 に戻り、古い画面の
/// 保存が通ってしまうため）。
#[tokio::test]
async fn update_replaces_all_fields_and_enforces_the_expected_revision() {
    let (svc, _tags, ids, pool) = service_with_tags(2).await;

    let first = svc
        .update(
            ids[0],
            &payload(Some(0.0), Some(10.0), Some(80.0), Some(90.0), Some(0)),
        )
        .await
        .expect("版 0 で最初の保存");
    assert_eq!(first.tag_name, "tag0");
    assert_eq!(first.thresholds.revision, 1);
    assert_eq!(first.thresholds.threshold_h, Some(80.0));
    assert_eq!(svc.get(ids[0]).await.unwrap(), first.thresholds);

    // 古い版（0）では保存できず、値は変わらない。
    let err = svc
        .update(ids[0], &payload(None, None, Some(70.0), None, Some(0)))
        .await
        .expect_err("古い版で保存できてしまった");
    assert!(is_revision_conflict(&err), "{err:?}");
    assert_eq!(svc.get(ids[0]).await.unwrap(), first.thresholds);

    // 置換: 省略（None）した欄は消える。
    let second = svc
        .update(ids[0], &payload(None, None, Some(70.0), None, Some(1)))
        .await
        .unwrap();
    assert_eq!(second.thresholds.revision, 2);
    assert_eq!(
        (
            second.thresholds.threshold_ll,
            second.thresholds.threshold_l,
            second.thresholds.threshold_h,
            second.thresholds.threshold_hh
        ),
        (None, None, Some(70.0), None)
    );

    // 検証エラーは保存しない（版も進めない）。
    let err = svc
        .update(
            ids[0],
            &payload(None, Some(90.0), Some(70.0), None, Some(2)),
        )
        .await
        .expect_err("大小関係が崩れたまま保存できてしまった");
    assert_eq!(fields(&err), ["thresholdH"]);
    assert_eq!(svc.get(ids[0]).await.unwrap().revision, 2);

    // 全部消す: 行は残り、版が進む。収集に渡す表からは外れる。
    let cleared = svc
        .update(ids[0], &payload(None, None, None, None, Some(2)))
        .await
        .unwrap();
    assert!(cleared.thresholds.is_empty());
    assert_eq!(cleared.thresholds.revision, 3);
    assert_eq!(svc.list().await.unwrap(), vec![cleared.thresholds.clone()]);
    let err = svc
        .update(ids[0], &payload(None, None, Some(1.0), None, Some(0)))
        .await
        .expect_err("消した後に版 0 で保存できてしまった");
    assert!(is_revision_conflict(&err));

    // 版を省略した保存は確かめない（後勝ち）。
    svc.update(ids[1], &payload(None, None, Some(5.0), None, None))
        .await
        .expect("版の省略");
    let map = collect_thresholds(&pool).await.unwrap();
    assert_eq!(
        map,
        HashMap::from([(
            ids[1],
            banto_collect::Thresholds {
                hh: None,
                h: Some(5.0),
                l: None,
                ll: None,
            }
        )])
    );
}

/// タグを消すと、そのタグのしきい値の行も同じトランザクションで消える。参照されて
/// いて削除を断られたタグの行は残る。
#[tokio::test]
async fn deleting_a_tag_deletes_its_thresholds_in_the_same_transaction() {
    let (svc, tags, ids, pool) = service_with_tags(2).await;
    for &id in &ids {
        svc.update(id, &payload(None, None, Some(1.0), None, None))
            .await
            .unwrap();
    }
    let groups = DisplayGroupService::new(pool.clone());
    groups
        .create(&crate::display_groups::tests::payload(
            "ライン",
            "digital",
            &ids[1..],
        ))
        .await
        .unwrap();

    groups
        .delete_tag_unless_referenced(&tags, ids[0])
        .await
        .expect("参照されていないタグは消せる");
    groups
        .delete_tag_unless_referenced(&tags, ids[1])
        .await
        .expect_err("参照されているタグが消せてしまった");

    let remaining: Vec<i64> = sqlx::query_scalar("SELECT tag_id FROM recorder_tag_settings")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(remaining, vec![ids[1]], "消したタグの行だけが消える");
}

/// 「版の確認 → 書き込み」は `BEGIN IMMEDIATE` の中（読んでから書くトランザク
/// ション、チェックリスト §5）: 同じ版を持った 2 つの保存を同時に走らせると、
/// 片方だけが通り、もう片方は**版の食い違い**で断られる（ロックの取り合いの
/// 失敗や後勝ちにならない）。複数の接続で同時に書けるよう on-disk の DB。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_saves_with_the_same_revision_let_exactly_one_win() {
    let dir = crate::test_support::TempDir::new();
    let pool = crate::db::init_db(dir.path().join("th.sqlite3"))
        .await
        .expect("init_db");
    let (_tags, ids) = seed_tags(&pool, 1).await;
    let svc = TagThresholdService::new(pool.clone());
    for round in 0..20 {
        let revision = svc.get(ids[0]).await.unwrap().revision;
        let (a, b) = (svc.clone(), svc.clone());
        let id = ids[0];
        let first = tokio::spawn(async move {
            a.update(
                id,
                &payload(None, None, Some(round as f64), None, Some(revision)),
            )
            .await
        });
        let second = tokio::spawn(async move {
            b.update(id, &payload(None, Some(-1.0), None, None, Some(revision)))
                .await
        });
        let (first, second) = (first.await.unwrap(), second.await.unwrap());
        assert!(
            first.is_ok() != second.is_ok(),
            "round {round}: どちらか一方だけが通る（{first:?} / {second:?}）"
        );
        let loser = first.err().or(second.err()).unwrap();
        assert!(
            is_revision_conflict(&loser),
            "round {round}: 負けた側は版の食い違いで断られる: {loser:?}"
        );
        assert_eq!(svc.get(ids[0]).await.unwrap().revision, revision + 1);
    }
    pool.close().await;
}
