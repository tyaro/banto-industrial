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
//! cargo build --release -p banto-hub-host --bin banto-hub --bin banto-hub-elev --features embed-ui
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
//! `banto-hub-sink.exe grant-service-acl` → `banto-hub-elev.exe
//! prepare-sink-config`（#630: 保護された置き場 `%ProgramData%\BantoHub\sink\` を
//! 整え、`$INSTDIR` の `banto-hub-sink.toml.example` をそこへ差し替える）→ PREINSTALL で止めた
//! サービスの再開、の順で実行する。サービスの停止・シェルの終了・再開の部品は
//! ChronoGazer のインストーラと共通の `installer-common/service-common.nsh` にあり、
//! このツールがフックの先頭へその絶対パスを `!define` してから tauri-bundler に渡す
//! （[`render_hooks`]）。PREUNINSTALL はシェルを止め、サイドカーと Hub の
//! サービスを止める（登録の解除は下の P6 で Section Uninstall の途中へ移した）。
//! サブコマンドの失敗ではインストーラ自体を中断させない（`DetailPrint` で案内する
//! だけ - T5-2 以来の方針）。例外（置き場・ファイルの状態が壊れたまま成功に
//! 見えるのを防ぐ `Abort`）はフックの冒頭の一覧を参照。
//!
//! ## NSIS テンプレートの当て直し（`installer-common/nsis-template/`、[`TEMPLATE_PATCHES`]）
//!
//! tauri-bundler 2.9.4 の `installer.nsi` には、このインストーラの方針と合わない
//! 箇所がある。ChronoGazer のインストーラ（`apps/chronogazer/installer`、#972 の
//! P1〜P6）と同じ仕組み - tauri-bundler の `NsisSettings::template` に、**2.9.4 の
//! `installer.nsi` をそのまま写した `installer-common/nsis-template/installer.nsi`**
//! へ文字列の完全一致の置き換え（各 1 回だけ一致しなければエラー）を当てたものを
//! 渡す - で直す（#973 PR-3b。写しと当てる部品は ChronoGazer と共通の
//! [`nsis_template`]。写しが byte 単位で同じことの試験もそこ。`Cargo.toml` は
//! `=2.9.4` で固定）。当て直しの表（何を当てるか）は製品ごとにこのファイルに置く。
//! 番号は ChronoGazer の P1〜P6 と対応させてある。
//!
//! Hub に当てるもの（決定は docs/banto-hub-installer-design.md §4.1・§4.3）:
//!
//! 1. P1: `PageReinstall` の先頭で `Abort`（頁を出さない = 旧い版の
//!    アンインストーラを走らせず、**常にその場で上書きする**）。原本は既に
//!    入っていると「先にアンインストールしてから入れる」を既定の選択で出し、
//!    旧い版の PREUNINSTALL がサービスを止めて `service-uninstall` するので、
//!    新しい版の PREINSTALL にはサービスが「動いていない」と見え、POSTINSTALL は
//!    再開せず、起動種別（遅延自動開始）も手動に戻る。`PageLeaveReinstall` も
//!    先頭で `reinst_done` へ飛ばして二重に塞ぐ。
//! 3. P3: `Section WebView2` の先頭にフックの `BantoHubInstallPreflight`
//!    （インストール先の固定・所有者と ACL の戻し。何も変えずに止められる確かめ）
//!    を挿入する。当て直しなしでは確かめが Section Install（WebView2 の導入と
//!    `SetOutPath` の後）になり、駄目でも WebView2 だけ入り、空のフォルダが
//!    できていた（#973 PR-3a の Codex の指摘 1。この PR の主眼）。
//! 4. P4: `Function .onInstFailed` と `Function un.onUninstFailed` を足し、
//!    フックの `BantoHubOnInstFailed`／`BantoHubOnUninstFailed`（止めたままの
//!    サービスを戻す）を呼ぶ。原本には無く、`File` の「使用中」で中止すると
//!    止めたサービスが止まったまま残っていた。
//! 5. P5: インストーラ属性 `AllowSkipFiles off`（`File` の失敗の窓から「無視」が
//!    消え、silent では中止になる）、`File` の直前に `ClearErrors`、外部バイナリの
//!    `File` の直後にフックの `RequireBantoHubFilesCopied`（エラーフラグが
//!    立っていれば `Abort`。旧い exe と新しい exe が混ざったまま POSTINSTALL に
//!    進まない）。
//! 6. P6: Section Uninstall で、テンプレートの `CheckIfAppIsRunning` の直後・
//!    `Delete` の前にフックの `RequireBantoHubExesUnlocked`（4 つの exe を
//!    `FileOpen ... a` で開けなければ `Abort`。まだ何も変更していない）と
//!    `UnregisterBantoHubServices`（PREUNINSTALL から移した登録の解除。解除できなければ
//!    ファイルを消す前に `Abort`）、外部
//!    バイナリの `Delete` の直後・`uninstall.exe` の `Delete` と Uninstall キーの
//!    削除の前に `RequireBantoHubExesDeleted`（残っていれば頼める分の登録を戻して
//!    `Abort`）。
//!
//! 当てないもの: **P2**（`CheckIfAppIsRunning` をフックの「頼んで待つ」再確認に
//! 置き換える）。ChronoGazer はシェルに `--quit-for-update` で終了を頼み「強制終了
//! しない」約束があるが、Hub のシェルには終了を頼む道が無く、フック自身が
//! `taskkill` で閉じる方針なので、テンプレートの確認（残っていれば案内の後に
//! KillProcess）はそのまま使う。

use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use tauri_bundler::{
    BundleBinary, BundleSettings, NsisSettings, PackageSettings, PackageType, SettingsBuilder,
    WindowsSettings,
};
use tauri_utils::config::{NSISInstallerMode, WebviewInstallMode};

/// tauri-bundler 2.9.4 の `installer.nsi` の写しと当て直しの部品（ChronoGazer の
/// xtask と同じファイルを取り込む。モジュール doc「NSIS テンプレートの当て直し」）。
#[path = "../../../../installer-common/nsis-template/vendored_template.rs"]
mod nsis_template;
use nsis_template::{patch_template, TemplatePatch, VENDORED_TEMPLATE};

/// 既存2アプリの `dev.tyaro.{name}` 命名規則を踏襲
/// （apps/relay-wright/src-tauri/tauri.conf.json の `identifier` 参照）。
const BUNDLE_IDENTIFIER: &str = "dev.tyaro.banto-hub";
/// docs/banto-hub-operations.md 全体で使われている表記に合わせる。
const PRODUCT_NAME: &str = "BantoHub";

/// デスクトップシェル（`apps/banto-hub/src-tauri`、パッケージ名
/// `banto-hub-shell`）。I1 で main バイナリになった
/// （モジュール doc「同梱バイナリ」節参照）。
const SHELL_BINARY_NAME: &str = "banto-hub-shell";
/// Hub 本体（`apps/banto-hub/host/src/bin/banto-hub.rs`）。
const HUB_BINARY_NAME: &str = "banto-hub";
/// UAC 昇格ヘルパー（`apps/banto-hub/host/src/bin/banto-hub-elev.rs`）。
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
const PRODUCT_VERSION: &str = "0.3.0-alpha.1";

/// [`VENDORED_TEMPLATE`] に当てる置き換え（`(名前, 土台, 置き換え後)`、モジュール doc
/// 「NSIS テンプレートの当て直し」）。土台は写しの中に**ちょうど 1 回**現れる文字列で
/// なければならず、違えば [`patch_template`] が名前を挙げて止まる（tauri-bundler の
/// 版を上げて土台が変わったときに黙って当たらないままにしない）。順に当てる。
/// 置き換え後の文字列は NSIS のソースなので LF 改行・2 空白のインデント（原本に
/// 合わせる）。版を上げたときは、各パッチの土台がまだ 1 回だけ一致するかを試験
/// （`vendored_template_contains_every_patch_anchor_exactly_once`・`patch_template_*`）
/// で確かめる。
///
/// 置き換え後で挿入するマクロ（`BantoHubInstallPreflight` など）は
/// `hooks/service-hooks.nsh` のもの（テンプレートはフックを先頭近くで `!include`
/// するので、Section・関数の時点で定義済み）。
const TEMPLATE_PATCHES: &[TemplatePatch] = &[
    (
        // P1: 再インストールの頁を出さない。頁の関数の `Abort` は「この頁を
        // 飛ばす」（テンプレート自身が「入っていなければ飛ばす」に使っている形）。
        // 頁が無いので `PageLeaveReinstall`（旧い版のアンインストーラの起動）は
        // 呼ばれず、常にその場で上書きする。silent はもともと頁の関数を呼ばない
        // ので、silent と UI の挙動が揃う。WiX（msi）版の検出も頁の中にあるが、
        // Hub に WiX 版は無い。
        "P1-reinstall-page",
        "Function PageReinstall\n",
        "Function PageReinstall\n\
         \x20 ; banto-hub-installer (#973 PR-3b P1): 旧い版のアンインストーラを走らせず、\n\
         \x20 ; 常にその場で上書きするので、この頁は出さない（Abort = 頁を飛ばす）。\n\
         \x20 Abort\n",
    ),
    (
        // P1 の二重の塞ぎ: 万一 leave が呼ばれても `reinst_uninstall` には進まない
        // （`/UPDATE` のときの `Goto reinst_done` と同じ道）。
        "P1-reinstall-leave",
        "Function PageLeaveReinstall\n",
        "Function PageLeaveReinstall\n\
         \x20 ; banto-hub-installer (#973 PR-3b P1): 常にその場で上書き（/UPDATE と同じ道）。\n\
         \x20 Goto reinst_done\n",
    ),
    (
        // P5（属性）: `File` の失敗の窓から「無視」を消す。インストーラ属性なので
        // どの Section にも効く（silent では中止 = `Abort` = `.onInstFailed`）。
        "P5-allow-skip-files-off",
        "OutFile \"${OUTFILE}\"\n",
        "OutFile \"${OUTFILE}\"\n\
         ; banto-hub-installer (#973 PR-3b P5): 使用中で書けない File を「無視」で飛ばせない\n\
         ; ようにする（再試行／中止だけ。silent では中止）。\n\
         AllowSkipFiles off\n",
    ),
    (
        // P5（Install、前）: 上書きの前にエラーフラグを落とす（直前の
        // `CheckIfAppIsRunning` や `ReadRegStr` の失敗で立っていることがある）。
        "P5-clear-errors-before-file",
        "  ; Copy main executable\n\
         \x20 File \"${MAINBINARYSRCPATH}\"\n",
        "  ; banto-hub-installer (#973 PR-3b P5): File の失敗だけを見るため、直前で落とす。\n\
         \x20 ClearErrors\n\
         \n\
         \x20 ; Copy main executable\n\
         \x20 File \"${MAINBINARYSRCPATH}\"\n",
    ),
    (
        // P5（Install、後）: main・resources・external binaries の `File` の直後に、
        // エラーフラグが立っていれば `Abort`（POSTINSTALL に進まない）。
        "P5-require-files-copied",
        "  ; Copy external binaries\n\
         \x20 {{#each binaries}}\n\
         \x20   File /a \"/oname={{this}}\" \"{{no-escape @key}}\"\n\
         \x20 {{/each}}\n",
        "  ; Copy external binaries\n\
         \x20 {{#each binaries}}\n\
         \x20   File /a \"/oname={{this}}\" \"{{no-escape @key}}\"\n\
         \x20 {{/each}}\n\
         \n\
         \x20 ; banto-hub-installer (#973 PR-3b P5): 上書きに失敗したものがあれば（エラーフラグ）\n\
         \x20 ; 混ざったまま成功にせず中止する（.onInstFailed が止めたサービスを戻す）。\n\
         \x20 !insertmacro RequireBantoHubFilesCopied\n",
    ),
    (
        // P6（Uninstall、前）: テンプレートの `CheckIfAppIsRunning`（P2 は当てないので
        // 残る）の直後・`Delete` の前に、使用中の exe の確かめと、PREUNINSTALL から
        // 移した登録の解除。土台は Uninstall 側の挿入（直後のコメントで Install 側と
        // 区別する）。
        "P6-uninstall-unlock-and-unregister",
        "  !insertmacro CheckIfAppIsRunning \"${MAINBINARYNAME}.exe\" \"${PRODUCTNAME}\"\n\
         \n\
         \x20 ; Delete the app directory and its content from disk\n",
        "  !insertmacro CheckIfAppIsRunning \"${MAINBINARYNAME}.exe\" \"${PRODUCTNAME}\"\n\
         \n\
         \x20 ; banto-hub-installer (#973 PR-3b P6): 使用中の exe があれば、登録を解除する前に\n\
         \x20 ; 中止（まだ何も変更していない。止めたサービスは un.onUninstFailed が戻す）。\n\
         \x20 !insertmacro RequireBantoHubExesUnlocked\n\
         \x20 !insertmacro UnregisterBantoHubServices\n\
         \n\
         \x20 ; Delete the app directory and its content from disk\n",
    ),
    (
        // P6（Uninstall、後）: 4 つの exe の `Delete` の直後に残っていないかを確かめ、
        // 残っていれば `uninstall.exe` の `Delete` と Uninstall キーの削除より前に
        // `Abort`（フック側で頼める分のサービスの登録を戻す）。
        "P6-require-exes-deleted",
        "  ; Delete external binaries\n\
         \x20 {{#each binaries}}\n\
         \x20   Delete \"$INSTDIR\\\\{{this}}\"\n\
         \x20 {{/each}}\n",
        "  ; Delete external binaries\n\
         \x20 {{#each binaries}}\n\
         \x20   Delete \"$INSTDIR\\\\{{this}}\"\n\
         \x20 {{/each}}\n\
         \n\
         \x20 ; banto-hub-installer (#973 PR-3b P6): exe が残っていれば、uninstall.exe と「アプリ」の\n\
         \x20 ; 登録を消す前に止める（サービスの登録は残っている exe で戻す）。\n\
         \x20 !insertmacro RequireBantoHubExesDeleted\n",
    ),
    (
        // P3: 事前の確かめを WebView2 の取得より前に置く（この PR の主眼）。Section は
        // EarlyChecks → WebView2 → Install の順に全部走るので、WebView2 の先頭 =
        // Install（`SetOutPath`）より前。ここでの `Abort` は Section WebView2 ごと
        // 止める（Install には進まない）。
        "P3-preflight-before-webview2",
        "Section WebView2\n",
        "Section WebView2\n\
         \x20 ; banto-hub-installer (#973 PR-3b P3): WebView2 を入れる前・インストール先を\n\
         \x20 ; 作る前に、何も変えずに止められる確かめ（インストール先の固定・ACL の戻し）。\n\
         \x20 !insertmacro BantoHubInstallPreflight\n",
    ),
    (
        // P4（Install）: `.onInstFailed`（Section が `Abort` した・`File` の「使用中」で
        // 中止した後に呼ばれる。silent でも呼ばれる）で、PREINSTALL で止めてまだ
        // 戻していないサービスを戻す。
        "P4-oninstfailed",
        "Function .onInstSuccess\n",
        "Function .onInstFailed\n\
         \x20 ; banto-hub-installer (#973 PR-3b P4): 途中で止まったら、上書き前に止めた\n\
         \x20 ; サービスを戻す（フックの BantoHubOnInstFailed）。\n\
         \x20 !insertmacro BantoHubOnInstFailed\n\
         FunctionEnd\n\
         \n\
         Function .onInstSuccess\n",
    ),
    (
        // P4（Uninstall）: 同じく `un.onUninstFailed`（Section Uninstall の `Abort` の後に
        // 呼ばれる）で、PREUNINSTALL で止めたサービスを戻す。ChronoGazer は中止の道
        // ごとに戻しているが、Hub は 1 か所にまとめる。
        "P4-onuninstfailed",
        "Function un.onInit\n",
        "Function un.onUninstFailed\n\
         \x20 ; banto-hub-installer (#973 PR-3b P4): アンインストールが途中で止まったら、\n\
         \x20 ; その前に止めたサービスを戻す（フックの BantoHubOnUninstFailed）。\n\
         \x20 !insertmacro BantoHubOnUninstFailed\n\
         FunctionEnd\n\
         \n\
         Function un.onInit\n",
    ),
];

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
             \u{20}\u{20}cargo build --release -p banto-hub-host --bin banto-hub --bin banto-hub-elev --features embed-ui\n\
             \u{20}\u{20}cargo build --release -p banto-hub-shell --features banto-hub-core/embed-ui\n\
             \u{20}\u{20}cargo build --release -p banto-hub-sink\n\
             別のディレクトリに4つの exe が揃っている場合は、第1引数でそのディレクトリを指定してください。",
            binaries_dir.display()
        );
    }

    let icon_dir = installer_dir.join("icons");
    let icon_png = require_file(icon_dir.join("icon.png"))?;
    let icon_ico = require_file(icon_dir.join("icon.ico"))?;

    // 共通部品（installer-common/service-common.nsh）の場所を、フックの先頭へ絶対パスで
    // `!define` して渡す。makensis の作業ディレクトリは tauri-bundler の出力先なので、
    // フック内の相対 `!include` は頼れない。
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
    let hooks_work_dir =
        env::temp_dir().join(format!("banto-hub-installer-hooks-{}", std::process::id()));
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

    // 当て直した NSIS テンプレート（冒頭の「NSIS テンプレートの当て直し」）。
    // tauri-bundler は handlebars で描画した結果を出力先の `nsis/x64/installer.nsi` に
    // 書くので、生成物を確かめるときはそちらを見る。
    let installer_template = hooks_work_dir.join("installer.nsi");
    std::fs::write(
        &installer_template,
        patch_template(VENDORED_TEMPLATE, TEMPLATE_PATCHES)?,
    )
    .context("テンプレートの書き出しに失敗しました")?;

    let sink_config_example = require_file(
        installer_dir
            .join("assets")
            .join("banto-hub-sink.toml.example"),
    )?;
    let cdla_license = require_file(installer_dir.join("assets").join("CDLA-Permissive-2.0.txt"))?;

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

    // §4.3「サイドカーの設定ファイルの置き場」。雛形は $INSTDIR 直下に置き、
    // `banto-hub-elev.exe prepare-sink-config`（#630）が exe の隣から読んで
    // 保護された置き場 `%ProgramData%\BantoHub\sink\` へ差し替える。resources_map の値を空文字列に
    // すると、tauri-bundler は「元のファイル名のまま $INSTDIR 直下」に置く
    // （tauri-utils 2.9.3 の `ResourcePaths::resource_from_path` -
    // `{ "README.md": "" }` は `$INSTDIR/README.md` になる仕様、ソース確認
    // 済み）。絶対パスを渡しているので `cargo run` の実行時カレント
    // ディレクトリに依存しない。
    let mut resources_map = HashMap::new();
    resources_map.insert(path_to_string(&sink_config_example)?, String::new());
    // The bundled root certificate data is shared with the binaries; install its license text.
    resources_map.insert(path_to_string(&cdla_license)?, String::new());

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
                // Section Uninstall の途中（P6）でサービス登録解除
                // （hooks/service-hooks.nsh 参照）。
                installer_hooks: Some(installer_hooks),
                template: Some(installer_template),
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
        .context("tauri_bundler::bundle_project に失敗しました");
    let _ = std::fs::remove_dir_all(&hooks_work_dir);
    let bundles = bundles?;

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

/// フック本体の先頭に `BANTO_INSTALLER_COMMON_DIR`（共通部品のディレクトリの
/// 絶対パス）の `!define` を足す。NSIS の文字列に埋められないパス
/// （`"`・`$`・改行）は拒否する。
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
    fn render_hooks_rejects_paths_nsis_cannot_embed() {
        assert!(render_hooks("b", Path::new("C:/a$b")).is_err());
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

    /// 各パッチの土台が写しにちょうど 1 回ある（版を上げて原本が変わったら
    /// ここで気づく。写しそのものが原本のままかは共通の `nsis_template` の試験で見る）。
    /// 置き換え後の文字列は土台と違う。
    #[test]
    fn vendored_template_contains_every_patch_anchor_exactly_once() {
        for (name, anchor, replacement) in TEMPLATE_PATCHES {
            assert_eq!(
                VENDORED_TEMPLATE.matches(anchor).count(),
                1,
                "パッチ {name} の土台が 1 回だけ一致しない"
            );
            assert_ne!(anchor, replacement, "パッチ {name} が何も変えない");
        }
    }

    /// 最初の命令（コメント以外）。
    fn first_instruction<'a>(text: &'a str, function: &str) -> &'a str {
        let start = text.find(function).expect("function") + function.len();
        text[start..]
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty() && !line.starts_with(';'))
            .expect("instruction")
    }

    /// `needle` が 1 回だけあり、その位置。
    fn pos(text: &str, needle: &str) -> usize {
        assert_eq!(text.matches(needle).count(), 1, "{needle} が 1 回ではない");
        text.find(needle).expect("needle")
    }

    /// 当て直した結果: P1 - 再インストールの頁の関数が先頭で `Abort`、leave が
    /// 先頭で `reinst_done` へ飛ぶ。P2 は当てないので、Section Install／Uninstall の
    /// `CheckIfAppIsRunning` は 2 つとも残り、フックの直後にある。
    #[test]
    fn patch_template_applies_p1_and_keeps_check_if_app_is_running() {
        let out = patch_template(VENDORED_TEMPLATE, TEMPLATE_PATCHES).expect("patch");

        assert_eq!(first_instruction(&out, "Function PageReinstall\n"), "Abort");
        assert_eq!(
            first_instruction(&out, "Function PageLeaveReinstall\n"),
            "Goto reinst_done"
        );
        // 原本の「入っていなければ飛ばす」と `reinst_uninstall` のラベル自体は
        // 残る（届かないだけ）。
        assert!(out.contains("reinst_uninstall:"));
        assert!(out.contains("reinst_done:"));

        assert_eq!(out.matches("!insertmacro CheckIfAppIsRunning").count(), 2);
        let install = pos(&out, "Section Install\n");
        let uninstall = pos(&out, "Section Uninstall\n");
        let body = &out[install..uninstall];
        let hook = pos(body, "!insertmacro NSIS_HOOK_PREINSTALL");
        let check = pos(body, "!insertmacro CheckIfAppIsRunning");
        assert!(hook < check);
        // フックはテンプレートの先頭近くで include されるので、マクロは Section の
        // 時点で定義済み。
        let include = pos(&out, "!include \"{{installer_hooks}}\"");
        assert!(include < install);
    }

    /// 当て直した結果: P3 - 事前の確かめ（`BantoHubInstallPreflight`）が
    /// `Section WebView2` の最初の命令で、WebView2 の取得（`ExecWait`）より前・
    /// `Section Install`（`SetOutPath`）より前にある。P4 - `.onInstFailed` と
    /// `un.onUninstFailed` が 1 つずつあり、フックのマクロを呼ぶ。
    #[test]
    fn patch_template_applies_p3_and_p4() {
        let out = patch_template(VENDORED_TEMPLATE, TEMPLATE_PATCHES).expect("patch");

        let include = pos(&out, "!include \"{{installer_hooks}}\"");
        let webview2 = pos(&out, "Section WebView2\n");
        let install = pos(&out, "Section Install\n");
        let preflight = pos(&out, "!insertmacro BantoHubInstallPreflight");
        assert!(include < webview2 && webview2 < preflight && preflight < install);
        let webview2_body = &out[webview2..install];
        let first_exec = webview2_body.find("ExecWait").expect("webview2 ExecWait");
        assert!(preflight - webview2 < first_exec);
        assert_eq!(
            first_instruction(&out, "Section WebView2\n"),
            "!insertmacro BantoHubInstallPreflight"
        );
        // Section Install の `SetOutPath`（空のインストール先の作成）より前。
        let set_out_path = pos(&out, "  SetOutPath $INSTDIR\n");
        assert!(preflight < set_out_path && install < set_out_path);

        // P4: .onInstFailed が 1 つ、.onInstSuccess の前。
        let failed = pos(&out, "Function .onInstFailed\n");
        let failed_end = out[failed..].find("FunctionEnd").expect("FunctionEnd") + failed;
        assert!(out[failed..failed_end].contains("!insertmacro BantoHubOnInstFailed"));
        assert!(failed < pos(&out, "Function .onInstSuccess\n"));
        // un.onUninstFailed が 1 つ、un.onInit の前。
        let un_failed = pos(&out, "Function un.onUninstFailed\n");
        let un_failed_end = out[un_failed..].find("FunctionEnd").expect("FunctionEnd") + un_failed;
        assert!(out[un_failed..un_failed_end].contains("!insertmacro BantoHubOnUninstFailed"));
        assert!(un_failed < pos(&out, "Function un.onInit\n"));
    }

    /// 当て直した結果: P5 - `AllowSkipFiles off` が 1 つ、Section より前にある。
    /// Section Install では PREINSTALL → `CheckIfAppIsRunning` → `ClearErrors` →
    /// main の `File` → 外部バイナリの `File` → `RequireBantoHubFilesCopied` →
    /// `WriteUninstaller` → POSTINSTALL の順。P6 - Section Uninstall では
    /// PREUNINSTALL → `CheckIfAppIsRunning` → `RequireBantoHubExesUnlocked` →
    /// `UnregisterBantoHubServices` → main の `Delete` → 外部バイナリの `Delete` →
    /// `RequireBantoHubExesDeleted` → `uninstall.exe` の `Delete` → Uninstall
    /// キーの削除の順。
    #[test]
    fn patch_template_applies_p5_and_p6() {
        let out = patch_template(VENDORED_TEMPLATE, TEMPLATE_PATCHES).expect("patch");

        // P5: 属性は 1 つ、最初の Section より前（インストーラ属性は Section の外）。
        let allow = pos(&out, "AllowSkipFiles off\n");
        let early = pos(&out, "Section EarlyChecks\n");
        assert!(allow < early);

        let install = pos(&out, "Section Install\n");
        let uninstall = pos(&out, "Section Uninstall\n");
        let body = &out[install..uninstall];
        let hook = pos(body, "!insertmacro NSIS_HOOK_PREINSTALL");
        let check = pos(body, "!insertmacro CheckIfAppIsRunning");
        let clear = pos(body, "  ClearErrors\n");
        let main_file = pos(body, "File \"${MAINBINARYSRCPATH}\"");
        let bin_file = pos(body, "File /a \"/oname={{this}}\"");
        let copied = pos(body, "!insertmacro RequireBantoHubFilesCopied");
        let write_uninst = pos(body, "WriteUninstaller");
        let post = pos(body, "!insertmacro NSIS_HOOK_POSTINSTALL");
        assert!(hook < check && check < clear && clear < main_file && main_file < bin_file);
        assert!(bin_file < copied && copied < write_uninst && write_uninst < post);
        // `ClearErrors` と main の `File` の間に命令が無い（`CheckIfAppIsRunning` の
        // 中の命令で立ったフラグを、落とした後に見る）。
        let between = &body[clear + "  ClearErrors\n".len()..main_file];
        assert!(between
            .lines()
            .map(str::trim)
            .all(|line| line.is_empty() || line.starts_with(';')));

        // P6: 解除の前の確かめ、Delete の後の確かめ、の順。
        let un = &out[uninstall..];
        let un_hook = pos(un, "!insertmacro NSIS_HOOK_PREUNINSTALL");
        let un_check = pos(un, "!insertmacro CheckIfAppIsRunning");
        let unlocked = pos(un, "!insertmacro RequireBantoHubExesUnlocked");
        let unreg = pos(un, "!insertmacro UnregisterBantoHubServices");
        let main_del = pos(un, "Delete \"$INSTDIR\\${MAINBINARYNAME}.exe\"");
        let bin_del = pos(un, "Delete \"$INSTDIR\\\\{{this}}\"");
        let deleted = pos(un, "!insertmacro RequireBantoHubExesDeleted");
        let uninst_del = pos(un, "Delete \"$INSTDIR\\uninstall.exe\"");
        let key_del = pos(un, "DeleteRegKey HKLM \"${UNINSTKEY}\"");
        assert!(un_hook < un_check && un_check < unlocked && unlocked < unreg);
        assert!(unreg < main_del && main_del < bin_del && bin_del < deleted);
        assert!(deleted < uninst_del && uninst_del < key_del);
    }

    /// フックの当て直しが呼ぶマクロが、フックに全部ある（名前のずれで makensis まで
    /// 気づかないのを防ぐ）。
    #[test]
    fn hooks_define_every_macro_the_patches_insert() {
        let hooks = include_str!("../hooks/service-hooks.nsh");
        let out = patch_template(VENDORED_TEMPLATE, TEMPLATE_PATCHES).expect("patch");
        let inserted = out
            .lines()
            .filter_map(|line| line.trim().strip_prefix("!insertmacro "))
            .map(|rest| rest.split_whitespace().next().expect("macro name"))
            .filter(|name| name.starts_with("BantoHub") || name.contains("BantoHub"))
            .collect::<std::collections::BTreeSet<_>>();
        assert!(!inserted.is_empty());
        for name in inserted {
            assert!(
                hooks.contains(&format!("!macro {name}\n"))
                    || hooks.contains(&format!("!macro {name}\r\n")),
                "フックに !macro {name} が無い"
            );
        }
    }
}
