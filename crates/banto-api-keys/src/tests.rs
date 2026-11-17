//! この crate の単体テスト。banto-hub の `api_keys.rs` から、アプリに依らない
//! 部分のテストを移した物（スコープの書式と判定のテストは banto-hub に残る）。
//! 表は banto-hub の `0101_api_keys.sql` と同じ列で、メモリの SQLite に作る。

use super::*;
use sqlx::sqlite::SqlitePoolOptions;

/// テスト用のアプリ。接頭辞は banto-hub と違う `tk` にして、接頭辞がアプリから
/// 渡っていることも確かめる。スコープは `read` と、`*` を含まない `write:…`
/// だけを受ける。
struct TestScopes;

impl ApiKeyScopes for TestScopes {
    const KEY_PREFIX: &'static str = "tk";

    fn validate_scope(scope: &str) -> Result<(), String> {
        if scope == "read" {
            return Ok(());
        }
        match scope.strip_prefix("write:") {
            Some(target) if !target.is_empty() && !target.contains('*') => Ok(()),
            _ => Err(format!("不明なスコープです: {scope}")),
        }
    }
}

type Svc = ApiKeysService<TestScopes>;

/// banto-hub の `0101_api_keys.sql` と同じ表（この crate はマイグレーションを
/// 持たない）。
const CREATE_API_KEYS: &str = "CREATE TABLE api_keys (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  name TEXT NOT NULL UNIQUE,
  prefix TEXT NOT NULL UNIQUE,
  key_hash TEXT NOT NULL,
  scopes TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  last_used_at TEXT,
  revoked_at TEXT,
  tripped_at TEXT,
  expires_at TEXT
)";

/// 接続 1 本のメモリの SQLite（接続ごとに別の DB になるので 1 本に限り、
/// 閉じて消えないよう期限を外す）。
async fn memory_pool() -> Result<SqlitePool, sqlx::Error> {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .idle_timeout(None)
        .max_lifetime(None)
        .connect("sqlite::memory:")
        .await?;
    sqlx::query(CREATE_API_KEYS).execute(&pool).await?;
    Ok(pool)
}

async fn service() -> Svc {
    let pool = memory_pool().await.expect("memory_pool");
    Svc::new(pool)
}

// --- 照合の結果の分け方（banto-hub から移した） ---------------------------

/// #434: 照合の結果の分け方の表。DB が答えたもの（有効・失効・トリップ・
/// 期限切れ・存在しない）は `Answered`、DB エラーとタイムアウトは
/// `Unavailable`。
#[tokio::test]
async fn classify_lookup_separates_answers_from_failures() {
    let pool = memory_pool().await.unwrap();
    let service = Svc::new(pool);
    let issued = service
        .issue("classify", vec!["read".to_string()], None)
        .await
        .unwrap();
    let valid = service.lookup(&issued.key, 0).await.unwrap();
    assert!(matches!(valid, ApiKeyLookup::Valid(_)));

    type Outcome = Option<Result<ApiKeyLookup, BantoError>>;
    let table: Vec<(&str, Outcome, bool)> = vec![
        ("valid", Some(Ok(valid)), true),
        (
            "revoked",
            Some(Ok(ApiKeyLookup::Revoked {
                id: 1,
                name: "k".to_string(),
            })),
            true,
        ),
        (
            "tripped",
            Some(Ok(ApiKeyLookup::Tripped {
                id: 2,
                name: "k".to_string(),
            })),
            true,
        ),
        (
            "expired",
            Some(Ok(ApiKeyLookup::Expired {
                id: 3,
                name: "k".to_string(),
            })),
            true,
        ),
        ("not found", Some(Ok(ApiKeyLookup::NotFound)), true),
        (
            "db error",
            Some(Err(BantoError::Storage("database is locked".to_string()))),
            false,
        ),
        ("timeout", None, false),
    ];
    for (label, outcome, answered) in table {
        let check = classify_lookup(outcome);
        assert_eq!(
            matches!(check, ApiKeyCheck::Answered(_)),
            answered,
            "{label}: {check:?}"
        );
    }
}

/// #435: 照合の結果 → 使える / どう拒否するか（+ 監査用の情報）の表。
/// 3 経路（REST・MCP・gRPC）はこの分類だけを応答に変換する。
#[tokio::test]
async fn api_key_verdict_table() {
    let pool = memory_pool().await.unwrap();
    let service = Svc::new(pool);
    let issued = service
        .issue("verdict", vec!["read".to_string()], None)
        .await
        .unwrap();
    let valid = service.lookup(&issued.key, 0).await.unwrap();

    let answered = ApiKeyCheck::Answered;
    let k = || "k".to_string();
    // (ラベル, 照合の結果, 期待: None = 使える / Some(拒否の種類), 監査の理由)
    let table: Vec<(&str, ApiKeyCheck, Option<&str>, Option<&str>)> = vec![
        ("valid", answered(valid), None, None),
        (
            "revoked",
            answered(ApiKeyLookup::Revoked { id: 1, name: k() }),
            Some("unauthenticated:revoked"),
            Some("revoked"),
        ),
        (
            "expired",
            answered(ApiKeyLookup::Expired { id: 2, name: k() }),
            Some("unauthenticated:expired"),
            Some("expired"),
        ),
        (
            "tripped",
            answered(ApiKeyLookup::Tripped { id: 3, name: k() }),
            Some("tripped"),
            Some("tripped"),
        ),
        (
            "not found",
            answered(ApiKeyLookup::NotFound),
            Some("unauthenticated:not_found"),
            None,
        ),
        (
            "unavailable",
            ApiKeyCheck::Unavailable(BantoError::Storage("db down".to_string())),
            Some("unavailable"),
            None,
        ),
    ];
    for (label, check, rejection, audit) in table {
        match api_key_verdict(check) {
            Ok(_) => assert_eq!(rejection, None, "{label}"),
            Err((got, denied)) => {
                let got = match got {
                    ApiKeyRejection::Unauthenticated(UnauthenticatedReason::Revoked) => {
                        "unauthenticated:revoked"
                    }
                    ApiKeyRejection::Unauthenticated(UnauthenticatedReason::Expired) => {
                        "unauthenticated:expired"
                    }
                    ApiKeyRejection::Unauthenticated(UnauthenticatedReason::NotFound) => {
                        "unauthenticated:not_found"
                    }
                    ApiKeyRejection::Tripped => "tripped",
                    ApiKeyRejection::Unavailable(_) => "unavailable",
                };
                assert_eq!(Some(got), rejection, "{label}");
                assert_eq!(denied.map(|d| d.reason), audit, "{label}");
            }
        }
    }
}

/// #434: `check` は DB エラーを「照合できなかった」に分ける（401 に潰さない
/// 元になる判定）。
#[tokio::test]
async fn check_reports_a_db_failure_as_unavailable() {
    let pool = memory_pool().await.unwrap();
    let service = Svc::new(pool.clone());
    let issued = service
        .issue("check", vec!["read".to_string()], None)
        .await
        .unwrap();
    assert!(matches!(
        service.check(&issued.key, 0).await,
        ApiKeyCheck::Answered(ApiKeyLookup::Valid(_))
    ));
    sqlx::query("ALTER TABLE api_keys RENAME TO api_keys_away")
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        service.check(&issued.key, 0).await,
        ApiKeyCheck::Unavailable(_)
    ));
}

// --- キー生成・パース・ハッシュ ---------------------------------------

#[test]
fn generated_prefix_is_always_prefix_len_chars() {
    for _ in 0..50 {
        assert_eq!(generate_prefix().len(), PREFIX_LEN);
    }
}

#[test]
fn parse_key_round_trips_a_generated_key() {
    let prefix = generate_prefix();
    let secret = generate_secret();
    let key = format!("tk_{prefix}_{secret}");
    let (parsed_prefix, parsed_secret) = parse_key("tk", &key).expect("should parse");
    assert_eq!(parsed_prefix, prefix);
    assert_eq!(parsed_secret, secret);
}

/// base64url は `_` を含みうる文字集合なので、prefix/secret の内部に
/// `_` が現れても固定長パースが壊れないことを明示的に確認する
/// (この crate の doc comment「キー形式」の核心)。
#[test]
fn parse_key_survives_underscores_inside_prefix_and_secret() {
    let prefix = "ab_d_f_h"; // 8 chars, contains '_'
    let secret = "s_e_c_r_e_t_with_underscores";
    let key = format!("tk_{prefix}_{secret}");
    let (parsed_prefix, parsed_secret) = parse_key("tk", &key).expect("should parse");
    assert_eq!(parsed_prefix, prefix);
    assert_eq!(parsed_secret, secret);
}

#[test]
fn parse_key_rejects_malformed_tokens() {
    assert!(parse_key("tk", "not-a-tk-key").is_none());
    assert!(parse_key("tk", "tk_tooshort").is_none());
    assert!(parse_key("tk", "tk_12345678_").is_none()); // empty secret
}

#[test]
fn constant_time_eq_matches_regular_equality() {
    assert!(constant_time_eq("abc", "abc"));
    assert!(!constant_time_eq("abc", "abd"));
    assert!(!constant_time_eq("abc", "abcd"));
}

// --- 60秒スロットル（純関数） -------------------------------------

#[test]
fn should_touch_last_used_is_true_when_never_used() {
    assert!(should_touch_last_used(1_000_000, None));
}

#[test]
fn should_touch_last_used_is_false_within_the_window() {
    assert!(!should_touch_last_used(1_000_000, Some(999_500)));
    assert!(!should_touch_last_used(1_000_000, Some(940_001)));
}

#[test]
fn should_touch_last_used_is_true_at_and_after_the_threshold() {
    assert!(should_touch_last_used(1_000_000, Some(940_000)));
    assert!(should_touch_last_used(1_000_000, Some(0)));
}

// --- H10 ①: 有効期限判定（純関数） -----------------------------------

#[test]
fn is_expired_is_false_when_unlimited() {
    assert!(!is_expired(1_000_000, None));
    assert!(!is_expired(i64::MAX, None));
}

#[test]
fn is_expired_is_false_before_the_deadline() {
    assert!(!is_expired(999_999, Some(1_000_000)));
}

#[test]
fn is_expired_is_true_at_and_after_the_deadline() {
    // 境界含む(ちょうど期限の瞬間は「期限切れ」側)。
    assert!(is_expired(1_000_000, Some(1_000_000)));
    assert!(is_expired(1_000_001, Some(1_000_000)));
}

// --- サービス: issue/list/revoke/lookup ------------------------------

#[tokio::test]
async fn issue_then_lookup_round_trips() {
    let svc = service().await;
    let issued = svc
        .issue("mes-gateway", vec!["read".to_string()], None)
        .await
        .expect("issue should succeed");
    assert!(issued.key.starts_with("tk_"));

    let lookup = svc
        .lookup(&issued.key, 1_000_000)
        .await
        .expect("lookup should succeed");
    match lookup {
        ApiKeyLookup::Valid(ctx) => {
            assert_eq!(ctx.id, issued.id);
            assert_eq!(ctx.name, "mes-gateway");
            assert_eq!(ctx.scopes, vec!["read".to_string()]);
            assert_eq!(ctx.last_used_at_ms, None);
        }
        other => panic!("expected Valid, got {other:?}"),
    }
}

#[tokio::test]
async fn lookup_with_wrong_secret_is_not_found() {
    let svc = service().await;
    let issued = svc
        .issue("mes-gateway", vec!["read".to_string()], None)
        .await
        .unwrap();
    let (prefix, _secret) = parse_key("tk", &issued.key).unwrap();
    let forged = format!("tk_{prefix}_{}", generate_secret());

    let lookup = svc.lookup(&forged, 1_000_000).await.unwrap();
    assert_eq!(lookup, ApiKeyLookup::NotFound);
}

#[tokio::test]
async fn lookup_unknown_prefix_is_not_found() {
    let svc = service().await;
    let lookup = svc
        .lookup(
            &format!("tk_{}_{}", generate_prefix(), generate_secret()),
            1_000_000,
        )
        .await
        .unwrap();
    assert_eq!(lookup, ApiKeyLookup::NotFound);
}

#[tokio::test]
async fn issue_rejects_invalid_scope_syntax() {
    let svc = service().await;
    let err = svc
        .issue("bad-scope", vec!["write:line1.fast.*".to_string()], None)
        .await
        .unwrap_err();
    assert!(matches!(err, BantoError::Validation { .. }));
}

#[tokio::test]
async fn issue_rejects_duplicate_name() {
    let svc = service().await;
    svc.issue("dup", vec!["read".to_string()], None)
        .await
        .unwrap();
    let err = svc
        .issue("dup", vec!["read".to_string()], None)
        .await
        .unwrap_err();
    match err {
        BantoError::Validation { field_errors } => {
            assert_eq!(field_errors[0].field, "name");
        }
        other => panic!("expected Validation, got {other:?}"),
    }
}

#[tokio::test]
async fn revoke_then_lookup_reports_revoked_not_not_found() {
    let svc = service().await;
    let issued = svc
        .issue("revoke-me", vec!["read".to_string()], None)
        .await
        .unwrap();

    let summary = svc.revoke(issued.id).await.expect("revoke should succeed");
    assert!(summary.revoked_at.is_some());

    let lookup = svc.lookup(&issued.key, 1_000_000).await.unwrap();
    match lookup {
        ApiKeyLookup::Revoked { id, name } => {
            assert_eq!(id, issued.id);
            assert_eq!(name, "revoke-me");
        }
        other => panic!("expected Revoked, got {other:?}"),
    }
}

#[tokio::test]
async fn revoke_is_idempotent_and_keeps_the_first_timestamp() {
    let svc = service().await;
    let issued = svc
        .issue("idempotent", vec!["read".to_string()], None)
        .await
        .unwrap();

    let first = svc.revoke(issued.id).await.unwrap();
    let second = svc.revoke(issued.id).await.unwrap();
    assert_eq!(first.revoked_at, second.revoked_at);
}

#[tokio::test]
async fn revoke_unknown_id_is_not_found() {
    let svc = service().await;
    let err = svc.revoke(999).await.unwrap_err();
    assert!(matches!(err, BantoError::NotFound { .. }));
}

#[tokio::test]
async fn wrong_secret_against_a_revoked_key_is_still_not_found() {
    // Info-leak guard: a forged secret against a *revoked* key's prefix
    // must not distinguish itself from a forged secret against a live
    // key's prefix - both are NotFound (see ApiKeyLookup's doc comment).
    let svc = service().await;
    let issued = svc
        .issue("revoked-forged", vec!["read".to_string()], None)
        .await
        .unwrap();
    svc.revoke(issued.id).await.unwrap();

    let (prefix, _secret) = parse_key("tk", &issued.key).unwrap();
    let forged = format!("tk_{prefix}_{}", generate_secret());
    let lookup = svc.lookup(&forged, 1_000_000).await.unwrap();
    assert_eq!(lookup, ApiKeyLookup::NotFound);
}

#[tokio::test]
async fn list_reflects_issued_and_revoked_keys() {
    let svc = service().await;
    svc.issue("a", vec!["read".to_string()], None)
        .await
        .unwrap();
    let b = svc
        .issue("b", vec!["read".to_string()], None)
        .await
        .unwrap();
    svc.revoke(b.id).await.unwrap();

    let listed = svc.list().await.unwrap();
    assert_eq!(listed.len(), 2);
    let a = listed.iter().find(|k| k.name == "a").unwrap();
    assert!(a.revoked_at.is_none());
    let b = listed.iter().find(|k| k.name == "b").unwrap();
    assert!(b.revoked_at.is_some());
    // key_hash must never appear on the wire - ApiKeySummary has no such
    // field at all, so this is a compile-time guarantee, not a runtime
    // assertion; documented here for visibility.
}

#[tokio::test]
async fn touch_last_used_respects_the_throttle() {
    let svc = service().await;
    let issued = svc
        .issue("throttled", vec!["read".to_string()], None)
        .await
        .unwrap();

    svc.touch_last_used(issued.id, 1_000_000, None)
        .await
        .unwrap();
    let after_first = svc.list().await.unwrap();
    let entry = after_first.iter().find(|k| k.id == issued.id).unwrap();
    assert_eq!(entry.last_used_at, Some(1_000_000));

    // Within the 60s window: must not move.
    svc.touch_last_used(issued.id, 1_000_500, Some(1_000_000))
        .await
        .unwrap();
    let still = svc.list().await.unwrap();
    let entry = still.iter().find(|k| k.id == issued.id).unwrap();
    assert_eq!(entry.last_used_at, Some(1_000_000));

    // Past the window: must advance.
    svc.touch_last_used(issued.id, 1_061_000, Some(1_000_000))
        .await
        .unwrap();
    let moved = svc.list().await.unwrap();
    let entry = moved.iter().find(|k| k.id == issued.id).unwrap();
    assert_eq!(entry.last_used_at, Some(1_061_000));
}

// --- T2-4: trip/clear_trip/lookup ordering -----------------------------

#[tokio::test]
async fn trip_then_lookup_reports_tripped_not_valid_or_not_found() {
    let svc = service().await;
    let issued = svc
        .issue("writer", vec!["write:line1.fast.temp01".to_string()], None)
        .await
        .unwrap();

    let summary = svc.trip(issued.id).await.expect("trip should succeed");
    assert!(summary.tripped_at.is_some());
    assert!(summary.revoked_at.is_none(), "trip must not revoke");

    let lookup = svc.lookup(&issued.key, 1_000_000).await.unwrap();
    match lookup {
        ApiKeyLookup::Tripped { id, name } => {
            assert_eq!(id, issued.id);
            assert_eq!(name, "writer");
        }
        other => panic!("expected Tripped, got {other:?}"),
    }
}

#[tokio::test]
async fn trip_is_idempotent_and_keeps_the_first_timestamp() {
    let svc = service().await;
    let issued = svc
        .issue("idempotent-trip", vec!["read".to_string()], None)
        .await
        .unwrap();

    let first = svc.trip(issued.id).await.unwrap();
    let second = svc.trip(issued.id).await.unwrap();
    assert_eq!(first.tripped_at, second.tripped_at);
}

#[tokio::test]
async fn clear_trip_restores_valid_lookup() {
    let svc = service().await;
    let issued = svc
        .issue("clear-me", vec!["read".to_string()], None)
        .await
        .unwrap();
    svc.trip(issued.id).await.unwrap();

    let cleared = svc
        .clear_trip(issued.id)
        .await
        .expect("clear_trip should succeed");
    assert!(cleared.tripped_at.is_none());

    let lookup = svc.lookup(&issued.key, 1_000_000).await.unwrap();
    match lookup {
        ApiKeyLookup::Valid(ctx) => assert_eq!(ctx.id, issued.id),
        other => panic!("expected Valid after clear_trip, got {other:?}"),
    }
}

#[tokio::test]
async fn clear_trip_is_idempotent() {
    let svc = service().await;
    let issued = svc
        .issue("clear-idempotent", vec!["read".to_string()], None)
        .await
        .unwrap();

    // Clearing a key that was never tripped must not error.
    let cleared = svc
        .clear_trip(issued.id)
        .await
        .expect("clear_trip should succeed");
    assert!(cleared.tripped_at.is_none());
}

#[tokio::test]
async fn revoked_key_is_reported_as_revoked_even_if_also_tripped() {
    // 判定順: ハッシュ一致 -> revoked -> tripped. A revoked-and-tripped
    // key must report Revoked, matching the doc comment's ordering.
    let svc = service().await;
    let issued = svc
        .issue("revoked-and-tripped", vec!["read".to_string()], None)
        .await
        .unwrap();
    svc.trip(issued.id).await.unwrap();
    svc.revoke(issued.id).await.unwrap();

    let lookup = svc.lookup(&issued.key, 1_000_000).await.unwrap();
    match lookup {
        ApiKeyLookup::Revoked { id, .. } => assert_eq!(id, issued.id),
        other => panic!("expected Revoked, got {other:?}"),
    }
}

#[tokio::test]
async fn wrong_secret_against_a_tripped_key_is_still_not_found() {
    // Same info-leak guard as the revoked case (this module's doc
    // comment): a forged secret against a tripped key's prefix must not
    // distinguish itself from any other forged secret.
    let svc = service().await;
    let issued = svc
        .issue("tripped-forged", vec!["read".to_string()], None)
        .await
        .unwrap();
    svc.trip(issued.id).await.unwrap();

    let (prefix, _secret) = parse_key("tk", &issued.key).unwrap();
    let forged = format!("tk_{prefix}_{}", generate_secret());
    let lookup = svc.lookup(&forged, 1_000_000).await.unwrap();
    assert_eq!(lookup, ApiKeyLookup::NotFound);
}

#[tokio::test]
async fn trip_unknown_id_is_not_found() {
    let svc = service().await;
    let err = svc.trip(999).await.unwrap_err();
    assert!(matches!(err, BantoError::NotFound { .. }));
}

#[tokio::test]
async fn list_reflects_tripped_state() {
    let svc = service().await;
    let a = svc
        .issue("a", vec!["read".to_string()], None)
        .await
        .unwrap();
    svc.issue("b", vec!["read".to_string()], None)
        .await
        .unwrap();
    svc.trip(a.id).await.unwrap();

    let listed = svc.list().await.unwrap();
    let a = listed.iter().find(|k| k.name == "a").unwrap();
    assert!(a.tripped_at.is_some());
    let b = listed.iter().find(|k| k.name == "b").unwrap();
    assert!(b.tripped_at.is_none());
}

// --- H10 ①: expires_at / lookup ordering -------------------------------

/// 無期限キー（`expires_at: None`）は今までどおり - 期限判定を追加した
/// ことで既定動作が変わっていないことの回帰防止（実装指示: 「無期限
/// キーの従来動作不変」）。`now_ms` にどれだけ大きな値を渡しても
/// `Valid` のまま。
#[tokio::test]
async fn unlimited_key_is_valid_regardless_of_now_ms() {
    let svc = service().await;
    let issued = svc
        .issue("unlimited", vec!["read".to_string()], None)
        .await
        .unwrap();

    let lookup = svc.lookup(&issued.key, i64::MAX).await.unwrap();
    match lookup {
        ApiKeyLookup::Valid(ctx) => assert_eq!(ctx.id, issued.id),
        other => panic!("expected Valid, got {other:?}"),
    }
}

/// 期限付きキー: 期限前は `Valid`、期限ちょうど/以降は `Expired`
/// （実装指示の受け入れ条件: 「期限切れキーの 401 と... テスト」の
/// サービス層側）。
#[tokio::test]
async fn expiring_key_is_valid_before_and_expired_at_or_after_the_deadline() {
    let svc = service().await;
    let issued = svc
        .issue("expiring", vec!["read".to_string()], Some(2_000_000))
        .await
        .unwrap();

    let before = svc.lookup(&issued.key, 1_999_999).await.unwrap();
    match before {
        ApiKeyLookup::Valid(ctx) => assert_eq!(ctx.id, issued.id),
        other => panic!("expected Valid before the deadline, got {other:?}"),
    }

    let at_deadline = svc.lookup(&issued.key, 2_000_000).await.unwrap();
    match at_deadline {
        ApiKeyLookup::Expired { id, name } => {
            assert_eq!(id, issued.id);
            assert_eq!(name, "expiring");
        }
        other => panic!("expected Expired at the deadline, got {other:?}"),
    }

    let after = svc.lookup(&issued.key, 2_000_001).await.unwrap();
    assert!(matches!(after, ApiKeyLookup::Expired { .. }));
}

/// 判定順: ハッシュ一致 -> revoked -> tripped -> expired。失効済みかつ
/// 期限切れのキーは `Revoked` を報告する（`revoked_key_is_reported_
/// as_revoked_even_if_also_tripped` と同じ精度で expired にも確認）。
#[tokio::test]
async fn revoked_key_is_reported_as_revoked_even_if_also_expired() {
    let svc = service().await;
    let issued = svc
        .issue("revoked-and-expired", vec!["read".to_string()], Some(1))
        .await
        .unwrap();
    svc.revoke(issued.id).await.unwrap();

    let lookup = svc.lookup(&issued.key, 1_000_000).await.unwrap();
    match lookup {
        ApiKeyLookup::Revoked { id, .. } => assert_eq!(id, issued.id),
        other => panic!("expected Revoked, got {other:?}"),
    }
}

/// トリップ中かつ期限切れのキーは `Tripped` を報告する（tripped の判定が
/// expired より先に来る、というこの crate の doc comment の順序を
/// 固定する）。
#[tokio::test]
async fn tripped_key_is_reported_as_tripped_even_if_also_expired() {
    let svc = service().await;
    let issued = svc
        .issue("tripped-and-expired", vec!["read".to_string()], Some(1))
        .await
        .unwrap();
    svc.trip(issued.id).await.unwrap();

    let lookup = svc.lookup(&issued.key, 1_000_000).await.unwrap();
    match lookup {
        ApiKeyLookup::Tripped { id, .. } => assert_eq!(id, issued.id),
        other => panic!("expected Tripped, got {other:?}"),
    }
}

/// Info-leak guard（この crate の doc comment参照）: 期限切れキーの
/// prefix に対する偽造 secret は、他の偽造と同じく常に `NotFound` -
/// `wrong_secret_against_a_revoked_key_is_still_not_found`/
/// `wrong_secret_against_a_tripped_key_is_still_not_found` と同型。
#[tokio::test]
async fn wrong_secret_against_an_expired_key_is_still_not_found() {
    let svc = service().await;
    let issued = svc
        .issue("expired-forged", vec!["read".to_string()], Some(1))
        .await
        .unwrap();

    let (prefix, _secret) = parse_key("tk", &issued.key).unwrap();
    let forged = format!("tk_{prefix}_{}", generate_secret());
    let lookup = svc.lookup(&forged, 1_000_000).await.unwrap();
    assert_eq!(lookup, ApiKeyLookup::NotFound);
}

/// `list`/`ApiKeySummary` にも `expires_at` がそのまま(epoch ミリ秒)で
/// 反映される。
#[tokio::test]
async fn list_reflects_expires_at() {
    let svc = service().await;
    svc.issue("unlimited", vec!["read".to_string()], None)
        .await
        .unwrap();
    svc.issue("expiring", vec!["read".to_string()], Some(2_000_000))
        .await
        .unwrap();

    let listed = svc.list().await.unwrap();
    let unlimited = listed.iter().find(|k| k.name == "unlimited").unwrap();
    assert_eq!(unlimited.expires_at, None);
    let expiring = listed.iter().find(|k| k.name == "expiring").unwrap();
    assert_eq!(expiring.expires_at, Some(2_000_000));
}

// --- この crate で足した確かめ ---------------------------------------------

/// 接頭辞はアプリの物だけを受ける: 別のアプリの接頭辞・接頭辞の後に `_` が
/// 無い・接頭辞が前方一致するだけ（`tkx_`）のキーは読めない。
#[test]
fn parse_key_accepts_only_the_apps_own_prefix() {
    let key = format!("tk_{}_{}", generate_prefix(), generate_secret());
    assert!(parse_key("tk", &key).is_some());
    assert!(parse_key("bh", &key).is_none());
    let other = format!("bh_{}_{}", generate_prefix(), generate_secret());
    assert!(parse_key("tk", &other).is_none());
    assert!(parse_key("tk", &format!("tkx{}", &key[2..])).is_none());
    assert!(parse_key("tk", "tk12345678_secret").is_none());
}

/// 非 ASCII が先頭 8 バイトをまたぐトークンは、panic せずに読めないとする。
#[test]
fn parse_key_rejects_a_prefix_that_splits_a_character() {
    assert!(parse_key("tk", "tk_abcdefgあ_secret").is_none());
    assert!(parse_key("tk", "tk_ああ_secret_secret").is_none());
}

/// 生成するキーは `{接頭辞}_{8 文字}_{43 文字}`（base64url）で、保存する
/// ハッシュは SHA-256 の 16 進（64 文字）。
#[tokio::test]
async fn issued_key_has_the_documented_shape_and_only_its_hash_is_stored() {
    let pool = memory_pool().await.unwrap();
    let svc = Svc::new(pool.clone());
    let issued = svc
        .issue("shape", vec!["read".to_string()], None)
        .await
        .unwrap();
    let rest = issued.key.strip_prefix("tk_").unwrap();
    assert_eq!(rest.len(), PREFIX_LEN + 1 + 43, "{}", issued.key);
    let (prefix, secret) = parse_key("tk", &issued.key).unwrap();
    assert_eq!(prefix, issued.prefix);
    assert_eq!(secret.len(), 43);
    let (stored_prefix, key_hash): (String, String) =
        sqlx::query_as("SELECT prefix, key_hash FROM api_keys WHERE id = ?")
            .bind(issued.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored_prefix, issued.prefix);
    assert_eq!(key_hash, hash_key(&issued.key));
    assert_eq!(key_hash.len(), 64);
    assert!(!key_hash.contains(secret));
}

/// 同じ長さで 1 バイトだけ違うハッシュも、長さの違うハッシュも一致しない
/// （定数時間の比較が `==` と同じ答えを返す）。
#[test]
fn constant_time_eq_agrees_with_eq_on_digests() {
    let a = hash_key("tk_aaaaaaaa_x");
    let b = hash_key("tk_aaaaaaaa_y");
    assert!(constant_time_eq(&a, &a.clone()));
    assert_eq!(constant_time_eq(&a, &b), a == b);
    let mut flipped = a.clone().into_bytes();
    flipped[63] = if flipped[63] == b'0' { b'1' } else { b'0' };
    assert!(!constant_time_eq(
        &a,
        std::str::from_utf8(&flipped).unwrap()
    ));
    assert!(!constant_time_eq(&a, &a[..63]));
    assert!(constant_time_eq("", ""));
}

/// 発行の入口の確認: 名前が空白だけ・スコープが空・スコープが不正は
/// `Validation`（欄 `name` / `scopes`）。名前は前後の空白を落として保存する。
#[tokio::test]
async fn issue_validates_name_and_scopes() {
    let svc = service().await;
    for (name, scopes, field) in [
        ("   ", vec!["read".to_string()], "name"),
        ("empty", vec![], "scopes"),
        (
            "bad",
            vec!["read".to_string(), "admin".to_string()],
            "scopes",
        ),
    ] {
        match svc.issue(name, scopes, None).await.unwrap_err() {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors[0].field, field, "{name}")
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }
    let issued = svc
        .issue("  padded  ", vec!["read".to_string()], None)
        .await
        .unwrap();
    assert_eq!(issued.name, "padded");
    assert!(svc.list().await.unwrap().iter().any(|k| k.name == "padded"));
}

/// 削除は失効したキーだけ。失効していなければ `StillActive`、無い id は
/// `NotFound`。削除した後は同じ名前で発行し直せ、古いキーは `NotFound`。
#[tokio::test]
async fn delete_only_removes_revoked_keys_and_frees_the_name() {
    let svc = service().await;
    let issued = svc
        .issue("reuse", vec!["read".to_string()], None)
        .await
        .unwrap();
    assert_eq!(
        svc.delete(issued.id).await.unwrap(),
        ApiKeyDeleteOutcome::StillActive
    );
    svc.revoke(issued.id).await.unwrap();
    match svc.delete(issued.id).await.unwrap() {
        ApiKeyDeleteOutcome::Deleted(summary) => {
            assert_eq!(summary.id, issued.id);
            assert_eq!(summary.prefix, issued.prefix);
        }
        other => panic!("expected Deleted, got {other:?}"),
    }
    assert!(matches!(
        svc.delete(issued.id).await.unwrap_err(),
        BantoError::NotFound { .. }
    ));
    assert_eq!(
        svc.lookup(&issued.key, 0).await.unwrap(),
        ApiKeyLookup::NotFound
    );
    let again = svc
        .issue("reuse", vec!["read".to_string()], None)
        .await
        .unwrap();
    assert_ne!(again.prefix, issued.prefix);
}

/// `last_used_at` の間引きは 60 秒ちょうどで書き、それより前は書かない
/// （境界を DB 経由で確かめる）。
#[tokio::test]
async fn touch_last_used_writes_at_exactly_sixty_seconds() {
    let svc = service().await;
    let issued = svc
        .issue("edge", vec!["read".to_string()], None)
        .await
        .unwrap();
    let last = |svc: Svc| async move { svc.list().await.unwrap()[0].last_used_at };
    svc.touch_last_used(issued.id, 0, Some(-1)).await.unwrap();
    assert_eq!(last(svc.clone()).await, None, "1 ms 後は書かない");
    svc.touch_last_used(issued.id, 60_000, Some(1))
        .await
        .unwrap();
    assert_eq!(last(svc.clone()).await, None, "59.999 秒後は書かない");
    svc.touch_last_used(issued.id, 60_001, Some(1))
        .await
        .unwrap();
    assert_eq!(last(svc.clone()).await, Some(60_001), "60 秒ちょうどで書く");
}

/// 照合した文脈の主体の表示は `名前（prefix）`。
#[tokio::test]
async fn actor_label_is_name_and_prefix() {
    let svc = service().await;
    let issued = svc
        .issue("mes", vec!["read".to_string()], None)
        .await
        .unwrap();
    let ApiKeyLookup::Valid(ctx) = svc.lookup(&issued.key, 0).await.unwrap() else {
        panic!("expected Valid");
    };
    assert_eq!(ctx.actor_label(), format!("mes（{}）", issued.prefix));
}

/// 使えないキーへの HTTP 応答の表: 失効・期限切れ・存在しないは 401、
/// 一時停止は 403 `key_tripped`、照合できないは 500。
#[tokio::test]
async fn api_key_rejection_response_table() {
    use ApiKeyRejection as R;
    use UnauthenticatedReason as U;
    let table: Vec<(&str, R, StatusCode, serde_json::Value)> = vec![
        (
            "revoked",
            R::Unauthenticated(U::Revoked),
            StatusCode::UNAUTHORIZED,
            serde_json::json!("unauthorized"),
        ),
        (
            "expired",
            R::Unauthenticated(U::Expired),
            StatusCode::UNAUTHORIZED,
            serde_json::json!("unauthorized"),
        ),
        (
            "not found",
            R::Unauthenticated(U::NotFound),
            StatusCode::UNAUTHORIZED,
            serde_json::json!("unauthorized"),
        ),
        (
            "tripped",
            R::Tripped,
            StatusCode::FORBIDDEN,
            serde_json::json!("key_tripped"),
        ),
        (
            "unavailable",
            R::Unavailable(BantoError::Storage("db down".to_string())),
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!("storage"),
        ),
    ];
    for (label, rejection, status, marker) in table {
        let response = api_key_rejection_response(rejection);
        assert_eq!(response.status(), status, "{label}");
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let got = if body.get("error").is_some() {
            &body["error"]
        } else {
            &body["kind"]
        };
        assert_eq!(*got, marker, "{label}: {body}");
    }
}
