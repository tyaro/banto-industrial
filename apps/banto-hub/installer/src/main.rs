//! banto-hub 一体インストーラ生成ツール（T5-2 → I1、
//! docs/banto-hub-installer-design.md §3 案 A・§4）。
//!
//! ## 変遷（短縮）
//!
//! - **T5-2**: banto-hub はヘッドレス axum サーバー（`banto-hub.exe`）が
//!   一次形態（T0 決定・`src-tauri` を持たせない）だったため、既存2アプリ
//!   （chronogazer/relay-wright）が `cargo tauri build` 内部で使っている
//!   `tauri-bundler` を単体ライブラリとして直接呼び出し、`banto-hub.exe`
//!   1つだけを NSIS インストーラに包んだ（`BundleBinary` 1つ、
//!   post-install で `install`、pre-uninstall で `uninstall` を呼ぶだけ）。
//! - **T16-0**: `HubRuntime` を埋め込み Hub の localhost UI を WebView で
//!   開む薄いシェル（`apps/banto-hub/src-tauri`、`banto-hub-shell`）が
//!   二次ホストとして追加されたが、このインストーラは対象外のままだった
//!   （T17 §2.3「パッケージング自体は T17 スコープ」）。
//! - **I1（本スライス、docs/banto-hub-installer-design.md §6 row I1）**:
//!   シェル・Hub 本体・UAC ヘルパ（`banto-hub-elev.exe`）・DB Sink サイド
//!   カー（`banto-hub-sink.exe`）の**4 バイナリ**を同じ `$INSTDIR` に揃える
//!   一体インストーラへ拡張した。方式は変えない（引き続き `tauri-bundler`
//!   をライブラリとして直接呼ぶ、`tauri.conf.json`/`cargo tauri` は経由
//!   しない）- 変わったのは「対象が 1 exe か 4 exe か」だけ。
//!
//! この `apps/banto-hub/installer/` パッケージ自体が「xtask 的」な立ち位置で、
//! ルートワークスペースの member ではない（`Cargo.toml` のコメント参照。
//! 理由は `cargo check --workspace --all-targets` を Windows 専用の
//! バンドル処理に巻き込まないため）。
//!
//! ## 同梱バイナリ（[`REQUIRED_BINARY_NAMES`]）
//!
//! | バイナリ                | main | 役割                                                              |
//! | ------------------------ | ---- | ------------------------------------------------------------------ |
//! | `banto-hub-shell.exe`     | ○   | デスクトップシェル（Tauri v2 薄いシェル、Hub を in-process で持つ） |
//! | `banto-hub.exe`           |      | Hub 本体（ヘッドレス axum サーバー、Service モード用）              |
//! | `banto-hub-elev.exe`      |      | UAC 昇格ヘルパー（サービス登録・ACL 付与の実処理）                 |
//! | `banto-hub-sink.exe`      |      | DB Sink サイドカー（外部 PostgreSQL 連携）                          |
//!
//! main をシェルにしたことで、tauri-bundler NSIS テンプレート標準の
//! 「インストール後に ${PRODUCTNAME} を実行する」完了ページチェックボックス
//! が**意味のある動作**になる（T5-2 当時は Hub 単体だったため、oncheck で
//! コンソール無しの前面プロセスが起動する既知の制約があった -
//! docs/banto-hub-operations.md §12 参照。この制約自体は I1 で消える）。
//!
//! ## 使い方
//!
//! ```powershell
//! # 1. 先に4つの release バイナリをビルド（docs/banto-hub-installer-design.md §4.6）
//! pnpm --filter banto-hub build
//! cargo build --release -p banto-hub-core --bin banto-hub --bin banto-hub-elev --features embed-ui
//! cargo build --release -p banto-hub-shell --features banto-hub-core/embed-ui
//! cargo build --release -p banto-hub-sink
//!
//! # 2. インストーラ生成（既定でリポジトリルートの target/release/ を対象にする）
//! cargo run --manifest-path apps/banto-hub/installer/Cargo.toml --release
//!
//! # 別のディレクトリを対象にしたい場合は第1引数でディレクトリを渡す
//! # （4つの exe が同じディレクトリに揃っている必要がある）
//! cargo run --manifest-path apps/banto-hub/installer/Cargo.toml --release -- D:\path\to\bin\dir
//! ```
//!
//! 対象ディレクトリに4つの exe のいずれかが無い場合は、どれが無いかを
//! 名指しした日本語エラーで即座に止まる（[`missing_binaries`]）- 4 バイナリ
//! 化で「どれか1つだけビルドし忘れた」事故が起きやすくなったため。
//!
//! 生成物は `<対象ディレクトリ>/bundle/nsis/BantoHub_<version>_x64-setup.exe`
//! （tauri-bundler の既定命名規則）。
//!
//! **このツールは生成したインストーラを実行しない** - `bundle_project`
//! はファイルを作るだけで、生成された `.exe` を起動する処理は一切含まない
//! （実行してのインストール確認はオーナー判断領域 - T5-1 と同じ理由で
//! このセッションでは行わない）。
//!
//! ## フックの流れ（`hooks/service-hooks.nsh`、
//! docs/banto-hub-installer-design.md §4.3）
//!
//! PREINSTALL でサービス（`BantoHubSink`→`BantoHub`の順）とシェルを止め、
//! POSTINSTALL で `banto-hub-elev.exe service-install`（Hub のサービス登録
//! と Operators グループ・ACL 一式）→ `banto-hub-sink.exe install` →
//! `banto-hub-sink.exe grant-service-acl` → `%ProgramData%\BantoHub\` の
//! 作成と `banto-hub-sink.toml.example` の複製 → PREINSTALL で止めた
//! サービスの再開、の順で実行する。PREUNINSTALL はシェルとサイドカーを
//! 止めてからサービス登録を解除する。どのステップも失敗してインストーラ
//! 自体を中断させない（`DetailPrint` で案内するだけ - T5-2 以来の方針）。

use std::collections::HashMap;
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

/// デスクトップシェル（`apps/banto-hub/src-tauri`、パッケージ名
/// `banto-hub-shell`）。I1 で main バイナリになった
/// （モジュール doc「同梱バイナリ」節参照）。
const SHELL_BINARY_NAME: &str = "banto-hub-shell";
/// Hub 本体（`apps/banto-hub/core/src/bin/banto-hub.rs`）。
const HUB_BINARY_NAME: &str = "banto-hub";
/// UAC 昇格ヘルパー（`apps/banto-hub/core/src/bin/banto-hub-elev.rs`）。
const ELEV_BINARY_NAME: &str = "banto-hub-elev";
/// DB Sink サイドカー（`apps/banto-hub-sink`）。
const SINK_BINARY_NAME: &str = "banto-hub-sink";

/// 同梱する4バイナリの名前（拡張子なし）。順序は「不足時のエラー
/// メッセージに出す順」以上の意味は無い。
const REQUIRED_BINARY_NAMES: [&str; 4] = [
    SHELL_BINARY_NAME,
    HUB_BINARY_NAME,
    ELEV_BINARY_NAME,
    SINK_BINARY_NAME,
];

/// ワークスペース共通の `[workspace.package] version`（ルート
/// `Cargo.toml`）と同じ値。このインストーラ用パッケージは
/// ワークスペース外にあるため `version.workspace = true` が使えず、
/// 手動で追従させる必要がある - バージョンを上げたら、ここも合わせて
/// 更新すること。
const PRODUCT_VERSION: &str = "0.2.0-alpha.5";

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let installer_dir = installer_manifest_dir();

    let binaries_dir = match env::args_os().nth(1) {
        Some(arg) => PathBuf::from(arg),
        None => default_binaries_dir(&installer_dir)?,
    };

    let missing = missing_binaries(&binaries_dir);
    if !missing.is_empty() {
        let missing_list = missing
            .iter()
            .map(|name| format!("{name}.exe"))
            .collect::<Vec<_>>()
            .join(", ");
        bail!(
            "次の実行ファイルが見つかりません（対象ディレクトリ: {}）: {missing_list}\n\
             先に以下を実行してから再度お試しください:\n\
             \u{20}\u{20}pnpm --filter banto-hub build\n\
             \u{20}\u{20}cargo build --release -p banto-hub-core --bin banto-hub --bin banto-hub-elev --features embed-ui\n\
             \u{20}\u{20}cargo build --release -p banto-hub-shell --features banto-hub-core/embed-ui\n\
             \u{20}\u{20}cargo build --release -p banto-hub-sink\n\
             別のディレクトリに4つの exe が揃っている場合は、第1引数でそのディレクトリを指定してください。",
            binaries_dir.display()
        );
    }

    let icon_dir = installer_dir.join("icons");
    let icon_png = require_file(icon_dir.join("icon.png"))?;
    let icon_ico = require_file(icon_dir.join("icon.ico"))?;

    let installer_hooks = require_file(installer_dir.join("hooks").join("service-hooks.nsh"))?;

    let sink_config_example = require_file(
        installer_dir
            .join("assets")
            .join("banto-hub-sink.toml.example"),
    )?;

    let package_settings = PackageSettings {
        product_name: PRODUCT_NAME.to_string(),
        version: PRODUCT_VERSION.to_string(),
        description: "banto-hub（産業用 PLC タグサーバー）。デスクトップシェル・Hub 本体・\
            UAC ヘルパー・DB Sink サイドカーの4実行ファイルを同梱し、PLC からタグを収集して\
            REST/WebSocket/MQTT/gRPC・外部 PostgreSQL へ公開する一体インストーラです。"
            .to_string(),
        homepage: None,
        authors: Some(vec!["tyaro".to_string()]),
        default_run: Some(SHELL_BINARY_NAME.to_string()),
    };

    // §4.3「サイドカーの設定ファイルの置き場」（I1 時点では雛形の複製のみ、
    // 実ファイルの生成・探索順変更は I2）。resources_map の値を空文字列に
    // すると、tauri-bundler は「元のファイル名のまま $INSTDIR 直下」に置く
    // （tauri-utils 2.9.3 の `ResourcePaths::resource_from_path` -
    // `{ "README.md": "" }` は `$INSTDIR/README.md` になる仕様、ソース確認
    // 済み）。絶対パスを渡しているので `cargo run` の実行時カレント
    // ディレクトリに依存しない。
    let mut resources_map = HashMap::new();
    resources_map.insert(path_to_string(&sink_config_example)?, String::new());

    let bundle_settings = BundleSettings {
        identifier: Some(BUNDLE_IDENTIFIER.to_string()),
        icon: Some(vec![path_to_string(&icon_png)?, path_to_string(&icon_ico)?]),
        short_description: Some(
            "BantoHub 産業用タグサーバー（シェル・Hub・DB Sink 同梱）".to_string(),
        ),
        resources_map: Some(resources_map),
        windows: WindowsSettings {
            // main がシェル（WebView を使う Tauri アプリ）になったので、
            // WebView2 ランタイムの導入が必要 - T5-2 時点の `Skip`
            // （Hub 単体はネイティブ WebView を使わないため許されていた）
            // から変更する（docs/banto-hub-installer-design.md §4.2・
            // §7-2 オーナー決定）。工場 PC は Edge 由来の WebView2 が
            // 入っていることがほとんどなので、無い場合だけ取得しに行く
            // `EmbedBootstrapper` を選ぶ（+約2MB、完全オフラインではない）。
            webview_install_mode: WebviewInstallMode::EmbedBootstrapper { silent: true },
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
                // I1: post-install で `banto-hub-elev.exe service-install`
                // （Hub サービス登録 + Operators グループ・ACL 一式）→
                // `banto-hub-sink.exe install` → `grant-service-acl` →
                // ProgramData 作成、pre-install でサービス・シェルの停止、
                // pre-uninstall でサービス登録解除（hooks/service-hooks.nsh
                // 参照）。
                installer_hooks: Some(installer_hooks),
                ..Default::default()
            }),
            ..Default::default()
        },
        ..Default::default()
    };

    let binaries = vec![
        BundleBinary::new(SHELL_BINARY_NAME.to_string(), true),
        BundleBinary::new(HUB_BINARY_NAME.to_string(), false),
        BundleBinary::new(ELEV_BINARY_NAME.to_string(), false),
        BundleBinary::new(SINK_BINARY_NAME.to_string(), false),
    ];

    let settings = SettingsBuilder::new()
        .package_settings(package_settings)
        .bundle_settings(bundle_settings)
        .binaries(binaries)
        .package_types(vec![PackageType::Nsis])
        .project_out_directory(&binaries_dir)
        .build()
        .context("tauri_bundler::SettingsBuilder::build に失敗しました")?;

    log::info!(
        "banto-hub-installer: {} を対象に NSIS インストーラをビルドします（出力先: {}）",
        binaries_dir.display(),
        binaries_dir.join("bundle").display()
    );

    let bundles = tauri_bundler::bundle_project(&settings)
        .context("tauri_bundler::bundle_project に失敗しました")?;

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
/// どのディレクトリから呼ばれても `icons/`・`hooks/`・`assets/` を正しく
/// 解決できるよう、実行時 CWD ではなくビルド時に埋め込まれるこのパスを
/// 基準にする。
fn installer_manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// リポジトリルートの `target/release/`
/// （docs/banto-hub-installer-design.md §4.6 記載の既定ビルド出力先）を
/// 既定の対象ディレクトリとする。
fn default_binaries_dir(installer_dir: &Path) -> Result<PathBuf> {
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
    Ok(repo_root.join("target").join("release"))
}

/// `dir` に [`REQUIRED_BINARY_NAMES`] の exe が全て揃っているか確認し、
/// 見つからなかったものの名前（拡張子なし）を順序どおりに返す
/// （空なら全て揃っている）。Win32 API を一切呼ばない純粋なファイル
/// システム確認なので非 Windows でも単体テストできる（`tests` モジュール
/// 参照）。
fn missing_binaries(dir: &Path) -> Vec<&'static str> {
    REQUIRED_BINARY_NAMES
        .into_iter()
        .filter(|name| !dir.join(format!("{name}.exe")).is_file())
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_binaries_lists_all_when_dir_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(missing_binaries(dir.path()), REQUIRED_BINARY_NAMES.to_vec());
    }

    #[test]
    fn missing_binaries_empty_when_all_present() {
        let dir = tempfile::tempdir().expect("tempdir");
        for name in REQUIRED_BINARY_NAMES {
            std::fs::write(dir.path().join(format!("{name}.exe")), b"stub").expect("write stub");
        }
        assert!(missing_binaries(dir.path()).is_empty());
    }

    #[test]
    fn missing_binaries_reports_only_absent_ones() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join(format!("{SHELL_BINARY_NAME}.exe")), b"stub")
            .expect("write stub");
        std::fs::write(dir.path().join(format!("{SINK_BINARY_NAME}.exe")), b"stub")
            .expect("write stub");

        assert_eq!(
            missing_binaries(dir.path()),
            vec![HUB_BINARY_NAME, ELEV_BINARY_NAME]
        );
    }

    /// ディレクトリと同名の exe（ファイルではない）は「見つからない」扱い
    /// にする - `is_file()` を使っているため、壊れたビルド出力（同名
    /// ディレクトリが誤って存在する等）を実行ファイルと誤認しない回帰
    /// テスト（`apps/banto-hub-sink/src/service.rs` の同種テストと同じ
    /// 考え方）。
    #[test]
    fn missing_binaries_ignores_directory_with_same_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join(format!("{HUB_BINARY_NAME}.exe"))).expect("mkdir");

        assert!(missing_binaries(dir.path()).contains(&HUB_BINARY_NAME));
    }
}
