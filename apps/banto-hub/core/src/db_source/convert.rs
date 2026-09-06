//! DB Source の「列 → f64」変換戦略（docs/banto-hub-external-db-design.md
//! §4.4「値の型（v1 は数値・bool のみ）」）。
//!
//! ## なぜ describe + 生成ラッパーなのか
//!
//! [`crate::computed::ServerTagStore`] の値スロットは `Option<f64>` 1種類
//! しかない（設計 §3-5）。一方 PostgreSQL の結果列は `int4`/`numeric`/
//! `bool`/`timestamptz`/… と多様で、sqlx で素直に受けようとすると列型ごとに
//! `try_get::<i32>` / `try_get::<rust_decimal::Decimal>` /
//! `try_get::<chrono::DateTime<Utc>>` … と**型ごとの feature とクレート**が
//! 必要になる（`numeric` は `rust_decimal`/`bigdecimal`、時刻は
//! `chrono`/`time`）。S0 の依存実測（§3.1）で足したのは
//! `postgres` + `tls-rustls-ring-native-roots` だけであり、ここで型 feature を
//! 積み増すのは「依存追加は慎重に」という本ワークスペースの方針に反する。
//!
//! そこで v1 は**キャストを DB 側にやらせる**:
//!
//! 1. 起動時（と再接続後）に利用者の SQL を1回だけ
//!    [`describe_wrapper_sql`]（`SELECT * FROM (<sql>) AS q`）で `describe`
//!    し、**結果列名と PostgreSQL 型**を得る。
//! 2. タグが対応づけている列だけを選び、型に応じたキャストを付けた
//!    [`build_wrapper_sql`] を組み立てて実行する。全列が `float8` で返るので、
//!    sqlx 側は `Option<f64>` の一様な取り出ししか行わない。
//!
//! | PostgreSQL 型                                  | 生成するキャスト                        | 値              |
//! | ---------------------------------------------- | --------------------------------------- | --------------- |
//! | `int2`/`int4`/`int8`/`float4`/`float8`/`numeric` | `("col")::float8`                       | そのまま        |
//! | `bool`                                          | `("col")::int4::float8`                 | `1.0` / `0.0`   |
//! | `timestamp`/`timestamptz`/`date`                | `(EXTRACT(EPOCH FROM "col") * 1000)::float8` | epoch ミリ秒 |
//! | それ以外（`text`/`bytea`/配列/`json`…）         | 生成しない                              | そのタグは Bad  |
//!
//! `numeric` → `float8` は精度が落ちうる（PostgreSQL の `numeric` は任意精度）
//! が、`ServerTagStore` が `f64` である以上どこかで必ず起きる丸めであり、
//! DB 側でやるか Rust 側でやるかの違いしかない。§4.4 の「numeric/decimal
//! （f64 へ変換）」がまさにこれを織り込んだ決定。
//!
//! ## 識別子のクォート
//!
//! 列名は [`quote_ident`] で必ず `"..."` で囲み、内部の `"` は `""` に
//! 二重化する。`banto_tags` 側の登録時検証（`db` タグの `address` は
//! `^[A-Za-z_][A-Za-z0-9_]*$`）が既に `"` を含む列名を弾いているので二重の
//! 防御だが、生成 SQL を組み立てる側が自前で正しく閉じられることを、
//! レジストリの検証に依存せず単体で示せるようにしてある。

use sqlx::{Column, TypeInfo};

/// 結果列の PostgreSQL 型を、v1 が扱える3つの変換クラスへ分類したもの
/// （このモジュールの doc comment の表）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnKind {
    /// 整数・浮動小数・`numeric` - `::float8` でそのまま。
    Numeric,
    /// `bool` - `::int4::float8` で `1.0`/`0.0`。
    Bool,
    /// `timestamp`/`timestamptz`/`date` - epoch ミリ秒。
    Epoch,
}

impl ColumnKind {
    /// `sqlx` の `PgTypeInfo::name()` が返す型名（大文字、例 `"INT4"`・
    /// `"TIMESTAMPTZ"`）から分類する。未対応型は `None`（そのタグは Bad -
    /// [`crate::db_source::plan::TagBadReason::UnsupportedColumnType`]）。
    ///
    /// ドメイン型・列挙型など、名前が既知集合に無いものはすべて未対応扱い
    /// にする（安全側 - 知らない型を `::float8` へキャストしようとして
    /// 実行時に文全体を落とすより、そのタグ1本だけを Bad にする方が影響が
    /// 小さい）。
    pub fn classify(pg_type_name: &str) -> Option<Self> {
        match pg_type_name.to_ascii_uppercase().as_str() {
            "INT2" | "SMALLINT" | "INT4" | "INT" | "INTEGER" | "INT8" | "BIGINT" | "FLOAT4"
            | "REAL" | "FLOAT8" | "DOUBLE PRECISION" | "NUMERIC" | "DECIMAL" => {
                Some(ColumnKind::Numeric)
            }
            "BOOL" | "BOOLEAN" => Some(ColumnKind::Bool),
            "TIMESTAMP" | "TIMESTAMPTZ" | "DATE" => Some(ColumnKind::Epoch),
            _ => None,
        }
    }
}

/// `describe` に渡す文（このモジュールの doc comment 手順1）。利用者の SQL
/// を副問い合わせで包むので、`SELECT ...` でも `WITH ... SELECT ...` でも
/// 同じ形で列一覧を取れる。
pub fn describe_wrapper_sql(query_sql: &str) -> String {
    format!("SELECT * FROM (\n{query_sql}\n) AS {SUBQUERY_ALIAS}")
}

/// 生成 SQL が利用者の SQL を包むときの副問い合わせ別名。利用者の SQL 内の
/// 別名と衝突しないよう、意図的に長く固有な名前にしてある。
const SUBQUERY_ALIAS: &str = "banto_db_source_q";

/// 1本のタグが対応づけている結果列と、その変換クラス。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MappedColumn {
    /// 結果列名（`db` タグの `address` そのもの、大文字小文字を保つ）。
    pub column: String,
    pub kind: ColumnKind,
}

/// SQL 識別子を `"..."` で囲む（内部の `"` は `""` へ二重化）。
pub fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// このモジュールの doc comment 手順2の生成 SQL。`columns` の順に
/// `c0`, `c1`, … の別名を付けるので、呼び出し側は**列名ではなく位置**で
/// 値を取り出せる（利用者の列名がどんな大文字小文字・記号を含んでいても、
/// 取り出し側は影響を受けない）。
///
/// 末尾の `LIMIT 2` は「先頭1行採用・2行以上なら warn 1回」（§4.2・§6-8）を
/// 判定するのに必要十分な行数 - 1周期ごとに何万行も Hub のメモリへ引き込む
/// ことを防ぐ。結果として状態表示の `row_count_last` は 0/1/2 の3値になり、
/// `2` は「2行以上」を意味する（[`crate::db_source::status`] 参照）。
pub fn build_wrapper_sql(query_sql: &str, columns: &[MappedColumn]) -> String {
    debug_assert!(
        !columns.is_empty(),
        "build_wrapper_sql must not be called for a group with no mapped columns"
    );
    let projection = columns
        .iter()
        .enumerate()
        .map(|(index, mapped)| {
            let quoted = quote_ident(&mapped.column);
            let expr = match mapped.kind {
                ColumnKind::Numeric => format!("({quoted})::float8"),
                // bool → int4 → float8 の2段キャスト: PostgreSQL は
                // bool から float8 への直接キャストを持たない。
                ColumnKind::Bool => format!("({quoted})::int4::float8"),
                ColumnKind::Epoch => {
                    format!("(EXTRACT(EPOCH FROM {quoted}) * 1000)::float8")
                }
            };
            format!("{expr} AS c{index}")
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("SELECT {projection} FROM (\n{query_sql}\n) AS {SUBQUERY_ALIAS} LIMIT 2")
}

/// `describe` の結果を「列名 → PostgreSQL 型名」の一覧へ落とす。列名は
/// **そのまま**（大文字小文字を変えない）- §4.1「大文字小文字は DB 側の
/// 規則に委ねずそのまま比較」。
pub fn describe_columns(describe: &sqlx::Describe<sqlx::Postgres>) -> Vec<(String, String)> {
    describe
        .columns()
        .iter()
        .map(|column| {
            (
                column.name().to_string(),
                column.type_info().name().to_string(),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_maps_the_v1_type_vocabulary() {
        for name in [
            "INT2", "INT4", "INT8", "FLOAT4", "FLOAT8", "NUMERIC", "int4", "numeric",
        ] {
            assert_eq!(
                ColumnKind::classify(name),
                Some(ColumnKind::Numeric),
                "{name}"
            );
        }
        assert_eq!(ColumnKind::classify("BOOL"), Some(ColumnKind::Bool));
        for name in ["TIMESTAMP", "TIMESTAMPTZ", "DATE"] {
            assert_eq!(
                ColumnKind::classify(name),
                Some(ColumnKind::Epoch),
                "{name}"
            );
        }
        for name in [
            "TEXT", "VARCHAR", "BYTEA", "JSONB", "INT4[]", "UUID", "TIME",
        ] {
            assert_eq!(ColumnKind::classify(name), None, "{name}");
        }
    }

    #[test]
    fn quote_ident_wraps_and_doubles_embedded_quotes() {
        assert_eq!(quote_ident("rate"), "\"rate\"");
        assert_eq!(quote_ident("Rate_1"), "\"Rate_1\"");
        // レジストリの検証がここまで来させない形だが、生成側単体で閉じて
        // いることを固定する（このモジュールの doc comment「識別子の
        // クォート」節）。
        assert_eq!(quote_ident("a\"b"), "\"a\"\"b\"");
    }

    #[test]
    fn build_wrapper_sql_casts_each_kind_and_aliases_by_position() {
        let sql = build_wrapper_sql(
            "SELECT a, b, ts FROM v1",
            &[
                MappedColumn {
                    column: "a".to_string(),
                    kind: ColumnKind::Numeric,
                },
                MappedColumn {
                    column: "b".to_string(),
                    kind: ColumnKind::Bool,
                },
                MappedColumn {
                    column: "ts".to_string(),
                    kind: ColumnKind::Epoch,
                },
            ],
        );
        assert!(sql.starts_with("SELECT (\"a\")::float8 AS c0, "), "{sql}");
        assert!(sql.contains("(\"b\")::int4::float8 AS c1"), "{sql}");
        assert!(
            sql.contains("(EXTRACT(EPOCH FROM \"ts\") * 1000)::float8 AS c2"),
            "{sql}"
        );
        assert!(sql.contains("SELECT a, b, ts FROM v1"), "{sql}");
        assert!(sql.ends_with(") AS banto_db_source_q LIMIT 2"), "{sql}");
    }

    #[test]
    fn describe_wrapper_sql_wraps_the_user_statement() {
        assert_eq!(
            describe_wrapper_sql("SELECT 1 AS a"),
            "SELECT * FROM (\nSELECT 1 AS a\n) AS banto_db_source_q"
        );
    }
}
