//! T4（docs/tag-server-design.md §5.4）: `proto/tagserver/v1/tagserver.proto`
//! をビルド時に Rust へコード生成する。生成先は `OUT_DIR`（`tonic-prost-build`
//! の既定）で、生成コードはリポジトリへコミットしない（実装指示「生成コード
//! は OUT_DIR 方式」）— `crate::grpc` が
//! `include!(concat!(env!("OUT_DIR"), "/tagserver.v1.rs"))` で取り込む。
//!
//! ## proto パス（リポジトリルート相対、深さに注意）
//!
//! この crate は `apps/banto-hub/core` にあり、proto はリポジトリルートの
//! `proto/` 配下（`proto/tagserver/v1/tagserver.proto`）にある。`build.rs` の
//! カレントディレクトリは常にこの crate のディレクトリ（`CARGO_MANIFEST_DIR`）
//! なので、`../../../proto`（`core` → `banto-hub` → `apps` → リポジトリルート
//! の3階層分）で参照する。
//!
//! ## protoc の自己完結（このコンテナ・CI 双方でビルドが通る条件）
//!
//! `tonic-prost-build`（内部は `prost-build`）は既定でシステムの `protoc`
//! 実行体を `PATH`/`PROTOC` 環境変数から探す。このコンテナにも GitHub
//! Actions の `ubuntu-latest` ランナーにも system `protoc` がインストール
//! されている保証がなく、CI 側で `apt-get install protobuf-compiler` を
//! 足すのは「ビルドの再現性が CI 環境のパッケージ状態に依存する」という
//! 弱点を持ち込む。代わりに `protoc-bin-vendored`（Google がビルド・配布する
//! `protoc` 実行体をクレートに静的同梱したもの、ビルド時にネットワークへ
//! 問い合わせない）を build-dependency として使い、
//! `prost_build::Config::protoc_executable` へそのパスを明示的に渡す -
//! これでシステム `protoc` の有無に関わらず、`cargo build`/`cargo test` が
//! 常に同じ `protoc` バイナリで再現する。

use std::path::PathBuf;

fn main() {
    let manifest_dir =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set"));
    // apps/banto-hub/core -> apps/banto-hub -> apps -> リポジトリルート
    let repo_root = manifest_dir
        .parent()
        .and_then(std::path::Path::parent)
        .and_then(std::path::Path::parent)
        .expect("apps/banto-hub/core は3階層分の親を持つ")
        .to_path_buf();
    let proto_dir = repo_root.join("proto");
    let proto_file = proto_dir
        .join("tagserver")
        .join("v1")
        .join("tagserver.proto");

    println!("cargo:rerun-if-changed={}", proto_file.display());

    let protoc_path = protoc_bin_vendored::protoc_bin_path()
        .expect("protoc-bin-vendored はこのプラットフォーム向けの protoc を同梱していない");

    let mut config = tonic_prost_build::Config::new();
    config.protoc_executable(&protoc_path);

    tonic_prost_build::configure()
        .build_client(true)
        .build_server(true)
        .compile_with_config(config, &[proto_file], &[proto_dir])
        .expect("tagserver.proto のコード生成に失敗しました");

    // T17-2 スライス2（docs/banto-hub-t17-design.md §3「T17-2」）:
    // UAC 昇格ヘルパー `banto-hub-elev.exe` にだけ `requireAdministrator`
    // マニフェスト（`banto-hub-elev.manifest`）を埋め込む。
    // `embed_resource::compile_for` は指定した `[[bin]]` 名（第2引数）にのみ
    // `cargo:rustc-link-arg-bin=banto-hub-elev=...` を発行するため、この
    // build.rs を共有する `banto-hub` 本体・テストバイナリには一切影響
    // しない（tauri-build も内部で同じ embed-resource crate を使っており、
    // このワークスペースの依存木に新規バージョン系列は増えない）。
    //
    // `manifest_required()` は非 Windows では常に `Ok(())`
    // （`CompilationResult::NotWindows`）を返すので、非 Windows CI/開発機の
    // `cargo build --workspace` を壊さない。Windows では実際に埋め込みに
    // 失敗した場合にビルドを失敗させる - UAC 昇格マニフェストはこのヘルパー
    // の安全性の前提（実装指示「管理者権限が必要」）そのものなので、
    // 埋め込み漏れを検知せずに通すべきではない。
    embed_resource::compile_for(
        "banto-hub-elev.rc",
        ["banto-hub-elev"],
        embed_resource::NONE,
    )
    .manifest_required()
    .expect("banto-hub-elev.exe への requireAdministrator マニフェスト埋め込みに失敗しました");
}
