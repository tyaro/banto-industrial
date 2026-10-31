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
//! cargo build --release -p chronogazer --features embed-ui,tauri/custom-protocol
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
//! 事前の確かめ（Section WebView2 の先頭、下の P3）: インストール先が既定の場所
//! `$PROGRAMFILES64\ChronoGazer` そのものか（Program Files の中の別のフォルダも不可。
//! #972 のオーナーレビュー（2b9ad63））・登録済みのサービスと前回の場所が `$INSTDIR` か
//! → 旧い利用者単位の版の案内
//! （O-3）→ シェルに `--quit-for-update` で正常終了を頼み、`chronogazer.exe` の
//! プロセスが無くなるまで最大 90 秒待つ（超えたら強制終了せずに再試行／中止を聞く、
//! O-5）。PREINSTALL: シェルの再確認 → サービス `ChronoGazer` が動いていれば止めて
//! 記録 → 止まっていなければ中止。POSTINSTALL: `chronogazer-elev.exe service-install`
//! → 止めたサービスだけ再開し RUNNING まで待つ（どちらも失敗は `Abort`）。
//! `.onInstFailed`（下の P4）: 途中で止まったら止めたサービスを戻す。PREUNINSTALL:
//! シェルの終了（同じ待ち）→ サービスの停止 →（再確認の後に）`service-uninstall`
//! （`%ProgramData%\ChronoGazer` は残す）。サービスの停止・再開の部品は Hub と共通の
//! `installer-common/service-common.nsh` にあり、このツールがフックの先頭へその
//! 絶対パスを `!define` してから tauri-bundler に渡す（[`render_hooks`]）。
//!
//! ## NSIS テンプレートの当て直し（`templates/installer.nsi`、[`TEMPLATE_PATCHES`]）
//!
//! tauri-bundler 2.9.4 の `installer.nsi` には、このインストーラの方針と合わない
//! 箇所が 6 つある（PR #972 のオーナーレビュー P1・P2、Codex の指摘 P3、総点検 P4、
//! 5 回目の P5・P6）:
//!
//! - **P5 `File` の失敗の無視**: 上書きの `File` が使用中で書けないと NSIS は
//!   中止／再試行／無視の窓を出し、「無視」はエラーフラグを立てて続行する。原本は
//!   フラグを見ないので、旧い exe と新しい exe が混ざったまま POSTINSTALL
//!   （`ClearErrors` する）を通って成功と報告される。
//! - **P6 `Delete` の失敗の無視**: Section Uninstall はサービスの登録を解除した
//!   後にファイルを `Delete` するが、失敗（ロック）を見ずに `uninstall.exe` と
//!   「アプリ」の登録まで消す。exe が残っているのにアンインストール済みに見える。
//!
//! - **P3 Section の順**: `Section EarlyChecks` → `Section WebView2`（ランタイムが
//!   無ければ取得して入れる）→ `Section Install`（フック）の順なので、PREINSTALL の
//!   確かめ（インストール先・登録済みのサービスの場所・シェルの終了待ち）は
//!   WebView2 の後になり、駄目でも [キャンセル] でも WebView2 だけ入ってしまう。
//! - **P4 `.onInstFailed`**: テンプレートに無い。`File` の「使用中」で中止すると、
//!   PREINSTALL で止めたサービスが止まったまま残る。
//! - **P1 再インストールの頁**（`PageReinstall`／`PageLeaveReinstall`）: 既に
//!   入っていると「先にアンインストールしてから入れる」が既定の選択で、旧い版の
//!   アンインストーラが走る。そのアンインストーラはサービスを止めて
//!   `service-uninstall` するので、新しい版の `PREINSTALL` にはサービスが「動いて
//!   いない」と見え、`POSTINSTALL` は再開せず、起動種別（自動開始）も手動に戻る。
//! - **P2 `CheckIfAppIsRunning`**（`utils.nsh`。Section Install／Uninstall の
//!   フックの直後）: `chronogazer.exe` が残っていれば silent では問答無用で、UI
//!   では OK の後に `KillProcess` する。フックの待ちの後にシェルが起動された
//!   だけで、「強制終了はしない」（最終 flush を失わない）の約束が破れる。
//!
//! tauri-bundler の `NsisSettings::template`（handlebars の自前テンプレート）で、
//! **2.9.4 の `installer.nsi` をそのまま写した `templates/installer.nsi`** に
//! [`TEMPLATE_PATCHES`]（文字列の完全一致の置き換え。各 1 回だけ一致しなければ
//! エラー）を当てたものを渡す（[`patch_template`]）。写しは byte 単位で同じ
//! （[`VENDORED_TEMPLATE_SHA256`] を試験で確かめる。`Cargo.toml` は `=2.9.4`
//! で固定）。tauri-bundler は自前テンプレートでも `utils.nsh`・
//! `FileAssociation.nsh` を同じ出力先に書くので、`!include` はそのまま通る
//! （`nsis/mod.rs` で確認済み）。
//!
//! 当てる内容（決定は docs/chronogazer-service-design.md §4 の B3-4 PR-C の実装メモ）:
//!
//! 1. P1: `PageReinstall` の先頭で `Abort`（頁を出さない = 旧い版の
//!    アンインストーラを走らせず、**常にその場で上書きする**。テンプレートが
//!    `/UPDATE` のときにしているのと同じ）。`PageLeaveReinstall` も先頭で
//!    `reinst_done` へ飛ばし、`reinst_uninstall` に届かないよう二重に塞ぐ。
//!    これで `PREINSTALL` が動いているサービスを止めて記録し、`POSTINSTALL` の
//!    `service-install` は登録済みなら作り直さない（`banto-service-control`
//!    の `install`。起動種別は保たれる）ので、止めたサービスをそのまま再開できる。
//! 2. P2: Section Install／Uninstall の `CheckIfAppIsRunning` の挿入を、フックの
//!    `QuitShellForUpdate`（再試行／中止だけで強制終了しない。プロセスが無ければ
//!    すぐ返る）の再確認に置き換える。
//! 3. P3: `Section WebView2` の先頭にフックの `ChronoGazerInstallPreflight`
//!    （何も変えずに止められる確かめの全部）を挿入する。
//! 4. P4: `Function .onInstFailed` を足し、フックの `ChronoGazerOnInstFailed`
//!    （止めたままのサービスを戻す）を呼ぶ。
//! 5. P5: インストーラ属性 `AllowSkipFiles off`（`OutFile` の直後。インストーラ
//!    全体に効き、`File` の失敗の窓から「無視」が消え、silent では中止になる）、
//!    `File` の直前に `ResetChronoGazerInstallDirAcl`（インストール先の所有者を
//!    Administrators にして ACL を既定へ戻す。失敗なら `Abort`）と `ClearErrors`、外部バイナリの `File` の直後にフックの
//!    `RequireChronoGazerFilesCopied`（エラーフラグが立っていれば `Abort`。
//!    `.onInstFailed` が止めたサービスを戻す）。
//! 6. P6: Section Uninstall で、再確認の直後・登録の解除の前にフックの
//!    `RequireChronoGazerExesUnlocked`（3 つの exe を `FileOpen ... a` で開けなければ
//!    止めたサービスを戻して `Abort`。まだ何も変更していない）、外部バイナリの
//!    `Delete` の直後・`uninstall.exe` の `Delete` と Uninstall キーの削除の前に
//!    `RequireChronoGazerExesDeleted`（残っていればサービスの登録を
//!    `service-install` で戻して `Abort`）。

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

/// tauri-bundler **2.9.4** の `src/bundle/windows/nsis/installer.nsi` の写し
/// （crates.io の tauri-bundler-2.9.4、2026-10-31 に byte 単位でそのまま写した。
/// 改行は LF）。手を入れずに置き、当て直しは [`TEMPLATE_PATCHES`] で行う。
/// tauri-bundler の版を上げるときは写しを取り直し（`Cargo.toml` の `=2.9.4` も）、
/// [`VENDORED_TEMPLATE_SHA256`] を更新し、各パッチの土台がまだ 1 回だけ一致するかを
/// 試験（`vendored_template_*`・`patch_template_*`）で確かめる。
const VENDORED_TEMPLATE: &str = include_str!("../templates/installer.nsi");

/// [`VENDORED_TEMPLATE`] の SHA-256（`sha256sum installer.nsi` の値）。写しが黙って
/// 変わっていない（= 2.9.4 の原本のまま）ことを試験で確かめる（試験だけが使う）。
#[cfg(test)]
const VENDORED_TEMPLATE_SHA256: &str =
    "20f4ecc730defb71f1342eaeaec4021df13be3d843abba0effe88ea5835fa079";

/// [`VENDORED_TEMPLATE`] に当てる置き換え（`(名前, 土台, 置き換え後)`）。土台は
/// 写しの中に**ちょうど 1 回**現れる文字列でなければならず、違えば
/// [`patch_template`] が名前を挙げて止まる（tauri-bundler の版を上げて土台が
/// 変わったときに黙って当たらないままにしない）。順に当てる。置き換え後の文字列は
/// NSIS のソースなので LF 改行・2 空白のインデント（原本に合わせる）。
///
/// 置き換え後で挿入する `QuitShellForUpdate` は `hooks/service-hooks.nsh` の
/// マクロ（テンプレートはフックを先頭近くで `!include` するので、Section
/// Install／Uninstall の時点で定義済み）。引数の意味はフック側を参照。
const TEMPLATE_PATCHES: &[(&str, &str, &str)] = &[
    (
        // P1: 再インストールの頁を出さない。頁の関数の `Abort` は「この頁を
        // 飛ばす」（テンプレート自身が「入っていなければ飛ばす」に使っている形）。
        // 頁が無いので `PageLeaveReinstall`（旧い版のアンインストーラの起動）は
        // 呼ばれず、常にその場で上書きする。silent はもともと頁の関数を呼ばない
        // ので、silent と UI の挙動が揃う。WiX（msi）版の検出も頁の中にあるが、
        // ChronoGazer に WiX 版は無い。
        "P1-reinstall-page",
        "Function PageReinstall\n",
        "Function PageReinstall\n\
         \x20 ; chronogazer-installer (#972 P1): 旧い版のアンインストーラを走らせず、\n\
         \x20 ; 常にその場で上書きするので、この頁は出さない（Abort = 頁を飛ばす）。\n\
         \x20 Abort\n",
    ),
    (
        // P1 の二重の塞ぎ: 万一 leave が呼ばれても `reinst_uninstall` には進まない
        // （`/UPDATE` のときの `Goto reinst_done` と同じ道）。
        "P1-reinstall-leave",
        "Function PageLeaveReinstall\n",
        "Function PageLeaveReinstall\n\
         \x20 ; chronogazer-installer (#972 P1): 常にその場で上書き（/UPDATE と同じ道）。\n\
         \x20 Goto reinst_done\n",
    ),
    (
        // P2（Install）: 強制終了する `CheckIfAppIsRunning` の代わりに、フックと
        // 同じ「頼んで待つ・再試行／中止」で再確認する。[キャンセル] なら
        // PREINSTALL で止めたサービスを戻してから中止（ファイルはまだ触っていない）。
        "P2-install-check",
        "    !insertmacro NSIS_HOOK_PREINSTALL\n\
         \x20 !endif\n\
         \n\
         \x20 !insertmacro CheckIfAppIsRunning \"${MAINBINARYNAME}.exe\" \"${PRODUCTNAME}\"\n",
        "    !insertmacro NSIS_HOOK_PREINSTALL\n\
         \x20 !endif\n\
         \n\
         \x20 ; chronogazer-installer (#972 P2): tauri-bundler の CheckIfAppIsRunning（残って\n\
         \x20 ; いれば KillProcess）の代わりに、フックと同じ強制終了しない再確認。\n\
         \x20 !insertmacro QuitShellForUpdate \"インストール\" 1 \"まだファイルは変更していません。上書き前に止めたサービスは再開します\"\n",
    ),
    (
        // P2（Uninstall）: 同上。PREUNINSTALL はサービスを止めるだけにして、登録の
        // 解除（UnregisterChronoGazerService）をこの再確認の後に置く。[キャンセル]
        // は止めたサービスを戻して中止（まだ何も変更していない）。解除に失敗したら
        // マクロ側で `Abort` し、ファイルは消えない（#972 のオーナーレビュー）。
        "P2-uninstall-check",
        "    !insertmacro NSIS_HOOK_PREUNINSTALL\n\
         \x20 !endif\n\
         \n\
         \x20 !insertmacro CheckIfAppIsRunning \"${MAINBINARYNAME}.exe\" \"${PRODUCTNAME}\"\n",
        "    !insertmacro NSIS_HOOK_PREUNINSTALL\n\
         \x20 !endif\n\
         \n\
         \x20 ; chronogazer-installer (#972 P2): tauri-bundler の CheckIfAppIsRunning（残って\n\
         \x20 ; いれば KillProcess）の代わりに、フックと同じ強制終了しない再確認。\n\
         \x20 !insertmacro QuitShellForUpdate \"アンインストール\" 1 \"止めたサービスは再開します。まだ何も変更していません\"\n\
         \x20 ; chronogazer-installer (#972 P6): 使用中の exe があれば、登録を解除する前に\n\
         \x20 ; 止めたサービスを戻して中止（まだ何も変更していない）。\n\
         \x20 !insertmacro RequireChronoGazerExesUnlocked\n\
         \x20 !insertmacro UnregisterChronoGazerService\n",
    ),
    (
        // P5（属性）: `File` の失敗の窓から「無視」を消す。インストーラ属性なので
        // どの Section にも効く（silent では中止 = `Abort` = `.onInstFailed`）。
        "P5-allow-skip-files-off",
        "OutFile \"${OUTFILE}\"\n",
        "OutFile \"${OUTFILE}\"\n\
         ; chronogazer-installer (#972 P5): 使用中で書けない File を「無視」で飛ばせない\n\
         ; ようにする（再試行／中止だけ。silent では中止）。\n\
         AllowSkipFiles off\n",
    ),
    (
        // P5（Install、前）: 上書きの前にエラーフラグを落とす（直前の再確認や
        // `ReadRegStr` の失敗で立っていることがある）。
        "P5-clear-errors-before-file",
        "  ; Copy main executable\n\
         \x20 File \"${MAINBINARYSRCPATH}\"\n",
        "  ; chronogazer-installer (#972 のオーナーレビュー（2b9ad63）): 既存のインストール先の\n\
         \x20 ; 所有者と ACL を、ファイルを置く前に既定へ戻す（弱い ACL のフォルダに置かない）。\n\
         \x20 ; ClearErrors の前に置く（エラーフラグを乱さない）。\n\
         \x20 !insertmacro ResetChronoGazerInstallDirAcl\n\
         \n\
         \x20 ; chronogazer-installer (#972 P5): File の失敗だけを見るため、直前で落とす。\n\
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
         \x20 ; chronogazer-installer (#972 P5): 上書きに失敗したものがあれば（エラーフラグ）\n\
         \x20 ; 混ざったまま成功にせず中止する（.onInstFailed が止めたサービスを戻す）。\n\
         \x20 !insertmacro RequireChronoGazerFilesCopied\n",
    ),
    (
        // P6（Uninstall、後）: 3 つの exe の `Delete` の直後に残っていないかを確かめ、
        // 残っていれば `uninstall.exe` の `Delete` と Uninstall キーの削除より前に
        // `Abort`（フック側でサービスの登録を戻す）。
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
         \x20 ; chronogazer-installer (#972 P6): exe が残っていれば、uninstall.exe と「アプリ」の\n\
         \x20 ; 登録を消す前に止める（サービスの登録は elev が残っていれば戻す）。\n\
         \x20 !insertmacro RequireChronoGazerExesDeleted\n",
    ),
    (
        // P3: 事前の確かめを WebView2 の取得より前に置く（#972 の Codex の指摘:
        // インストール先が駄目でも、シェルの終了待ちで [キャンセル] しても、
        // WebView2 だけ入ってしまう）。Section は EarlyChecks → WebView2 → Install の
        // 順に全部走るので、WebView2 の先頭 = Install より前。ここでの `Abort` は
        // Section WebView2 ごと止める（Install には進まない）。
        "P3-preflight-before-webview2",
        "Section WebView2\n",
        "Section WebView2\n\
         \x20 ; chronogazer-installer (#972 P3): WebView2 を入れる前に、何も変えずに\n\
         \x20 ; 止められる確かめ（インストール先・サービスの場所・シェルの終了待ち）。\n\
         \x20 !insertmacro ChronoGazerInstallPreflight\n",
    ),
    (
        // P4: `.onInstFailed`（Section Install が `Abort` した・`File` の「使用中」で
        // 中止した後に呼ばれる。silent でも呼ばれる）で、PREINSTALL で止めて
        // まだ戻していないサービスを戻す。フックの ChronoGazerOnInstFailed を参照。
        "P4-oninstfailed",
        "Function .onInstSuccess\n",
        "Function .onInstFailed\n\
         \x20 ; chronogazer-installer (#972 P4): 途中で止まったら、上書き前に止めた\n\
         \x20 ; サービスを戻す（フックの ChronoGazerOnInstFailed）。\n\
         \x20 !insertmacro ChronoGazerOnInstFailed\n\
         FunctionEnd\n\
         \n\
         Function .onInstSuccess\n",
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
             \u{20}\u{20}pnpm --filter chronogazer build\n\
             \u{20}\u{20}cargo build --release -p chronogazer --features embed-ui,tauri/custom-protocol\n\
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

    // 当て直した NSIS テンプレート（冒頭の「NSIS テンプレートの当て直し」）。
    // tauri-bundler は handlebars で描画した結果を出力先の installer.nsi に書くので、
    // 生成物を確かめるときはそちら（<対象ディレクトリ>/bundle/nsis/installer.nsi）を見る。
    let installer_template = hooks_work_dir.join("installer.nsi");
    std::fs::write(&installer_template, patch_template(VENDORED_TEMPLATE)?)
        .context("テンプレートの書き出しに失敗しました")?;

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
                template: Some(installer_template),
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

/// [`TEMPLATE_PATCHES`] を順に当てる。土台が 1 回も無い・2 回以上ある、の
/// どちらも名前を挙げたエラー（tauri-bundler の版を上げて原本が変わったときの
/// 合図）。写しそのものが原本と違うかは試験（`vendored_template_*`）で見る。
fn patch_template(template: &str) -> Result<String> {
    let mut out = template.to_string();
    for (name, anchor, replacement) in TEMPLATE_PATCHES {
        match out.matches(anchor).count() {
            1 => out = out.replacen(anchor, replacement, 1),
            n => bail!(
                "NSIS テンプレートの当て直し {name} の土台が {n} 回一致しました（ちょうど 1 回のはず）。\
                 tauri-bundler の installer.nsi が変わっていないか、templates/installer.nsi の写しと \
                 TEMPLATE_PATCHES を見直してください"
            ),
        }
    }
    Ok(out)
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

    /// 写しは tauri-bundler 2.9.4 の原本のまま（黙って手を入れない。当て直しは
    /// `TEMPLATE_PATCHES` で）。版を上げたら写しを取り直してこの定数も更新する。
    #[test]
    fn vendored_template_is_the_unmodified_2_9_4_original() {
        use sha2::{Digest, Sha256};
        let digest = hex::encode(Sha256::digest(VENDORED_TEMPLATE.as_bytes()));
        assert_eq!(digest, VENDORED_TEMPLATE_SHA256);
        // 原本は LF。土台の文字列も LF で書いてあるので、CRLF に変わると当たらない。
        assert!(!VENDORED_TEMPLATE.contains('\r'));
    }

    /// 各パッチの土台が写しにちょうど 1 回ある（版を上げて原本が変わったら
    /// ここで気づく）。置き換え後の文字列は土台と違う。
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

    /// 当て直した結果: P2 - Section Install／Uninstall の `CheckIfAppIsRunning` の
    /// 挿入が無く、フックの `QuitShellForUpdate` の再確認に置き換わっている。
    /// P1 - 再インストールの頁の関数が先頭で `Abort`、leave が先頭で
    /// `reinst_done` へ飛ぶ。フックの `!include` は Section より前にある。
    #[test]
    fn patch_template_applies_p1_and_p2() {
        let out = patch_template(VENDORED_TEMPLATE).expect("patch");

        // P2: KillProcess に進む挿入が残っていない（utils.nsh のマクロ定義はそのまま
        // include されるが、挿入が無ければ呼ばれない）。
        assert!(!out.contains("!insertmacro CheckIfAppIsRunning"));
        assert_eq!(out.matches("!insertmacro QuitShellForUpdate").count(), 2);
        let install = out.find("Section Install\n").expect("Section Install");
        let uninstall = out.find("Section Uninstall\n").expect("Section Uninstall");
        let install_body = &out[install..uninstall];
        assert!(install_body.contains("!insertmacro NSIS_HOOK_PREINSTALL"));
        assert!(install_body.contains("!insertmacro QuitShellForUpdate \"インストール\" 1 "));
        let uninstall_body = &out[uninstall..];
        assert!(uninstall_body.contains("!insertmacro QuitShellForUpdate \"アンインストール\" 1 "));
        // 登録の解除は再確認の後（フックの後でも、ファイルを消す前）。
        let un_hook = uninstall_body
            .find("!insertmacro NSIS_HOOK_PREUNINSTALL")
            .expect("un hook");
        let un_recheck = uninstall_body
            .find("!insertmacro QuitShellForUpdate")
            .expect("un recheck");
        let un_unreg = uninstall_body
            .find("!insertmacro UnregisterChronoGazerService")
            .expect("unregister");
        // 使用中の exe の確かめ（P6）は再確認の後・解除の前。
        let un_unlocked = uninstall_body
            .find("!insertmacro RequireChronoGazerExesUnlocked")
            .expect("unlocked");
        assert!(un_hook < un_recheck && un_recheck < un_unlocked && un_unlocked < un_unreg);
        // 再確認はフックの直後（テンプレートの挿入位置と同じ）。
        let hook = install_body
            .find("!insertmacro NSIS_HOOK_PREINSTALL")
            .expect("hook");
        let recheck = install_body
            .find("!insertmacro QuitShellForUpdate")
            .expect("recheck");
        assert!(hook < recheck);
        // フックはテンプレートの先頭近くで include されるので、マクロは Section の
        // 時点で定義済み。
        let include = out
            .find("!include \"{{installer_hooks}}\"")
            .expect("include");
        assert!(include < install);

        // P1: 頁の関数は最初の命令が Abort、leave は最初の命令が Goto reinst_done。
        fn first_instruction<'a>(text: &'a str, function: &str) -> &'a str {
            let start = text.find(function).expect("function") + function.len();
            text[start..]
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty() && !line.starts_with(';'))
                .expect("instruction")
        }
        assert_eq!(first_instruction(&out, "Function PageReinstall\n"), "Abort");
        assert_eq!(
            first_instruction(&out, "Function PageLeaveReinstall\n"),
            "Goto reinst_done"
        );
        // 原本の「入っていなければ飛ばす」と `reinst_uninstall` のラベル自体は
        // 残る（届かないだけ）。
        assert!(out.contains("reinst_uninstall:"));
        assert!(out.contains("reinst_done:"));
    }

    /// 当て直した結果: P3 - 事前の確かめ（`ChronoGazerInstallPreflight`）が
    /// `Section WebView2` の最初の命令で、WebView2 の取得（`webview2`）より前・
    /// `Section Install` より前にある。フックの `!include` はその前。P4 -
    /// `.onInstFailed` が 1 つだけあり、`ChronoGazerOnInstFailed` を呼ぶ。
    #[test]
    fn patch_template_applies_p3_and_p4() {
        let out = patch_template(VENDORED_TEMPLATE).expect("patch");

        let include = out
            .find("!include \"{{installer_hooks}}\"")
            .expect("include");
        let webview2 = out.find("Section WebView2\n").expect("Section WebView2");
        let install = out.find("Section Install\n").expect("Section Install");
        let preflight = out
            .find("!insertmacro ChronoGazerInstallPreflight")
            .expect("preflight");
        assert_eq!(
            out.matches("!insertmacro ChronoGazerInstallPreflight")
                .count(),
            1
        );
        assert!(include < webview2 && webview2 < preflight && preflight < install);
        // WebView2 の取得（ExecWait や download）より前。
        let webview2_body = &out[webview2..install];
        let first_exec = webview2_body.find("ExecWait").expect("webview2 ExecWait");
        assert!(preflight - webview2 < first_exec);
        // Section WebView2 の最初の命令（コメント以外）がこの挿入。
        let first = out[webview2 + "Section WebView2\n".len()..]
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty() && !line.starts_with(';'))
            .expect("instruction");
        assert_eq!(first, "!insertmacro ChronoGazerInstallPreflight");

        // P4: .onInstFailed が 1 つ、フックのマクロを呼び、.onInstSuccess は残る。
        assert_eq!(out.matches("Function .onInstFailed\n").count(), 1);
        let failed = out.find("Function .onInstFailed\n").expect("onInstFailed");
        let failed_end = out[failed..].find("FunctionEnd").expect("FunctionEnd") + failed;
        assert!(out[failed..failed_end].contains("!insertmacro ChronoGazerOnInstFailed"));
        assert_eq!(out.matches("Function .onInstSuccess\n").count(), 1);
        assert!(
            failed
                < out
                    .find("Function .onInstSuccess\n")
                    .expect("onInstSuccess")
        );
    }

    /// 当て直した結果: P5 - `AllowSkipFiles off` が 1 つ、Section より前にある。
    /// Section Install では再確認 → `ClearErrors` → main の `File` → 外部バイナリの
    /// `File` → `RequireChronoGazerFilesCopied` → `WriteUninstaller` → POSTINSTALL
    /// の順。P6 - Section Uninstall では再確認 → `RequireChronoGazerExesUnlocked`
    /// → 解除 → main の `Delete` → 外部バイナリの `Delete` →
    /// `RequireChronoGazerExesDeleted` → `uninstall.exe` の `Delete` → Uninstall
    /// キーの削除の順。
    #[test]
    fn patch_template_applies_p5_and_p6() {
        let out = patch_template(VENDORED_TEMPLATE).expect("patch");

        fn pos(text: &str, needle: &str) -> usize {
            assert_eq!(text.matches(needle).count(), 1, "{needle} が 1 回ではない");
            text.find(needle).expect("needle")
        }

        // P5: 属性は 1 つ、最初の Section より前（インストーラ属性は Section の外）。
        let allow = pos(&out, "AllowSkipFiles off\n");
        let early = pos(&out, "Section EarlyChecks\n");
        assert!(allow < early);

        let install = pos(&out, "Section Install\n");
        let uninstall = pos(&out, "Section Uninstall\n");
        let body = &out[install..uninstall];
        let recheck = pos(body, "!insertmacro QuitShellForUpdate \"インストール\" 1 ");
        let clear = pos(body, "  ClearErrors\n");
        let acl = pos(body, "!insertmacro ResetChronoGazerInstallDirAcl");
        assert!(recheck < acl && acl < clear);
        let main_file = pos(body, "File \"${MAINBINARYSRCPATH}\"");
        let bin_file = pos(body, "File /a \"/oname={{this}}\"");
        let copied = pos(body, "!insertmacro RequireChronoGazerFilesCopied");
        let write_uninst = pos(body, "WriteUninstaller");
        let post = pos(body, "!insertmacro NSIS_HOOK_POSTINSTALL");
        assert!(recheck < clear && clear < main_file && main_file < bin_file);
        assert!(bin_file < copied && copied < write_uninst && write_uninst < post);
        // `ClearErrors` と main の `File` の間に命令が無い（再確認のマクロの中の
        // 命令で立ったフラグを、落とした後に見る）。
        let between = &body[clear + "  ClearErrors\n".len()..main_file];
        assert!(between
            .lines()
            .map(str::trim)
            .all(|line| line.is_empty() || line.starts_with(';')));

        // P6: 解除の前の確かめ、Delete の後の確かめ、の順。
        let un = &out[uninstall..];
        let un_recheck = pos(
            un,
            "!insertmacro QuitShellForUpdate \"アンインストール\" 1 ",
        );
        let unlocked = pos(un, "!insertmacro RequireChronoGazerExesUnlocked");
        let unreg = pos(un, "!insertmacro UnregisterChronoGazerService");
        let main_del = pos(un, "Delete \"$INSTDIR\\${MAINBINARYNAME}.exe\"");
        let bin_del = pos(un, "Delete \"$INSTDIR\\\\{{this}}\"");
        let deleted = pos(un, "!insertmacro RequireChronoGazerExesDeleted");
        let uninst_del = pos(un, "Delete \"$INSTDIR\\uninstall.exe\"");
        let key_del = pos(un, "DeleteRegKey HKLM \"${UNINSTKEY}\"");
        assert!(un_recheck < unlocked && unlocked < unreg && unreg < main_del);
        assert!(main_del < bin_del && bin_del < deleted);
        assert!(deleted < uninst_del && uninst_del < key_del);
    }

    /// 土台が 1 回だけでなければ名前を挙げて止まる（0 回・2 回の両方）。
    #[test]
    fn patch_template_rejects_a_template_whose_anchor_count_is_not_one() {
        let (name, anchor, _) = TEMPLATE_PATCHES[0];
        let missing = VENDORED_TEMPLATE.replacen(anchor, "", 1);
        let err = patch_template(&missing).expect_err("0 回");
        assert!(err.to_string().contains(name), "{err}");
        assert!(err.to_string().contains("0 回"), "{err}");

        let doubled = format!("{VENDORED_TEMPLATE}\n{anchor}");
        let err = patch_template(&doubled).expect_err("2 回");
        assert!(err.to_string().contains("2 回"), "{err}");
    }
}
