use super::super::*;
use super::common::{test_env, test_env_with_clock, TestEnv, CLIENT_HEADER};
use axum::body::Body;
use axum::http::Request as HttpRequest;
use banto_tstore::{Clock, ManualClock};
use tower::ServiceExt;

// --- T19 S2-d (docs/banto-hub-t19-design.md §5.1、UX-39): 履歴の保持
// 期間 - GET/PUT /api/store-settings と POST /api/store-settings/
// prune-preview・prune-now -------------------------------------------

/// `env`が使う実際のtstoreデータディレクトリ（`test_manager_with_clock`
/// が`dir.path().join("data")`を`CollectorManager`へ渡している - この
/// テストではファイルを直接置く/確認するために同じパスを再現する）。
fn store_settings_test_data_dir(env: &TestEnv) -> std::path::PathBuf {
    env._dir.path().join("data")
}

fn touch_data_file(data_dir: &std::path::Path, date: LocalDate, seq: u32) -> std::path::PathBuf {
    std::fs::create_dir_all(data_dir).expect("create data dir");
    let path = data_dir.join(format!("{}-{:03}.sqlite3", date.to_yyyymmdd(), seq));
    std::fs::write(&path, b"").expect("touch data file");
    path
}

/// `GET/PUT /api/store-settings`: admin 限定（viewer は403）、既定値
/// （`Some(7)`）、`null`（無制限）を含めた往復、バリデーション範囲
/// （1〜3650、実装指示どおり）を確認する。
#[tokio::test]
async fn store_settings_round_trips_requires_admin_and_validates_range() {
    let env = test_env().await;

    let get = |token: String| {
        let router = env.router.clone();
        async move {
            let request = HttpRequest::builder()
                .method("GET")
                .uri("/api/store-settings")
                .header("Authorization", format!("Bearer {token}"))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap();
            router.oneshot(request).await.unwrap()
        }
    };

    let response = get(env.viewer_token.clone()).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let response = get(env.admin_token.clone()).await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["retentionDays"], 7);

    let put = |token: String, value: serde_json::Value| {
        let router = env.router.clone();
        async move {
            let request = HttpRequest::builder()
                .method("PUT")
                .uri("/api/store-settings")
                .header("Authorization", format!("Bearer {token}"))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({ "retentionDays": value })).unwrap(),
                ))
                .unwrap();
            router.oneshot(request).await.unwrap()
        }
    };

    // viewer は書けない。
    let response = put(env.viewer_token.clone(), serde_json::json!(30)).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    // バリデーション: 0以下・上限超過は422。
    for bad in [
        serde_json::json!(0),
        serde_json::json!(-5),
        serde_json::json!(3651),
    ] {
        let response = put(env.admin_token.clone(), bad.clone()).await;
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "retentionDays={bad:?} should be rejected"
        );
    }

    // 正常な保存 → 再取得で往復する。
    let response = put(env.admin_token.clone(), serde_json::json!(30)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let applied: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(applied["retentionDays"], 30);

    let response = get(env.admin_token.clone()).await;
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let refetched: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(refetched["retentionDays"], 30);

    // UX-39: 無制限（null）も保存・読み戻しできる。
    let response = put(env.admin_token.clone(), serde_json::Value::Null).await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let applied: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(applied["retentionDays"], serde_json::Value::Null);

    let response = get(env.admin_token.clone()).await;
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let refetched: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(refetched["retentionDays"], serde_json::Value::Null);

    // `resource = 'store_settings'`は成功した更新（action='update'）に
    // 加えて、viewer の403（`require_session_role`が同じ`resource`名で
    // 記録する`action='denied'`エントリ）も含む - ここでは「保存が
    // 何回成功したか」だけを見るため`action='update'`に絞る。
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT action, resource, result FROM audit_log WHERE resource = 'store_settings' AND action = 'update' ORDER BY id",
    )
    .fetch_all(&env.pool)
    .await
    .unwrap();
    // 正常な PUT（30 → null）の2件だけが記録される（バリデーション
    // エラーになった3件・viewer の403は記録されない）。
    assert_eq!(rows.len(), 2);
    for row in rows {
        assert_eq!(
            row,
            (
                "update".to_string(),
                "store_settings".to_string(),
                "ok".to_string()
            )
        );
    }
}

/// オーナー決定1の核心: **`PUT /api/store-settings` は保存するだけで、
/// 履歴ファイルを一切削除しない**。保持日数を1日に絞っても、既存の
/// 古いデータファイルはディスク上に残ることを確認する。
#[tokio::test]
async fn store_settings_put_never_deletes_history_files() {
    let today = LocalDate::new(2026, 7, 12);
    let now_ms = today.to_days_since_epoch() * 86_400_000 + 12 * 3_600_000;
    let clock: Arc<dyn Clock> = Arc::new(ManualClock::new(now_ms, 0));
    let env = test_env_with_clock(clock).await;

    let data_dir = store_settings_test_data_dir(&env);
    let old_file = touch_data_file(&data_dir, LocalDate::new(2026, 6, 1), 1);
    let today_file = touch_data_file(&data_dir, today, 1);

    // 保持日数を1日という厳しい値に変更しても、PUT 自体は削除しない。
    let put = HttpRequest::builder()
        .method("PUT")
        .uri("/api/store-settings")
        .header("Authorization", format!("Bearer {}", env.admin_token))
        .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({ "retentionDays": 1 })).unwrap(),
        ))
        .unwrap();
    let response = env.router.clone().oneshot(put).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    assert!(old_file.exists(), "PUT must not prune the old file");
    assert!(today_file.exists());
}

/// `POST /api/store-settings/prune-preview`・`prune-now`が**保存済みの**
/// 保持方針で動くこと、preview は削除しないこと、prune-now は実際に
/// 削除して監査ログに残すことを確認する（admin 限定も併せて確認）。
#[tokio::test]
async fn store_settings_prune_preview_and_prune_now_use_saved_policy() {
    let today = LocalDate::new(2026, 7, 12);
    let now_ms = today.to_days_since_epoch() * 86_400_000 + 12 * 3_600_000;
    let clock: Arc<dyn Clock> = Arc::new(ManualClock::new(now_ms, 0));
    let env = test_env_with_clock(clock).await;

    let data_dir = store_settings_test_data_dir(&env);
    // 7日保持のとき、30日以上前のファイルは削除対象、当日ファイルは
    // 対象外 - `banto-tstore`の`prune_files`単体テストと同じ境界判定。
    let old_file = touch_data_file(&data_dir, LocalDate::new(2026, 6, 1), 1);
    let today_file = touch_data_file(&data_dir, today, 1);

    let put = HttpRequest::builder()
        .method("PUT")
        .uri("/api/store-settings")
        .header("Authorization", format!("Bearer {}", env.admin_token))
        .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({ "retentionDays": 7 })).unwrap(),
        ))
        .unwrap();
    let response = env.router.clone().oneshot(put).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // viewer は preview/prune-now どちらも403。
    for path in [
        "/api/store-settings/prune-preview",
        "/api/store-settings/prune-now",
    ] {
        let request = HttpRequest::builder()
            .method("POST")
            .uri(path)
            .header("Authorization", format!("Bearer {}", env.viewer_token))
            .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
            .body(Body::empty())
            .unwrap();
        let response = env.router.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "path={path}");
    }

    // preview は件数を返すだけで削除しない。
    let preview = HttpRequest::builder()
        .method("POST")
        .uri("/api/store-settings/prune-preview")
        .header("Authorization", format!("Bearer {}", env.admin_token))
        .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
        .body(Body::empty())
        .unwrap();
    let response = env.router.clone().oneshot(preview).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["wouldDeleteCount"], 1);
    assert!(old_file.exists(), "preview must not delete anything");

    // prune-now は実際に削除し、削除数を返し、監査ログに残す。
    let prune_now = HttpRequest::builder()
        .method("POST")
        .uri("/api/store-settings/prune-now")
        .header("Authorization", format!("Bearer {}", env.admin_token))
        .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
        .body(Body::empty())
        .unwrap();
    let response = env.router.clone().oneshot(prune_now).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["deletedCount"], 1);
    assert!(
        !old_file.exists(),
        "prune-now must delete the aged-out file"
    );
    assert!(today_file.exists(), "today's file must survive");

    let rows: Vec<(String, String, String, Option<String>)> = sqlx::query_as(
        "SELECT action, resource, result, detail FROM audit_log WHERE resource = 'store_settings_prune' ORDER BY id",
    )
    .fetch_all(&env.pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, "delete");
    assert_eq!(rows[0].1, "store_settings_prune");
    assert_eq!(rows[0].2, "ok");
    let detail: serde_json::Value = serde_json::from_str(rows[0].3.as_deref().unwrap()).unwrap();
    assert_eq!(detail["deletedCount"], 1);
}

/// UX-39（無制限の選択肢）: `retentionDays: null`のときは
/// preview/prune-now とも何も削除しない。
#[tokio::test]
async fn store_settings_prune_with_unlimited_policy_deletes_nothing() {
    let today = LocalDate::new(2026, 7, 12);
    let now_ms = today.to_days_since_epoch() * 86_400_000 + 12 * 3_600_000;
    let clock: Arc<dyn Clock> = Arc::new(ManualClock::new(now_ms, 0));
    let env = test_env_with_clock(clock).await;

    let data_dir = store_settings_test_data_dir(&env);
    let old_file = touch_data_file(&data_dir, LocalDate::new(2020, 1, 1), 1);

    let put = HttpRequest::builder()
        .method("PUT")
        .uri("/api/store-settings")
        .header("Authorization", format!("Bearer {}", env.admin_token))
        .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({ "retentionDays": null })).unwrap(),
        ))
        .unwrap();
    let response = env.router.clone().oneshot(put).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let preview = HttpRequest::builder()
        .method("POST")
        .uri("/api/store-settings/prune-preview")
        .header("Authorization", format!("Bearer {}", env.admin_token))
        .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
        .body(Body::empty())
        .unwrap();
    let response = env.router.clone().oneshot(preview).await.unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["wouldDeleteCount"], 0);

    let prune_now = HttpRequest::builder()
        .method("POST")
        .uri("/api/store-settings/prune-now")
        .header("Authorization", format!("Bearer {}", env.admin_token))
        .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
        .body(Body::empty())
        .unwrap();
    let response = env.router.clone().oneshot(prune_now).await.unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["deletedCount"], 0);
    assert!(old_file.exists(), "unlimited retention must delete nothing");
}

/// レビュー指摘の安全化: 保持日数が`u32::MAX`を超える値で保存されて
/// いる状態（`parse_retention`は上限クランプをしないため、保存経路を
/// 通らず直接設定された等でこの状態になり得る）で prune-preview /
/// prune-now を叩くと、どちらも削除0件になり（既存ファイルが消えず）、
/// 両者の判定が一致すること（`resolve_prune_retention_days`を両方の
/// ハンドラが共有していることの固定）を確認する。
#[tokio::test]
async fn store_settings_prune_with_out_of_range_retention_days_deletes_nothing() {
    let today = LocalDate::new(2026, 7, 12);
    let now_ms = today.to_days_since_epoch() * 86_400_000 + 12 * 3_600_000;
    let clock: Arc<dyn Clock> = Arc::new(ManualClock::new(now_ms, 0));
    let env = test_env_with_clock(clock).await;

    let data_dir = store_settings_test_data_dir(&env);
    let old_file = touch_data_file(&data_dir, LocalDate::new(2020, 1, 1), 1);

    // `PUT /api/store-settings`のバリデーション（1〜3650）を経由せず、
    // 保存経路そのものへ想定外の巨大値を直接書き込む - レビュー指摘が
    // 想定する「設定が壊れている・DBを直接編集された」状況の再現。
    SettingsService::new(Db::Sqlite(env.pool.clone()))
        .set_store_config(&StoreSettings {
            data_dir: "./data".to_string(),
            retention_days: Some(u32::MAX as i64 + 1),
        })
        .await
        .expect("set_store_config");

    let preview = HttpRequest::builder()
        .method("POST")
        .uri("/api/store-settings/prune-preview")
        .header("Authorization", format!("Bearer {}", env.admin_token))
        .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
        .body(Body::empty())
        .unwrap();
    let response = env.router.clone().oneshot(preview).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["wouldDeleteCount"], 0);
    assert!(old_file.exists(), "preview must not delete anything");

    let prune_now = HttpRequest::builder()
        .method("POST")
        .uri("/api/store-settings/prune-now")
        .header("Authorization", format!("Bearer {}", env.admin_token))
        .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
        .body(Body::empty())
        .unwrap();
    let response = env.router.clone().oneshot(prune_now).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["deletedCount"], 0);
    assert!(
        old_file.exists(),
        "out-of-range retention_days must delete nothing, matching the preview"
    );
}
