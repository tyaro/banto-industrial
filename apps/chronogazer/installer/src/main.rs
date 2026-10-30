//! ChronoGazer 一体インストーラ生成ツール（#392 B3-4、
//! docs/chronogazer-service-design.md §3.3「インストーラ」・§4 の B3-4 の実装メモ）。
//!
//! banto-hub のインストーラ（`apps/banto-hub/installer/src/main.rs`）と同じ方式:
//! `tauri-bundler` をライブラリとして直接呼び、リリースビルド済みの exe を 1 本の
//! NSIS インストーラ（PerMachine）に包む。`tauri.conf.json`／`cargo tauri build` は
//! 経由しない（シェル以外の 2 exe を同じ `$INSTDIR` に揃え、Windows サービスの
//! 停止・登録・再開をフックで行うため）。この `apps/chronogazer/installer/`
//! パッケージ自体が「xtask 的」な立ち位置で、ルートワークスペースの member では
//! ない（`Cargo.toml` のコメント参照）。
//!
//! ## 同梱バイナリ（[`REQUIRED_BINARY_NAMES`]）
//!
//! | バイナリ                  | main | 役割                                                                 |
//! | ------------------------- | ---- | -------------------------------------------------------------------- |
//! | `chronogazer.exe`         | ○   | デスクトップシェル（Tauri v2、`apps/chronogazer/src-tauri`）           |
//! | `chronogazer-server.exe`  |      | 記録エンジン（headless host。Windows サービス `ChronoGazer` の本体）   |
//! | `chronogazer-elev.exe`    |      | UAC 昇格ヘルパー（サービス登録・Operators・ACL、`apps/chronogazer/elev`） |
//!
//! シェルの exe 名は `cargo build -p chronogazer` が出す `chronogazer.exe` のまま
//! （2026-10-31 オーナー決定 O-4。`cargo tauri build` が付ける `ChronoGazer.exe` には
//! 改名しない）。tauri-bundler の NSIS テンプレートは `${MAINBINARYNAME}.exe` を
//! レジストリの `MainBinaryName` と比べ、違えば旧い main を消すが、ChronoGazer の
//! PerMachine のインストーラはこれが最初で HKLM に旧い値は無く、旧い利用者単位の
//! `cargo tauri build` 版は HKCU に書くので比較の対象にならない（Hub の
//! `FixupHubOnlyInstallerMainBinaryName` に当たる直しは要らない。フック側の
//! 「旧い利用者単位の版」の節を参照）。
//!
//! ## 使い方
//!
//! ```powershell
//! # 1. 先に 3 つの release バイナリをビルド（scripts/build-release-chronogazer.ps1 が一括で行う）
//! pnpm --filter chronogazer build
//! cargo build --release -p chronogazer --features embed-ui
//! cargo build --release -p chronogazer-core --bin chronogazer-server --features embed-ui
//! cargo build --release -p chronogazer-elev
//!
//! # 2. インストーラ生成（既定でリポジトリルートの target/release/ を対象にする）
//! cargo run --manifest-path apps/chronogazer/installer/Cargo.toml --release
//!
//! # 別のディレクトリを対象にしたい場合は第 1 引数でディレクトリを渡す
//! cargo run --manifest-path apps/chronogazer/installer/Cargo.toml --release -- D:\path\to\bin\dir
//! ```
//!
//! 対象ディレクトリに 3 つの exe のいずれかが無い場合は、どれが無いかを名指しした
//! 日本語エラーで即座に止まる（[`missing_binaries`]）。
//!
//! 生成物は `<対象ディレクトリ>/bundle/nsis/ChronoGazer_<version>_x64-setup.exe`
//! （tauri-bundler の既定命名規則）。**このツールは生成したインストーラを実行しない**
//! （Hub と同じ。実行してのインストール確認はオーナー同席の実機確認）。
//!
//! ## フックの流れ（`hooks/service-hooks.nsh`）
//!
//! PREINSTALL: 旧い利用者単位の版の案内（O-3）→ シェルに `--quit-for-update` で
//! 正常終了を頼み、`chronogazer.exe` のプロセスが無くなるまで最大 90 秒待つ
//! （超えたら強制終了せずに再試行／中止を聞く、O-5）→ サービス `ChronoGazer` が
//! 動いていれば止めて記録。POSTINSTALL: `chronogazer-elev.exe service-install` →
//! 止めたサービスだけ再開。PREUNINSTALL: シェルの終了（同じ待ち）→ サービスの停止
//! → `service-uninstall`（`%ProgramData%\ChronoGazer` は残す）。サービスの停止・
//! 再開の部品は Hub と共通の `installer-common/service-common.nsh` にあり、この
//! ツールがフックの先頭へその絶対パスを `!define` してから tauri-bundler に渡す
//! （[`render_hooks`]）。

use std::env;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use tauri_bundler::{
    BundleBinary, BundleSettings, NsisSettings, PackageSettings, PackageType, SettingsBuilder,
    WindowsSettings,
};
use tauri_utils::config::{NSISInstallerMode, WebviewInstallMode};

/// インストーラの識別子。**シェルの `tauri.conf.json` の `identifier`
/// （`dev.tyaro.chronogazer`）とは意図的に変えてある**（2026-10-31 オーナー決定 O-8）。
///
/// tauri-bundler 2.9.4 の NSIS テンプレート（`installer.nsi` の Section Uninstall、
/// 「Delete app data if the checkbox is selected」）は、アンインストーラの「アプリの
/// データを削除する」にチェックが付くと `SetShellVarContext current` の後に
/// `RmDir /r "$APPDATA\${BUNDLEID}"` と `RmDir /r "$LOCALAPPDATA\${BUNDLEID}"` を
/// 行う（ソース確認済み。ほかに `BUNDLEID` を使うのは URL スキームの登録と
/// ジャンプリストの AppID だけで、どちらもこのインストーラでは使わない）。
/// 識別子を `dev.tyaro.chronogazer` にすると、以前の版のデスクトップ版が
/// `%APPDATA%\dev.tyaro.chronogazer\` に残している旧い DB（`chronogazer.sqlite3`、
/// `chronogazer_core::paths::LEGACY_DESKTOP_DIR_NAME`）がそのチェック 1 つで消える。
/// 今の版のデータは `%ProgramData%\ChronoGazer\` にあり、そもそも `%APPDATA%` は
/// 読まない・移さないので、識別子を別にして旧い置き場に触らないようにする。
/// Hub も同じ形（シェルの `dev.tyaro.banto-hub-shell` に対してインストーラは
/// `dev.tyaro.banto-hub`）。
const BUNDLE_IDENTIFIER: &str = "dev.tyaro.chronogazer-setup";
/// `tauri.conf.json` の `productName` と同じ。NSIS テンプレートの
/// アンインストールのレジストリキー（`...\Uninstall\ChronoGazer`）・既定の
/// `$INSTDIR`（`C:\Program Files\ChronoGazer`、`chronogazer-elev` が前提にする
/// 置き場）・スタートメニューの名前になる。
const PRODUCT_NAME: &str = "ChronoGazer";

/// デスクトップシェル（`apps/chronogazer/src-tauri`、パッケージ名 `chronogazer`）。
/// main バイナリ。
const SHELL_BINARY_NAME: &str = "chronogazer";
/// 記録エンジン＝サービスの本体（`apps/chronogazer/core/src/bin/chronogazer-server.rs`）。
const SERVER_BINARY_NAME: &str = "chronogazer-server";
/// UAC 昇格ヘルパー（`apps/chronogazer/elev`）。
const ELEV_BINARY_NAME: &str = "chronogazer-elev";

/// 同梱する 3 バイナリの名前（拡張子なし）。順序は「不足時のエラーメッセージに
/// 出す順」以上の意味は無い。
const REQUIRED_BINARY_NAMES: [&str; 3] = [SHELL_BINARY_NAME, SERVER_BINARY_NAME, ELEV_BINARY_NAME];

/// ワークスペース共通の `[workspace.package] version`（ルート `Cargo.toml`）と同じ値。
/// このパッケージはワークスペース外にあるため `version.workspace = true` が使えず、
/// 手動で追従させる（Hub のインストーラと同じ）。バージョンを上げたらここも更新する
/// こと。`scripts/build-release-chronogazer.ps1` は生成物のファイル名をルートの
/// 版から組み立てるので、ずれると「インストーラが見つかりません」で止まる。
const PRODUCT_VERSION: &str = "0.3.0-alpha.1";

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
             \u{20}\u{20}pnpm --filter chronogazer build\n\
             \u{20}\u{20}cargo build --release -p chronogazer --features embed-ui\n\
             \u{20}\u{20}cargo build --release -p chronogazer-core --bin chronogazer-server --features embed-ui\n\
             \u{20}\u{20}cargo build --release -p chronogazer-elev\n\
             別のディレクトリに 3 つの exe が揃っている場合は、第 1 引数でそのディレクトリを指定してください。",
            binaries_dir.display()
        );
    }

    // アイコンはシェルの `apps/chronogazer/src-tauri/icons/` をそのまま使う
    // （写しを持たない。シェルの exe のアイコンと同じものがインストーラ・
    // アンインストーラ・「アプリと機能」の表示になる）。
    let icon_dir = installer_dir.join("..").join("src-tauri").join("icons");
    let icon_png = require_file(icon_dir.join("icon.png"))?;
    let icon_ico = require_file(icon_dir.join("icon.ico"))?;

    // 共通部品（installer-common/service-common.nsh）の場所を、フックの先頭へ絶対パスで
    // `!define` して渡す。makensis の作業ディレクトリは tauri-bundler の出力先なので、
    // フック内の相対 `!include` は頼れない（Hub と同じ）。
    let common_dir = require_file(
        installer_dir
            .join("..")
            .join("..")
            .join("..")
            .join("installer-common")
            .join("service-common.nsh"),
    )?
    .parent()
    .map(Path::to_path_buf)
    .context("installer-common の場所を解決できません")?;
    let common_dir = common_dir
        .canonicalize()
        .context("installer-common を正規化できません")?;
    let hooks_src = require_file(installer_dir.join("hooks").join("service-hooks.nsh"))?;
    let hooks_work_dir = env::temp_dir().join(format!(
        "chronogazer-installer-hooks-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&hooks_work_dir).context("フックの作業ディレクトリを作れません")?;
    let installer_hooks = hooks_work_dir.join("service-hooks.nsh");
    std::fs::write(
        &installer_hooks,
        render_hooks(
            &std::fs::read_to_string(&hooks_src)
                .with_context(|| format!("読めません: {}", hooks_src.display()))?,
            &common_dir,
        )?,
    )
    .context("フックの書き出しに失敗しました")?;

    let package_settings = PackageSettings {
        product_name: PRODUCT_NAME.to_string(),
        version: PRODUCT_VERSION.to_string(),
        description: "ChronoGazer（デジタル記録計）。デスクトップシェル・記録エンジン\
            （Windows サービス）・UAC ヘルパーの 3 実行ファイルを同梱した一体インストーラです。"
            .to_string(),
        homepage: None,
        authors: Some(vec!["tyaro".to_string()]),
        default_run: Some(SHELL_BINARY_NAME.to_string()),
    };

    let bundle_settings = BundleSettings {
        identifier: Some(BUNDLE_IDENTIFIER.to_string()),
        icon: Some(vec![path_to_string(&icon_png)?, path_to_string(&icon_ico)?]),
        short_description: Some(
            "ChronoGazer デジタル記録計（シェル・記録エンジン同梱）".to_string(),
        ),
        windows: WindowsSettings {
            // シェルは WebView を使う Tauri アプリなので WebView2 ランタイムが要る。
            // 無い PC だけ取得しに行く `EmbedBootstrapper`（+約 2MB、完全オフラインでは
            // ない）を Hub と同じく選ぶ（2026-10-31 オーナー決定 O-7）。
            webview_install_mode: WebviewInstallMode::EmbedBootstrapper { silent: true },
            nsis: Some(NsisSettings {
                installer_icon: Some(icon_ico.clone()),
                // Windows サービスを登録・解除するインストーラなので PerMachine
                // （インストーラ自体を UAC で昇格させ、`$INSTDIR` を Program Files に
                // する。`chronogazer-elev` は Program Files の外では動かない）。
                install_mode: NSISInstallerMode::PerMachine,
                languages: Some(vec!["Japanese".to_string(), "English".to_string()]),
                installer_hooks: Some(installer_hooks),
                ..Default::default()
            }),
            ..Default::default()
        },
        ..Default::default()
    };

    let binaries = vec![
        BundleBinary::new(SHELL_BINARY_NAME.to_string(), true),
        BundleBinary::new(SERVER_BINARY_NAME.to_string(), false),
        BundleBinary::new(ELEV_BINARY_NAME.to_string(), false),
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
        "chronogazer-installer: {} を対象に NSIS インストーラをビルドします（出力先: {}）",
        binaries_dir.display(),
        binaries_dir.join("bundle").display()
    );

    let bundles = tauri_bundler::bundle_project(&settings)
        .context("tauri_bundler::bundle_project に失敗しました");
    let _ = std::fs::remove_dir_all(&hooks_work_dir);
    let bundles = bundles?;

    for bundle in &bundles {
        for path in &bundle.bundle_paths {
            log::info!("chronogazer-installer: 生成しました -> {}", path.display());
        }
    }

    if bundles.is_empty() {
        bail!("インストーラが1つも生成されませんでした（package_types の設定を確認してください）");
    }

    Ok(())
}

/// この Cargo パッケージ自身の `Cargo.toml` があるディレクトリ
/// （`apps/chronogazer/installer/`）。`cargo run --manifest-path ...` でどの
/// ディレクトリから呼ばれても `hooks/` とシェルの `icons/` を正しく解決できるよう、
/// 実行時 CWD ではなくビルド時に埋め込まれるこのパスを基準にする。
fn installer_manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// リポジトリルートの `target/release/` を既定の対象ディレクトリとする。
fn default_binaries_dir(installer_dir: &Path) -> Result<PathBuf> {
    let repo_root = installer_dir
        .parent() // apps/chronogazer
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
/// 見つからなかったものの名前（拡張子なし）を順序どおりに返す（空なら全て
/// 揃っている）。Win32 API を一切呼ばない純粋なファイルシステム確認なので
/// 非 Windows でも単体テストできる。
fn missing_binaries(dir: &Path) -> Vec<&'static str> {
    REQUIRED_BINARY_NAMES
        .into_iter()
        .filter(|name| !dir.join(format!("{name}.exe")).is_file())
        .collect()
}

/// フック本体の先頭に `BANTO_INSTALLER_COMMON_DIR`（共通部品のディレクトリの
/// 絶対パス）の `!define` を足す。NSIS の文字列に埋められないパス
/// （`"`・`$`・改行）は拒否する（Hub の同名関数と同じ）。
fn render_hooks(hooks: &str, common_dir: &Path) -> Result<String> {
    let dir = path_to_string(common_dir)?;
    if dir.contains(['"', '$', '\n', '\r']) {
        bail!("共通部品のパスに NSIS で扱えない文字があります: {dir}");
    }
    let nl = if hooks.contains("\r\n") { "\r\n" } else { "\n" };
    Ok(format!(
        "!define BANTO_INSTALLER_COMMON_DIR \"{dir}\"{nl}{hooks}"
    ))
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
    fn render_hooks_prepends_the_common_dir_define() {
        let out = render_hooks("body\r\n", Path::new("C:/x/installer-common")).expect("render");
        assert_eq!(
            out,
            "!define BANTO_INSTALLER_COMMON_DIR \"C:/x/installer-common\"\r\nbody\r\n"
        );
    }

    #[test]
    fn render_hooks_keeps_lf_when_the_hooks_use_lf() {
        let out = render_hooks("body\n", Path::new("C:/x")).expect("render");
        assert_eq!(out, "!define BANTO_INSTALLER_COMMON_DIR \"C:/x\"\nbody\n");
    }

    #[test]
    fn render_hooks_rejects_paths_nsis_cannot_embed() {
        assert!(render_hooks("b", Path::new("C:/a$b")).is_err());
        assert!(render_hooks("b", Path::new("C:/a\"b")).is_err());
    }

    /// O-8: 識別子はシェルの `tauri.conf.json` の `identifier` と違っていなければ
    /// ならない（同じだと、アンインストーラの「アプリのデータを削除する」が
    /// `%APPDATA%\dev.tyaro.chronogazer` の旧い DB を消す）。
    #[test]
    fn bundle_identifier_differs_from_the_shell_identifier() {
        let conf = std::fs::read_to_string(
            installer_manifest_dir()
                .join("..")
                .join("src-tauri")
                .join("tauri.conf.json"),
        )
        .expect("tauri.conf.json");
        assert!(
            conf.contains("\"identifier\": \"dev.tyaro.chronogazer\""),
            "シェルの identifier が変わった - この試験と BUNDLE_IDENTIFIER の説明を見直す"
        );
        assert_ne!(BUNDLE_IDENTIFIER, "dev.tyaro.chronogazer");
    }

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

        assert_eq!(
            missing_binaries(dir.path()),
            vec![SERVER_BINARY_NAME, ELEV_BINARY_NAME]
        );
    }

    /// ディレクトリと同名の exe（ファイルではない）は「見つからない」扱いにする
    /// （`is_file()`。Hub の同名テストと同じ考え方）。
    #[test]
    fn missing_binaries_ignores_directory_with_same_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join(format!("{SERVER_BINARY_NAME}.exe"))).expect("mkdir");

        assert!(missing_binaries(dir.path()).contains(&SERVER_BINARY_NAME));
    }
}
