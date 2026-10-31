# banto-hub 一体インストーラ設計（シェル・Hub・elev・サイドカー同梱）

作成日: 2026-09-07
状態: **I1〜I4 完了（2026-09-07）**。2026-10-31（#973 PR-3a、実機確認は未）: §4.1・§4.3 に、ChronoGazer（#972）にそろえたインストール先の固定（既定の場所の実パスだけ）と、所有者・ACL の既定への戻し（共通部品 `installer-common/install-dir-common.nsh`。オーナーのレビューで、サービスとシェルを止める前の位置に移した。WebView2・`SetOutPath` より前への確かめの移動は PR-3b）を追記。#975 で elev が Program Files の外の `banto-hub.exe` を登録しない前提も §4.1 に追記済み。2026-10-20: §8.6 に alpha.32（migration 0022・設定パッケージ版 2）の実機の DB の複製での確認を追記。2026-10-17（#630 PR-2）: §4.4・§4.3 の POSTINSTALL (4)・決定 5 を、サイドカーの設定の置き場 `%ProgramData%\BantoHub\sink\`（`banto-hub-elev prepare-sink-config` が整える）に改めた（§4.3 の「`.example` から複製」という実装とのずれも直した）。I4（Windows 実機検証、§8）で新規 / 稼働中の上書き / Hub 単体インストーラからの更新 / アンインストールが設計どおり動くことを確認。判明した不具合（シェルがロック保持中の `sc start BantoHub` が `START_PENDING` で固まる）と追従 2 件（サイドカーのサービスログ置き場、デスクトップショートカット抑止）は §8.2 参照。併せて VC++ ランタイム前提を `+crt-static` で排除（決定 11、§4.7、2026-09-07）。v0.2.0-alpha.3 の配布物で実機再確認済み（§8.3）。§8.3 で見つかった停止の二重報告（#330）は v0.2.0-alpha.4 で修正し、実機確認済み（§8.4）。v0.2.0-alpha.24 の配布物で、alpha.7 からの上書きインストールと DB の移行を実機確認済み（§8.5、2026-09-27）。
対象: Windows 向け NSIS インストーラ 1 本で、デスクトップシェル（`banto-hub-shell.exe`）・Hub 本体（`banto-hub.exe`）・UAC ヘルパ（`banto-hub-elev.exe`）・DB Sink サイドカー（`banto-hub-sink.exe`）を同じディレクトリに配置し、サービス登録と権限設定まで行う。

関連: [banto-hub-t17-design.md](banto-hub-t17-design.md)（SCM 管理・profile・UAC・インストーラ再設計。§2.3 に現行インストーラの棚卸し）、[banto-hub-desktop-plan.md](banto-hub-desktop-plan.md) §16.3（配布まわりの未決事項）、[banto-hub-operations.md](banto-hub-operations.md) §12（現行インストーラのビルド手順と挙動）、[banto-hub-external-db-design.md](banto-hub-external-db-design.md) §5（サイドカー）。

---

## 1. 背景と狙い

v0.2.0-alpha.2 の配布物は、NSIS インストーラ（**Hub 本体のみ**）と、シェル・elev・サイドカーの **exe 単体** の組み合わせである。シェルは Hub を in-process で動かせるが、Service モードへの切替やサービス一覧の操作には同じディレクトリの `banto-hub.exe` / `banto-hub-elev.exe` が要り、サイドカーの `grant-service-acl` も同梱の elev を呼ぶ。つまり 4 つの exe は**同じディレクトリに揃っていることが前提**で、単体配布では利用者がそれを手で揃える必要がある。desktop-plan §2 の「PowerShell を意識せず導入」に反する。

狙いは次の 3 点。

- **1 本のインストーラで 4 つの exe を `C:\Program Files\BantoHub\` に揃える。**
- **インストール完了時点で、Hub と Sink の Windows サービスが登録され、`BantoHub Operators` の権限が付いている**（起動はしない。T17-4 の「収集を勝手に始めない」を守る）。
- **上書きインストールが稼働中でも安全に通る**（サービスとシェルを止めてから差し替え、動いていたサービスは戻す）。

## 2. 現状（2026-09-07 調査）

| 項目                       | 現状                                                                                                                                                                                                                            |
| -------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 生成ツール                 | `apps/banto-hub/installer/`（ワークスペース外の xtask 的パッケージ）。`tauri-bundler` をライブラリとして直接呼び、NSIS のみ生成。`PerMachine`、日英、`WebviewInstallMode::Skip`、フックは `hooks/service-hooks.nsh`             |
| 同梱バイナリ               | `banto-hub.exe` の 1 つだけ（`BundleBinary::new("banto-hub", main = true)`）。T17 §2.3 でシェルは「対象外、パッケージング自体は T17 スコープ」と記され、その後も未着手                                                          |
| post-install               | `banto-hub.exe install`（サービス登録のみ。`BantoHub Operators` の作成・サービス ACL・profile ACL は行わない）。T17 は「NSIS からの elev 呼び出し統合は未了」と記録                                                             |
| pre-uninstall              | `banto-hub.exe uninstall`                                                                                                                                                                                                       |
| 上書き時                   | サービスが稼働中だと exe の差し替えに失敗しうる（停止フック無し）。既存サービスの設定は `install` が冪等なので保持される（T17-4）                                                                                               |
| elev の能力                | `service-install`（`banto-hub.exe` のサービス登録 → `setup-operators` → `grant-service-acl` → `grant-profile-acl` を一括）、`service-uninstall`、`grant-service-acl [service]`（#316 でサービス名引数化）                       |
| サイドカー                 | `install` / `uninstall`（冪等、`OnDemand`、LocalSystem）、`grant-service-acl`（同梱 elev を呼ぶ）。設定は **`%ProgramData%\BantoHub\sink\banto-hub-sink.toml` だけ**（#630、2026-10-17。§4.4。当初は exe 隣）                   |
| シェル                     | Tauri v2 の薄いシェル。`frontendDist` は `src-tauri/ui`（静的 1 ページ）。Hub は in-process。`banto-hub-elev.exe` を同ディレクトリで探す。`tauri.conf.json` の `bundle.targets = "all"` だが `cargo tauri build` は使っていない |
| 版数                       | 4 箇所（workspace、package.json、インストーラの `PRODUCT_VERSION`、`tauri.conf.json` は数値のみ）。NSIS は `0.2.0-alpha.2` のプレリリース識別子を受理（alpha.2 で実証）                                                         |
| 未決（desktop-plan §16.3） | コード署名（未署名 NSIS は SmartScreen 警告）、自動更新は初版スコープ外。WebView2 の同梱方式は決定 2（`EmbedBootstrapper`）、VC++ ランタイム前提は決定 11（静的リンクで排除、§4.7）で決着                                       |

## 3. 方式の選択

| 案                                      | 内容                                                                                                                                                                   | 得るもの                                                                                                                         | 代償                                                                                                                                                                                                                   |
| --------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **A. 既存ツールを複数バイナリ化**       | `apps/banto-hub/installer/` の `binaries` を `[shell (main), banto-hub, banto-hub-elev, banto-hub-sink]` にし、WebView2 の導入方式を Skip から変更、フックを置き換える | 既に動いているツール（PerMachine・日英・フック・xtask としての CI 分離）をそのまま使える。src-tauri の設定を配布の都合で汚さない | ツールが 4 つの exe の所在を知る必要がある（`target/release` 既定）                                                                                                                                                    |
| B. `cargo tauri build` でシェルを主体に | `tauri.conf.json` の `bundle.externalBin` に hub / elev / sink を登録し、Tauri CLI に NSIS を作らせる                                                                  | Tauri 標準の流儀。ChronoGazer / relay-wright と同じコマンド                                                                      | externalBin は `name-x86_64-pc-windows-msvc.exe` の命名規則を要求し、ビルド前のリネームが要る。フック・PerMachine・言語設定を `tauri.conf.json` に移し替える。CI の `cargo check --workspace` にバンドル設定が入り込む |

**推奨は A。** 生成ツールは「インストーラの都合をアプリのビルド設定から切り離す」ために作られており（installer/src/main.rs 冒頭）、その方針を保ったまま最小の変更で 4 exe 化できる。B は将来 Tauri の自動更新（updater）を使う段階になれば再検討する。

## 4. インストーラの中身と挙動（案 A）

### 4.1 配置と表示

- インストール先: `C:\Program Files\BantoHub\`（PerMachine 固定、現状どおり）。**固定（2026-10-31、#973 PR-3a、オーナー決定「基本 Hub と同じで」= ChronoGazer の #972 にそろえる）**: PREINSTALL の先頭（ファイルを変える前）で `$INSTDIR` の本当の置き場（`..` を畳み、ジャンクション・シンボリックリンクを解いた実パス）が `$PROGRAMFILES64\BantoHub` と書いたとおりに等しいときだけ通し、違えば案内して `Abort`（ディレクトリの頁・`/D=`・Program Files の中の一般の利用者が書けるサブフォルダ・Program Files の外へのジャンクション・既定の場所かその上がジャンクションのとき。実パスが求まらないときも止める）。通れば `$INSTDIR` を畳んだ形に揃え `SetOutPath` をやり直す。**所有者と ACL の戻し（同日・同 PR、位置は同日のオーナーレビューで前へ）**: PREINSTALL の (0) の直後（サービスとシェルを止める前、レジストリを触る前）で、インストール先そのものと直下の普通のファイルに `icacls /setowner *S-1-5-32-544` と `icacls /reset`（/L 付き・/T なし）を掛け、再解析ポイント・サブフォルダ・icacls の失敗は案内して `Abort`（まだ何も止めていないので戻すものは無い。使用中の exe でも所有者・DACL は書き換えられる: WRITE_OWNER・WRITE_DAC は共有モードの対象外で、実行中イメージの排他は書込み・削除の拒否だけ。戻した後は一般の利用者が直下に手を出せないので、`File` までの間が空いても穴は開かない）。**注記**: 共通部品の中止の案内の「まだ何も変更していません」は Hub では厳密でない（WebView2 の導入と空のインストール先の作成がテンプレートの都合でこの前に済み得る。ファイル・レジストリ・サービスは変えていない）。文言は ChronoGazer と共通なので変えず、確かめを WebView2 と `SetOutPath` より前に出すのはテンプレートの当て直しが要るため PR-3b。部品は ChronoGazer と共通の `installer-common/install-dir-common.nsh`（`RequireDefaultInstallDir`・`ResetInstallDirAcl`。製品固有の文言は include の前の `!define`）。線引き（2026-10-31 オーナー了承）: 管理者の操作が前提でその前提下で攻撃者が同等以上の権限を既に取れる指摘は P3 として #973 に記録し、直さない。
- インストール先: `C:\Program Files\BantoHub\`（PerMachine 固定、現状どおり）。
- elev の `service-install` / `autostart-enable` / `autostart-disable` は、隣の `banto-hub.exe` が Program Files（x86 を含む）の外にあると、何も登録せずに止める（#973。一般の利用者が書き換えられる場所の exe を LocalSystem で登録させない。Program Files の場所が分からないときも止める。比べる前に、`banto-hub.exe` と Program Files の両方を `canonicalize`（長い名前・ジャンクションの解決・`\\?\` を外す）で正規化し、登録するのも正規化後のパス。8.3 や `\\?\` での起動を断らず、ジャンクションで外へ向けた配置は断る）。
- 同梱: `banto-hub-shell.exe`（main）、`banto-hub.exe`、`banto-hub-elev.exe`、`banto-hub-sink.exe`、`banto-hub-sink.toml.example`、ライセンス・README 抜粋。
- スタートメニューに **BantoHub（シェル）** のショートカット。デスクトップショートカットは既定では作らない（§7-3）。**補足（2026-09-07、I4 追従）**: tauri-bundler のテンプレートは silent / passive ではセクション内で無条件に作り、対話モードでは完了ページの「デスクトップにショートカットを作成」チェック（既定 ON、フックより後に実行）で作る。前者は POSTINSTALL で削除し、後者は利用者の選択に委ねる（フックからは介入できない）。
- 完了ページの「インストール後に BantoHub を実行する」は、main がシェルになることで**意味のある動作**になる（現状の Hub 単体では消せない既知の制約だったもの）。
- 製品名 `BantoHub`、ファイル名 `BantoHub_<version>_x64-setup.exe`、識別子 `dev.tyaro.banto-hub`（現状どおり）。

### 4.2 WebView2

シェルは WebView2 が要る。現状の `Skip` は Hub 単体だから許されていた。選択肢は `DownloadBootstrapper`（既定、要ネット）、`EmbedBootstrapper`（+約 2 MB、ランタイム未導入時のみネット）、`OfflineInstaller`（+約 130 MB、完全オフライン）、`FixedRuntime`（固定版同梱）。**推奨は `EmbedBootstrapper`**: Windows 10/11 の工場 PC は Edge 由来の WebView2 が入っていることがほとんどで、無い場合だけ取得に行く。完全オフライン要件が実案件で出た時点で `OfflineInstaller` 版を別途作る（§7-2）。

なお、もう 1 つの実行時前提だった VC++ ランタイムは §4.7 で静的リンクにより排除した（WebView2 と違い、こちらは前提そのものが無くなる）。

### 4.3 フック（NSIS）

| フック       | 処理                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    | 備考                                                                                                                                                                                            |
| ------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| PREINSTALL   | (0) インストール先の固定（§4.1、#973 PR-3a。既定の場所でなければ何も変えずに `Abort`） (1) インストール先の所有者・ACL を既定に戻す（§4.1、#973 PR-3a。サービスとシェルを止める前、レジストリを触る前に行うので、失敗の `Abort` で戻すものは無い） (2) `BantoHubSink` が Running なら停止し、その事実を記録 (3) `BantoHub` が Running なら停止し記録 (4) `banto-hub-shell.exe` が動いていれば終了を促す（single-instance なので `taskkill` で閉じるか、利用者に閉じてもらう案内を出して待つ）                                           | 稼働中の exe は差し替えられないため必須。**上書きインストールの前提条件**を作る                                                                                                                 |
| POSTINSTALL  | (1) `banto-hub-elev.exe service-install`（Hub サービス登録 → `BantoHub Operators` 作成と対話ユーザー追加 → サービス ACL → profile ACL） (2) `banto-hub-sink.exe install` (3) `banto-hub-sink.exe grant-service-acl` (4) `banto-hub-elev.exe prepare-sink-config`（#630: 保護された置き場 `%ProgramData%\BantoHub\sink\` を確かめてから整え、`.example` を毎回差し替える。本物の toml は作らない。以前の記述「toml が無ければ `.example` から複製」は実装（`.example` だけを置く）と違っていた） (5) PREINSTALL で停止したサービスを再開 | (1) は現状の `banto-hub.exe install` を置き換え、T17 の「elev 統合未了」を閉じる。いずれも冪等で、既存の起動種別・自動起動設定は変えない（T17-4）。**新規インストールではサービスを起動しない** |
| PREUNINSTALL | (1) `banto-hub-sink.exe uninstall` (2) `banto-hub-elev.exe service-uninstall`（`banto-hub.exe uninstall` と同等） (3) シェルが動いていれば終了                                                                                                                                                                                                                                                                                                                                                                                          | `%ProgramData%\BantoHub\` と profile（DB・データ・ログ）は**残す**（現状どおり。§7-7）                                                                                                          |

フック内の失敗はインストーラを中断せず、`DetailPrint` で手動手順（operations.md の該当節）を案内する（現状の流儀を踏襲）。例外は PREINSTALL の (0)(4)（#973 PR-3a）: ファイルを置く前に置き場と ACL が駄目なら `Abort` する（置いてからでは一般の利用者がサービスの exe を差し替えられる状態を作ってしまうため）。まだファイルは変えていない。(4) で止めたサービスは `Abort` の前に `sc start` で戻す（Hub のテンプレートには `.onInstFailed` が無い。ChronoGazer と同じ `.onInstFailed` にそろえるのは #973 PR-3b の候補）。なお、テンプレート（当て直しなし）の順は WebView2 → Install なので、(0) で止めても WebView2 だけは入る（ChronoGazer は当て直しで事前の確かめを WebView2 の前に出した。そろえるのも PR-3b）。

### 4.4 サイドカーの設定ファイルの置き場

**2026-10-17 の形（#630、オーナー決定。破壊的変更）**: 置き場は
**`%ProgramData%\BantoHub\sink\banto-hub-sink.toml` だけ**。`sink\` は SYSTEM・
Administrators だけ・所有者 Administrators の保護された DACL で、
`banto-hub-elev.exe prepare-sink-config` が確かめ終えてから作る（あれば整える）。
インストーラの POSTINSTALL (4) はこのアクションを呼ぶだけで、NSIS の
`CreateDirectory`/`CopyFiles`（継承の DACL で作ってしまう）は使わない。`.example` は
置き場の中にインストールのたびに差し替える。サイドカーは置き場とファイルが保護されて
いなければ、旧い置き場（ルート直下・exe 隣）にファイルがあれば起動しない。
LocalSystem のサービスは `BANTO_HUB_SINK_CONFIG` を見ない。手順は
docs/banto-hub-operations.md §12「サイドカーの初回セットアップ」。

初回セットアップの流れ: インストール → シェルから Hub を起動 → API キー画面で
`admin` + `read` のキーを発行 → 管理者のメモ帳で `sink\` の中の `.example` をコピーして
`banto-hub-sink.toml` を作り、キーを書く → `banto-hub-elev.exe prepare-sink-config` →
シェルのサービス一覧から `BantoHubSink` を開始。

**I2（2026-09-07）までの経緯**: 当初は exe 隣（`Program Files` 配下）で、書き込みに
管理者権限が要り、インストーラは API キーを書けなかった。I2 で探索順を
`BANTO_HUB_SINK_CONFIG` → `%ProgramData%\BantoHub\banto-hub-sink.toml` → exe 隣 にし、
インストーラが `%ProgramData%\BantoHub\` を作って `.example` を置く（既存は上書き
しない）形にしたが、ルート直下・exe 隣とも一般の利用者が読めた（ルート直下は
一般の利用者が先に同じ名前のファイルを作ることもできた）ため、#630 で上の形に改めた。

### 4.5 上書きインストールとアンインストール

- 上書き: PREINSTALL で止めたものを POSTINSTALL で戻す。**停止していたサービスは停止のまま**、シェルは再起動しない（「実行する」チェックに委ねる）。既存サービスの起動種別・自動起動は変えない。
- アンインストール: サービス登録を解除し、ファイルを消す。`%ProgramData%\BantoHub\` と profile は残す（再インストールで設定・データが戻る）。完全削除は手順として docs に書く。
- 版数の後戻り（ダウングレード）は想定しない。NSIS は同じ製品の上書きとして扱う。

### 4.6 ビルド手順の一本化

4 つの exe と UI ビルド、インストーラ、SHA256SUMS を 1 スクリプトで作る `scripts/build-release.ps1`（Windows 専用、CI 外）を用意する。手順は alpha.2 で実際に踏んだもの:

```powershell
pnpm --filter banto-hub build
cargo build --release -p banto-hub-host --bin banto-hub --bin banto-hub-elev --features embed-ui
cargo build --release -p banto-hub-shell --features banto-hub-core/embed-ui
cargo build --release -p banto-hub-sink
cargo run --manifest-path apps/banto-hub/installer/Cargo.toml --release
```

`webpki-root-certs` のルート証明書データを含む配布物には、CDLA-Permissive-2.0 の全文をデータと共に提供する。NSIS インストーラは `apps/banto-hub/installer/assets/CDLA-Permissive-2.0.txt` をインストール先に配置し、`scripts/build-release.ps1` は単体 exe と同じ配布ディレクトリへコピーして `SHA256SUMS.txt` に含める。単体 exe を別の経路で配布する場合も、この本文を添付する。証明書データを解析結果として扱って省略しない。

生成ツールは `target/release/` の 4 exe を既定で拾い、無ければ明確なエラーで止める。

### 4.7 VC++ ランタイム（C ランタイムの静的リンク）

WebView2（§4.2）と並ぶもう 1 つの実行時前提が **Microsoft Visual C++ 再頒布可能パッケージ**だった。MSVC ターゲットの Rust バイナリは既定で C ランタイムを動的リンクするため、4 exe すべてが `VCRUNTIME140.dll` を、シェルは加えて `VCRUNTIME140_1.dll`（x64 の C++ 例外処理ランタイム、VS2019 = 14.20 で追加）をインポートしていた（`MSVCP140.dll` はどれも未使用）。redist 未導入の PC ではシェルだけが「`VCRUNTIME140_1.dll` が見つかりません」で起動できない。

**採用: `+crt-static` による静的リンク**（`.cargo/config.toml`、決定 11）。redist を同梱する案は**インストーラ経由の配布しか救えない**のが決め手だった。§7 決定 9 で exe 単体配布を継続すると決めており、そちらにはインストーラが無いため、利用者が redist を自力で導入する手段を持たない。静的リンクは両方の配布経路を一度に解決する。

- 併せて UCRT（`api-ms-win-crt-*.dll`）も静的リンクされるため、Windows 8.1 / 7 での KB2999226（Universal CRT 更新）前提も消える。
- 代償は、vcruntime に更新が入っても Windows Update では配布先に届かず、再ビルド・再配布が要ること。対象は `memcpy` や例外処理レベルの薄い層で、攻撃面は WebView2（OS 更新で維持される）側にあるため許容する。
- 設定はワークスペース全体（テスト・proc macro・chronogazer / relay-wright の src-tauri も含む）に効く。CI の `rust` ジョブは windows-latest で `cargo clippy --workspace --all-targets` と `cargo test --workspace` を回すため、`cargo test --workspace --no-run` がローカルで通ることを確認済み（`sqlx-macros` 等の proc macro dylib もリンク可）。

検証（2026-09-07、この開発 PC）: 4 exe の PE インポートテーブルを実測し、`VCRUNTIME140*` と `api-ms-win-crt-*` がすべて消えたことを確認した。残る `api-ms-win-core-synch-l1-2-0.dll` は OS の API セットで redist とは無関係。サイズ増は 1 exe あたり +110〜145 KB にとどまる。`banto-hub.exe` / `banto-hub-sink.exe` は再ビルド後も使い方表示まで正常に起動する。

## 5. 非スコープ

- コード署名（未署名のため SmartScreen の「発行元不明」は残る。desktop-plan §16.3 の別件）。
- 自動更新（Tauri updater）。手動の上書きインストールのみ。
- MSI 形式、ChronoGazer / relay-wright との統合インストーラ。
- ChronoGazer 等の別アプリとの同居時のポート・サービス名の調停。

## 6. スライス

| slice | 内容                                                                                                                                                                                                              | 完了条件                                                                                                                                                 |
| ----- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------- |
| I1    | 生成ツールの複数バイナリ化（main = シェル）、WebView2 方式、フック置換（PREINSTALL の停止、POSTINSTALL の elev `service-install` / sink `install` / `grant-service-acl` / ProgramData 作成 / 再開、PREUNINSTALL） | ローカルで `BantoHub_<ver>_x64-setup.exe` が生成され、Windows 実機で新規インストール → 4 exe とショートカット、両サービス登録、`sc sdshow` で ACE を確認 |
| I2    | サイドカーの設定探索順（`BANTO_HUB_SINK_CONFIG` → ProgramData → exe 隣）と `.example` の同梱。シェル sink 画面の案内パス更新                                                                                      | 単体テスト（探索順）と、ProgramData の toml でサービス起動                                                                                               |
| I3    | `scripts/build-release.ps1` と docs（operations.md §12 の全面改訂、README の配布物説明）                                                                                                                          | スクリプト 1 回でインストーラと SHA256SUMS が揃う                                                                                                        |
| I4    | 実機検証: 新規インストール / 稼働中の上書き / アンインストール / 再インストールで設定が戻る。S7（外部 DB 検証）と同日に実施                                                                                       | 結果を docs に記録                                                                                                                                       |

## 7. オーナー決定項目（2026-09-07 決定済み: 1〜11 すべて推奨どおり）

| #   | 項目                                 | 推奨                                                                                                                                  |
| --- | ------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------- |
| 1   | 方式（§3）                           | **案 A**（既存ツールの複数バイナリ化）                                                                                                |
| 2   | WebView2 の導入方式（§4.2）          | **`EmbedBootstrapper`**。完全オフライン版は要望が出てから                                                                             |
| 3   | ショートカット                       | スタートメニューのみ。デスクトップには作らない                                                                                        |
| 4   | 上書き時のサービス停止・再開（§4.3） | **自動で停止し、動いていたものだけ再開**                                                                                              |
| 5   | サイドカー設定の置き場（§4.4）       | **`%ProgramData%\BantoHub\sink\banto-hub-sink.toml` だけ**（2026-10-17 #630 で改定。当初は ProgramData 直下を優先、exe 隣は後方互換） |
| 6   | サイドカーのサービス登録（§4.3）     | **インストーラが登録**（起動はしない、Hub と同じ）                                                                                    |
| 7   | アンインストール時のデータ（§4.5）   | **ProgramData と profile は残す**（現状どおり）                                                                                       |
| 8   | ビルドスクリプト（§4.6）             | 作る（`scripts/build-release.ps1`）                                                                                                   |
| 9   | exe 単体配布の継続                   | インストーラと**併記**して続ける（サービス化しない評価用途向け）                                                                      |
| 10  | 着手時期                             | S7（実 DB 検証）と同じ実機セッションで I4 を消化できるよう、I1〜I3 を先に済ませる                                                     |
| 11  | VC++ ランタイム（§4.7）              | **`+crt-static` で静的リンク**。redist の同梱はしない（単体 exe 配布を救えないため）                                                  |

## 8. 実機検証結果（I4、2026-09-07、この開発 PC）

`scripts/build-release.ps1` で生成した `dist/0.2.0-alpha.2/BantoHub_0.2.0-alpha.2_x64-setup.exe`（21.1 MB、main = I1〜I3 マージ後）を、既存の Hub をアンインストールした状態から対話モードで実行した。

### 8.1 結果

| #   | 手順                                                                                                           | 結果                                                                                                                                                                                                                                                                                                                                                                                                                    |
| --- | -------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 1   | 新規インストール（UAC、47 秒）                                                                                 | **合格**。`C:\Program Files\BantoHub\` に 4 exe + `banto-hub-sink.toml.example` + `uninstall.exe`、スタートメニューのショートカット、レジストリ `MainBinaryName = banto-hub-shell.exe`、`BantoHub` / `BantoHubSink` とも登録済み・手動起動・LocalSystem、両サービスに `BantoHub Operators` の ACE（`CCLCRPWP`）。既存の `.example` は上書きされない。完了ページの「実行する」でシェル起動、Hub が 127.0.0.1:8722 で応答 |
| 2   | Operators 権限（非管理者）で `sc start BantoHubSink`                                                           | **合格**。SCM に受理され、設定ファイル未配置のため終了コード 2 で自ら停止。ログに探索した 3 パスが列挙される                                                                                                                                                                                                                                                                                                            |
| 3   | シェル稼働中（ロック保持）に `sc start BantoHub`                                                               | **不具合**。サービスプロセスは起動するが `START_PENDING`（`NOT_STOPPABLE`、checkpoint 0）のまま固まり、シェルを終了してロックが解放されても回復しない。`sc stop` 1052 / `sc start` 1056。管理者で `taskkill /F` して初めて STOPPED（1067）。その後の通常起動は 2 秒で RUNNING                                                                                                                                           |
| 4   | サービス稼働中の上書きインストール（29 秒）                                                                    | **合格**。PREINSTALL で停止 → 差し替え → POSTINSTALL で `BantoHub` だけ再開（新 PID、lock は `service`、openapi 200）。停止していた `BantoHubSink` は停止のまま。ACE・レジストリ・雛形は保持。完了後に起動したシェルはサービス稼働を検知して Desktop Hub を起動しない                                                                                                                                                   |
| 5   | Hub 単体インストーラ（alpha.2）からの更新経路（レジストリの `MainBinaryName` を `banto-hub.exe` に戻して再現） | **合格**。上書き後も `banto-hub.exe` が残り、`MainBinaryName` は `banto-hub-shell.exe` に更新                                                                                                                                                                                                                                                                                                                           |
| 6   | アンインストール（サービス稼働中・シェル起動中）                                                               | **合格**。シェル終了、両サービスの登録解除、exe / ショートカット / レジストリ削除。`%ProgramData%\BantoHub`（profile・雛形）と Operators グループは保持                                                                                                                                                                                                                                                                 |

### 8.2 判明した事項と追従

- **サービス起動の固着（#3）**: `try_acquire_profile_lock` が `AlreadyHeld` のときにサービス本体が SCM へ `Stopped` を報告せずに留まる経路の疑い。修正 PR を別途作成（即時失敗・サービス固有の終了コード・ログ出力）。運用上の回避は「シェルを閉じてから `sc start`」または「シェルの切替操作を使う」。
- **サイドカーのサービスログ置き場**: `banto-hub-sink-service.log` が exe 隣（`Program Files`）に書かれ、アンインストール後にフォルダが残る。`%ProgramData%\BantoHub\logs\` へ移した（追従、`BANTO_HUB_SINK_LOG` で上書き可）。
- **デスクトップショートカット**: 対話インストールで作られたのは完了ページのチェック（既定 ON）による利用者操作で、フックより後に走るため介入できない。silent / passive で無条件に作られる分は POSTINSTALL で削除する（追従、§4.1 補足）。
- 事前に残っていた `profile.lock` の内容は診断用で、実体は名前付きミューテックス。プロセス終了で解放される（表示上の pid が古くても異常ではない）。

### 8.3 alpha.3（C ランタイム静的リンク）の実機確認（2026-09-07、この開発 PC）

`dist/0.2.0-alpha.3/BantoHub_0.2.0-alpha.3_x64-setup.exe`（21.3 MB、§4.7 の `+crt-static` を入れたブランチでビルド）を、I4 のアンインストール直後の状態（サービス未登録・`C:\Program Files\BantoHub` 無し）から対話モードで実行した。

| #   | 手順                                        | 結果                                                                                                                                                                                      |
| --- | ------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 1   | 新規インストール                            | **合格**。4 exe + `banto-hub-sink.toml.example` + `uninstall.exe`、両サービスとも登録済み・`DEMAND_START`・停止、`BantoHub Operators` の ACE（`CCLCRPWP`）                                |
| 2   | シェルの GUI（WebView2 × 静的 CRT）         | **合格**。ウィンドウに Hub の UI が描画され、状態画面のバージョンは `0.2.0-alpha.3`。in-process Hub が `GET /` と `/openapi.json` に 200                                                  |
| 3   | 稼働中プロセスのモジュール実測              | **合格**。シェルの 69 モジュールのうち CRT 系は `C:\Windows\System32` の `ucrtbase.dll` / `msvcp_win.dll`（OS インボックス）のみで、redist の `VCRUNTIME140*` / `MSVCP140.dll` は未ロード |
| 4   | シェル稼働中の `sc start BantoHub`          | **合格**（#327 の修正確認）。I4 の `START_PENDING` 固着は再現せず、即座に失敗して `Stopped`。ログに `profile 'default' は既に別プロセスが使用中です（owner: pid=..., host_kind=shell）`   |
| 5   | シェル終了後のサービス起動                  | **合格**。0.3 秒で `Running`、`/openapi.json` 200                                                                                                                                         |
| 6   | `BantoHubSink` の起動（設定ファイル未配置） | **合格**。自ら停止し、`%ProgramData%\BantoHub\logs\banto-hub-sink-service.log` に探索した 3 パスを記録（I2 の探索順と #328 のログ置き場を同時に確認）                                     |
| 7   | サービス稼働中のアンインストール            | **合格**。両サービスの登録解除、exe / スタートメニュー / インストール先の削除。`%ProgramData%\BantoHub`（profile の DB）と `BantoHub Operators` グループは保持                            |

配布物 4 exe は、通常インポート・**遅延ロードインポート**・実行時ロード用の文字列のいずれにも `vcruntime` / `msvcp` を含まない（NSIS インストーラ本体も同様）。redist 未導入 PC そのものでの確認は、この開発 PC に redist が入っているため未実施だが、ローダが参照する経路が無いことは上記で確定している。

小さな観察: 手順 4 の起動失敗の直後、サービスログに `サービス状態の報告に失敗しました: IO error in winapi call` が残っていた。原因は、`block_on` に渡した async ブロック内の `return` が関数を抜けないため末尾の `report_status(Stopped, Win32(0))` が二重に走ることで、#330 として切り出したうえで **v0.2.0-alpha.4 で修正済み**（`Stopped` を一度報告したら以降の報告を送らないゲート）。修正後は起動失敗時に `Windows サービスを停止しました` のログも出なくなる（起動できていないため）。

### 8.4 alpha.4（#330 の修正）の実機確認（2026-09-08、この開発 PC）

§8.3 の手順 4 で観察した「起動失敗直後の `サービス状態の報告に失敗しました`」を #330 として修正したので、`dist/0.2.0-alpha.4/BantoHub_0.2.0-alpha.4_x64-setup.exe` で新規インストールから確認した。

| #   | 手順                                    | 結果                                                                                                                                                                                                                                               |
| --- | --------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 1   | シェル稼働中に `Start-Service BantoHub` | **合格**。2.1 秒で失敗し `Stopped`。ログは `起動に失敗しました - profile 'default' は既に別プロセスが使用中です（owner: pid=..., host_kind=shell）` の 1 行のみで、`サービス状態の報告に失敗しました` も `Windows サービスを停止しました` も出ない |
| 2   | `sc query BantoHub` の終了コード        | **合格**。`SERVICE_EXIT_CODE = 2`（プロファイルロック保持）、`WIN32_EXIT_CODE = 1066`（`ERROR_SERVICE_SPECIFIC_ERROR`）。修正前は 2 回目の報告（`Win32(0)`）が API エラーになる偶然で 2 が残っていただけで、今はゲートが明示的に止めている         |
| 3   | シェル終了後のサービス起動 → 正常停止   | **合格**（ログ抑止が効きすぎていないことの回帰確認）。0.3 秒で `Running`・`/openapi.json` 200、停止時は `Windows サービスを停止しました` が従来どおり出力され `SERVICE_EXIT_CODE = 0`                                                              |
| 4   | アンインストール                        | **合格**。両サービスの登録解除とインストール先の削除、`%ProgramData%\BantoHub` と `BantoHub Operators` グループは保持                                                                                                                              |

配布物 4 exe の CRT 依存が無いこと（§8.3 と同じ検査）も再ビルド後に確認済み。

### 8.5 alpha.24（alpha.7 からの上書き）の実機確認（2026-09-27、この開発 PC）

`dist/0.2.0-alpha.24/BantoHub_0.2.0-alpha.24_x64-setup.exe`（21.5 MB、タグ `v0.2.0-alpha.24` = `e7d13a4` から `scripts/build-release.ps1` でビルド）を、**alpha.7 がインストール済み**（両サービスとも停止中）の状態から silent モード（`/S`、UAC あり）で実行した。alpha.8〜alpha.23 は公開していないので、利用者が実際に通る「alpha.7 → alpha.24」の更新経路を確かめた。実データの入った `default` プロファイルは、移行の前にバックアップした。

| #   | 手順                                        | 結果                                                                                                                                                                                                                                      |
| --- | ------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 1   | 上書きインストール（silent、8 秒）          | **合格**。4 exe が差し替わり、ハッシュは配布物と一致。レジストリの `DisplayVersion` は `0.2.0-alpha.24`。両サービスの登録（`DEMAND_START`・LocalSystem）、`BantoHub Operators` の ACE（`CCLCRPWP`）、`banto-hub-sink.toml.example` は保持 |
| 2   | サービスの起動                              | **合格**。0.3 秒で `Running`。`/api/status` の `version` は `0.2.0-alpha.24`、`/api/v1/openapi.json` は 200                                                                                                                               |
| 3   | alpha.7 の DB からの移行                    | **合格**。`users` に `auth_epoch`（#204 の追従）が加わり、既存のユーザーと監査ログは保持。`POST /api/audit-log/list?asOfId=`（#428）も 200                                                                                                |
| 4   | 書き込み受付の状態（#340・#433）            | **合格**。DB の値（2026-09-08 に明示的に「有効」へ切り替えたもの）を引き継いで起動。状態ファイル `write-control-state.json` は未作成（無い場合は想定どおりの経路で扱われ、最初の切り替えで作られる）                                      |
| 5   | 実機への接続                                | **合格**。`plc_slmp`・`plc_modbus` が `connected` になり、収集が再開                                                                                                                                                                      |
| 6   | `BantoHubSink` の起動（設定ファイル未配置） | **合格**。自ら停止し、探索した 3 パスをログに記録                                                                                                                                                                                         |
| 7   | CRT 依存                                    | **合格**。配布物の 5 exe（4 exe + `uninstall.exe`）は `vcruntime` / `msvcp` を含まない                                                                                                                                                    |
| 8   | サービスの停止                              | **合格**。`SERVICE_EXIT_CODE = 0`、ログに `Windows サービスを停止しました`                                                                                                                                                                |

補足: 以前の対話モードのインストールで作られていた公開デスクトップのショートカット（`BantoHub.lnk`）は、今回の silent の上書きで削除された（§4.1 補足の設計どおり。スタートメニューのショートカットは残る）。シェルの GUI の描画は、この確認では見ていない。

### 8.6 alpha.32（migration 0022・設定パッケージ版 2）の実機の DB の複製での確認（2026-10-20、この開発 PC）

alpha.32（タグ `v0.2.0-alpha.32` = `4cb6b44`、#718）は DB の形が変わり（banto-tags migration 0022: 収集グループ名を接続内で一意に）、元に戻せない。そこで公開の前に、**alpha.31 で使っている本番の `default` プロファイルの `config\` と `data\` を別フォルダへ複製**し、本番のサービスを止めた状態で、main の `banto-hub.exe`（コンソール、`--features embed-ui`）を `BANTO_DB`・`BANTO_HUB_DATA`・`PORT=8733` で複製に向けて起動して確かめた。本番の DB には触れていない。

| #   | 確かめたこと                                                 | 結果     |
| --- | ------------------------------------------------------------ | -------- |
| 1   | 起動時に migration 0022 が当たり、起動できる                 | **合格** |
| 2   | ログインでき、接続・グループ・タグがそのまま見える           | **合格** |
| 3   | 収集が今までどおり動く                                       | **合格** |
| 4   | 別の接続に既存と同じ名前のグループを作れる                   | **合格** |
| 5   | 新しい画面で設定パッケージ（版 2）を書き出し、取り込み直せる | **合格** |

**踏んだこと**: 最初は画面を建て直さずに `cargo build --features embed-ui` だけで起動したため、手元に残っていた alpha.31 の画面が埋め込まれ、取り込みで「collectionGroups に重複した name 'group1' が含まれています」（版 1 の画面の検査）が出た。`pnpm --filter banto-hub build` で画面を建て直してから `banto-hub` をビルドし直し、ブラウザを Ctrl+Shift+R で読み直すと解消した。`scripts/build-release.ps1` は既定で画面を先に建て直すので、`-SkipFrontend` を付けない通常のリリースビルドでは起きない。

その後、オーナーがタグ `v0.2.0-alpha.32`（= `4cb6b44`）を付けた（2026-10-20）。その後、`scripts/build-release.ps1` で作った配布物 6 点（4 exe・インストーラ・`SHA256SUMS.txt`）を添付した GitHub Release を公開した（2026-10-20 02:21 UTC、https://github.com/tyaro/banto-industrial/releases/tag/v0.2.0-alpha.32 ）。インストーラでの本番の上書き更新は下の「本番の更新」。

**本番の更新（2026-10-20、この開発 PC）**: 本番を alpha.31 から alpha.32 に上げた。

- 1 回目は画面がすぐ消えて開けなかった。`C:\Program Files\BantoHub` には `uninstall.exe`（版 `0.2.0-alpha.2`、当日の日付）と 9/7 の古い `banto-hub.exe` しかなく、`banto-hub-shell.exe`・`banto-hub-elev.exe`・`banto-hub-sink.exe` が無かった。アンインストールの登録も無かった。シェルの exe が無いので、起動しても何も出なかった。
- 原因は、alpha.2 の Release のページの `BantoHub_0.2.0-alpha.2_x64-setup.exe`（7 MB）を使った可能性が高い（アンインストーラの版が alpha.2 だった。全 Release が Pre-release なので、一覧で版を取り違えやすい）。alpha.32 のインストーラ（27 MB）で実行し直した後は正常に動いたので、今回の事象は旧版の取り違えによると考えられる（SHA256 の一致は配布物との同一性を示すもので、不具合がないことの証明ではない）。
- alpha.32 のインストーラを管理者で実行し直すと直った。画面の版は `0.2.0-alpha.32`、既存の 4 接続（calc・mem・plc_slmp・plc_q）は残り、収集も動いた（migration 0022 は本番の DB にもこのとき当たった）。
- 教訓: 配布物は Release の版（ファイル名の版）を確かめてから使う。更新前の DB のバックアップは、この取り違えでも戻せるよう必ず取る。
