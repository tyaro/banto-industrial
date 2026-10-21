//! `describe` の結果と、欲しい列名の突き合わせ（docs/banto-hub-external-db-design.md
//! §4.1・§4.3・§4.4）。
//!
//! 元は `banto-hub-core` の `db_source/plan.rs` の `resolve_group` /
//! `TagBadReason` / `ResolvedGroup`。Hub の `DbGroupPlan` / `DbTagPlan` に
//! 依存していた入力を、**列名の並び**（添字で対応づける）という中立な形に
//! 替えた - 呼び出し側（Hub は `plan::resolve_group`、ブリッジは送るキーの並び）
//! が添字をタグ・キーへ写し戻す。判断のロジックは移す前と同じ。

use std::collections::HashMap;

use crate::convert::{build_wrapper_sql, ColumnKind, MappedColumn};

/// 1列（= 1タグ）が値を取れない確定的な理由（describe の時点で分かるもの）。
/// 毎ティック Bad を書きつつ、`warn` はプラン単位で1回だけ出す
/// （§4.3 の表「列が無い / 型変換不能 → タグ単位 Bad（起動時と設定変更時に
/// `warn`）」）。Hub は `TagBadReason` の名前で再輸出している。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColumnBadReason {
    /// SELECT の結果にその名前の列が無い。
    MissingColumn,
    /// 列はあるが v1 が扱えない型（`text`/`bytea`/配列/`json`…、§4.4）。
    UnsupportedColumnType(String),
    /// グループの SQL 自体を describe（prepare）できなかった - 構文エラー・
    /// 存在しない表・権限不足など。列単位ではなくグループ全体の問題だが、
    /// 値を書く単位はタグなので同じ列挙で扱う（[`resolve_columns`] は返さず、
    /// 呼び出し側が describe の失敗時に使う）。
    StatementNotPrepared(String),
}

impl ColumnBadReason {
    pub fn message(&self) -> String {
        match self {
            ColumnBadReason::MissingColumn => "SELECT の結果にこの列がありません".to_string(),
            ColumnBadReason::UnsupportedColumnType(type_name) => {
                format!("列の型 {type_name} は v1 では扱えません（数値・bool・timestamp のみ）")
            }
            ColumnBadReason::StatementNotPrepared(detail) => {
                format!("グループの SQL を実行できません: {detail}")
            }
        }
    }
}

/// [`resolve_columns`] の結果 - describe の結果と欲しい列を突き合わせた、
/// 1グループ分の実行形。添字はすべて `resolve_columns` の `wanted` の添字。
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedColumns {
    /// 実際に実行する生成済み SQL（[`build_wrapper_sql`]）。値の取れる列が
    /// 1つも無いグループでは `None`（SELECT を投げても意味が無いので、その
    /// グループはティックごとに全タグ Bad を書くだけになる）。
    pub wrapper_sql: Option<String>,
    /// 値の取れる列の `wanted` の添字。`wrapper_sql` の射影順で、`c{index}` の
    /// `index` がそのままこの `Vec` の添字。
    pub value_columns: Vec<usize>,
    /// 値が取れない列の `wanted` の添字とその理由（`wanted` の順）。
    pub bad_columns: Vec<(usize, ColumnBadReason)>,
}

/// `describe` が返した「列名 → PostgreSQL 型名」に対して、欲しい列
/// （`wanted`。タグの `address` の並び）を解決する（純関数 - 実 DB を触らない
/// のでユニットテストできる）。
pub fn resolve_columns(
    query_sql: &str,
    wanted: &[&str],
    described: &[(String, String)],
) -> ResolvedColumns {
    // §4.1「大文字小文字は DB 側の規則に委ねずそのまま比較」: 完全一致の
    // マップを作る。同名列が2つある SELECT（`SELECT a, a FROM t`）では
    // 先に現れた方が勝つ - どちらを採るかは本質的に恣意的で、先頭優先は
    // 「先頭行を採る」§4.2 と同じ一貫した選択。
    let mut by_name: HashMap<&str, &str> = HashMap::with_capacity(described.len());
    for (name, type_name) in described {
        by_name.entry(name.as_str()).or_insert(type_name.as_str());
    }

    let mut columns = Vec::new();
    let mut value_columns = Vec::new();
    let mut bad_columns = Vec::new();
    for (index, column) in wanted.iter().enumerate() {
        match by_name.get(column) {
            None => bad_columns.push((index, ColumnBadReason::MissingColumn)),
            Some(type_name) => match ColumnKind::classify(type_name) {
                None => bad_columns.push((
                    index,
                    ColumnBadReason::UnsupportedColumnType((*type_name).to_string()),
                )),
                Some(kind) => {
                    columns.push(MappedColumn {
                        column: (*column).to_string(),
                        kind,
                    });
                    value_columns.push(index);
                }
            },
        }
    }

    let wrapper_sql = (!columns.is_empty()).then(|| build_wrapper_sql(query_sql, &columns));

    ResolvedColumns {
        wrapper_sql,
        value_columns,
        bad_columns,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_columns_splits_value_columns_from_bad_ones() {
        let wanted = ["a", "flag", "ts", "note", "missing"];
        let described = vec![
            ("a".to_string(), "INT4".to_string()),
            ("flag".to_string(), "BOOL".to_string()),
            ("ts".to_string(), "TIMESTAMPTZ".to_string()),
            ("note".to_string(), "TEXT".to_string()),
        ];
        let resolved = resolve_columns("SELECT * FROM v1", &wanted, &described);
        assert_eq!(
            resolved
                .value_columns
                .iter()
                .map(|&i| wanted[i])
                .collect::<Vec<_>>(),
            vec!["a", "flag", "ts"]
        );
        assert_eq!(resolved.bad_columns.len(), 2);
        assert_eq!(
            resolved.bad_columns[0],
            (
                3,
                ColumnBadReason::UnsupportedColumnType("TEXT".to_string())
            )
        );
        assert_eq!(resolved.bad_columns[1], (4, ColumnBadReason::MissingColumn));
        let sql = resolved.wrapper_sql.expect("three value columns");
        assert!(sql.contains("(\"a\")::float8 AS c0"), "{sql}");
        assert!(sql.contains("(\"flag\")::int4::float8 AS c1"), "{sql}");
    }

    /// 列名の比較は大文字小文字を区別する（§4.1）。
    #[test]
    fn resolve_columns_compares_column_names_case_sensitively() {
        let described = vec![("rate".to_string(), "INT4".to_string())];
        let resolved = resolve_columns("SELECT * FROM v1", &["Rate"], &described);
        assert!(resolved.wrapper_sql.is_none());
        assert_eq!(resolved.bad_columns[0].1, ColumnBadReason::MissingColumn);
    }

    #[test]
    fn resolve_columns_with_no_usable_column_emits_no_sql() {
        let described = vec![("note".to_string(), "TEXT".to_string())];
        let resolved = resolve_columns("SELECT * FROM v1", &["note"], &described);
        assert!(resolved.wrapper_sql.is_none());
        assert!(resolved.value_columns.is_empty());
        assert_eq!(resolved.bad_columns.len(), 1);
    }
}
