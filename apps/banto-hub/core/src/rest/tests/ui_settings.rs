//! #1147: `/api/ui-settings/{key}`（利用者ごとの UI 設定）。ChronoGazer の
//! `ui_settings_*` テストと同じ観点（往復・利用者ごとの分離・無効なキー・
//! トークン無し）に、Hub 固有の「API キーでは使えない」を足す。

use super::super::*;
use super::common::{issue_api_key, session_call, session_request, test_env};
use serde_json::json;

async fn put_setting(
    router: &Router,
    key: &str,
    token: &str,
    value: &str,
) -> (StatusCode, serde_json::Value) {
    session_call(
        router,
        session_request(
            "PUT",
            &format!("/api/ui-settings/{key}"),
            Some(token),
            Some(json!({ "value": value })),
        ),
    )
    .await
}

async fn get_setting(router: &Router, key: &str, token: &str) -> (StatusCode, serde_json::Value) {
    session_call(
        router,
        session_request("GET", &format!("/api/ui-settings/{key}"), Some(token), None),
    )
    .await
}

#[tokio::test]
async fn ui_settings_round_trip_with_the_viewer_role() {
    let env = test_env().await;
    let (status, body) = get_setting(&env.router, "theme", &env.viewer_token).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["value"].is_null());

    // viewer でも自分の設定は書ける（ロールの下限なし）。banto v8 は `200 { revision }`。
    let (status, _) = put_setting(&env.router, "theme", &env.viewer_token, "glass").await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = get_setting(&env.router, "theme", &env.viewer_token).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["value"], "glass");
}

#[tokio::test]
async fn ui_settings_are_isolated_per_user() {
    let env = test_env().await;
    let (status, _) = put_setting(&env.router, "theme", &env.admin_token, "glass").await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = get_setting(&env.router, "theme", &env.viewer_token).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["value"].is_null(), "別の利用者の値が見えた: {body}");
    let (_, body) = get_setting(&env.router, "theme", &env.admin_token).await;
    assert_eq!(body["value"], "glass");
}

#[tokio::test]
async fn ui_settings_reject_an_invalid_key() {
    let env = test_env().await;
    let (status, body) = put_setting(&env.router, "bad%20key!", &env.viewer_token, "x").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["kind"], "validation");
    let (status, _) = get_setting(&env.router, "bad%20key!", &env.viewer_token).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn ui_settings_need_a_session() {
    let env = test_env().await;
    let (status, _) = session_call(
        &env.router,
        session_request("GET", "/api/ui-settings/theme", None, None),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn ui_settings_are_not_available_to_api_keys() {
    let env = test_env().await;
    let (status, issued) = issue_api_key(&env.router, &env.admin_token, "ui", &["read"]).await;
    assert_eq!(status, StatusCode::CREATED, "{issued}");
    let key = issued["key"].as_str().unwrap().to_string();
    let (status, _) = get_setting(&env.router, "theme", &key).await;
    assert!(
        status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN,
        "API キーで UI 設定が読めた: {status}"
    );
    let (status, _) = put_setting(&env.router, "theme", &key, "glass").await;
    assert!(
        status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN,
        "API キーで UI 設定が書けた: {status}"
    );
}
