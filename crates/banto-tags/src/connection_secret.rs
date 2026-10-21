//! 接続の秘密値の行（`plc_connection_secrets`）と、`settings` の秘密の参照の扱い
//! （R2b-1d、docs/banto-hub-connection-settings-design.md §3.1・§3.3・§5.3・§8）。
//!
//! # 置き場
//!
//! migration `0025` が作る秘密の表 `plc_connection_secrets (ref, connection_id, key,
//! scheme, blob)` に、**封印済み**の値を 1 キー 1 行で置く。接続の `settings` には値の
//! 代わりに参照 `{"$secret": "<ref>"}` を置く（設計 §3.3）。FK は張らない（Q15）ので、
//! 行と参照を食い違わせないのはこの crate の書き込みの全入口（作成・更新・削除・一括削除）
//! の責任で、すべて**接続の行と同じトランザクション**の中で行う（設計 §5.3、
//! [`crate::PlcConnectionService`] の各メソッド）。
//!
//! # この crate は封印しない
//!
//! 封印（`plain-v1` / `dpapi-machine-v1`）と開封は Hub の `secrets` モジュール
//! （`apps/banto-hub/core/src/secrets.rs` の `ConnectionSecrets`・`SealedSecret`）の仕事で、
//! この crate は Hub の crate に依存できない。ここにあるのは、Hub が封印した値を運ぶ
//! 同じ形の型 [`SealedSecretRow`]（`ref`・`scheme`・`blob`。Hub の `SealedSecret` と
//! 1 対 1 で、変換は Hub の側に置く）と、書き込みの意図 [`SecretChange`] だけ。
//! この crate は平文を受け取る口を持たない（checklist §5「平文の資格情報は、それを持つ層の
//! 外に出さない」）。
//!
//! # 平文を DB に入れない最後の砦（設計 §3.3・§8 の 1）
//!
//! `settings` の秘密のキー（ドライバの `secret_keys()`。レジストリの無い旧規則では
//! [`builtin_secret_keys`]: `postgres` → `password`）の値は、**常に**秘密の参照で
//! なければならない。文字列（平文）やほかの形が来たら、保存の層
//! （[`check_settings_shape`]）が断る。入口のどれかで封印を忘れても DB には入らない。
//!
//! # `Debug` に blob を出さない
//!
//! `plain-v1` の blob は平文そのものなので、[`SealedSecretRow`] の `Debug` は参照と方式
//! だけを出し、blob は `Zeroizing` で持つ。

use std::collections::BTreeMap;
use std::fmt;

use banto_core::FieldError;
use serde_json::{Map, Value};
use sqlx::{Sqlite, SqliteConnection};
use zeroize::Zeroizing;

use crate::plc_connection::{HUB_PROTOCOL, MODBUS_PROTOCOL, POSTGRES_PROTOCOL, VIRTUAL_PROTOCOL};
use crate::settings_json::{is_secret_ref, SECRET_REF_KEY};

/// 秘密値の参照の文字数（128 ビットの乱数の小文字 16 進。migration `0025` の
/// `lower(hex(randomblob(16)))`、Hub の `secrets::SECRET_REF_LEN` と同じ）。
pub const SECRET_REF_LEN: usize = 32;

/// 封印の方式 `plain-v1`（blob = UTF-8 の平文。Windows 以外の既定、設計 §5.8）。
pub const SCHEME_PLAIN_V1: &str = "plain-v1";

/// 封印の方式 `dpapi-machine-v1`（blob = DPAPI の machine スコープの暗号文。R2b-1e から
/// Windows の既定）。
pub const SCHEME_DPAPI_MACHINE_V1: &str = "dpapi-machine-v1";

/// `postgres` の秘密のキー（設計 §3.2。`apps/banto-hub/core/src/db_source/driver.rs` の
/// `secret_keys` と同じ）。
pub const PASSWORD_KEY: &str = "password";

/// レジストリを持たない旧規則（[`crate::PlcConnectionService::new`]、ChronoGazer と
/// この crate のテスト）での、プロトコルの秘密のキー。R2b-1 の時点で秘密値を持つのは
/// `postgres` の `password` だけ（設計 §1.5）。レジストリがあるときは
/// ドライバの `secret_keys()` が正。
pub fn builtin_secret_keys(protocol: &str) -> &'static [&'static str] {
    if protocol == POSTGRES_PROTOCOL {
        &[PASSWORD_KEY]
    } else {
        &[]
    }
}

/// Hub が封印した秘密値 1 つ（`plc_connection_secrets` の `ref`・`scheme`・`blob`）。
///
/// Hub の `secrets::SealedSecret` と同じ中身を、この crate の型で運ぶ（この crate は Hub に
/// 依存できない）。`scheme` は文字列のまま（[`SCHEME_PLAIN_V1`] /
/// [`SCHEME_DPAPI_MACHINE_V1`]。知らない値は DB の `CHECK` が断る）。`Debug` は参照と方式
/// だけ（blob は `plain-v1` なら平文そのもの）。
#[derive(Clone, PartialEq, Eq)]
pub struct SealedSecretRow {
    pub r#ref: String,
    pub scheme: String,
    pub blob: Zeroizing<Vec<u8>>,
}

impl SealedSecretRow {
    /// A row from its parts (`blob` is moved into `Zeroizing`).
    pub fn new(r#ref: impl Into<String>, scheme: impl Into<String>, blob: Vec<u8>) -> Self {
        SealedSecretRow {
            r#ref: r#ref.into(),
            scheme: scheme.into(),
            blob: Zeroizing::new(blob),
        }
    }
}

impl fmt::Debug for SealedSecretRow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SealedSecretRow")
            .field("ref", &self.r#ref)
            .field("scheme", &self.scheme)
            .finish_non_exhaustive()
    }
}

/// DB から読んだ秘密の行（どの接続のどのキーか、と封印した値）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredSecretRow {
    pub connection_id: i64,
    pub key: String,
    pub sealed: SealedSecretRow,
}

impl<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow> for StoredSecretRow {
    fn from_row(row: &'r sqlx::sqlite::SqliteRow) -> Result<Self, sqlx::Error> {
        use sqlx::Row;
        let blob: Vec<u8> = row.try_get("blob")?;
        Ok(StoredSecretRow {
            connection_id: row.try_get("connection_id")?,
            key: row.try_get("key")?,
            sealed: SealedSecretRow {
                r#ref: row.try_get("ref")?,
                scheme: row.try_get("scheme")?,
                blob: Zeroizing::new(blob),
            },
        })
    }
}

/// 秘密のキー 1 つへの書き込みの意図（設計 §5.3 の表）。
///
/// | 場面 | `Keep` | `Clear` | `Set(row)` |
/// | ---- | ------ | ------- | ---------- |
/// | 作成 | 値無し | 値無し | `row` を書き、`settings[key]` を参照にする |
/// | 更新 | 今の参照と行を保つ（新しいプロトコルもそのキーを秘密に持つときだけ） | 参照と行を消す | 新しい行を足し、古い行を消し、参照を替える |
///
/// REST・MCP の平らな形の tri-state（`password` の省略・`null` / 空文字 / 値）は、Hub の
/// 入口でそれぞれ `Keep` / `Clear` / `Set(封印した値)` に写す（作成では省略と空文字が
/// どちらも「パスワード無し」なので、`Keep` と `Clear` は同じ結果になる。今の
/// `PlcConnectionInput::password` の作成の規則と同じ）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum SecretChange {
    #[default]
    Keep,
    Clear,
    Set(SealedSecretRow),
}

/// 秘密のキーごとの [`SecretChange`]。**無いキーは `Keep`**。
pub type SecretChanges = BTreeMap<String, SecretChange>;

/// `{"$secret": ref}`（設計 §3.3）。
pub fn secret_ref_value(r#ref: &str) -> Value {
    let mut object = Map::new();
    object.insert(SECRET_REF_KEY.to_string(), Value::String(r#ref.to_string()));
    Value::Object(object)
}

/// `settings[key]` の秘密の参照を読んだ結果（PR #777 のオーナーレビュー P2）。
///
/// **「キーが無い」と「キーはあるが参照の形でない」を分ける**: 後者を「値が無い」に潰すと、
/// 読む側が空のパスワードで繋ぎに行く（`trust` 認証なら通ってしまう）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretRefLookup<'a> {
    /// キーが無い（値が設定されていない）。
    Absent,
    /// 形の正しい参照（`{"$secret": "<32 文字の小文字 16 進>"}`、余計なキーなし）。
    Ref(&'a str),
    /// キーはあるが形の正しい参照ではない（`{"$secret": 123}`・余計なキー・16 進でない
    /// 参照・平文の文字列・`null` など）。書き込みの入口は [`check_settings_shape`] で
    /// 断るので、生の SQL や壊れた DB でだけ起こる。読む側は「読めない」に倒す。
    Malformed,
}

/// `settings[key]` を秘密の参照として読む（[`SecretRefLookup`]）。
pub fn secret_ref_lookup<'a>(settings: &'a Map<String, Value>, key: &str) -> SecretRefLookup<'a> {
    let Some(value) = settings.get(key) else {
        return SecretRefLookup::Absent;
    };
    if !is_secret_ref(value) {
        return SecretRefLookup::Malformed;
    }
    match value.get(SECRET_REF_KEY).and_then(Value::as_str) {
        Some(r#ref) if is_valid_secret_ref(r#ref) => SecretRefLookup::Ref(r#ref),
        _ => SecretRefLookup::Malformed,
    }
}

/// `settings[key]` が形の正しい秘密の参照なら、その参照の文字列。キーが無いときと形が
/// 違うときを分けるには [`secret_ref_lookup`] を使う（読む側はそちら）。
pub fn secret_ref_of<'a>(settings: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    match secret_ref_lookup(settings, key) {
        SecretRefLookup::Ref(r#ref) => Some(r#ref),
        SecretRefLookup::Absent | SecretRefLookup::Malformed => None,
    }
}

/// `value` が秘密の参照のつもりの値か（`$secret` のキーを持つオブジェクト。形が正しいかは
/// 問わない）。起動時の整合の確認が、レジストリの秘密のキー以外の壊れた参照も拾うため。
pub fn looks_like_secret_ref(value: &Value) -> bool {
    value
        .as_object()
        .is_some_and(|object| object.contains_key(SECRET_REF_KEY))
}

/// 参照の形（32 文字の小文字 16 進。DB の `CHECK` と同じ）。
pub fn is_valid_secret_ref(r#ref: &str) -> bool {
    r#ref.len() == SECRET_REF_LEN
        && r#ref
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// REST の項目名（`validate_plc_connection_input` の `field` と同じ綴り）。
fn rest_field(key: &str) -> String {
    match key {
        "unit_id" => "unitId".to_string(),
        "word_order" => "wordOrder".to_string(),
        "cpu_type" => "cpuType".to_string(),
        other => other.to_string(),
    }
}

/// 組み込みのプロトコルが読むキーの JSON の型（設計 §3.2 の表）。
#[derive(Clone, Copy)]
enum JsonKind {
    String,
    Integer,
}

fn builtin_key_kinds(protocol: &str) -> &'static [(&'static str, JsonKind)] {
    use JsonKind::{Integer, String};
    if protocol == MODBUS_PROTOCOL {
        &[
            ("host", String),
            ("port", Integer),
            ("unit_id", Integer),
            ("word_order", String),
        ]
    } else if protocol == crate::settings_json::legacy::SLMP_PROTOCOL {
        &[
            ("host", String),
            ("port", Integer),
            ("word_order", String),
            ("cpu_type", String),
        ]
    } else if protocol == POSTGRES_PROTOCOL {
        &[
            ("host", String),
            ("port", Integer),
            ("database", String),
            ("username", String),
        ]
    } else {
        &[]
    }
}

/// 保存の層の `settings` の検査（設計 §3.2・§3.3・§8 の 1、§10 の R2b-1d の申し送り）。
/// 書き込みの全入口が、DB へ書く直前の最終的な `settings` に掛ける。空なら通った。
///
/// 1. **秘密のキーに平文を入れない**: `secret_keys` のキーの値は、秘密の参照
///    （`{"$secret": "<32 文字の小文字 16 進>"}`）でなければ断る。エラーの文言に値を載せない。
/// 2. **組み込みのプロトコルのキーの型**（§3.2）: `modbus-tcp`・`slmp`・`postgres` の
///    `host`・`word_order`・`cpu_type`・`database`・`username` は文字列、`port`・`unit_id` は
///    整数。`virtual`・`hub` は `{}` だけ。これで「検証に通った JSON は
///    `legacy::flat_view` で同じ値に戻る」ことを保つ（`flat_view` は文字列は文字列として、
///    整数は JSON の整数として読む。例えば `"database": 123` は `settings_to_api` を経た
///    ドライバの検証は通るが、`flat_view` は `None` にしてしまう）。`push` の中身は
///    `validate_push_settings` が、プラグインのプロトコルはドライバが見る。
pub fn check_settings_shape(
    protocol: &str,
    settings: &Map<String, Value>,
    secret_keys: &[String],
) -> Vec<FieldError> {
    let mut errors = Vec::new();
    for key in secret_keys {
        let Some(value) = settings.get(key) else {
            continue;
        };
        let ok = is_secret_ref(value)
            && value
                .get(SECRET_REF_KEY)
                .and_then(Value::as_str)
                .is_some_and(is_valid_secret_ref);
        if !ok {
            errors.push(FieldError {
                field: rest_field(key),
                message: "秘密値は封印した参照としてだけ保存できます（平文は保存しません）"
                    .to_string(),
            });
        }
    }
    for (key, kind) in builtin_key_kinds(protocol) {
        let Some(value) = settings.get(*key) else {
            continue;
        };
        let ok = match kind {
            JsonKind::String => value.is_string(),
            JsonKind::Integer => value.as_i64().is_some(),
        };
        if !ok {
            let expected = match kind {
                JsonKind::String => "文字列",
                JsonKind::Integer => "整数",
            };
            errors.push(FieldError {
                field: rest_field(key),
                message: format!("{key} は{expected}で指定してください"),
            });
        }
    }
    if (protocol == VIRTUAL_PROTOCOL || protocol == HUB_PROTOCOL) && !settings.is_empty() {
        errors.push(FieldError {
            field: "settings".to_string(),
            message: format!("{protocol} 接続は設定を持ちません"),
        });
    }
    // push の中身は `validate_push_settings`（書き込みの (1)）が見る。
    errors
}

/// `connection_id` の秘密の行を書く（作成・更新の `Set`）。呼び出し側のトランザクションの中で。
pub(crate) async fn insert_secret_rows(
    connection: &mut SqliteConnection,
    connection_id: i64,
    rows: &[(String, SealedSecretRow)],
) -> Result<(), sqlx::Error> {
    for (key, row) in rows {
        sqlx::query(
            "INSERT INTO plc_connection_secrets (ref, connection_id, key, scheme, blob) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&row.r#ref)
        .bind(connection_id)
        .bind(key)
        .bind(&row.scheme)
        .bind(row.blob.as_slice())
        .execute(&mut *connection)
        .await?;
    }
    Ok(())
}

/// `connection_id` の秘密の行のうち、参照が `keep_refs` に無いものを消す（更新の `Clear`・
/// `Set` の古い行・プロトコルの変更で秘密のキーでなくなったもの。設計 §5.3）。
pub(crate) async fn delete_secret_rows_except(
    connection: &mut SqliteConnection,
    connection_id: i64,
    keep_refs: &[String],
) -> Result<(), sqlx::Error> {
    let keep = Value::Array(keep_refs.iter().cloned().map(Value::String).collect()).to_string();
    sqlx::query(
        "DELETE FROM plc_connection_secrets WHERE connection_id = ? \
         AND ref NOT IN (SELECT value FROM json_each(?))",
    )
    .bind(connection_id)
    .bind(keep)
    .execute(&mut *connection)
    .await?;
    Ok(())
}

/// `connection_id` の秘密の行をすべて消す（削除・一括削除、設計 §5.3）。
pub(crate) async fn delete_all_secret_rows(
    connection: &mut SqliteConnection,
    connection_id: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM plc_connection_secrets WHERE connection_id = ?")
        .bind(connection_id)
        .execute(&mut *connection)
        .await?;
    Ok(())
}

const SECRET_ROWS_SQL: &str = "SELECT ref, connection_id, key, scheme, blob \
     FROM plc_connection_secrets ORDER BY connection_id, key";
const SECRET_ROWS_OF_SQL: &str = "SELECT ref, connection_id, key, scheme, blob \
     FROM plc_connection_secrets WHERE connection_id = ? ORDER BY key";

/// 秘密の行をすべて読む（収集の計画の `RegistrySnapshot`、設計 §7）。`executor` は
/// プールか、呼び出し側のトランザクション（同じ時点の接続の行と揃えるため）。
pub async fn load_all_secret_rows<'c, E>(executor: E) -> Result<Vec<StoredSecretRow>, sqlx::Error>
where
    E: sqlx::Executor<'c, Database = Sqlite>,
{
    sqlx::query_as::<_, StoredSecretRow>(SECRET_ROWS_SQL)
        .fetch_all(executor)
        .await
}

/// `connection_id` の秘密の行を読む。
pub async fn load_secret_rows_of<'c, E>(
    executor: E,
    connection_id: i64,
) -> Result<Vec<StoredSecretRow>, sqlx::Error>
where
    E: sqlx::Executor<'c, Database = Sqlite>,
{
    sqlx::query_as::<_, StoredSecretRow>(SECRET_ROWS_OF_SQL)
        .bind(connection_id)
        .fetch_all(executor)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn object(value: Value) -> Map<String, Value> {
        match value {
            Value::Object(map) => map,
            other => panic!("not an object: {other}"),
        }
    }

    const REF: &str = "0123456789abcdef0123456789abcdef";

    fn keys(keys: &[&str]) -> Vec<String> {
        keys.iter().map(|k| k.to_string()).collect()
    }

    #[test]
    fn a_secret_key_accepts_only_a_well_formed_reference() {
        let pw = keys(&["password"]);
        let ok = object(json!({"host": "h", "port": 1, "password": {"$secret": REF}}));
        assert!(check_settings_shape("postgres", &ok, &pw).is_empty());
        let absent = object(json!({"host": "h", "port": 1}));
        assert!(check_settings_shape("postgres", &absent, &pw).is_empty());
        for bad in [
            json!("plain-text-password"),
            json!(""),
            json!(42),
            json!(null),
            json!({"$secret": "not-hex"}),
            json!({"$secret": REF.to_uppercase()}),
            json!({"$secret": REF, "extra": 1}),
            json!(["plain"]),
        ] {
            let settings = object(json!({"host": "h", "port": 1, "password": bad.clone()}));
            let errors = check_settings_shape("postgres", &settings, &pw);
            assert_eq!(errors.len(), 1, "{bad}");
            assert_eq!(errors[0].field, "password");
            assert!(
                !errors[0].message.contains("plain-text-password"),
                "the message must not echo the value"
            );
        }
        // The same plaintext under a key the protocol does not hold as a secret is not
        // judged here (the key list comes from the driver / the built-in table).
        let other = object(json!({"password": "x"}));
        assert!(check_settings_shape("plugin-x", &other, &[]).is_empty());
        // Any protocol: a driver-declared secret key refuses plaintext too.
        let token = object(json!({"token": "x"}));
        assert_eq!(
            check_settings_shape("plugin-x", &token, &keys(&["token"])).len(),
            1
        );
    }

    #[test]
    fn builtin_keys_must_have_the_design_3_2_types() {
        let good = [
            (
                "modbus-tcp",
                json!({"host": "h", "port": 502, "unit_id": 0, "word_order": "high_low"}),
            ),
            (
                "slmp",
                json!({"host": "h", "port": 5007, "word_order": "low_high", "cpu_type": "Q"}),
            ),
            (
                "postgres",
                json!({"host": "h", "port": 5432, "database": "d", "username": "u"}),
            ),
            ("virtual", json!({})),
            ("hub", json!({})),
            ("push", json!({"stale_timeout_ms": 1000})),
        ];
        for (protocol, settings) in good {
            assert!(
                check_settings_shape(protocol, &object(settings.clone()), &[]).is_empty(),
                "{protocol}: {settings}"
            );
        }
        let bad = [
            ("modbus-tcp", json!({"port": "502"}), "port"),
            ("modbus-tcp", json!({"port": 1.5}), "port"),
            ("modbus-tcp", json!({"unit_id": "1"}), "unitId"),
            ("modbus-tcp", json!({"host": 5}), "host"),
            ("slmp", json!({"word_order": 1}), "wordOrder"),
            ("slmp", json!({"cpu_type": null}), "cpuType"),
            ("postgres", json!({"database": 123}), "database"),
            ("postgres", json!({"username": true}), "username"),
            ("virtual", json!({"host": "h"}), "settings"),
            ("hub", json!({"x": 1}), "settings"),
        ];
        for (protocol, settings, field) in bad {
            let errors = check_settings_shape(protocol, &object(settings.clone()), &[]);
            assert_eq!(errors.len(), 1, "{protocol}: {settings}");
            assert_eq!(errors[0].field, field, "{protocol}: {settings}");
        }
    }

    #[test]
    fn the_sealed_row_debug_shows_no_blob() {
        let row = SealedSecretRow {
            r#ref: REF.to_string(),
            scheme: SCHEME_PLAIN_V1.to_string(),
            blob: Zeroizing::new(b"TOPSECRET-blob".to_vec()),
        };
        let shown = format!("{row:?} {:?}", SecretChange::Set(row.clone()));
        assert!(shown.contains(REF), "{shown}");
        assert!(!shown.contains("TOPSECRET"), "{shown}");
        let stored = StoredSecretRow {
            connection_id: 1,
            key: "password".to_string(),
            sealed: row,
        };
        assert!(!format!("{stored:?}").contains("TOPSECRET"));
    }

    #[test]
    fn secret_ref_helpers_round_trip() {
        let settings = object(json!({"password": secret_ref_value(REF), "host": "h"}));
        assert_eq!(secret_ref_of(&settings, "password"), Some(REF));
        assert_eq!(secret_ref_of(&settings, "host"), None);
        assert_eq!(secret_ref_of(&settings, "missing"), None);
        assert_eq!(
            secret_ref_lookup(&settings, "missing"),
            SecretRefLookup::Absent
        );
        assert_eq!(
            secret_ref_lookup(&settings, "password"),
            SecretRefLookup::Ref(REF)
        );
        // A present key that is not a well-formed reference is Malformed, never Absent.
        for bad in [
            serde_json::json!({ "$secret": 123 }),
            serde_json::json!({ "$secret": REF, "extra": 1 }),
            serde_json::json!({ "$secret": "NOT-HEX" }),
            serde_json::json!({ "$secret": REF.to_uppercase() }),
            serde_json::json!("plain-text"),
            serde_json::Value::Null,
            serde_json::json!({}),
        ] {
            let mut settings = Map::new();
            settings.insert("password".to_string(), bad.clone());
            assert_eq!(
                secret_ref_lookup(&settings, "password"),
                SecretRefLookup::Malformed,
                "{bad}"
            );
            assert_eq!(secret_ref_of(&settings, "password"), None, "{bad}");
        }
        assert!(looks_like_secret_ref(&serde_json::json!({ "$secret": 1 })));
        assert!(!looks_like_secret_ref(&serde_json::json!("x")));
        assert!(is_valid_secret_ref(REF));
        assert!(!is_valid_secret_ref(&REF[..31]));
        assert_eq!(builtin_secret_keys("postgres"), ["password"]);
        assert!(builtin_secret_keys("modbus-tcp").is_empty());
    }
}
