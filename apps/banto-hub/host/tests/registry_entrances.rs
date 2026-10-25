//! R2b-1d（docs/banto-hub-connection-settings-design.md §4.2・§8 の 10）／R2b-2b
//! （docs/banto-hub-driver-address-design.md §3.1）: Hub の入口が旧規則で書けないことを、
//! ソースの grep で固定する。R2b-2c でアドレスの解析の 2 つ（`Address::parse(`・
//! `parse_slmp(`）を、R2b-3a-2（docs/banto-hub-driver-io-design.md §4.2）で broker の
//! 旧い要求の型と橋（`BatchReadRequest`・`BatchWriteRequest`・`from_driver_address`・
//! `banto_plc::Address`・`to_driver_address(`）とプロトコルの名前での比較
//! （`conn.protocol == "slmp"`・`conn.protocol == "modbus-tcp"`）を足した。見るのは
//! `NEEDLES` の全部:
//!
//! - `PlcConnectionService::new(`（R2b-1d: `ALLOWED_PROTOCOLS` と手書きの検証）
//! - `TagService::new(`（R2b-2b: アドレスを綴りのまま保存する旧規則）
//! - `CollectionGroupService::new(`（R2b-2b: グループの移動で配下のタグを解析し直さない旧規則）
//! - `Address::parse(`・`parse_slmp(`（R2b-2c: アドレスをプロトコルの名前で選んだ文法で解析する
//!   旧規則。計画・即時の読み書き・接続テストはレジストリのドライバの `parse_address`
//!   を使う。R2b-3a-2 で接続テストの固定のリテラル `40001`・`D0` の例外を外した:
//!   読む 1 点はドライバの `probe()`）
//! - `BatchReadRequest`・`BatchWriteRequest`（R2b-3a-2: broker の要求は `TagSpec` /
//!   `WriteItem`）、`from_driver_address`・`banto_plc::Address`（R2b-3a-2: ドライバへは
//!   `DriverAddress` をそのまま渡す。`Address` へ戻さない）、`to_driver_address(`
//!   （R2b-3a-2 の一時の変換。R2b-3a-3 で収集の計画が `TagSpec` になり、橋ごと消した。
//!   例外はもう無い）
//! - `conn.protocol == "slmp"`（#605 の CPU を同一性に入れる分岐。R2b-3a-2 で設定の指紋に
//!   した）・`conn.protocol == "modbus-tcp"`（シミュレータの種類を名前で選ぶ分岐。R2b-3a-3 で
//!   `BrokerSimRegistry` ごと `banto-collect` へ移したので、Hub には例外が無い。R2b-5 で消す）
//!
//! 例外は (needle, ファイル) ごとに**件数まで固定**する。増えても減ってもテストが落ちる。
//!
//! Hub は `TagRegistryState`・MCP の状態・`PendingChangesAdminState`・予約接続
//! （`runtime::ensure_virtual_connection`）・移行の preflight（`upgrade`）・即時の読み書き
//! （`read_path`・`write_path`）のすべてで `with_registry`（`CollectorManager::registry()`
//! 経由）を使う。`new` を使ったままの入口が 1 つでも残ると、レジストリに無いプロトコル・
//! ドライバが断る設定やアドレスを書けてしまう。読むだけの箇所も含めて `new` を禁じる
//! （読む口と書く口を区別しないと、後で書く口に育ったときに見落とす。`new` に例外の一覧は作らない）。
//!
//! 見ないもの（テストのコード）: `src/**/tests/` の下・`tests.rs`・`test_support.rs`
//! という名前のファイル、ファイルの中の `#[cfg(test)]` の直後のインラインの `mod … {`
//! （その閉じ括弧まで。rustfmt が閉じ括弧を `mod` の行と同じ字下げの単独の行に置くことを
//! 使う。ファイルの途中にあっても、その後の本番のコードは走査する）、コメントの行。

use std::path::{Path, PathBuf};

/// (needle, 直す先の案内)。
const NEEDLES: &[(&str, &str)] = &[
    (
        "PlcConnectionService::new(",
        "`PlcConnectionService::with_registry`（`CollectorManager::plc_connections()`）",
    ),
    ("TagService::new(", "`TagService::with_registry`"),
    (
        "CollectionGroupService::new(",
        "`CollectionGroupService::with_registry`",
    ),
    (
        "PlcConnectionInput {",
        "use ConnectionSettingsInput for production connection writes",
    ),
    (
        "ALLOWED_PROTOCOLS",
        "use the driver registry for production protocol validation",
    ),
    (
        "Address::parse(",
        "レジストリのドライバの `parse_address`（`read_path::resolve_plc_address`）",
    ),
    (
        "parse_slmp(",
        "レジストリのドライバの `parse_address`（`read_path::resolve_plc_address`）",
    ),
    (
        "BatchReadRequest",
        "`banto_driver_api::TagSpec`（broker の要求、R2b-3a-2）",
    ),
    (
        "BatchWriteRequest",
        "`banto_driver_api::WriteItem`（broker の要求、R2b-3a-2）",
    ),
    (
        "from_driver_address",
        "ドライバの `DriverAddress` をそのまま `TagSpec` に（R2b-3a-2）",
    ),
    (
        "banto_plc::Address",
        "ドライバの `DriverAddress`（`read_path::resolve_plc_address`、R2b-3a-2）",
    ),
    (
        "to_driver_address(",
        "ドライバの `DriverAddress` をそのまま `TagSpec` に（R2b-3a-3 で一時の変換も消した）",
    ),
    (
        "conn.protocol == \"slmp\"",
        "`banto_broker::SettingsFingerprint`（設定の指紋、R2b-3a-2）",
    ),
    (
        "conn.protocol == \"modbus-tcp\"",
        "レジストリのドライバ（シミュレータの種類の選択は `banto_collect::BrokerSimRegistry`）",
    ),
];

/// 例外（needle, ファイルの末尾のパス, 許す件数, 理由）。意図して最小に保つ。
/// 件数は `offending_lines`（コメント・テストのコードを除く）の数とちょうど同じでなければ
/// ならない。増えたら新しい呼び出しが紛れ込んでおり、減ったら例外を下げる／外す。
// R2b-3a-3: `to_driver_address(`（`BrokerReadClient` の一時の変換）と
// `conn.protocol == "modbus-tcp"`（`BrokerSimRegistry::resolve`、`banto-collect` へ移した）の
// 例外を外した。今は例外が無い。
const EXCEPTIONS: &[(&str, &str, usize, &str)] = &[];

/// `file` が `needle` の例外に当たるなら、許す件数。
fn allowed_count(needle: &str, file: &Path) -> Option<usize> {
    let file = file.to_string_lossy().replace('\\', "/");
    EXCEPTIONS
        .iter()
        .find(|(n, suffix, _, _)| *n == needle && file.ends_with(suffix))
        .map(|(_, _, count, _)| *count)
}

/// 例外つきのファイルの見つかった行が、許す件数とちょうど同じか。
/// 多ければ全行を違反として返し、少なければ例外を下げるよう促す。
fn check_exception_count(found: &[usize], allowed: usize) -> Result<(), String> {
    use std::cmp::Ordering;
    match found.len().cmp(&allowed) {
        Ordering::Equal => Ok(()),
        Ordering::Greater => Err(format!(
            "例外は {allowed} 件までしか許さないのに {} 件あります（行 {found:?}）",
            found.len()
        )),
        Ordering::Less => Err(format!(
            "例外は {allowed} 件を許していますが {} 件しかありません（行 {found:?}）。\
             `EXCEPTIONS` の件数を下げるか、例外を外してください",
            found.len()
        )),
    }
}

/// `dir` の下の `.rs` を集める（テストのディレクトリ・ファイルを除く）。
fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read the source dir") {
        let path = entry.expect("dir entry").path();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if path.is_dir() {
            if name != "tests" {
                sources(&path, out);
            }
        } else if name.ends_with(".rs") && name != "tests.rs" && name != "test_support.rs" {
            out.push(path);
        }
    }
}

/// `text` の中で、テストのモジュールの外・コメントでない行に `needle` がある行番号。
/// ファイルの途中のインラインのテストのモジュールも、閉じ括弧までだけを飛ばして走査を続ける。
fn offending_lines(text: &str, needle: &str) -> Vec<usize> {
    let lines: Vec<&str> = text.lines().collect();
    let mut found = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let trimmed = lines[index].trim_start();
        if trimmed.starts_with("#[cfg(test)]") {
            let next = lines
                .iter()
                .enumerate()
                .skip(index + 1)
                .find(|(_, line)| !line.trim().is_empty());
            // An inline test module (`mod tests {`) runs to its closing brace; a
            // `mod tests;` declaration (its own file, already skipped) does not.
            if let Some((mod_index, mod_line)) = next {
                let head = mod_line.trim_start();
                if (head.starts_with("mod ")
                    || head.starts_with("pub mod ")
                    || head.starts_with("pub(crate) mod "))
                    && head.trim_end().ends_with('{')
                {
                    // rustfmt puts the closing `}` alone, at the `mod` line's indentation.
                    let indent = &mod_line[..mod_line.len() - head.len()];
                    let closing = format!("{indent}}}");
                    match lines
                        .iter()
                        .enumerate()
                        .skip(mod_index + 1)
                        .find(|(_, line)| line.trim_end() == closing)
                    {
                        Some((end, _)) => {
                            index = end + 1;
                            continue;
                        }
                        None => break,
                    }
                }
            }
        }
        if !trimmed.starts_with("//") && trimmed.contains(needle) {
            found.push(index + 1);
        }
        index += 1;
    }
    found
}

#[test]
fn hub_non_test_code_never_builds_a_registry_free_service() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    for dir in ["core/src", "host/src", "src-tauri/src"] {
        let dir = root.join(dir);
        if dir.is_dir() {
            sources(&dir, &mut files);
        }
    }
    assert!(
        files.iter().any(|p| p.ends_with("runtime.rs")),
        "the scan must see banto-hub-core's sources: {files:?}"
    );
    let texts: Vec<(PathBuf, String)> = files
        .into_iter()
        .map(|file| {
            let text = std::fs::read_to_string(&file).expect("read a source file");
            (file, text)
        })
        .collect();
    for (needle, remedy) in NEEDLES {
        let mut offenders = Vec::new();
        for (file, text) in &texts {
            let found = offending_lines(text, needle);
            if let Some(allowed) = allowed_count(needle, file) {
                if let Err(message) = check_exception_count(&found, allowed) {
                    offenders.push(format!("{}: {message}", file.display()));
                }
                continue;
            }
            for line in found {
                offenders.push(format!("{}:{line}", file.display()));
            }
        }
        assert!(
            offenders.is_empty(),
            "banto-hub の非テストのコードに `{needle}` があります。{remedy} を使ってください\
             （設計 §4.2・アドレス設計 §3.1）:\n{}",
            offenders.join("\n")
        );
    }
}

/// The scanner itself: a hit in code is found, hits in comments and in a trailing
/// `#[cfg(test)] mod` are not.
#[test]
fn the_scanner_skips_comments_and_the_test_module_only() {
    let text = "fn a() {\n    let s = PlcConnectionService::new(pool);\n}\n\
                // PlcConnectionService::new(pool) in a comment\n\
                #[cfg(test)]\nfn helper() { PlcConnectionService::new(pool); }\n\
                #[cfg(test)]\nmod support;\nfn b() { PlcConnectionService::new(pool); }\n\
                #[cfg(test)]\n\nmod tests {\n    PlcConnectionService::new(pool);\n}\n";
    assert_eq!(offending_lines(text, NEEDLES[0].0), vec![2, 6, 9]);
}

/// A test module in the middle of a file ends at its closing brace: production code after
/// it is still scanned (write_path.rs has one at line 444).
#[test]
fn the_scanner_resumes_after_a_mid_file_test_module() {
    let text = "fn a() {}\n#[cfg(test)]\nmod tests {\n    fn t() {\n        TagService::new(p);\n    }\n}\n\
                fn prod() {\n    TagService::new(p);\n}\n\
                mod outer {\n    #[cfg(test)]\n    mod inner {\n        TagService::new(p);\n    }\n    fn q() { TagService::new(p); }\n}\n";
    assert_eq!(offending_lines(text, "TagService::new("), vec![9, 16]);
}

/// Every needle is a distinct call: `TagService::new(` must not match the longer
/// `PlcConnectionService::new(` or `CollectionGroupService::new(` and vice versa.
#[test]
fn the_needles_do_not_overlap() {
    let text = "let a = PlcConnectionService::new(p);\nlet b = CollectionGroupService::new(p);\n\
                let c = TagService::new(p);\n";
    let hits: Vec<Vec<usize>> = NEEDLES
        .iter()
        .take(3)
        .map(|(needle, _)| offending_lines(text, needle))
        .collect();
    assert_eq!(hits, vec![vec![1], vec![3], vec![2]]);
}

/// R2b-2c: the immediate read / write paths no longer choose an address grammar by
/// protocol name. (`"modbus-tcp" =>` as a match arm; the protocol gate is
/// `HubSessions::is_broker_managed`, not a literal.) Comments are not scanned.
/// R2b-3a-2: the connection test (`rest/plc_connections.rs`) joined them - it
/// used to dispatch `"modbus-tcp" =>` / `"slmp" =>` to two protocol-specific tests.
#[test]
fn read_and_write_paths_have_no_protocol_name_match_arm() {
    let core = Path::new(env!("CARGO_MANIFEST_DIR")).join("../core/src");
    for name in ["read_path.rs", "write_path.rs", "rest/plc_connections.rs"] {
        let text = std::fs::read_to_string(core.join(name)).expect("read a source file");
        let offenders = offending_lines(&text, "\"modbus-tcp\" =>");
        assert!(
            offenders.is_empty(),
            "{name} に `\"modbus-tcp\" =>` の分岐があります（行 {offenders:?}）。\
             アドレスの解析は `resolve_plc_address` へ"
        );
    }
}

/// An exception is exact: same needle and the file's tail; another needle in the same
/// file, or the same needle elsewhere, is still an offence. The allowed count is pinned.
#[test]
fn exceptions_are_per_needle_and_per_file() {
    // `rest/tags.rs` の例外は R2b-2d（`address-writable` の削除）で、
    // `rest/plc_connections.rs` の例外（接続テストの固定のリテラル）は R2b-3a-2 で外した。
    for file in [
        "/x/core/src/rest/tags.rs",
        "/x/core/src/rest/plc_connections.rs",
    ] {
        let file = Path::new(file);
        assert_eq!(allowed_count("Address::parse(", file), None);
        assert_eq!(allowed_count("parse_slmp(", file), None);
    }
    assert_eq!(
        allowed_count("Address::parse(", Path::new("/x/core/src/read_path.rs")),
        None
    );
    // R2b-3a-3: `broker_glue.rs` の 2 つの例外（`to_driver_address(`・
    // `conn.protocol == "modbus-tcp"`）を外した。
    let glue = Path::new("/x/core/src/broker_glue.rs");
    assert_eq!(allowed_count("to_driver_address(", glue), None);
    assert_eq!(allowed_count("conn.protocol == \"modbus-tcp\"", glue), None);
    assert_eq!(allowed_count("conn.protocol == \"slmp\"", glue), None);
    assert_eq!(allowed_count("banto_plc::Address", glue), None);
    assert_eq!(
        allowed_count("to_driver_address(", Path::new("/x/core/src/read_path.rs")),
        None
    );
}

/// R2b-3a-2: `use banto_plc::{..}` の中の `Address` も旧い要求の型の名前（needle の
/// `banto_plc::Address` は `{` の中を見ないので、ここで見る）。
#[test]
fn hub_non_test_code_does_not_import_banto_plc_address() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    for dir in ["core/src", "host/src", "src-tauri/src"] {
        let dir = root.join(dir);
        if dir.is_dir() {
            sources(&dir, &mut files);
        }
    }
    let mut offenders = Vec::new();
    for file in files {
        let text = std::fs::read_to_string(&file).expect("read a source file");
        for line in offending_lines(&text, "use banto_plc::{") {
            // The import list runs to the first `}` (rustfmt keeps it on one
            // statement).
            let lines: Vec<&str> = text.lines().collect();
            let mut list = String::new();
            for l in &lines[line - 1..] {
                list.push_str(l);
                if l.contains('}') {
                    break;
                }
            }
            if list
                .split(|c: char| !c.is_alphanumeric() && c != '_')
                .any(|token| token == "Address")
            {
                offenders.push(format!("{}:{line}", file.display()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "banto-hub の非テストのコードが `banto_plc::Address` を import しています:\n{}",
        offenders.join("\n")
    );
}

/// The exception count is pinned: equal passes, more lists every line, fewer asks to
/// lower the count so the list stays minimal.
#[test]
fn exception_counts_are_pinned_exactly() {
    assert!(check_exception_count(&[10, 20], 2).is_ok());
    assert!(check_exception_count(&[], 0).is_ok());
    let more = check_exception_count(&[10, 20, 30], 2).unwrap_err();
    assert!(more.contains("2 件までしか許さない") && more.contains("[10, 20, 30]"));
    let fewer = check_exception_count(&[10], 2).unwrap_err();
    assert!(fewer.contains("下げる"));
}
