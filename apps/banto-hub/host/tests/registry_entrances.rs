//! R2b-1d（docs/banto-hub-connection-settings-design.md §4.2・§8 の 10）: Hub の入口が
//! 旧規則（`PlcConnectionService::new`、`ALLOWED_PROTOCOLS` と手書きの検証）で接続を
//! 書けないことを、ソースの grep で固定する。
//!
//! Hub は `TagRegistryState`・MCP の状態・`PendingChangesAdminState`・予約接続
//! （`runtime::ensure_virtual_connection`）・移行の preflight（`upgrade`）のすべてで
//! `PlcConnectionService::with_registry`（`CollectorManager::plc_connections()` を含む）を
//! 使う。`new` を使ったままの入口が 1 つでも残ると、レジストリに無いプロトコル・ドライバが
//! 断る設定を書けてしまう。読むだけの箇所も含めて `new` を禁じる（読む口と書く口を
//! 区別しないと、後で書く口に育ったときに見落とす）。
//!
//! 見ないもの（テストのコード）: `src/**/tests/` の下・`tests.rs`・`test_support.rs`
//! という名前のファイル、ファイルの中の `#[cfg(test)]` の直後のインラインの `mod … {`
//! （そこからファイルの終わりまで。この crate はテストのモジュールをファイルの末尾に置く
//! 慣行）、コメントの行。

use std::path::{Path, PathBuf};

const NEEDLE: &str = "PlcConnectionService::new(";

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

/// `text` の中で、テストのモジュールより前・コメントでない行に `needle` がある行番号。
fn offending_lines(text: &str, needle: &str) -> Vec<usize> {
    let lines: Vec<&str> = text.lines().collect();
    let mut found = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let trimmed = lines[index].trim_start();
        if trimmed.starts_with("#[cfg(test)]") {
            let next = lines[index + 1..]
                .iter()
                .map(|line| line.trim_start())
                .find(|line| !line.is_empty());
            // An inline test module (`mod tests {`) runs to the end of the file; a
            // `mod tests;` declaration (its own file, already skipped) does not.
            if next.is_some_and(|line| {
                (line.starts_with("mod ")
                    || line.starts_with("pub mod ")
                    || line.starts_with("pub(crate) mod "))
                    && line.trim_end().ends_with('{')
            }) {
                break;
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
fn hub_non_test_code_never_builds_a_registry_free_connection_service() {
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
    let mut offenders = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).expect("read a source file");
        for line in offending_lines(&text, NEEDLE) {
            offenders.push(format!("{}:{line}", file.display()));
        }
    }
    assert!(
        offenders.is_empty(),
        "banto-hub の非テストのコードに `PlcConnectionService::new(` があります。\
         `PlcConnectionService::with_registry`（`CollectorManager::plc_connections()`）を\
         使ってください（設計 §4.2）:\n{}",
        offenders.join("\n")
    );
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
    assert_eq!(offending_lines(text, NEEDLE), vec![2, 6, 9]);
}
