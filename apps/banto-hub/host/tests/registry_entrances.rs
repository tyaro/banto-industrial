//! R2b-1d（docs/banto-hub-connection-settings-design.md §4.2・§8 の 10）／R2b-2b
//! （docs/banto-hub-driver-address-design.md §3.1）: Hub の入口が旧規則で書けないことを、
//! ソースの grep で固定する。見るのは次の 3 つ（`NEEDLES`）:
//!
//! - `PlcConnectionService::new(`（R2b-1d: `ALLOWED_PROTOCOLS` と手書きの検証）
//! - `TagService::new(`（R2b-2b: アドレスを綴りのまま保存する旧規則）
//! - `CollectionGroupService::new(`（R2b-2b: グループの移動で配下のタグを解析し直さない旧規則）
//!
//! Hub は `TagRegistryState`・MCP の状態・`PendingChangesAdminState`・予約接続
//! （`runtime::ensure_virtual_connection`）・移行の preflight（`upgrade`）・即時の読み書き
//! （`read_path`・`write_path`）のすべてで `with_registry`（`CollectorManager::registry()`
//! 経由）を使う。`new` を使ったままの入口が 1 つでも残ると、レジストリに無いプロトコル・
//! ドライバが断る設定やアドレスを書けてしまう。読むだけの箇所も含めて `new` を禁じる
//! （読む口と書く口を区別しないと、後で書く口に育ったときに見落とす。例外の一覧も作らない）。
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
];

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
            for line in offending_lines(text, needle) {
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
        .map(|(needle, _)| offending_lines(text, needle))
        .collect();
    assert_eq!(hits, vec![vec![1], vec![3], vec![2]]);
}
