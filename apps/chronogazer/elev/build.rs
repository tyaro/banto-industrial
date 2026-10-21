//! #392 B3-3b: `chronogazer-elev.exe` にだけ `requireAdministrator` のマニフェスト
//! （`chronogazer-elev.manifest`）を埋め込む（banto-hub-host の `build.rs` の
//! `banto-hub-elev` と同じ作り）。
//!
//! `embed_resource::compile_for` は指定した `[[bin]]` 名にだけ
//! `cargo:rustc-link-arg-bin=chronogazer-elev=...` を出すので、lib のテストの
//! バイナリには付かない（テストは昇格を求めずに動く）。`manifest_required()` は
//! Windows 以外では常に `Ok(())`、Windows では埋め込みに失敗したらビルドを失敗させる
//! （昇格のマニフェストはこのヘルパーの前提そのもので、埋め込み漏れを黙って
//! 通さない）。

fn main() {
    println!("cargo:rerun-if-changed=chronogazer-elev.rc");
    println!("cargo:rerun-if-changed=chronogazer-elev.manifest");
    embed_resource::compile_for(
        "chronogazer-elev.rc",
        ["chronogazer-elev"],
        embed_resource::NONE,
    )
    .manifest_required()
    .expect("chronogazer-elev.exe への requireAdministrator マニフェストの埋め込みに失敗しました");
}
