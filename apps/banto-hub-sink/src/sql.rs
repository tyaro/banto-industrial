//! 保存先テーブルの識別子検証・引用と、SQL の組み立て（設計 §5.3・§5.4）。
//!
//! ## 値は 1 つも SQL に埋め込まない
//!
//! multi-row INSERT の**行数だけ**が動的で、値はすべて bind パラメータ
//! （`$1`, `$2`, …）で渡す。組み立て時に値の文字列連結を行わないので、
//! タグ名や quality の中身が SQL に混ざる経路が構造的に存在しない。
//!
//! ## テーブル名は「Hub で検証済み」だが、ここでも必ず再検証する
//!
//! `banto_hub_core::sink::service` の `is_valid_table_name` と**同じ規則**
//! （`[A-Za-z_][A-Za-z0-9_]{0,62}` を `.` で最大 2 セグメント）を
//! [`is_valid_table_name`] に写してある。Hub 側が検証済みだからといって
//! サイドカーが信用すると、「Hub の DB を直接書き換えた」「別バージョンの
//! Hub と話している」といった経路で識別子が SQL へそのまま入ってしまう -
//! 境界を越えた値は境界のこちら側で検証する、という原則に従う。
//!
//! ## 引用の作法
//!
//! 検証を通った識別子をダブルクォートで囲む（`"schema"."table"`）。
//! 規則上 `"` は含まれ得ないので脱出処理は不要だが、引用する以上
//! **大文字小文字は区別される**（PostgreSQL は無引用の識別子を小文字へ
//! 畳む）。`TagHistory` という Hub 設定は `"TagHistory"` を指す -
//! 無引用で `create table TagHistory` した表（実体は `taghistory`）とは
//! 別物になるので、UI 側では小文字での登録を案内する（S6）。
//!
//! ## パラメータ数の上限
//!
//! PostgreSQL の 1 文あたりの bind パラメータ上限は 65535。1 行 5 列
//! （`ts`/`tag_id`/`external_name`/`value`/`quality`）なので理論上限は
//! 13,107 行で、`batch_size` の上限 10,000（[`crate::config`]）はその内側。

/// long スキーマの列（設計 §5.3）。`SELECT ... LIMIT 0` の検査もこの順で
/// 行うので、定義はここ 1 箇所。
pub const COLUMNS: [&str; 5] = ["ts", "tag_id", "external_name", "value", "quality"];

/// 1 行あたりの bind パラメータ数。
pub const PARAMS_PER_ROW: usize = COLUMNS.len();

/// 識別子 1 セグメントの最大長（PostgreSQL の `NAMEDATALEN - 1`。Hub 側
/// `MAX_TABLE_IDENTIFIER_LEN` と同じ値）。
const MAX_IDENTIFIER_LEN: usize = 63;

/// テーブルが無いときに `warn` で案内する推奨 DDL（設計 §5.3・§6-7:
/// **Hub もサイドカーも DDL は発行しない**。人が見て流すための文面）。
pub fn recommended_ddl(table_name: &str) -> String {
    format!(
        "CREATE TABLE {} (ts timestamptz NOT NULL, tag_id bigint NOT NULL, \
         external_name text NOT NULL, value double precision, quality text NOT NULL)",
        quote_table_name(table_name)
    )
}

fn is_valid_identifier_segment(segment: &str) -> bool {
    if segment.is_empty() || segment.len() > MAX_IDENTIFIER_LEN {
        return false;
    }
    let mut chars = segment.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `table` または `schema.table`（このモジュールの doc comment参照）。
pub fn is_valid_table_name(table_name: &str) -> bool {
    let segments: Vec<&str> = table_name.split('.').collect();
    match segments.as_slice() {
        [table] => is_valid_identifier_segment(table),
        [schema, table] => {
            is_valid_identifier_segment(schema) && is_valid_identifier_segment(table)
        }
        _ => false,
    }
}

/// 検証済みのテーブル名を `"schema"."table"` へ引用する。**検証を通って
/// いない文字列を渡してはいけない**（[`Table::new`] が唯一の入口）。
fn quote_table_name(table_name: &str) -> String {
    table_name
        .split('.')
        .map(|segment| format!("\"{segment}\""))
        .collect::<Vec<_>>()
        .join(".")
}

/// 検証と引用を 1 度だけ行い、以後は引用済み文字列を使い回すための型。
/// 「検証されていないテーブル名は存在しない」ことを型で表す。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Table {
    raw: String,
    quoted: String,
}

impl Table {
    /// 検証に通らなければ `None`（呼び出し側はそのグループを `error` に
    /// する - 設計 §5.3）。
    pub fn new(table_name: &str) -> Option<Self> {
        if !is_valid_table_name(table_name) {
            return None;
        }
        Some(Self {
            raw: table_name.to_string(),
            quoted: quote_table_name(table_name),
        })
    }

    pub fn raw(&self) -> &str {
        &self.raw
    }

    pub fn quoted(&self) -> &str {
        &self.quoted
    }

    /// 起動時・再接続後のテーブル検査（設計 §5.3「`SELECT ... LIMIT 0` で
    /// 列の存在を検査」）。行は 1 件も返らないので、DB の負荷も権限要件も
    /// 最小（`SELECT` 権限のみ）。
    pub fn check_sql(&self) -> String {
        format!("SELECT {} FROM {} LIMIT 0", COLUMNS.join(", "), self.quoted)
    }

    /// `rows` 行ぶんの multi-row INSERT（設計 §5.4「1 回の INSERT は
    /// トランザクション 1 つ」）。`ts` だけ `to_timestamp($n)` で包むのは、
    /// このクレートが日付型クレート（chrono/time の sqlx 統合）を持たず
    /// epoch 秒（`double precision`）で渡すため - PostgreSQL 側で
    /// `timestamptz` へ変換する。
    ///
    /// `rows == 0` では呼ばない（呼び出し側が空バッチを弾く）。
    pub fn insert_sql(&self, rows: usize) -> String {
        debug_assert!(rows > 0, "insert_sql must not be called with zero rows");
        let mut sql = String::with_capacity(64 + rows * 32);
        sql.push_str("INSERT INTO ");
        sql.push_str(&self.quoted);
        sql.push_str(" (");
        sql.push_str(&COLUMNS.join(", "));
        sql.push_str(") VALUES ");
        for row in 0..rows {
            if row > 0 {
                sql.push(',');
            }
            let base = row * PARAMS_PER_ROW;
            sql.push_str(&format!(
                "(to_timestamp(${}),${},${},${},${})",
                base + 1,
                base + 2,
                base + 3,
                base + 4,
                base + 5
            ));
        }
        sql
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_table_names_match_the_hub_side_rule() {
        for good in [
            "t",
            "_t",
            "tag_history",
            "public.tag_history",
            "_s._t9",
            "T",
        ] {
            assert!(is_valid_table_name(good), "{good} should be valid");
        }
    }

    #[test]
    fn invalid_table_names_are_rejected() {
        let too_long = "a".repeat(64);
        for bad in [
            "",
            ".",
            "a.",
            ".a",
            "1tag",
            "a.b.c",
            "tag-history",
            "tag history",
            "tag\"history",
            "tag;drop",
            "タグ",
            too_long.as_str(),
        ] {
            assert!(!is_valid_table_name(bad), "{bad} should be rejected");
            assert!(Table::new(bad).is_none(), "{bad} should not build a Table");
        }
        // 63 文字ちょうどは通る（境界）。
        assert!(is_valid_table_name(&"a".repeat(63)));
    }

    #[test]
    fn quoting_is_per_segment() {
        let table = Table::new("public.tag_history").expect("valid");
        assert_eq!(table.quoted(), "\"public\".\"tag_history\"");
        assert_eq!(table.raw(), "public.tag_history");

        let table = Table::new("tag_history").expect("valid");
        assert_eq!(table.quoted(), "\"tag_history\"");
    }

    #[test]
    fn check_sql_selects_every_column_with_limit_zero() {
        let table = Table::new("public.tag_history").expect("valid");
        assert_eq!(
            table.check_sql(),
            "SELECT ts, tag_id, external_name, value, quality FROM \"public\".\"tag_history\" LIMIT 0"
        );
    }

    #[test]
    fn insert_sql_numbers_parameters_across_rows() {
        let table = Table::new("tag_history").expect("valid");
        assert_eq!(
            table.insert_sql(1),
            "INSERT INTO \"tag_history\" (ts, tag_id, external_name, value, quality) \
             VALUES (to_timestamp($1),$2,$3,$4,$5)"
        );
        assert_eq!(
            table.insert_sql(3),
            "INSERT INTO \"tag_history\" (ts, tag_id, external_name, value, quality) \
             VALUES (to_timestamp($1),$2,$3,$4,$5),(to_timestamp($6),$7,$8,$9,$10),\
             (to_timestamp($11),$12,$13,$14,$15)"
        );
    }

    /// バッチが大きくてもパラメータ番号が連番であること（PostgreSQL の
    /// 上限 65535 の内側に収まることの確認も兼ねる）。
    #[test]
    fn a_full_size_batch_stays_within_the_parameter_limit() {
        let table = Table::new("t").expect("valid");
        let rows = 10_000;
        let sql = table.insert_sql(rows);
        assert!(sql.ends_with(&format!(
            "(to_timestamp(${}),${},${},${},${})",
            rows * 5 - 4,
            rows * 5 - 3,
            rows * 5 - 2,
            rows * 5 - 1,
            rows * 5
        )));
        assert!(rows * PARAMS_PER_ROW <= 65_535);
    }

    #[test]
    fn recommended_ddl_quotes_the_table_and_matches_the_design_schema() {
        let ddl = recommended_ddl("public.tag_history");
        assert!(
            ddl.starts_with("CREATE TABLE \"public\".\"tag_history\" ("),
            "{ddl}"
        );
        assert!(ddl.contains("ts timestamptz NOT NULL"), "{ddl}");
        assert!(ddl.contains("tag_id bigint NOT NULL"), "{ddl}");
        assert!(ddl.contains("external_name text NOT NULL"), "{ddl}");
        assert!(ddl.contains("value double precision"), "{ddl}");
        assert!(ddl.contains("quality text NOT NULL"), "{ddl}");
    }
}
