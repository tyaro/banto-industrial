//! T17-2 スライス2（docs/banto-hub-t17-design.md §3「T17-2」）:
//! UAC 昇格ヘルパー `banto-hub-elev.exe` にだけ `requireAdministrator`
//! マニフェスト（`banto-hub-elev.manifest`）を埋め込む。R1-7a
//! （docs/banto-driver-api-design.md §2.4）で bin と一緒に `banto-hub-core` の
//! `build.rs` から移した（core の `build.rs` には proto のコード生成だけが残る）。
//! 移動で埋め込みが落ちないことは `src/manifest_tests.rs` が確かめる。
//!
//! `embed_resource::compile_for` は指定した `[[bin]]` 名（第2引数）にのみ
//! `cargo:rustc-link-arg-bin=banto-hub-elev=...` を発行するため、この
//! build.rs を共有する `banto-hub` 本体には影響しない（tauri-build も内部で
//! 同じ embed-resource crate を使っており、このワークスペースの依存木に新規
//! バージョン系列は増えない）。
//!
//! `manifest_required()` は非 Windows では常に `Ok(())`
//! （`CompilationResult::NotWindows`）を返すので、非 Windows CI/開発機の
//! `cargo build --workspace` を壊さない。Windows では実際に埋め込みに
//! 失敗した場合にビルドを失敗させる - UAC 昇格マニフェストはこのヘルパー
//! の安全性の前提（実装指示「管理者権限が必要」）そのものなので、
//! 埋め込み漏れを検知せずに通すべきではない。

fn main() {
    println!("cargo:rerun-if-changed=banto-hub-elev.rc");
    println!("cargo:rerun-if-changed=banto-hub-elev.manifest");
    embed_resource::compile_for(
        "banto-hub-elev.rc",
        ["banto-hub-elev"],
        embed_resource::NONE,
    )
    .manifest_required()
    .expect("banto-hub-elev.exe への requireAdministrator マニフェスト埋め込みに失敗しました");
}
