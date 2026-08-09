//! banto-hub 用 Windows インストーラ生成ツール（T5-2、
//! docs/t5-handoff.md §3「インストーラ（既存2アプリのインストーラ構成を
//! 踏襲）」）。
//!
//! banto-hub は T0 で「Tauri なし構成」と決定されたヘッドレス axum サーバー
//! （単一 exe、`apps/banto-hub/core/src/bin/banto-hub.rs`）を一次形態として
//! 持つ - この `banto-hub.exe` 自体には `src-tauri` を持たせない。既存2
//! アプリ（chronogazer/relay-wright）は `cargo tauri build` が内部で呼んで
//! いる tauri-bundler で NSIS/MSI インストーラを生成しているが、
//! `banto-hub.exe` を独自 `frontendDist`/`invoke` 面を持つフル Tauri
//! アプリ化はしない（オーナー決定 2026-08-06）ので、ここでは
//! `tauri-bundler` クレートを単体ライブラリとして直接呼び出す。
//!
//! **2026-08-09 追記**（docs/banto-hub-t16-design.md P1）: 上記の禁止事項は
//! 「独自 `frontendDist`/`invoke` を持つフル Tauri
//! アプリ化はしない」という意味に限定される。`HubRuntime` を埋め込み、
//! Hub 自身の localhost UI を WebView で開くだけの**薄いシェル**
//! （`apps/banto-hub/src-tauri`、パッケージ名 `banto-hub-shell`、T16-0）は
//! 二次ホストとして別途追加済み - このインストーラはヘッドレス
//! `banto-hub.exe` 用のままで変更しない（`banto-hub-shell` 自身のインス
//! トーラ化は T17 のスコープ、docs/banto-hub-desktop-plan.md §16.3）。
//!
//! この `apps/banto-hub/installer/` パッケージ自体が「xtask 的」な立ち位置
//! - ルートワークスペースの member ではない（`Cargo.toml` のコメント参照。
//! 理由は `cargo check --workspace --all-targets` を Windows 専用の
//! バンドル処理に巻き込まないため）。
//!
//! ## 使い方
//!
//! ```powershell
//! # 1. 先にリリースビルド（docs/t5-handoff.md 記載の既存手順そのまま）
//! cargo build -p banto-hub-core --bin banto-hub --features embed-ui --release
//!
//! # 2. インストーラ生成（既定でリポジトリルートの
//! #    target/release/banto-hub.exe を入力に取る）
//! cargo run --manifest-path apps/banto-hub/installer/Cargo.toml --release
//!
//! # 別の exe を対象にしたい場合は第1引数でパスを渡す
//! cargo run --manifest-path apps/banto-hub/installer/Cargo.toml --release -- D:\path\to\banto-hub.exe
//! ```
//!
//! 生成物は `<exeのあるディレクトリ>/bundle/nsis/BantoHub_<version>_x64-setup.exe`
//! （tauri-bundler の既定命名規則）。
//!
//! **このツールは生成したインストーラを実行しない** - `bundle_project`
//! はファイルを作るだけで、生成された `.exe` を起動する処理は一切含まない
//! （実行してのインストール確認はオーナー判断領域 - T5-1 と同じ理由で
//! このセッションでは行わない）。

use std::env;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use tauri_bundler::{
    BundleBinary, BundleSettings, NsisSettings, PackageSettings, PackageType, SettingsBuilder,
    WindowsSettings,
};
use tauri_utils::config::{NSISInstallerMode, WebviewInstallMode};

/// 既存2アプリの `dev.tyaro.{name}` 命名規則を踏襲
/// （apps/relay-wright/src-tauri/tauri.conf.json の `identifier` 参照）。
const BUNDLE_IDENTIFIER: &str = "dev.tyaro.banto-hub";
/// docs/banto-hub-operations.md 全体で使われている表記に合わせる。
const PRODUCT_NAME: &str = "BantoHub";
/// `apps/banto-hub/core/Cargo.toml` の `[[bin]] name` と一致させる
/// （拡張子なし - tauri-bundler が対象 OS ごとに付与する）。
const MAIN_BINARY_NAME: &str = "banto-hub";
/// ワークスペース共通の `[workspace.package] version`（ルート
/// `Cargo.toml`）と同じ値。このインストーラ用パッケージは
/// ワークスペース外にあるため `version.workspace = true` が使えず、
/// 手動で追従させる必要がある - バージョンを上げたら、ここも合わせて
/// 更新すること。
const PRODUCT_VERSION: &str = "0.1.0";

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let installer_dir = installer_manifest_dir();

    let exe_path = match env::args_os().nth(1) {
        Some(arg) => PathBuf::from(arg),
        None => default_exe_path(&installer_dir)?,
    };
    if !exe_path.is_file() {
        bail!(
            "banto-hub.exe が見つかりません: {}\n\
             先に `cargo build -p banto-hub-core --bin banto-hub --features embed-ui --release` \
             を実行するか、第1引数でビルド済み exe のパスを指定してください。",
            exe_path.display()
        );
    }
    let stem_ok = exe_path
        .file_stem()
        .map(|s| s == MAIN_BINARY_NAME)
        .unwrap_or(false);
    if !stem_ok {
        bail!(
            "対象 exe のファイル名は `{MAIN_BINARY_NAME}.exe` である必要があります（渡されたパス: {}）。\
             tauri-bundler の Settings::binary_path は project_out_directory と \
             バイナリ名からパスを合成するため、ファイル名がずれると入力を見つけられません。",
            exe_path.display()
        );
    }

    let project_out_directory = exe_path
        .parent()
        .context("exe path has no parent directory")?
        .to_path_buf();

    let icon_dir = installer_dir.join("icons");
    let icon_png = require_file(icon_dir.join("icon.png"))?;
    let icon_ico = require_file(icon_dir.join("icon.ico"))?;

    let installer_hooks = require_file(installer_dir.join("hooks").join("service-hooks.nsh"))?;

    let package_settings = PackageSettings {
        product_name: PRODUCT_NAME.to_string(),
        version: PRODUCT_VERSION.to_string(),
        description: "banto-hub（産業用 PLC タグサーバー）。PLC からタグを収集し、\
            REST/WebSocket/MQTT/gRPC で外部へ公開する単一 exe・ヘッドレスサーバーです。"
            .to_string(),
        homepage: None,
        authors: Some(vec!["tyaro".to_string()]),
        default_run: Some(MAIN_BINARY_NAME.to_string()),
    };

    let bundle_settings = BundleSettings {
        identifier: Some(BUNDLE_IDENTIFIER.to_string()),
        icon: Some(vec![
            path_to_string(&icon_png)?,
            path_to_string(&icon_ico)?,
        ]),
        short_description: Some("banto-hub 産業用タグサーバー".to_string()),
        windows: WindowsSettings {
            // banto-hub はネイティブ WebView を一切使わない（axum サーバー
            // + ブラウザで開く管理 UI）ので、WebView2 ランタイムの導入は
            // 不要 - 既定の DownloadBootstrapper のままだとインストーラが
            // 無関係な WebView2 セットアップを試みてしまう。
            webview_install_mode: WebviewInstallMode::Skip,
            nsis: Some(NsisSettings {
                installer_icon: Some(icon_ico.clone()),
                // 常駐 Windows サービスを扱うインストーラなので、既定の
                // CurrentUser（Program Files 外へのユーザー単位インストール）
                // ではなく PerMachine を選ぶ - install/uninstall サブコマンド
                // が Service Control Manager への登録・削除に管理者権限を
                // 要求するため（win_service.rs 参照）、インストーラ自体も
                // 昇格させておく必要がある。
                install_mode: NSISInstallerMode::PerMachine,
                languages: Some(vec!["Japanese".to_string(), "English".to_string()]),
                // T5-2 実装指示 §4: post-install で `banto-hub.exe install`
                // （サービス登録）、pre-uninstall で `uninstall`
                // （サービス削除）を自動的に呼ぶ（hooks/service-hooks.nsh
                // 参照）。
                installer_hooks: Some(installer_hooks),
                ..Default::default()
            }),
            ..Default::default()
        },
        ..Default::default()
    };

    let binaries = vec![BundleBinary::new(MAIN_BINARY_NAME.to_string(), true)];

    let settings = SettingsBuilder::new()
        .package_settings(package_settings)
        .bundle_settings(bundle_settings)
        .binaries(binaries)
        .package_types(vec![PackageType::Nsis])
        .project_out_directory(&project_out_directory)
        .build()
        .context("tauri_bundler::SettingsBuilder::build に失敗しました")?;

    log::info!(
        "banto-hub-installer: {} を対象に NSIS インストーラをビルドします（出力先: {}）",
        exe_path.display(),
        project_out_directory.join("bundle").display()
    );

    let bundles =
        tauri_bundler::bundle_project(&settings).context("tauri_bundler::bundle_project に失敗しました")?;

    for bundle in &bundles {
        for path in &bundle.bundle_paths {
            log::info!("banto-hub-installer: 生成しました -> {}", path.display());
        }
    }

    if bundles.is_empty() {
        bail!("インストーラが1つも生成されませんでした（package_types の設定を確認してください）");
    }

    Ok(())
}

/// この Cargo パッケージ自身の `Cargo.toml` があるディレクトリ
/// （`apps/banto-hub/installer/`）。`cargo run --manifest-path ...` で
/// どのディレクトリから呼ばれても `icons/`・`hooks/` を正しく解決できる
/// よう、実行時 CWD ではなくビルド時に埋め込まれるこのパスを基準にする。
fn installer_manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// リポジトリルートの `target/release/banto-hub.exe`
/// （docs/t5-handoff.md 記載の既定ビルド出力先）を既定の入力とする。
fn default_exe_path(installer_dir: &Path) -> Result<PathBuf> {
    let repo_root = installer_dir
        .parent() // apps/banto-hub
        .and_then(Path::parent) // apps
        .and_then(Path::parent) // リポジトリルート
        .with_context(|| {
            format!(
                "リポジトリルートを解決できませんでした（起点: {}）",
                installer_dir.display()
            )
        })?;
    Ok(repo_root.join("target").join("release").join(format!("{MAIN_BINARY_NAME}.exe")))
}

fn require_file(path: PathBuf) -> Result<PathBuf> {
    if !path.is_file() {
        bail!("必要なファイルが見つかりません: {}", path.display());
    }
    Ok(path)
}

fn path_to_string(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_string)
        .with_context(|| format!("パスを UTF-8 文字列に変換できません: {}", path.display()))
}
