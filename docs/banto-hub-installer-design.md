# banto-hub 一体インストーラ設計（シェル・Hub・elev・サイドカー同梱）

作成日: 2026-09-07
状態: **I1〜I4 完了（2026-09-07）**。I4（Windows 実機検証、§8）で新規 / 稼働中の上書き / Hub 単体インストーラからの更新 / アンインストールが設計どおり動くことを確認。判明した不具合（シェルがロック保持中の `sc start BantoHub` が `START_PENDING` で固まる）と追従 2 件（サイドカーのサービスログ置き場、デスクトップショートカット抑止）は §8.2 参照。併せて VC++ ランタイム前提を `+crt-static` で排除（決定 11、§4.7、2026-09-07）。v0.2.0-alpha.3 の配布物で実機再確認済み（§8.3）。§8.3 で見つかった停止の二重報告（#330）は v0.2.0-alpha.4 で修正し、実機確認済み（§8.4）。
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
| サイドカー                 | `install` / `uninstall`（冪等、`OnDemand`、LocalSystem）、`grant-service-acl`（同梱 elev を呼ぶ）。設定は **exe 隣の `banto-hub-sink.toml`**（`BANTO_HUB_SINK_CONFIG` で上書き可）                                              |
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

- インストール先: `C:\Program Files\BantoHub\`（PerMachine 固定、現状どおり）。
- 同梱: `banto-hub-shell.exe`（main）、`banto-hub.exe`、`banto-hub-elev.exe`、`banto-hub-sink.exe`、`banto-hub-sink.toml.example`、ライセンス・README 抜粋。
- スタートメニューに **BantoHub（シェル）** のショートカット。デスクトップショートカットは既定では作らない（§7-3）。**補足（2026-09-07、I4 追従）**: tauri-bundler のテンプレートは silent / passive ではセクション内で無条件に作り、対話モードでは完了ページの「デスクトップにショートカットを作成」チェック（既定 ON、フックより後に実行）で作る。前者は POSTINSTALL で削除し、後者は利用者の選択に委ねる（フックからは介入できない）。
- 完了ページの「インストール後に BantoHub を実行する」は、main がシェルになることで**意味のある動作**になる（現状の Hub 単体では消せない既知の制約だったもの）。
- 製品名 `BantoHub`、ファイル名 `BantoHub_<version>_x64-setup.exe`、識別子 `dev.tyaro.banto-hub`（現状どおり）。

### 4.2 WebView2

シェルは WebView2 が要る。現状の `Skip` は Hub 単体だから許されていた。選択肢は `DownloadBootstrapper`（既定、要ネット）、`EmbedBootstrapper`（+約 2 MB、ランタイム未導入時のみネット）、`OfflineInstaller`（+約 130 MB、完全オフライン）、`FixedRuntime`（固定版同梱）。**推奨は `EmbedBootstrapper`**: Windows 10/11 の工場 PC は Edge 由来の WebView2 が入っていることがほとんどで、無い場合だけ取得に行く。完全オフライン要件が実案件で出た時点で `OfflineInstaller` 版を別途作る（§7-2）。

なお、もう 1 つの実行時前提だった VC++ ランタイムは §4.7 で静的リンクにより排除した（WebView2 と違い、こちらは前提そのものが無くなる）。

### 4.3 フック（NSIS）

| フック       | 処理                                                                                                                                                                                                                                                                                                                                                     | 備考                                                                                                                                                                                            |
| ------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| PREINSTALL   | (1) `BantoHubSink` が Running なら停止し、その事実を記録 (2) `BantoHub` が Running なら停止し記録 (3) `banto-hub-shell.exe` が動いていれば終了を促す（single-instance なので `taskkill` で閉じるか、利用者に閉じてもらう案内を出して待つ）                                                                                                               | 稼働中の exe は差し替えられないため必須。**上書きインストールの前提条件**を作る                                                                                                                 |
| POSTINSTALL  | (1) `banto-hub-elev.exe service-install`（Hub サービス登録 → `BantoHub Operators` 作成と対話ユーザー追加 → サービス ACL → profile ACL） (2) `banto-hub-sink.exe install` (3) `banto-hub-sink.exe grant-service-acl` (4) `%ProgramData%\BantoHub\` を作成し、`banto-hub-sink.toml` が無ければ `.example` から複製 (5) PREINSTALL で停止したサービスを再開 | (1) は現状の `banto-hub.exe install` を置き換え、T17 の「elev 統合未了」を閉じる。いずれも冪等で、既存の起動種別・自動起動設定は変えない（T17-4）。**新規インストールではサービスを起動しない** |
| PREUNINSTALL | (1) `banto-hub-sink.exe uninstall` (2) `banto-hub-elev.exe service-uninstall`（`banto-hub.exe uninstall` と同等） (3) シェルが動いていれば終了                                                                                                                                                                                                           | `%ProgramData%\BantoHub\` と profile（DB・データ・ログ）は**残す**（現状どおり。§7-7）                                                                                                          |

フック内の失敗はインストーラを中断せず、`DetailPrint` で手動手順（operations.md の該当節）を案内する（現状の流儀を踏襲）。

### 4.4 サイドカーの設定ファイルの置き場

現状は exe 隣（`Program Files` 配下）で、書き込みに管理者権限が要る。API キーは Hub を起動してから発行するので、インストーラは書けない。次を提案する。

- サイドカーの設定探索を **`BANTO_HUB_SINK_CONFIG` → `%ProgramData%\BantoHub\banto-hub-sink.toml` → exe 隣** の順にする（小さなコード変更、§6 の I2）。
- インストーラは `%ProgramData%\BantoHub\` を作り、`banto-hub-sink.toml` が無ければ `.example`（`hub_url` と空の `api_key`、ループバック注意のコメント）を置く。**既存ファイルは上書きしない。**
- シェルの sink 画面（S6）の設定スニペットは、このパスを案内する。

初回セットアップの流れ: インストール → シェルから Hub を起動 → API キー画面で `admin` + `read` のキーを発行 → `%ProgramData%\BantoHub\banto-hub-sink.toml` に書く → シェルのサービス一覧から `BantoHubSink` を開始。

### 4.5 上書きインストールとアンインストール

- 上書き: PREINSTALL で止めたものを POSTINSTALL で戻す。**停止していたサービスは停止のまま**、シェルは再起動しない（「実行する」チェックに委ねる）。既存サービスの起動種別・自動起動は変えない。
- アンインストール: サービス登録を解除し、ファイルを消す。`%ProgramData%\BantoHub\` と profile は残す（再インストールで設定・データが戻る）。完全削除は手順として docs に書く。
- 版数の後戻り（ダウングレード）は想定しない。NSIS は同じ製品の上書きとして扱う。

### 4.6 ビルド手順の一本化

4 つの exe と UI ビルド、インストーラ、SHA256SUMS を 1 スクリプトで作る `scripts/build-release.ps1`（Windows 専用、CI 外）を用意する。手順は alpha.2 で実際に踏んだもの:

```powershell
pnpm --filter banto-hub build
cargo build --release -p banto-hub-core --bin banto-hub --bin banto-hub-elev --features embed-ui
cargo build --release -p banto-hub-shell --features banto-hub-core/embed-ui
cargo build --release -p banto-hub-sink
cargo run --manifest-path apps/banto-hub/installer/Cargo.toml --release
```

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

| #   | 項目                                 | 推奨                                                                                 |
| --- | ------------------------------------ | ------------------------------------------------------------------------------------ |
| 1   | 方式（§3）                           | **案 A**（既存ツールの複数バイナリ化）                                               |
| 2   | WebView2 の導入方式（§4.2）          | **`EmbedBootstrapper`**。完全オフライン版は要望が出てから                            |
| 3   | ショートカット                       | スタートメニューのみ。デスクトップには作らない                                       |
| 4   | 上書き時のサービス停止・再開（§4.3） | **自動で停止し、動いていたものだけ再開**                                             |
| 5   | サイドカー設定の置き場（§4.4）       | **`%ProgramData%\BantoHub\banto-hub-sink.toml`** を優先、exe 隣は後方互換            |
| 6   | サイドカーのサービス登録（§4.3）     | **インストーラが登録**（起動はしない、Hub と同じ）                                   |
| 7   | アンインストール時のデータ（§4.5）   | **ProgramData と profile は残す**（現状どおり）                                      |
| 8   | ビルドスクリプト（§4.6）             | 作る（`scripts/build-release.ps1`）                                                  |
| 9   | exe 単体配布の継続                   | インストーラと**併記**して続ける（サービス化しない評価用途向け）                     |
| 10  | 着手時期                             | S7（実 DB 検証）と同じ実機セッションで I4 を消化できるよう、I1〜I3 を先に済ませる    |
| 11  | VC++ ランタイム（§4.7）              | **`+crt-static` で静的リンク**。redist の同梱はしない（単体 exe 配布を救えないため） |

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
