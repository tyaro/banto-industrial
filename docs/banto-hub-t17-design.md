# banto-hub T17 詳細設計（SCM 管理・profile・UAC・インストーラ再設計）

作成日: 2026-08-10
状態: **設計確定。主要判断 P1〜P6 は 2026-08-10 オーナー承認済み。2026-10-18: §1 の末尾に「設定モード ⇔ 運用モード」のオーナー決定（#623・#661。T17-3 の切り替えのときにサービス自身が ACL を付け替える）を記録。
2026-10-17: §12 の #626 の決定 11（#630 の分離）に、#630 の結果（サイドカーの設定の
置き場を保護された `sink\` に分けた）を 1 行足した。
T17-0（SCM 状態取得＋start/stop/restart/autostart API 抽出、
`apps/banto-hub/core/src/service_manager.rs`）は同日実装済み（§7）。
T17-1（profile path 一本化＋mutex/排他、`profile_paths.rs`/
`profile_lock.rs`）も同日実装済み（§8）。T17-3（Desktop↔Service 切替
トランザクション、`hub_health.rs`/`host_switch.rs`）も同日実装済み
（§9）。T17-2（UAC helper / Operators / サービス ACL、§10）は
2026-08-10 にスライス1〜2を実装済み（`service_operators.rs`・
`service_elevated.rs`・`service_install.rs`・`banto-hub-elev.exe`）。
T17-4（P4「Demand 化」、§11）も同日実装済み
（`service_install.rs::install`の既定起動種別を`OnDemand`へ変更）。
2026-08-10 Windows 実機で T17-2/T17-4 の主要チェックを実施済み
（§10・§11。Demand 登録・OS 再起動後 Stopped・手動 Start・既存保持・
Operators 作成・サービス ACL・`setup-operators` 冪等・UAC プロンプト・
非管理者 Operators 委任）。**2026-08-10: T16-2 第一スライス実装済み**
（`apps/banto-hub/src-tauri/src/lib.rs` にサービス検出・接続 + native
fallback を配線、本書 §4 の`ServiceManager`/`HubHealthProbe`契約を消費。
併せて §3 T17-3 で trait のみだった実 HTTP probe を
`apps/banto-hub/core/src/http_hub_health.rs`として実装した。詳細は
[banto-hub-t16-design.md](banto-hub-t16-design.md) §3「T16-2 第一スライス
実装メモ」。フルの`HostSwitchEngine`（本書 §9）UI/切替ウィザードは当時未着手 -
Windows 実機での`WindowsServiceManager`経路検証も未了）。**
**2026-08-10: T16-2 第二スライス実装済み**（トレイ「サービスを開始/停止」を
本書 §9 の`HostSwitchEngine`による完了待ちへ配線し直し、`ServiceManager::start`/
`stop`後のポーリング・Desktop 引き継ぎ前の SCM`Stopped`＋旧 health
`Unreachable`確認を本書 §9 の不変条件どおり適用するようにした。navigate/probe
先ホストの`BANTO_BIND`対応、ローカル`Administrators`のトレイ操作許可も
同スライスで追加。当時フルの切替ウィザード UI は未着手（トレイの
「開始/停止」という単一の入口だけを`HostSwitchEngine`経由にした）。詳細は
[banto-hub-t16-design.md](banto-hub-t16-design.md) §3「T16-2 第二スライス
実装メモ」・§5。**同日 Windows 実機でトレイ開始/停止の完了待ちを確認済み**。**
**2026-08-11: 切替ウィザード UI 実装済み**（`/status` カード＋シェル最小
invoke。詳細は [banto-hub-t16-design.md](banto-hub-t16-design.md) §3
「切替ウィザード UI 実装メモ」。Windows 実機の UI 経路は当時未検証）。
**2026-09-01: 上記 UI 経路の Windows 実機検証完了**（Desktop→Service は
2026-08-31、Service→Desktop 逆経路・自動起動トグルの UAC 昇格・UAC
キャンセル時の異常系は 2026-09-01 にオーナーが実機確認、詳細は
[banto-hub-t16-design.md](banto-hub-t16-design.md) §3）。
T16-0（薄いシェル）・T16-1（トレイ状態表示）はマージ済みで本書の前提。
T16-2（サービス検出・native fallback）第一スライスは本書 §4 の引き渡し
契約（P5）に従い、T17-0/T17-3 が提供する API を消費する形で実装した
（トレイ開始/停止の`HostSwitchEngine`経路は第二スライスで実機確認済み。
切替ウィザード UI は 2026-08-11 実装）。**2026-08-10:
profile ACL 追加スライス（desktop-plan §11、`grant-profile-acl`）を実装済み
（§12。2026-10-16 に #626 で`profile_grant.rs`へ作り直し、§12 末尾）- T16-2 実機検証で見つかった
「LocalSystem 作成 profile が readonly になる」既知ギャップを解消する。
同日 Windows 実機で `grant-profile-acl`（UAC）による owner Modify 付与と
書き込み復旧を確認済み（§12「Windows 実機検証」）。サービス先行作成→
Desktop Hub 起動までのフル E2E は任意の追加確認として残す。**
**2026-10-16（#626 PR-B、オーナー決定）: `grant-profile-acl`を昇格の境界として
作り直した（確かめてから辿らずに開いたハンドルへ保護された DACL を書く・使用中は
断る・利用者のアカウントだけ・マシンの環境のルート・足していく）。§12 末尾
「#626 の作り直し」。実機確認は 2026-10-17 に M1・M2・M4〜M7 を確認済み（M3 は
監査の追試で代えた（実機では測っていない）。M8〜M10 は B3-4 のインストーラの実機確認へ）。**
**2026-10-17（#642）: #641 のオーナーレビューの Nit（作ろうとしたら既にあった段を親の
ハンドルから相対で開き直す・相対で作る名前の確かめを広げる・`COM0`/`LPT0`を予約名に）と
実機確認の Nit 6（断ったときの最後の行の接頭辞が二重）を直した。§12 末尾「#642 の追補」。
#642 の実機確認は未。**
最終検証日(コード照合): 2026-09-01
最終検証日(Windows 実機): 2026-10-17（#626 PR-B の M1・M2・M4〜M7。末尾「#626 の作り直し」の
実機確認の結果）。2026-08-10（§8・§10・§11・§12、管理者 Cursor +
オーナー対話。Operators 非管理者委任・profile ACL 付与まで完了）。
切替ウィザード UI 経路は 2026-08-31/09-01 に別途実機検証完了（§3、
[banto-hub-t16-design.md](banto-hub-t16-design.md) §3 参照）
基準コミット: `7178493`（main、T17-3 マージ後 #118）。

関連: [banto-hub-desktop-plan.md](banto-hub-desktop-plan.md)
（§8 シェル/サービス管理、§10 T16・T17、§11 データプロファイルと移行、
§16.2 mutex 命名決定、§16.3 T16/T17 未決事項）、
[banto-hub-t16-design.md](banto-hub-t16-design.md)、
[banto-hub-operations.md](banto-hub-operations.md) §10〜11、
[tag-server-design.md](tag-server-design.md) §3.1。

## 0. スコープ

本書は desktop-plan §10「T17: サービス管理、プロファイル、インストーラ」
と §16.3「T16/T17」未決事項のうち、**T17 着手に必要な判断**を現行コード
照合に基づき詳細設計へ落としたものである。T16-design が T16-0 着手可否を
扱ったのに対し、本書は T17-0 以降の着手可否と、T17 が T16-2 へ提供すべき
契約を扱う。

対象:

1. profile path・mutex/排他・`BantoHub Operators` 権限境界・UAC helper・
   Desktop↔Service 切替トランザクション・インストーラ再設計・構成
   パッケージ export/import の主要判断（P1〜P6、2026-08-10 承認済み）
2. 現行 `win_service.rs`/installer の棚卸し（file:line 付き）
3. T17 のサブスライス分割
4. T16-2 が消費する最小 API 面（引き渡し契約）

対象外:

- 収集ロジック本体（`crates/banto-collect` 等）— 変更しない
- T16-0/T16-1（薄いシェル・トレイ状態表示）— マージ済みで本書の前提と
  して扱うのみ
- タグ登録 UI/UX（T18 系）— 無関係
- Windows 固有 API の正確な呼び出し手順（`SetServiceObjectSecurity` の
  引数形、SDDL 文字列の正確な構文、UAC マニフェストの実装詳細等）—
  「要 Windows 実機スパイク」と明記するのみで、本書では確定しない

## 1. 主要判断（2026-08-10 承認済み）

| ID  | 論点                                                | 決定                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             | 主な代替                                                                                                                                          | 影響                                                                                                                                                                                                                        |
| --- | --------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| P1  | profile path をモード非依存の絶対パスに一本化するか | **一本化する**。3ホスト（console/service/shell）が個別に複製している既定値（`DEFAULT_DB_PATH = "./banto-hub.sqlite3"`、`store_config.data_dir` 既定 `"./data"`、`hub_log::resolve_service_log_dir` 既定 `"./data"`）を、`%ProgramData%\BantoHub\profiles\<profile-id>\` 配下の絶対パスを返す共通関数（新設、`banto_hub_core` 側）に置き換える                                                                                                                                                                    | 相対パスのまま運用規約（起動時 CWD を固定する）だけで対応する                                                                                     | `HubConfig`/3ホストの `build_hub_config`/`hub_log`/移行スクリプト（§11）に波及                                                                                                                                              |
| P2  | Desktop↔Service 排他をどれで正とするか              | **3層併用**。(a) SCM `query_status` を「サービスが動いているか」の一次情報源とする。(b) `Global\BantoHub.<profile-id>`（desktop-plan §16.2 で命名決定済み）named mutex を `HubRuntime::start` 冒頭でプロセス間排他の実体として使う。(c) profile ディレクトリのファイルロックへ所有者 PID・ホスト種別・タイムスタンプを書き、fallback UI の「mutex: 所有者不明」等の診断情報源にする（ロック自体の正当性は (b) が持つ）                                                                                           | mutex のみ（診断情報が「所有者不明」からいつまでも進まない） / SCM 状態のみ（同一 Session 内の二重起動を検知できない、desktop-plan §16.2 の指摘） | `HubRuntime::start` 冒頭に排他チェックを追加。§16.2 の決定と整合させる必要                                                                                                                                                  |
| P3  | `BantoHub Operators` と UAC helper の権限境界       | **サービスオブジェクト自体の SDDL で権限分割**。日常操作（start/stop/restart=stop+start/query）は install 時に UAC helper が `BantoHub` サービスの Security Descriptor へ `SC_MANAGER_CONNECT` + `SERVICE_QUERY_STATUS`/`QUERY_CONFIG`/`START`/`STOP` を `BantoHub Operators` SID に付与しておき、native shell はそれ以降 UAC を経由せず直接 SCM API を呼ぶ。install/uninstall/自動起動切替/ACL 変更/実行バイナリ変更は毎回 UAC helper（固定した5〜6種のアクション文字列のみを受け付ける単機能 exe）を経由させる | 常駐の昇格ヘルパーサービスが全操作を代行する方式（SYSTEM 権限の常駐面が増え攻撃面が広がるため不採用）                                             | desktop-plan §8.3 の権限表そのもの。SDDL 設定コードと UAC helper exe の新設が必要                                                                                                                                           |
| P4  | インストーラは登録しても収集を開始しない保証        | **`install()` の既定起動種別を `AutoStart`（現状）から `Demand`（手動）へ変更**し、自動起動 ON は初回セットアップウィザードでの明示操作のみで有効化する。サービス開始時に `Configured` 収集を即座に開始する既存挙動（`win_service.rs:287`）自体は変えない — 「サービスが動くたびに収集する」のは既定 OK、「OS 再起動だけで収集が始まる」のを禁止する                                                                                                                                                             | サービス起動種別は現状のまま、代わりに service body 側に「初回は収集しない」フラグを追加する（起動種別と収集開始の2重の意味論が増えて複雑）       | `win_service.rs::install` の1行変更＋インストーラ受入テストの追加。NSIS「インストール後に実行」チェックボックス問題（既知の制約、`banto-hub-operations.md` §11）は tauri-bundler 側の制約のため未解決のまま次項§5へ持ち越す |
| P5  | T16-2 との境界                                      | **T17-0 が状態取得＋start/stop/restart/autostart API を提供し、T16-2 はそれを消費するだけ**とする。API 契約は本書 §4 で確定し、T16-2 は self の native fallback UI 実装のみを担当する                                                                                                                                                                                                                                                                                                                            | T16-2 が SCM 呼び出しを直接持つ（T16 と T17 の責務境界が崩れ、`banto-hub-shell` に `windows-service` 依存が漏れる）                               | T16-2 着手条件が本書 §4 の確定を待つことになる                                                                                                                                                                              |
| P6  | 構成パッケージ（TAG-UX-5）の秘密除外リスト          | **除外対象を「MQTT `settings.password`（平文保存、`settings.rs:124` 明記）」「`users.password_hash`」「`api_keys.key_hash`」の3種に限定**し、それ以外（PLC接続・タグ・演算式・保持設定等）は import 先でそのまま使える形でエクスポートする。除外した3種は import 時にゼロ埋め/未設定のまま持ち込み、初回ログイン・API キー再発行・MQTT パスワード再入力を利用者に促す                                                                                                                                            | 秘密も暗号化して同梱する（復号鍵の配布・保管という別の秘密管理問題を持ち込むため不採用）                                                          | export/import 実装（T17-5）のフィールド選別ロジックとオンボーディング文言に影響                                                                                                                                             |

根拠: desktop-plan §8.3（Operators 権限表）、§11（データプロファイル）、
§16.2（mutex 命名決定）、§16.3（T16/T17 未決事項）、本書 §2 の現行コード
棚卸し、および 2026-08-10 のオーナー承認により P1〜P6 を確定。
**T17-0 着手可**。
**2026-10-18 オーナー決定（#623・#661）: 設定モード ⇔ 運用モード**（ChronoGazer と banto-hub で共通）

- **製品は同梱版**: 1 つのパッケージに、設定する段階（デスクトップで動かす。接続・タグの設定と手元での確かめ）と運用する段階（Windows サービスで 24 時間動かす）の両方を入れる。利用者は **GUI から容易に切り替える**（「設定モード ⇔ 運用モード」）。PC に詳しくない利用者でも迷わないことを優先する。データ（ChronoGazer のルート、banto-hub の profile）は両方のモードで共有し、設定した中身をそのまま運用で使う。
- 採らない案: サービスが入っている PC ではデスクトップで動かさない／モードごとにデータの置き場を分ける／開発（エンジニアリング）版とランタイム版に分ける。
- **#623 の穴（インストール利用者が「変更」を持つ場所に LocalSystem が書く）は、切り替えのときにサービス自身が ACL を付け替えて塞ぐ**（サービスは SYSTEM なので UAC は要らない。開始・停止は Operators なら UAC なし）。
  - 運用モードへ: デスクトップのエンジンを止める → サービスを開始する → サービスはデータに書く前に、データのフォルダ全体を辿らずに確かめ（リンク・ハードリンクがあれば締めずに起動を断る）、ACL を「SYSTEM・Administrators はフルコントロール、利用者は読み取り」に締める → 締め終わってからエンジンを動かす。
  - 設定モードへ: サービスがエンジンを止めて DB を閉じる → ACL を利用者の「変更」に戻す → サービスが終わる → デスクトップのエンジンが動く。
  - 利用者の権限で書いている間は LocalSystem は書かず、サービスが書いている間は利用者はフォルダの中に名前を作れない（2 者が同時に書く時間を作らない）。
  - サービスが落ちて ACL が締まったまま残ったときは、デスクトップが「運用モードから戻せませんでした」と案内し、昇格ヘルパー（UAC）の修復で戻せるようにする。
  - その上の守りとして、共有 crate `banto-safe-root`（industrial 側に置く。同日のオーナー決定）による起動時の確かめと辿らない書き込み、Redirection Guard を使う。
- **切り替えの間の収集**（同日のオーナー決定）: 切り替えの間は収集が止まることを許す（サービスで動いている間も GUI から設定を変えられるので、切り替えはまれな操作）。先につないでから切る形は作らない。ACL を締める時間はファイル数に対して測って短く保ち、切り替えの開始と完了をログに出して途切れた区間を欠測と分かるように残し、切り替えの画面に「切り替えの間は収集が止まります」と一言出す。
- 詳しい設計（ACL の付け替えの手順・確かめの範囲・PR の分け方）は #623 の PR-1 の設計で詰める。

## 2. 現行コード棚卸し

### 2.1 `win_service.rs`（`apps/banto-hub/core/src/bin/banto_hub/win_service.rs`）

| 機能            | 現状                                                                                                                                                                                                                                                                                                                                                                                                      |
| --------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| install         | `install()`（`win_service.rs:71-124`）。冪等ではない（既存サービスがあると `create_service` が失敗、`win_service.rs:105-109`）。起動種別は常に `ServiceStartType::AutoStart` + `set_delayed_auto_start(true)`（`win_service.rs:91,115`）— **autostart を OFF にする選択肢が無い**（P4 の対象）                                                                                                            |
| uninstall       | `uninstall()`（`win_service.rs:127-164`）。実行中なら停止してから削除（`win_service.rs:144-154`）。状態取得は `query_status()` 一度だけ、`current_state != Stopped` の分岐のみ                                                                                                                                                                                                                            |
| start/stop 単体 | **無い**。`install`/`uninstall` 以外に SCM を操作するコードはこのファイルにしか存在しない。「今すぐ開始」「今だけ停止」「再起動」「autostart だけ ON/OFF」に対応する API が存在しない                                                                                                                                                                                                                     |
| restart         | 無い（stop→start の合成すら未実装）                                                                                                                                                                                                                                                                                                                                                                       |
| status 取得     | `query_status()` の戻り値をそのまま2箇所（`uninstall`）で使うのみ。構造化した `ServiceStatusSummary` 相当の型は存在しない                                                                                                                                                                                                                                                                                 |
| data path       | `run_service_body`（`win_service.rs:197-305`）は `crate::build_hub_config()`（`banto-hub.rs:109-123`）を呼ぶだけで、service 固有の絶対パス解決は無い。ログディレクトリだけ `hub_log::resolve_service_log_dir()`（`hub_log.rs:72-82`）が独立に `BANTO_HUB_DATA`（既定 `"./data"`）を見る — DB パス解決（`runtime.rs:250`、設定 DB の `store_config.data_dir` 経由）とは**別の既定値ロジック**（P1 の対象） |
| mutex/排他      | **無い**。`HubRuntime::start` 自体にも profile 排他は無い（`runtime.rs:207-` 参照、DB open/bind 失敗を検知するのみ）。T16-design §4.2 も「T16-0 は bind 失敗を当面の衝突検知とする」と明記済み                                                                                                                                                                                                            |
| ACL/Operators   | **無い**。サービスオブジェクトの Security Descriptor を操作するコードはワークスペース全体に存在しない（`rg "SetServiceObjectSecurity\|SDDL\|LookupAccountName"` 該当 0 件）                                                                                                                                                                                                                               |
| UAC helper      | **無い**。`install`/`uninstall` 自体が「管理者権限の PowerShell から実行」を前提にしており（`banto-hub-operations.md:448-450`）、入力固定の昇格ヘルパー exe は存在しない                                                                                                                                                                                                                                  |

### 2.2 デスクトップシェル（`apps/banto-hub/src-tauri/src/lib.rs`）

| 機能             | 現状                                                                                                                                                                                                       |
| ---------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 環境変数読み取り | `build_hub_config()`（`lib.rs:110-124`）は `bin/banto-hub.rs::build_hub_config`（`banto-hub.rs:109-123`）と**同一ロジックを丸ごと複製**（doc comment `lib.rs:104-109` も明記）。共通クレート化されていない |
| 単一インスタンス | `tauri_plugin_single_instance`（`lib.rs:395-397`）— GUI プロセス内の二重起動のみ検知。**別プロセス（console/service）との排他は無い**                                                                      |
| サービス検出     | **無い**。T16-2 で追加予定（本書 §4 の契約待ち）                                                                                                                                                           |

### 2.3 インストーラ（`apps/banto-hub/installer/`）

| 機能                   | 現状                                                                                                                                                                                                                                                                                                                                     |
| ---------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 対象バイナリ           | ヘッドレス `banto-hub.exe` のみ（`installer/src/main.rs:65-67`、`MAIN_BINARY_NAME`）。`banto-hub-shell`（src-tauri、T16-0）は**このインストーラの対象外**（`main.rs:14-21` の追記コメント、パッケージング自体は T17 スコープと明記済み）                                                                                                 |
| post-install           | `service-hooks.nsh:31-39` の `NSIS_HOOK_POSTINSTALL` が `banto-hub.exe install` を実行 → 上記 P4 の `AutoStart` のまま登録される。**サービス開始（`Start-Service`）はインストーラの範囲外**（`banto-hub-operations.md:641-643` に明記済み）                                                                                              |
| pre-uninstall          | `service-hooks.nsh:41-47` の `NSIS_HOOK_PREUNINSTALL` が `banto-hub.exe uninstall` を実行                                                                                                                                                                                                                                                |
| 既知の制約             | 「インストール後に実行」チェックボックスを消せない（`banto-hub-operations.md:644-656`）。ON のままだと `banto-hub.exe` がコンソール無し前面プロセスとして直接起動し、サービスと二重 bind し得る — **収集開始そのものは起きない**（console/shell ホストは収集 Stopped で起動、§2.2 参照）が、ポート二重 bind で起動失敗する形の事故は残る |
| Operators グループ作成 | **無い**。インストーラは `BantoHub Operators` を作成せず、対話ユーザーを追加する選択肢も無い（desktop-plan §11 の要求は未実装）                                                                                                                                                                                                          |
| profile ACL 設定       | **無い**                                                                                                                                                                                                                                                                                                                                 |

### 2.4 まとめ

現行コードは「T5-1/T5-2 の一次形態（ヘッドレス exe + サービス + NSIS）を
最小実装した」段階で止まっており、T17 が要求する状態取得・部分操作
（start/stop/restart/autostart 分離）・排他・ACL・UAC helper・profile
一本化・構成パッケージのいずれも実装されていない。したがって T17 は
「既存の再利用可能な層を抽出する」のではなく、**大部分を新規実装**する
（desktop-plan §10 T17 冒頭の記述どおり）。

## 3. サブスライス表

| スライス | 内容                                                                                                                                                                                                                                | 依存                  | 受入の要点                                                                                                                       | 検証範囲                                                                                                                                                   |
| -------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------- | -------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------- |
| T17-0    | SCM 状態取得＋start/stop/restart/autostart API を、`win_service.rs` から「ロジック層」として抽出（§4 の `ServiceManager` trait 相当、desktop-plan §12.1「`ServiceManager` trait のモック単体」に対応）。P5 の契約はこの層が提供する | P1〜P5 承認           | 状態遷移・タイムアウト・エラー分類を型で表現できる。§4 の最小面を満たす                                                          | ロジック（状態機械・エラー分類）はモック実装で CI（windows-latest ランナー、非対話）でも単体テスト可能。実サービスへの実操作は Windows 実機必須（§5）      |
| T17-1    | profile path 一本化（P1）＋ mutex/排他（P2）                                                                                                                                                                                        | P1・P2 承認           | 3ホストが同じ絶対パス解決関数を使う。`Global\BantoHub.<profile-id>` 取得失敗時に安全側で起動を拒否する                           | パス解決の純関数部分は CI 可（非 Windows でもパス文字列組み立てはテストできる）。`Global\` named mutex の実際の取得・Session 0 越え動作は Windows 実機必須 |
| T17-2    | UAC helper（固定アクション exe）＋ `BantoHub Operators` グループ作成・SDDL 付与（P3）                                                                                                                                               | T17-0、P3 承認        | helper は install/uninstall/autostart 切替/ACL 変更の5〜6種のみ受け付ける。Operators は日常操作を UAC なしで行える               | Windows 実機必須（UAC プロンプト、SDDL、ローカルグループ作成はいずれも実対話セッションでのみ検証可能）                                                     |
| T17-3    | Desktop↔Service 切替トランザクション（T16-2 が消費、§4）                                                                                                                                                                            | T17-0、T17-1          | 切替の各段階と失敗到達状態を型で表現。二重接続を起こさない                                                                       | 状態機械のロジックは CI 可。実際の2プロセス間切替（DB lock・PLC 接続の実競合）は Windows 実機必須                                                          |
| T17-4    | インストーラ再設計（収集非開始、P4）                                                                                                                                                                                                | T17-0、T17-2、P4 承認 | 新規インストール直後は自動起動 OFF・収集非開始。上書きインストールは既存設定を保持（desktop-plan §16.3「上書きインストール」節） | NSIS フック自体の変更はロジックとして CI で検証しづらい（tauri-bundler は Windows 専用パッケージング）。実際のインストール完了確認は Windows 実機必須      |
| T17-5    | 構成パッケージ export/import（TAG-UX-5、P6）                                                                                                                                                                                        | T17-1                 | 秘密3種（P6）を除外して import 先で動く。除外分は明示的にオンボーディング導線を出す                                              | フィールド選別ロジック（何を含め何を除くか）は CI 可。実ファイルの Windows 上での読み書き権限は実機確認が望ましいが必須ではない                            |

## 4. T16-2 への引き渡し契約

T16-2（サービス検出時は Desktop Collector を起動せずその UI へ接続、
native fallback UI）は、desktop-plan §9.9 の2つのモックアップ
（「Banto Hub に接続できません」／「Banto Hub を開始できません」）が
そのまま**必要な最小 API 面**を規定している。T17-0/T17-1/T17-3 はこれを
満たす層を提供し、T16-2 はこれを消費するだけで SCM API や mutex API を
直接呼ばない。

```text
trait ServiceManager {
    // SCM 状態 + 起動種別。desktop-plan §9.9 の「サービス: 停止/実行中」
    // 「STOP_PENDING」等の文言はこの enum の Display 相当。
    fn query_status(&self) -> Result<ServiceStatusSummary, ServiceManagerError>;
    // 冪等: 既に Running なら現在状態を返すだけ（多重クリック対策、
    // desktop-plan §4.3「遷移中は新しい開始・停止要求を重ねない」）。
    fn start(&self) -> Result<TransitionHandle, ServiceManagerError>;
    fn stop(&self) -> Result<TransitionHandle, ServiceManagerError>;
    fn set_auto_start(&self, enabled: bool) -> Result<(), ServiceManagerError>;
}

struct ServiceStatusSummary {
    state: ScmState,       // NotInstalled/Stopped/StartPending/Running/StopPending/...
    auto_start: bool,      // Windows 起動時に自動開始するか（P4 既定 false）
    pid: Option<u32>,
}

// fallback UI の「別の Banto Hub が使用中、または状態を確認できません」
// （desktop-plan §9.9）を判定するための health/所有権確認。
trait HubHealthProbe {
    fn probe(&self, expected_profile: &ProfileId, expected_port: u16)
        -> Result<HealthOutcome, ProbeError>;
}

enum HealthOutcome {
    Healthy { version: String },
    WrongProfileOrVersion,
    MutexOwnerUnknown,   // 「mutex: 所有者不明」
    PortConflict,
}
```

T16-2 が満たすべき手順（desktop-plan §9.9 のフローそのもの）:

1. `query_status()` で `Stopped`/`StartPending`/`Running` を判定し、
   `Running` なら即座に「サービスが所有」表示へ切り替え、Desktop
   Collector は起動しない（T16 受入条件「サービス稼働中に同じ profile の
   Desktop Collector を起動しない」）。
2. `Stopped` から「サービスを開始」を押した場合、`start()` を呼んで
   `TransitionHandle` を health 確認まで待つ。`probe()` が
   `WrongProfileOrVersion`/`MutexOwnerUnknown`/`PortConflict` を返す間は
   開始ボタンを隠す（desktop-plan §9.9「別 port や別ホストへ自動で
   逃がさない」）。
3. Windows 操作権限（`BantoHub Operators` メンバーシップ、P3）を確認
   できない場合は `start()`/`stop()` 自体を呼ばず、案内文言のみ表示する。

**T16-2 は T17-0 が本契約の API を提供してから着手する**（P5、2026-08-10
承認済み）。契約の型・エラー種別はここで決めた形を変えない前提で T17-0 を
実装し、実装中に形が変わった場合は本書を更新してから T16-2 に反映する。

## 5. リスクと未決（Windows 実機待ち）

以下はいずれも Windows 実機（対話セッション、複数ユーザー、実際の UAC
プロンプト、OS 再起動、複数プロファイル）でなければ検証できない。CI の
`rust`（fmt/clippy/test）ジョブは `windows-latest` ランナーで動くため
`#[cfg(windows)]` コードのコンパイルとロジック単体テストはそこで検証
できるが、次の項目は非対話の CI ランナーでは確認できない、または
そもそも実行しない前提とする（**要 Windows 実機スパイク**の項目には
その旨を明記した）。

- **`Global\` named mutex の Session 0 越え挙動**: 2026-08-10 実機で
  **二重起動拒否は確認**（§8「Windows 実機検証」）— サービス（Session 0 /
  LocalSystem）稼働中に同 profile の Console 起動は exit=1 で失敗する。
  ただし Console 側のエラーは `ProfileLockError::AlreadyHeld` ではなく
  `ProfileLockError::Io`（os error 5 / Access Denied）になることがあり、
  owner 診断 JSON が T16-2 fallback UI に届かない。**改善候補**（
  improvement-plan.md §6 バックログ参照）。
- **UAC split-token の管理者判定落とし穴**（desktop-plan §16.3 既指摘）:
  `CheckTokenMembership` が UAC 昇格前トークンでは Administrators を
  deny-only として偽の非管理者判定を返し得る。`TokenLinkedToken` を
  使うかどうかの実装方針は要 Windows 実機スパイク。
- **サービス Security Descriptor（SDDL）への `BantoHub Operators`
  SID 付与の正確な API 呼び出し**（P3）: `SetServiceObjectSecurity` の
  引数構成、`windows-service`/`windows-sys` クレートでの表現方法は本書
  では確定しない。要 Windows 実機スパイク。**解消（2026-08-10）**: §10
  で `EXPLICIT_ACCESS_W` + `SetEntriesInAclW`（既存 ACE を壊さずマージ）→
  `SetServiceObjectSecurity` として実装し（`service_elevated.rs`
  `grant-service-acl`）、同日 Windows 実機で `sc sdshow` による確認・
  再実行時の冪等性まで確認済み（§10「Windows 実機検証」表参照）。
- **Operators グループ追加が既存ログオントークンに載らない**
  （desktop-plan §16.3 既指摘）: 再ログオン手順を受入シナリオに含める
  必要があるが、実際の遅延・挙動は実機確認が必要。
- **NSIS「インストール後に実行」チェックボックスを消せない制約**
  （`banto-hub-operations.md:644-656`、既知の制約）: tauri-bundler
  2.9.4 のテンプレート制約で、T17-4 でも解消できるか未確定。独自 `.nsi`
  への切り替えが必要になる可能性があり、その場合はスコープの再判断が
  必要。
- **コード署名・WebView2 Fixed Version 同梱**（desktop-plan §16.3
  既指摘）: 未署名 NSIS + UAC helper は SmartScreen で「発行元不明」
  警告が出る。**2026-08-12 オーナー決定: 当面は未署名のまま検証／社内
  配布のみで運用し、SmartScreen 警告は既知の制約として許容する。証明書
  調達・WebView2 Fixed Version 同梱は外部顧客配布が具体化した時点で
  再判断する（現時点ではスコープに入れない）。**
- **サービス実行アカウント**: 現状 LocalSystem。desktop-plan §16.3 で
  「仮想サービスアカウント（NT SERVICE\\BantoHub）の採否を T17 で評価」と
  していた件は、**2026-08-12 オーナー決定: LocalSystem を維持する**（T17
  で実機検証済みの構成を優先。最小権限化＝仮想アカウント移行は将来の
  堅牢化タスクとして切り出し、ACL／profile 権限まわりの再検証を伴う）。
- **上書きインストール時の既存サービス設定保持**（desktop-plan §16.3
  「上書きインストール時は既存サービスの起動種別・自動起動設定を保持」）:
  T17-4（§11）で対応済み - `install()`は SCM に同名サービスが既に存在
  する場合は`create_service`を呼ばず早期リターンする（既存の起動種別を
  含む設定に一切触れない）。より高度な「既存の起動種別を読み取って
  ログ表示する」等は見送った（本スライスの最小案）。
- **72時間ソーク・実 PLC 環境でのサービス／シェル往復**
  （desktop-plan §12.2）: T17-3 の切替トランザクションの最終確認は
  Windows 実機でのソークテストが必須。

## 6. 承認チェックリスト（2026-08-10 承認済み）

- [x] P1: profile path のモード非依存絶対パス一本化
- [x] P2: Desktop↔Service 排他の3層併用（SCM state / named mutex / file lock 診断）
- [x] P3: `BantoHub Operators` はサービス SDDL 直接付与、それ以外は UAC helper
- [x] P4: `install()` の既定起動種別を `Demand` に変更
- [x] P5: T17-0 が提供する API を T16-2 がそのまま消費する境界（§4）
- [x] P6: 構成パッケージの秘密除外リスト（MQTT password / password_hash / key_hash）

**上記すべて 2026-08-10 オーナー承認済み。T17-0 は同日実装済み（§7）。**

## 7. 実装メモ（2026-08-10、T17-0）

T17-0（§3「T17-0」・§4「T16-2 への引き渡し契約」）のロジック層を
`apps/banto-hub/core/src/service_manager.rs`（新設）に実装した。P1〜P6 は
同日オーナー承認済み（§6）。T17-0 は §4 の契約に沿った API 提供のみで、
他スライス（P1 パス一本化・P4 Demand 化等）には依存しない。

- **追加した公開型**: `service_manager::ServiceManager` trait
  （`query_status`/`start`/`stop`/`restart`/`set_auto_start`、§4 の契約
  そのまま）・`ScmState`（`NotInstalled`/`Stopped`/`StartPending`/
  `StopPending`/`Running`/`Other(String)`）・`ServiceStatusSummary`・
  `ServiceManagerError`（`NotFound`/`AccessDenied`/`Timeout`/`Other`、
  thiserror 由来）・`TransitionHandle`（目標状態を保持する薄い値型、
  `wait_until_settled`で`query_status`をポーリングして完了を待つ）。
  `install`/`uninstall`は trait に含めていない（§4 契約の必須面ではない）。
- **`MockServiceManager`**: ホスト非依存・常に利用可能（`#[cfg(test)]`
  ではない）。`installed`フラグ・現在状態・`auto_start`をメモリ上に持ち、
  `start`/`stop`は冪等（既に目標状態なら no-op）、`restart`は
  `stop`→`start`合成。一時的な失敗を再現する`inject_error`（1回消費）も
  持つ。単体テスト13件で状態遷移を検証、`cargo test -p banto-hub-core
--lib`で Linux 上でも実行できる。
- **`WindowsServiceManager`**（`#[cfg(windows)]`）: `windows-service`
  クレートで実 SCM を叩く。サービス名は`service_manager`モジュールに新設した
  `SERVICE_NAME`定数（値は従来どおり`"BantoHub"`）に一本化し、
  `win_service.rs`側の同名定数は`pub use`での再公開に変更した（値・挙動は
  変えていない）。`set_auto_start`は`ChangeServiceConfigW`が
  `ServiceInfo`全体を要求し、かつ`executable_path`/`launch_arguments`を
  その場で再エスケープする都合上、`query_config`で取得した生コマンドライン
  をそのまま渡すと二重エスケープで壊れる — そのため`win_service.rs::install`
  と同じ組み立て方（構築時に渡す`executable_path`＋固定引数
  `RUN_SERVICE_ARG`）で`ServiceInfo`を再構築する方式にした。
- **既存 CLI は不変**: `win_service.rs`の`install`/`uninstall`/
  `run-service`は本 T17-0 では一切変更していない。起動種別は現状のまま
  `AutoStart`＋遅延自動開始（P4「Demand 化」は T17-4 で扱う）。
- **Windows 実装の限界（Windows 実機未検証）**:
  - `WindowsServiceManager::new`が受け取る`executable_path`は呼び出し元が
    正しい headless exe パスを渡す前提（T16-2 では呼び出し側の責務）。
  - `restart`の`wait_until_settled`タイムアウト（30秒・200ms間隔）は暫定値。
  - `Paused`系遷移や実 UAC 権限不足時の検知は Windows 実機確認待ち
    （§5 と同様）。実 SCM 呼び出しの正しさは T16-2/T17-4 着手時の実機確認へ
    持ち越し。

## 8. 実装メモ（2026-08-10、T17-1）

T17-1（§3「T17-1」・P1「profile path 一本化」・P2「mutex/排他」、P1・P2 は
2026-08-10 オーナー承認済み・§6）を新設2モジュール
`apps/banto-hub/core/src/profile_paths.rs`・`profile_lock.rs`に実装した。
SCM 経由の状態確認（T17-0、§7）はこのスライスでは呼んでいない（§3
「T17-1」のスコープ注記どおり）。

- **`profile_paths.rs`（P1）**: 3ホスト（console `bin/banto-hub.rs`/
  service `win_service.rs`/shell `apps/banto-hub/src-tauri`）が個別に
  複製していた相対パス既定（`DEFAULT_DB_PATH = "./banto-hub.sqlite3"`・
  `data_dir`既定`"./data"`・`hub_log::resolve_service_log_dir`既定
  `"./data"`）を、desktop-plan §11 の layout
  （`{root}/profiles/<profile-id>/{config,data,logs}`）へ一本化した。
  - `resolve_hub_root`: `BANTO_HUB_ROOT`（空文字列は無視）が全 OS 共通で
    最優先。以降は Windows なら`%ProgramData%\BantoHub`（既定
    `C:\ProgramData`）、非 Windows なら`XDG_DATA_HOME/BantoHub`→
    `$HOME/.local/share/BantoHub`→`/var/lib/BantoHub`の順。実際の
    `cfg!(windows)`判定を`resolve_hub_root_impl(is_windows, ...)`という
    純関数として外に出し、非 Windows CI ランナーでも Windows 側の分岐
    （パス文字列組み立てロジック自体）を含めて両方テストできる（§3
    「パス解決の純関数部分は CI 可」）。
  - `validate_profile_id`: 空文字列・パス区切り（`/`・`\`）・`.`/`..`を
    拒否する。`build_hub_config_from_env`は不正な`BANTO_HUB_PROFILE`を
    stderr へ警告してから既定 profile（`"default"`）へフォールバックする
    - この関数自体が`Result`を返さない（3ホストが直接呼ぶ薄い組み立て
      関数のため）ので、「どの env でも起動を拒否しない」設計にした。
  - `build_hub_config_from_env(host_kind: HubHostKind) -> HubConfig`:
    3ホスト共通の`HubConfig`組み立て関数として新設 - 各ホストの
    `build_hub_config`（従来は3ホストがそれぞれ個別定義）はこれを呼ぶ
    だけの薄いラッパになった。`BANTO_DB`/`BANTO_HUB_DATA`が未設定時の
    既定値は、旧「相対パス文字列」から「profile の絶対パス」へ変わった
    （`crate::runtime::DEFAULT_DB_PATH`自体は後方互換のため残したが、
    この関数はもう使わない）。`BANTO_ALLOW_SETUP`/`PORT`/`BANTO_BIND`の
    読み取り自体は変えていない。
  - `hub_log::resolve_service_log_dir`: 引数に`profile_logs_dir`
    （`ProfilePaths::logs_dir`）を追加し、`BANTO_HUB_DATA`未設定時の既定を
    profile の`logs_dir`にした（`BANTO_HUB_DATA`設定時は従来どおりその
    配下を優先 - 挙動不変・後方互換）。
- **`profile_lock.rs`（P2）**: `HubRuntime::start`冒頭（DB init より前）で
  profile 排他を取る - 失敗時は DB を一度も開かずに安全側で起動を拒否する
  （§1 P2「失敗時は安全側で起動拒否」、`HubStartError::ProfileLock`）。
  - Windows: `Global\BantoHub.<profile-id>`（`profile_paths::mutex_name`、
    desktop-plan §16.2 の命名決定どおり）を`CreateMutexW`で取得し、
    `GetLastError() == ERROR_ALREADY_EXISTS`で既存所有を検知する
    （`windows-sys 0.61`、`Cargo.toml`の
    `[target.'cfg(windows)'.dependencies]`に追加 - 既存の
    `dev-dependencies`と同じバージョン系列なので新規ノードは増えない）。
  - 非 Windows: `Global\`名前空間の mutex が無いため、profile ディレクトリ
    直下の`profile.lock`への`flock(LOCK_EX|LOCK_NB)`**自体**を排他の実体
    にした（`libc`クレート追加、
    `[target.'cfg(not(windows))'.dependencies]`）。同一プロセス内で
    同じ profile を2重に`try_acquire_profile_lock`すると2回目が確実に
    失敗することを Linux CI で検証できる
    （`profile_lock::tests::second_acquire_on_the_same_profile_fails`）。
  - 全 OS 共通: 取得成功後、`profile.lock`へ所有者 PID・ホスト種別
    （`HubHostKind::{Console,Service,Shell}`）・取得時刻（UNIX ms）を
    JSON（`ProfileOwnerInfo`）で書く - ロックの正当性そのものはこの内容が
    持つのではなく（Windows は mutex、非 Windows は flock が持つ）、
    fallback UI（T16-2）の「所有者不明」等の診断情報源として使う想定
    （`ProfileLockGuard::lock_file_path`で参照可能にした）。
  - `HubConfig`に`profile_id`・`host_kind: HubHostKind`・
    `skip_profile_lock: bool`を追加した。既存の unit/integration テストは
    同一プロセス内で複数の`HubRuntime`を並行起動することがあり（各テストが
    自前の一時 DB/data_dir を使うだけで profile 衝突を避ける意図はそもそも
    無かった）、それらは`skip_profile_lock: true`で構築するよう更新した -
    新規に profile ロック競合で失敗するテストは無い。
  - `HubRuntime::start`は「このモジュール自身は環境変数を一切読まない」
    という T14-1 以来の原則に対する唯一の例外として、profile 排他のためだけ
    に`BANTO_HUB_ROOT`/`ProgramData`/`XDG_DATA_HOME`/`HOME`を直接読む
    （`runtime.rs`のモジュール doc「T17-1 での唯一の例外」節参照） -
    `db_path`/`data_dir_override`は`BANTO_DB`/`BANTO_HUB_DATA`で任意の
    場所へ上書きできるため、そこから逆算せず`profile_paths`が読む同じ
    env・同じ優先順位関数で独立に root を再解決する。
- **3ホストの配線**: `bin/banto-hub.rs`・`win_service.rs`・
  `apps/banto-hub/src-tauri/src/lib.rs`はいずれもローカル複製の
  `build_hub_config`を削除し、`profile_paths::build_hub_config_from_env`
  （それぞれ`HubHostKind::Console`/`Service`/`Shell`を渡す）を呼ぶだけに
  なった。`win_service.rs`は`HubRuntime::start`より前にログファイルを
  開く必要があるため、`resolve_profile_paths_from_env().logs_dir`を先に
  解決してから`hub_log::resolve_service_log_dir`へ渡す（同じ env を
  `build_hub_config_from_env`と2回読むが、同一プロセス内なので両者が
  食い違うことはない）。
- **旧`./banto-hub.sqlite3`/`./data`からの自動移行は行っていない**
  （desktop-plan §11「黙って移動しない」、本スライスのスコープ外）。
- **テスト**: `profile_paths`・`profile_lock`の単体テスト（root 解決の
  全分岐・profile id 検証・mutex 名組み立て・同一 profile の2重取得失敗・
  異なる profile 間の非干渉・guard drop 後の再取得）に加え、
  `HubRuntime::start`が実際に`skip_profile_lock: false`で lock 競合時に
  `HubStartError::ProfileLock`を返すことを検証する統合的なテスト
  （`runtime::tests::start_fails_with_profile_lock_when_another_guard_already_holds_it`）
  を追加した。`cargo test -p banto-hub-core`（lib + 全 integration
  テスト、250+10+その他計約300件）・`cargo test -p banto-hub-shell`が
  Linux 上で green。`cargo clippy --all-targets -- -D warnings`は
  非 Windows・`x86_64-pc-windows-gnu`ターゲットの両方で警告0件
  （Windows 側`profile_lock::acquire_windows`の`unsafe`FFI 呼び出し
  経路も`cfg(windows)`ビルドとしてコンパイル検証済み）。
- **Windows 実装の限界（Session 0 実機検証済み・診断文言に残課題）**:
  `CreateMutexW`による`Global\`名前空間への書き込みには通常
  `SeCreateGlobalPrivilege`相当の権限が必要（通常ユーザーは既定で保有）。
  Session 0（サービス）と Session 1+（Console）間の**排他そのもの**は
  2026-08-10 実機で確認済み（§8「Windows 実機検証」）— 2 本目は起動しない。
  同一ユーザーセッション内の Console×2 では `AlreadyHeld` と owner 診断が
  期待どおり返る。Cross-session では拒否は成功するがエラー種別が
  `Io(ACCESS_DENIED)` になり得る（§5・improvement-plan.md §6）。
  Linux CI で検証できるのはパス解決の純関数部分と、非 Windows の flock
  排他が実際に機能することのみ。

### Windows 実機検証（2026-08-10、管理者 PowerShell / Cursor）

環境: Windows 10、`whoami`/管理者判定 True、`banto-hub.exe` は T17-1 入り
（`Global\BantoHub` / `profile.lock` 文字列を含むビルド）。テスト用
`BANTO_HUB_ROOT` を Machine 環境変数で
`%LOCALAPPDATA%\Temp\banto-hub-t17-admin-v2-*` に向け、`BANTO_HUB_PROFILE=default`・
`BANTO_ALLOW_SETUP=1`・`PORT=18722`・`BANTO_BIND=127.0.0.1` を Machine に設定。
検証後はサービス登録解除と Machine 環境変数削除済み。

| 項目                                   | 結果                                                                                                                                      |
| -------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------- |
| `install` → `Start-Service BantoHub`   | 成功（管理者権限）                                                                                                                        |
| Session 0 起動                         | 成功（`SessionId: 0`、`run-service`）                                                                                                     |
| T17 profile layout                     | 成功（`{root}/profiles/default/{config,data,logs}`）                                                                                      |
| `profile.lock` 診断 JSON               | 成功（`host_kind: "service"`）                                                                                                            |
| HTTP 疎通                              | 成功（`GET /api/v1/openapi.json` → 200）                                                                                                  |
| 同一セッション Console×2               | 成功（2 本目 `AlreadyHeld` + owner 表示）                                                                                                 |
| Session 0 越え（Service 中に Console） | **起動拒否は成功**（exit=1）。初回は `Io(os error 5)` だったが **`c9a2d73` で `AlreadyHeld` + owner（`host_kind: service`）に正規化済み** |

**ハマりどころ（再現性）**:

- Cursor シェルは `CARGO_TARGET_DIR` が sandbox 配下を指すことがあり、
  `cargo build` しても `D:\...\target\debug\banto-hub.exe` が更新されない。
  T17-1 未入りの古い exe だと `./banto-hub.sqlite3`（相対パス時代）を使い、
  mutex 未到達で port bind エラー（10048）まで進む。**実機検証前に exe が
  T17-1 入りか**（mutex 文字列の有無、profile 絶対パスログ）を確認すること。
- 初回テストで repo 側 exe が古かったため Session 0 越え mutex は port
  競合に見えたが、T17-1 入り exe 差し替え後に再検証で上表のとおり。

### Windows 実機検証 — Console→Service 切替（2026-08-10 夕方）

同一 profile（`BANTO_HUB_ROOT` 共有 DB）で Console 収集 → 強制停止 → Service
起動の手動切替（`HostSwitchEngine` 配線前の疎通確認。SLMP
`192.168.11.200:5200`、タグ `line1.g1.d3000`）。

| 段階                          | 結果                                               |
| ----------------------------- | -------------------------------------------------- |
| Console 収集中                | `q=good`、PLC TCP Established **1**                |
| Console 停止直後              | PLC TCP Established **0**（セッション解放）        |
| Service 起動後                | PLC TCP Established **1**（二重接続なし）          |
| Service 中に Console 二重起動 | exit=1、`AlreadyHeld` + owner `host_kind: service` |

**未了（T17-3 / T16-2 スコープ）**: `HostSwitchEngine` 経由の自動切替、
graceful shutdown、実 HTTP `HubHealthProbe`、Shell↔Service 往復の全段階。

## 9. 実装メモ（2026-08-10、T17-3）

T17-3（§3「T17-3」・desktop-plan §9.9「タスクトレイと停止時 fallback」・
§16.3「desktop⇔service 切替の中間状態を追加」）を新設2モジュール
`apps/banto-hub/core/src/hub_health.rs`・`host_switch.rs`に実装した。
T17-0（`ServiceManager`、§7）・T17-1（profile path/mutex、§8）に依存し、
T17-2（UAC/Operators）はスタブ（`can_operate_service: bool`）で受け取る。

- **`hub_health.rs`**: §4 に記述だけがあった`HubHealthProbe`
  trait・`HealthOutcome`（`Healthy`/`WrongProfileOrVersion`/
  `MutexOwnerUnknown`/`PortConflict`/`Unreachable`）・`ProbeError`を実装した。
  `expected_profile`は§4 の記述（`&ProfileId`）と異なり`&str`にした -
  このワークスペースに専用の`ProfileId`型は存在せず（`profile_paths`/
  `profile_lock`ともに`profile_id: String`のまま扱っている）、実装指示の
  シグネチャ（`&str`）をそのまま採用した。テスト用の`MockHubHealthProbe`
  （既定 outcome ＋`push_sequence`で1回ずつ消費する queue の2段構成、
  `MockServiceManager::inject_error`と同じ発想）のみを実装し、**実 HTTP
  probe は当時入れていなかった**（実装指示「無くてもモックだけで T17-3
  完了可」の範囲 - Windows 実機・T16-2 実配線着手時に追加する予定だった。
  追加しても`host_switch`側の変更は不要 - trait 境界だけに依存するため）。
  **2026-08-10 追記**: T16-2 第一スライスで
  `apps/banto-hub/core/src/http_hub_health.rs::HttpHubHealthProbe`として
  実装した（`std::net::TcpStream`による素朴な HTTP/1.1 実装、新規クレート
  依存なし）。予告どおり`host_switch`側の変更は不要だった。詳細は
  [banto-hub-t16-design.md](banto-hub-t16-design.md) §3「T16-2 第一スライス
  実装メモ」参照。
- **`host_switch.rs`**: Desktop↔Service 切替トランザクションの状態機械
  `HostSwitchEngine<M: ServiceManager, P: HubHealthProbe, D: DesktopHostControl>`。
  `step(SwitchCommand) -> Result<StepOutcome, SwitchError>`で1ステップずつ
  進める - 内部にスレッド・タイマーは持たず、進行状態の所有者はシェル
  （呼び出し側が`SwitchCommand::Poll`を繰り返し送ることで前進させる、
  desktop-plan §16.3「進行状態はシェル（ネイティブ側）が所有する」）。
  - **`HostKind`**（定常）: `Offline`/`Desktop`/`Service`。
  - **`SwitchPhase`**（進行中、`Idle`は非進行）: `DesktopStopping`→
    `AwaitingDesktopRelease`→`ServiceStarting`→`AwaitingServiceHealth`→
    （完了で`Idle`+`current=Service`）、逆方向は`ServiceStopping`→
    `AwaitingServiceRelease`→`DesktopStarting`→（完了で`Idle`+
    `current=Desktop`）、終端の`Faulted { stage: FaultStage, reason }`。
    `DesktopStopping`/`ServiceStopping`は「停止要求発行の瞬間」を表すが、
    要求自体が同期 API（`DesktopHostControl::request_stop`/
    `ServiceManager::stop`）のため成功時は同じ`step`呼び出し内で即座に
    次段へ進み、外部からは定常観測されない（要求自体の失敗時のみ
    `FaultStage`として`Faulted`に残る）。
  - **段階×失敗到達表**（実装指示の不変条件3「もう一方を起動したまま
    戻ることがない」をこの表で固定した - `cargo test host_switch`の
    各テストがそれぞれの行を検証する）:

    | 方向            | 失敗段階（`FaultStage`）                   | 到達 `HostKind`                               | 根拠                                             |
    | --------------- | ------------------------------------------ | --------------------------------------------- | ------------------------------------------------ |
    | Desktop→Service | `DesktopStopping`（`request_stop`失敗）    | `Desktop`                                     | 未着手 - Desktop は一度も止めていない            |
    | Desktop→Service | `AwaitingDesktopRelease`（timeout/cancel） | `Desktop`                                     | 解放未確認 - Service には一度も`start`していない |
    | Desktop→Service | `ServiceStarting`（`start`失敗/timeout）   | `Offline`                                     | Desktop 解放済み・Service 未確認（両方停止）     |
    | Desktop→Service | `AwaitingServiceHealth`（timeout/cancel）  | SCM `Running`なら`Service`、それ以外`Offline` | 「Service だが Unhealthy」を`last_health`で表現  |
    | Service→Desktop | `ServiceStopping`（`stop`失敗）            | `Service`                                     | 未着手                                           |
    | Service→Desktop | `AwaitingServiceRelease`（timeout/cancel） | `Service`                                     | 解放未確認 - Desktop 起動許可を出していない      |
    | Service→Desktop | `DesktopStarting`（許可 timeout/cancel）   | `Offline`                                     | Service 解放済み・Desktop 未起動（両方停止）     |

  - **`DesktopHostControl`**（コールバック trait）:
    `request_stop(&mut self) -> Result<(), DesktopHostError>`・
    `is_stopped(&self) -> bool`（停止済み**かつ**mutex 解放済みの両方を
    満たしたときだけ`true`を返す実装にする契約）・
    `request_start_allowed(&self) -> bool`（既定`true`、追加の Windows
    操作権限確認等のフック）。コアは`HubRuntime`を直接触らず、実際の
    Desktop Hub 起動・停止はシェル（`apps/banto-hub/src-tauri`）側が
    この trait を実装して担う（実装指示「コアが HubRuntime を直接触らず、
    コールバック trait で分離」）。テスト用`MockDesktopHostControl`
    （`set_stopped`/`set_mutex_released`/`set_released`/`set_start_allowed`/
    `inject_stop_error`）を用意した。
  - **不変条件の担保**（`cargo test -p banto-hub-core --lib host_switch`
    17件で固定）:
    1. `ServiceManager::start`を呼ぶのは`desktop.is_stopped()`が`true`に
       なった`AwaitingDesktopRelease`のポーリング内のみ
       （`desktop_to_service_happy_path_respects_ordering`が
       `stopped=true`だが`mutex_released=false`の間は SCM が`Stopped`の
       ままであることまで検証する）。
    2. Desktop 起動許可（`attempt_desktop_start`）は`AwaitingServiceRelease`
       で SCM `Stopped`**かつ**probe が`Unreachable`を確認した後にのみ
       呼ばれる（`service_to_desktop_happy_path_respects_ordering`）。
    3. 上表のとおり、失敗到達はすべて「未着手のまま」または「両方
       停止（`Offline`）」のいずれかで、Desktop と Service が両方稼働した
       まま失敗到達することはない
       （`desktop_to_service_failure_at_service_start_lands_on_offline_not_desktop`
       等）。
    4. `Idle`/`Faulted`（終端）以外の間は新しい`SwitchToService`/
       `SwitchToDesktop`を`StepOutcome::TransitionInProgress`で拒否する
       （`overlapping_switch_to_desktop_during_progress_is_rejected`）。
       `Faulted`は「進行中」ではないため再試行は許す
       （`desktop_stop_request_failure_leaves_desktop_as_current`が
       再試行成功まで確認する）。
    5. `AwaitingServiceHealth`では`Healthy`以外（`WrongProfileOrVersion`/
       `MutexOwnerUnknown`/`PortConflict`/`Unreachable`）のいずれでも
       `Waiting`のまま進めない
       （`ambiguous_probe_outcomes_never_complete_the_switch`）。
       `HostSwitchState::last_health`に残すので、T16-2 fallback UI の
       「開始ボタンを隠す」判断にそのまま使える。
  - **T17-2 スタブ**: `HostSwitchConfig::can_operate_service: bool`が
    `false`の間は`ServiceManager::start`/`stop`を一切呼ばず
    `SwitchError::PermissionDenied`を返す
    （`permission_denied_stub_blocks_service_start_without_calling_scm`/
    `_stop_without_calling_scm`）。Desktop 自身の起動・停止はこの確認の
    対象外（Windows サービス操作ではないため）。T17-2 着手時は
    `HostSwitchEngine::set_can_operate_service`で実際の Operators
    メンバーシップ判定結果を反映すればよく、engine 再構築は不要。
  - **T16-2 が次に消費する入口**: `host_switch::{HostKind, SwitchPhase,
FaultStage, HostSwitchState, SwitchCommand, StepOutcome, SwitchError,
DesktopHostControl, HostSwitchEngine, HostSwitchConfig}`と
    `hub_health::{HubHealthProbe, HealthOutcome, ProbeError}`。T16-2 は
    シェル側で`DesktopHostControl`の実装（`HubRuntime`ラッパー）を1つ
    書き、`ServiceManager`（T17-0、実装済み）・`HubHealthProbe`（実 HTTP
    probe、未実装 - 上記`hub_health.rs`節参照）と組み合わせて
    `HostSwitchEngine`を構築し、シェル自身のタイマー/イベントループから
    `SwitchCommand::Poll`を送るだけでよい。
- **やっていないこと**（実装指示のスコープ外どおり）: T16-2 UI 組み込み
  （`lib.rs`に`pub mod host_switch;`/`hub_health;`を追加しただけで、
  Tauri コマンドや Svelte 側の配線はしていない）・実 Windows SCM 結合
  テスト（`MockServiceManager`/`MockHubHealthProbe`/
  `MockDesktopHostControl`のみ - 実際の2プロセス切替・DB lock・PLC
  接続競合は Windows 実機必須、§5 のリスク一覧に変更なし）。T17-2 UAC
  helper 本体は §10 で実装済み。
- **テスト**: `cargo fmt -p banto-hub-core -- --check`・
  `cargo clippy -p banto-hub-core --all-targets -- -D warnings`（非
  Windows・`x86_64-pc-windows-gnu`の両方で警告0件）・
  `cargo test -p banto-hub-core --lib host_switch`（17件）・
  `cargo test -p banto-hub-core --lib`（`hub_health`4件を含め全271件）が
  Linux 上で green。

## 10. T17-2 実装メモ（2026-08-10）

スライス1: [`service_operators.rs`](../apps/banto-hub/core/src/service_operators.rs)
— `is_current_process_operator()`（`LookupAccountNameW` +
`CheckTokenMembership`）。グループ未作成時は `Ok(false)`。

スライス2: UAC 昇格ヘルパー `banto-hub-elev.exe`
（[`service_elevated.rs`](../apps/banto-hub/core/src/service_elevated.rs) /
[`banto-hub-elev.rs`](../apps/banto-hub/core/src/bin/banto-hub-elev.rs)）。
固定6アクションのみ受理（`setup-operators` / `grant-service-acl` /
`service-install` / `service-uninstall` / `autostart-enable` /
`autostart-disable`）。`requireAdministrator` マニフェストは
`embed_resource::compile_for` で elev バイナリにのみ埋め込み。
サービス DACL は `SetEntriesInAclW` で既存 ACE を壊さず
`(A;;CCLCRPWP;;;OperatorsSID)` 相当（QUERY_CONFIG / QUERY_STATUS /
START / STOP）を追記。`install`/`uninstall` 本体は
[`service_install.rs`](../apps/banto-hub/core/src/service_install.rs) へ
移設し、`win_service` と elev の双方から呼ぶ。

**完了（下記実機検証で確認済み）**: Operators メンバーでの start/stop 実機受け入れ
（「Windows 実機検証」表・「Operators 委任」節参照）、T16-2 への
`can_operate_service` 配線（`apps/banto-hub/src-tauri/src/lib.rs` の
`AppState::can_operate_service`、T16-2 第二スライスで Operators **または**
Administrators 判定へ拡張、`host_switch.rs`/`host_switch_ipc.rs`・
トレイ操作可否ゲートへ配線済み。詳細は
[banto-hub-t16-design.md](banto-hub-t16-design.md) §3）。

未了: NSIS（`apps/banto-hub/installer/`）からの `banto-hub-elev.exe` 呼び出し
統合（インストーラフックに elev 起動処理はまだ無い）。

### Windows 実機検証（2026-08-10、管理者 Cursor）

テスト用ディレクトリ `_verify_t17/`（gitignored）に
`banto-hub.exe` / `banto-hub-elev.exe` を配置して実施。

| 項目                                                                 | 結果                                                                            |
| -------------------------------------------------------------------- | ------------------------------------------------------------------------------- |
| `banto-hub-elev` のみ `requireAdministrator` 埋め込み                | OK                                                                              |
| `setup-operators` 初回（グループ作成・対話ユーザー追加）             | OK（`TKent`）                                                                   |
| `grant-service-acl`（`sc sdshow` に `(A;;CCLCRPWP;;;OperatorsSID)`） | OK・再実行も OK                                                                 |
| `setup-operators` 2回目                                              | 初回検証時 **status 1379**（`ERROR_ALIAS_EXISTS`）で失敗 → 同日修正後 OK        |
| `is_current_process_operator`（ログオン後追加グループ）              | メンバー追加済みでも現トークンでは `false` になり得る（既知制約・要再ログオン） |
| Demand：OS 再起動後も `Stopped` / `DEMAND_START`                     | **OK**（2026-08-10 オーナー実機）                                               |
| `Start-Service` で `Running` になること                              | **OK**（同日）                                                                  |
| UAC 同意プロンプト（非昇格シェルから `banto-hub-elev`）              | **OK**（同日）                                                                  |
| 非管理者 Operators（`BantoOpTest`）での委任                          | **OK**（同日。下記「Operators 委任」節）                                        |

#### Operators 委任（2026-08-10、`BantoOpTest`、cmd）

- `sc query` — 成功（RUNNING を取得）
- `sc start` — 既に実行中のため **1056**（`StartService` 自体は受理され、
  Access Denied ではない = START 権限あり）
- `sc stop` — Access Denied にならず制御を受け付け（STOP 権限あり）
- `sc config ... start=auto` / `sc delete` — いずれも **OpenService FAILED 5
  （アクセスが拒否されました）** = CHANGE_CONFIG / DELETE は委任されていない

## 11. T17-4 実装メモ（2026-08-10、P4「Demand 化」）

P4（§1「P4」・§6 承認済み）に沿い、`install()`の既定起動種別を
`AutoStart`（+ 遅延自動開始）から `OnDemand`（手動）へ変更した。

- **変更箇所**: `apps/banto-hub/core/src/service_install.rs::install`
  （実体はここに一本化済み - `win_service.rs::install`は薄い委譲のまま、
  §10 参照）。`ServiceInfo.start_type`を`ServiceStartType::OnDemand`に
  変更し、`set_delayed_auto_start(true)`の呼び出しを削除した（`OnDemand`
  には遅延自動開始の概念が無い - Windows API 仕様どおり、呼んでも意味を
  持たない）。案内する`println!`を「起動種別: 手動（Demand） - OS 再起動
  だけでは開始しません」「`Start-Service`または管理 UI から明示的に
  開始してください」に更新した。
- **サービス開始後の挙動は不変**: サービスが実際に開始したときに
  `win_service.rs::run_service_body`が即座に`Configured`収集を開始する
  既存ロジックには一切手を入れていない（実装指示どおり「サービスが
  動くたびに収集する」のは既定 OK のまま）。
- **自動起動を有効化する経路は変更なし**: `service_manager::
WindowsServiceManager::set_auto_start(true)`（管理 UI 等から明示的に
  自動起動 ON にする操作、T17-0 で実装済み）は従来どおり`AutoStart`+
  遅延自動開始を組み立てる。P4 の決定は「新規インストール直後の既定」
  だけを変えるもので、自動起動を選べる操作自体は残す（design §1
  「自動起動 ON は初回セットアップウィザードでの明示操作のみで有効化」）。
- **上書きインストール時の既存設定保持（§5 リスク）**: `install()`は
  SCM に同名サービス（`BantoHub`）が既に存在する場合、`create_service`
  を呼ばず**何も変更せずに早期リターン**するようにした
  （`ServiceAccess::QUERY_CONFIG`で`open_service`を試すだけの軽い
  存在確認）。これにより:
  - 既存が`AutoStart`のまま（オーナーが手動で自動起動を有効化した環境
    等）でも、アップグレード時に`Demand`へ巻き戻されることはない。
  - NSIS post-install フック（`installer/hooks/service-hooks.nsh`、
    下記参照）が毎回`install`を呼んでも、既存インストールでは実質
    no-op になり、以前のように「サービスが既に存在するとエラー終了
    （終了コード非0）」にはならない。
  - より高度な「既存の起動種別を読み取って再現する」実装（一度
    `uninstall`→再`install`するような操作をした場合に限り意味を持つ）
    は本スライスでは見送った - 実装指示の「smallest safe option」を
    採用した。
  - **既知の制約**: 一度`uninstall`してから`install`し直すフロー
    （docs/banto-hub-operations.md §10 に記載の設定変更手順）では、
    この早期リターンは効かず新規作成経路を通る（`OnDemand`で作られる）。
    これは意図どおり - `uninstall`済みなら「既存設定」はもう無い。
- **NSIS フック**（`apps/banto-hub/installer/hooks/service-hooks.nsh`）:
  post-install の案内メッセージを、Demand であることと「OS 再起動では
  開始しない」ことが伝わる文言に更新した。フック自体のロジック
  （`ExecWait`→終了コード分岐）は変更していない - 上記のとおり
  `install`側が既存サービスを検出して正常終了するようになったことで、
  結果的にアップグレード時の「失敗」表示が出なくなる。
- **`service_manager.rs`のコメント更新**: T17-0 実装時点で残っていた
  「P4 Demand 化は T17-4 で扱う（未着手）」という趣旨のコメント（モジュール
  doc・`ServiceStatusSummary::auto_start`・`MockServiceManager::new`・
  `set_auto_start`内）を、P4 実装済みである旨に更新した。挙動自体
  （`query_status`/`set_auto_start`のロジック）は変更していない。
- **ドキュメント**: `docs/banto-hub-desktop-plan.md`（状態行）・
  `docs/banto-hub-operations.md`（§10「サービスの登録（install）」の
  起動種別表・説明文、「起動確認」節）を Demand 前提の記述に更新した。
- **テスト**: `cargo fmt --all`・`cargo clippy -p banto-hub-core
--all-targets -- -D warnings`（Windows 実機、警告0件）・`cargo test -p
banto-hub-core --lib`（`--test-threads=1`で283件 green。デフォルトの
  並列実行では`profile_lock`テスト2件が`Global\BantoHub.default`
  named mutex の競合で稀に失敗することを確認したが、これは T17-1
  時点から存在する既知のテスト間競合であり本スライスの変更とは無関係
  - `profile_lock.rs`は本スライスで変更していない）。
- **Windows 実機での確認（2026-08-10、管理者 Cursor + オーナー対話）**:
  1. [x] テスト用 exe で `install` → `sc.exe qc` が
         `START_TYPE: 3 DEMAND_START`、`Get-Service` が `Manual` / `Stopped`
  2. [x] 再 `install` → 早期リターン、`DEMAND_START` 維持
  3. [x] `Set-Service -StartupType Automatic` 後の再 `install` →
         `AUTO_START` 維持（Demand へ巻き戻らない）
  4. [x] OS 再起動後も `Stopped` / `Manual` / `DEMAND_START`（オーナー確認）
  5. [x] `Start-Service BantoHub` で `Running`（オーナー確認。Configured
         収集のログ目視は任意・未必須）
  6. [x] 確認後いったん `uninstall`（Cursor 検証後）。オーナーが再登録して
         ①〜③を実施。最終片付けは④完了後
  7. [x] UAC プロンプト（非昇格から elev）— オーナー確認 OK
  8. [x] 非管理者 Operators（`BantoOpTest`）: query/start(1056=既実行)/
         stop は Access Denied にならず、`sc config`/`sc delete` は
         FAILED 5 — オーナー確認 OK

## 12. profile ACL 実装メモ（2026-08-10、desktop-plan §11・t16-design §3 実機メモの解消）

T16-2 実機検証（§3・本書 §10 参照）で発見した既知ギャップ - LocalSystem
（Windows サービス）が先に作成した `%ProgramData%\BantoHub\profiles\
<profile-id>\...` は Users が書き込めず、Desktop/shell（対話ユーザー）
起動が `attempt to write a readonly database` で失敗する - を、
新設した `profile_acl.rs` と新規固定アクション `grant-profile-acl` で解消した。
**2026-10-16 に #626 で作り直した**（`profile_acl.rs`は消して
[`profile_grant.rs`](../apps/banto-hub/core/src/profile_grant.rs)に置き換えた。
下の「実装」「非 Windows ビルド」「テスト・検証状況」は 2026-08-10 時点の記録で、
今の形は末尾の「#626 の作り直し」）。

### 実装

- **`profile_acl::grant_profile_owner_acl(profile_dir, owner_account_name)`**
  （`#[cfg(windows)]`）: `SYSTEM`/`Administrators`/指定ユーザーの SID を
  `service_operators::windows_impl::lookup_account_sid`で解決し、
  `GetNamedSecurityInfoW`→`SetEntriesInAclW`（`SET_ACCESS`、
  `grant_service_acl`と同じマージ方式）→`SetNamedSecurityInfoW`で
  `profile_dir`自身とその配下に**既に存在する**全ファイル・
  全ディレクトリへ再帰的に ACE を適用する。
- **付与する権限**（desktop-plan §11 のとおり、`Users`全体には一切
  付与しない）:
  - `SYSTEM`/`Administrators`: `FILE_ALL_ACCESS`（Full Control）
  - profile owner（指定ユーザー）:
    `FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE | DELETE`
    （NTFS の「変更(Modify)」既定値、`0x1301BF`。`WRITE_DAC`/
    `WRITE_OWNER`は含めない - フルコントロールは付与しない）
  - いずれも`OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE`付き -
    LocalSystem サービスが以後新規作成するファイルも自動的に owner
    書き込み可能になる
  - `BantoHub Operators`には一切 ACE を追加しない（SCM の日常操作権限と
    profile ファイル権限は別レイヤ、実装指示のとおり）
- **既存ファイルの修復が必須だった理由**: ACL 継承は「今後作成される
  ファイル」にしか効かない。実機バグの実体（サービスが既に作成済みの
  `config/*.sqlite3`）を直すには、`profile_dir`配下の既存ファイルへの
  再帰適用が必要 - `grant_profile_owner_acl`はこれを行う。
- **固定アクション `grant-profile-acl`**（`service_elevated.rs`、
  `banto-hub-elev.exe`）: `[username] [profile-id]`（いずれも省略可、
  省略時は現在の対話ユーザー／既定 profile "default"）。root は
  `profile_paths::resolve_hub_root`（env `BANTO_HUB_ROOT`/`ProgramData`/
  `XDG_DATA_HOME`/`HOME`）で解決する。固定アクションは6→**7種類**に
  増えた（`ElevatedAction::ALL_NAMES`）。**2026-08-31 追記**: ロックダウン
  回復用に `reset-password`・`revert-to-commissioning` が追加され、
  現在は**9種類**（`apps/banto-hub/core/src/service_elevated.rs:199-209`）。
- **`service-install`への配線**: `setup-operators`→`grant-service-acl`に
  続けて`grant-profile-acl(None, None)`（対話ユーザー・既定 profile）を
  実行するようにした - 新規インストール直後から、Desktop/Service
  どちらが先に profile を作成しても readonly にならない。

### 非 Windows ビルド

`grant_profile_owner_acl`の非 Windows 版は`Ok(())`にはせず
`ProfileAclError::UnsupportedPlatform`を返す（Windows ACL という概念
自体が存在しないため、黙って成功したと誤解させない設計 -
`service_operators.rs`等の「安全側で false」とは異なり、こちらは
「操作自体が無意味なので明示的に失敗させる」判断）。

### テスト・検証状況

- `cargo fmt --all` / `cargo clippy -p banto-hub-core --all-targets -- -D warnings`
  （Windows、警告0件）/
  `cargo test -p banto-hub-core --lib -- --test-threads=1`
  （298件 green、4件 ignore）。
- `profile_acl::tests::grant_profile_owner_acl_applies_to_new_and_existing_files`
  は`#[ignore]`を付けず通常テストとして実装した - 自プロセスが作成した
  一時ディレクトリ（したがって自分がオーナー）に対する ACL 変更は
  Windows の仕様上オーナーに常に許可されるため、管理者権限なしで
  Windows CI（windows-latest）でも実行できる。実機（Windows 開発機）で
  green を確認済み。
- **Windows 実機検証（2026-08-10、非管理者 Cursor + UAC）**:
  `%ProgramData%\BantoHub\profiles\acl-verify` を SYSTEM/Administrators
  Full + Users RX のみ（対話ユーザー ACE なし）に制限したうえで
  `banto-hub-elev.exe grant-profile-acl <user> acl-verify` を UAC 経由で
  実行（exit 0）。結果:
  - profile ディレクトリに `MSI-A13\TKent:(OI)(CI)(M)` が追加された
  - `BUILTIN\Users` は `(OI)(CI)(RX)` のまま（書き込み ACE なし）
  - 制限後にブロックされていたファイル書き込みが復旧した
  - SDDL 方針（owner Modify / Users 全体へは付与しない）と一致
- **任意の追加確認**: LocalSystem の実 `BantoHub` サービスが profile を
  先に作成した直後の Desktop Hub 起動までのフル E2E は未実施
  （上記 ACL 付与そのものは確認済み。`#[ignore]`テスト
  `service_elevated::tests::grant_profile_acl_applies_to_default_profile`
  も管理者シェルから手動実行可能）。

### #626 の作り直し（2026-10-16、オーナー決定。上の「実装」は置き換えた）

**背景**: 上の実装は、昇格したプロセスが**呼び出し元の環境変数**（`BANTO_HUB_ROOT`・
`ProgramData`）でルートを決め、**パスで**（`SetNamedSecurityInfoW`・自動の伝播・
`read_dir`の再帰）書いていた。profile の owner（標準利用者、または同じ利用者の中で
動くマルウェア）が、昇格した書き込みを任意の場所へ向けられた（#626）。調査で、
profile-id に`C:`・`x:y`を渡すと Windows の`Path::join`がドライブ相対のパスに
なる穴も見つけた（issue 本文には無い）。

**オーナー決定（2026-10-16「おすすめで」。計画の論点 1〜11 すべておすすめどおり）**:

1. DACL は**保護された形**（親から継承しない。SYSTEM・Administrators のフル、
   owner ごとに「変更」`0x1301BF`、所有者は Administrators）。書き込みは
   `banto_secret_store::acl`の`pin_dir`（辿らずに開き、持っている間は名前の変更・
   削除をさせない）と`PinnedDir::reset_acl`（ハンドルへ`SetKernelObjectSecurity`、
   自動の伝播を使わない自前の再帰。reparse point・ハードリンク・開けない子は
   飛ばして報告）だけ。
2. ルートは**マシンの環境だけ**（HKLM の`BANTO_HUB_ROOT`を展開せずに読み、無ければ
   ProgramData の既知フォルダ + `BantoHub`。`%`を含む値・相対・ドライブのルート・
   UNC・OS とプログラムの置き場・ProgramData そのものは断る）。ルートと`profiles\`は
   開いて本当のパスと所有者を確かめるだけで書かない。
3. 所有者でない利用者のシェルの health は、`profile.lock`の「読めない（拒否）」と
   「無い」を分ける（PR-C #631、t16-design §3 第三スライスの追記）。
4. **足していく**意味を保つ: 書き直す前の DACL をハンドルから読み（`read_dacl`）、
   `guard::plan_carry_over`で明示の許可・`OICI`・「変更」以上・`SidTypeUser`の
   利用者を引き継ぎ、指定の利用者を足す（`DirAccess::with_install_users`）。
   フルコントロールは「変更」に狭め、落とした ACE は 1 行ずつ出す（拒否の ACE・
   確かめられずに落とした ACE・null DACL・所有者の継承の ACE・フルの狭めは警告）。
5. `user`引数は名前か SID。名前は SID の文字列にし、`InstallUserSid::parse` →
   `LookupAccountSidW`の`SidTypeUser`の順に確かめる。既定は**トークンの利用者の
   SID**。グループ・well-known・無い名前は断る。`InstallUserSid`の記録（ChronoGazer
   の`Parameters\InstallUserSid`）は作らない（profile ごとに owner が違いうる）。
6. profile が**使用中なら断る**（`Global\BantoHub.<id>`の mutex を`OpenMutexW`で
   見るだけ）。書き直しの間は子を共有の削除なしで開くので、稼働中の Hub の -wal の
   削除・原子的な置換を共有違反にしうる。
7. profile-id は elev では**許可リスト**（英数字・`_`・`-`の 64 文字まで、先頭は
   英数字、予約名を除く）。全体の`validate_profile_id`にも`:`・予約名・末尾の`.`と
   空白を断る処理を足した（既存の id を使えなくしないよう、全体は許可リストに
   しない）。
8. `reset-password`・`revert-to-commissioning`の対象も同じ「マシンのルート＋
   許可リストの profile-id」（`profile_grant::resolve_target_profile`）。
9. その場のすり替え（FSCTL）は CI（管理者）の反証テストで確かめる（実機では
   タイミングを合わせられない）。
10. 失敗の終了コードは今どおりすべて 1。
11. ルート直下の`banto-hub-sink.toml`を`Users`が読める件は別 issue（#630）。
    → #630（2026-10-17）: ルートは触らず、サイドカーの設定の置き場を保護された
    `%ProgramData%\BantoHub\sink\` だけにした（`banto-hub-elev prepare-sink-config`、
    docs/banto-hub-installer-design.md §4.4）。

**手順**（`apps/banto-hub/core/src/profile_grant.rs`）: 確かめ（`preflight`:
profile-id → 利用者 → ルート、ファイルシステムに触らない）→ 使用中か →
ルート → `profiles\` → profile の順に開く（無ければ作る: ルート・`profiles\`は親から
継承する普通のフォルダ、profile は SYSTEM・Administrators だけの保護された DACL）→
各段で本当のパスを「1 つ上の本当のパス + 名前」と比べ、`guard::data_acl_plan`
（reparse point・フォルダでない・見知らぬ所有者）を通す → profile の DACL を読んで
引き継ぐ利用者を決める → `reset_acl(.., Recurse::Tree)`。開いたフォルダは最後まで
持つ。断ったとき自分で作ったフォルダが残れば 1 行で知らせる。`service-install`は
登録の**前に**`preflight`を済ませ、通らなければ何も登録しない（ChronoGazer の B4 と
同じ）。OS の操作は`HubElevOps`トレイトを通す。

**所有者として許す相手**（#637 のオーナーレビュー Should の拡張点）: SYSTEM・
Administrators・指定の利用者・この昇格した利用者だけ（`data_acl_plan`は利用者 1 人の
シグネチャのまま、指定の利用者を渡す）。**引き継いだ利用者は所有者として許さない**
（デスクトップ版が別の利用者で先に作った profile を初めて別の利用者へ付けるときは
`ForeignOwner`で断る。文言で「その利用者を指定して実行」を案内する）。DACL に
並べる利用者は`profile_grant::access_plan`だけが決め、`with_install_users`へ渡す
一覧と報告の行が同じ関数から出る。

**継承された owner の ACE**: デスクトップ版が先に作った profile では、作った利用者の
権利が`CREATOR OWNER`由来の**継承された** ACE で付いていることがある。計画どおり
引き継ぐのは**明示の** ACE だけで、継承された ACE は落とす。その利用者が指定の利用者
なら新しい DACL に入るので問題ない。違う利用者で所有者のままなら上の所有者の
確かめで断る。所有者が実行した管理者で、指定の利用者が別のときだけ、その管理者の
継承の ACE が落ちて開けなくなりうるので、「この利用者は開けなくなる・
`grant-profile-acl <その利用者>`を実行」の警告の行を出す。

**利用者か確かめられなかった ACE**（`AccountKind::LookupFailed`）: 計画の「`SidTypeUser`
で変更相当のものを引き継ぎ、落としたものを 1 行ずつ出す」に従い、**引き継がずに
続ける**（警告の行。名前の解決が戻ってから`grant-profile-acl <その SID>`で足し直す
案内）。昇格の境界として広げる向きの失敗ではない（落とすだけ）。

**既知の制限**: 運用ガイド §11「profile ディレクトリの権限（ACL）」の 6 点
（over-the-shoulder で既定の利用者が管理者になる・継承を切るので親や手で足した ACE が
流れない・owner でない利用者は読めない・`BANTO_DB`/`BANTO_HUB_DATA`で外に置いた
ものは対象外・Entra ID の利用者はその PC で一度サインインが要る・マシンの環境変数の
変更は再起動までサービスと食い違う）。

**テスト**:

- 全 OS（偽の`HubElevOps`、`profile_grant/tests.rs`）: profile-id の許可リストの表、
  不正な profile-id・利用者・ルートで**開く・作る・読む・書く・使用中の確かめが
  1 回も呼ばれない**表（`invalid_input_has_no_side_effects`）、`service-install`が
  確かめの失敗で登録しない、使用中なら何も触らない、開く順序と持ち続けること、
  作った後に断ったときの知らせ、各段の断る分岐の到達、所有者の許可リスト、
  引き継ぎと報告の行の golden。
- Windows の実物（一時フォルダのルート、`profile_grant/win/tests.rs`）: 外の対象は
  **継承を受け入れる DACL**（各テストで`!protected`を確かめる）。中のジャンクション・
  末尾のリンク（profile とルート）・途中の要素のジャンクション・中のハードリンク・
  その場の FSCTL の 5 種で外の DACL が変わらない。書き直した形（保護・3 ACE・
  所有者 Administrators、中のファイルはフラグなし）、ルートと`profiles\`を書かない
  こと、mutex を持っている間の使用中、本物の名前の解決で組み込み・無い名前・
  グループの SID を断ること。書き直しを伴うものは所有者を Administrators に移すので
  **管理者でだけ走る**（CI は管理者。非管理者では理由を出して skip）。一時パスは
  本当のパス（長い名前）で組み立てる。
- 反証（2026-10-16、手元・非管理者）: テストの書き直しを旧実装（パスの
  `SetNamedSecurityInfoW`の再帰）に戻すと、末尾のリンク・ルートのジャンクション・
  途中の要素のジャンクション・中のハードリンクで外の DACL が書き換わり、テストが
  落ちた。**ジャンクションについては、旧実装でも先は変わらない（測定済み）**
  （リンクそのものの DACL が書き換わっただけ。Windows 11 26300、自分が所有する一時
  フォルダ。監査の追試でも同じ）。**ディレクトリのシンボリックリンクは未測**。issue の
  問題点 3 はこの範囲で確かめた（実機の M3 は、この監査の追試で代えて省いた。2026-10-17）。ディレクトリのシンボリックリンクは測らない（この実装は種類を問わず reparse point を辿らずに開き、飛ばすか断るので、シンボリックリンクもジャンクションと同じ扱いになる。なお、標準の利用者がシンボリックリンクを作れるのは開発者モードが有効なときなどに限られる）。中のジャンクション
  のテストは、リンクを辿って書く実装を差し込むと落ちることで、検出できることを
  確かめた。本当のパスの比較・所有者と reparse point の確かめ・使用中の確かめ・
  利用者の種類の確かめ・引き継ぎを外すと、それぞれ対応するテストが落ちる。

**監査の対応（2026-10-16、fable の監査。[A] 0 件。B-1・B-5 はオーナー決定）**:

- **B-1（オーナー決定 (a)）**: `service-install`の中では、profile が**使用中のとき
  だけ** ACL の付け直しを飛ばし、警告の行を出して成功（0）にする（シェルは × で
  トレイに隠れるだけで、インストーラの`taskkill`（`/F`なし）では終わらず、上書き
  インストールで mutex が残りうるため）。登録・Operators・サービス ACL は済む。
  使用中以外の断りは今どおり 1。手で実行する`grant-profile-acl`は使用中なら断って 1。
  手順は`grant_steps`の`GrantStop::InUse`だけを分けて、`service_install`が拾う。
- **B-2**: 拒否の ACE（種類・継承を問わない）を持つ利用者は**引き継がない**（許可だけを
  引き継ぐと、保護した DACL で拒否が消えて権利が広がる）。共有の
  `guard::plan_carry_over`に`DropReason::HasDeny`を足した（chronogazer-elev は
  `plan_carry_over`を使わないので影響なし）。指定の利用者そのものに拒否があれば
  指定どおり付け、拒否が消える旨の警告を出す。
- **B-3**: 管理者でしか走らないテストは、`CI`の環境変数があるのに管理者でなければ
  panic する（hub・banto-secret-store・chronogazer-elev のゲート）。手元では skip。
- **B-5（オーナー決定: この PR で直す）**: `reset-password`・`revert-to-commissioning`は、
  DB を開く前にルート → `profiles\` → profile → `config\` を`pin_dir`で辿らずに開き
  （本当のパスを「1 つ上の本当のパス + 名前」と比べ、reparse point でないこと）、DB の
  ファイルも辿らずに開いて（`FILE_FLAG_OPEN_REPARSE_POINT`・共有の削除なし）、reparse
  point・ハードリンク（リンク数 2 以上）・本当のパスの不一致を断る。開いたものは DB を
  閉じるまで持つ（`profile_grant::open_target_profile`）。`-wal`・`-shm`は確かめない
  （既知の制限）。
- **B-6**: ルート・`profiles\`の段の`ForeignOwner`は、所有者の取り直し
  （`takeown /f <path> /a`・`icacls <path> /setowner *S-1-5-32-544`）を案内する。
  `grant-profile-acl <その利用者>`の案内は profile の段だけ。
- **C-1**: 「変更」より広い権限を狭める警告は、引き継いだ ACE だけを見る。
- **C-3**: SID の引数は`S-1-`（大文字）だけを SID として扱い、小文字の`s-1-`は
  「S-1- は大文字で」と案内して断る（`InstallUserSid::parse`と揃えた）。
- **C-5**: `banto-secret-store::acl`の doc の「自動の伝播は子のジャンクションの先にも
  書く」を、「ジャンクションの先には届かないと測定（Win11 26300）、ディレクトリの
  シンボリックリンクは未測、ハードリンクは木の外の名前にも効く」に直した。
- **C-2・C-4・C-6**: 運用ガイド §11 の既知の制限 7〜10 に足した（マウントポイント・
  `subst`を含むルートは常に断る / 任意の利用者が同名の mutex を作れば ACL の段を
  止められる / `Recurse::Tree`は profile の中をすべて同じ DACL に揃える / `-wal`・
  `-shm`は確かめない）。
- 反証（手元・非管理者）: B-2（共有の確かめを外す → 共有 2 本・hub 2 本が落ちる）、
  B-5（reparse point・リンク数・本当のパスの確かめをそれぞれ外す → 偽物と実物が
  1 本ずつ落ちる）、B-6（段の分岐を外す → 1 本）、B-1（使用中を分けない → 1 本）。

**再レビューの対応（2026-10-17、オーナーと Codex のレビュー。PR #641）**:

- **1（P1/P2）: 無い段を確かめる前にパスで作っていた**（HKLM のルートが
  `D:\data\BantoHub`で`D:\data`が外へのジャンクションのとき、外に`BantoHub`を作ってから
  断っていた）。共有の`banto_secret_store::acl::PinnedDir::create_child_dir`を足し、無い段は
  **確かめ済みの親のハンドルを基準に**`NtCreateFile`（`RootDirectory` = 親のハンドル・名前
  1 要素・`FILE_CREATE | FILE_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT`・作る段の DACL）で
  相対に作り、作ったハンドルをそのまま pin として使う。ルートの親はパスで辿らずに開いて、
  本当のパス・reparse point でない・フォルダ・ネットワークでない・`forbidden_inside`の中で
  ないことを確かめてから作る（ProgramData そのものとドライブのルートは親として認める）。
  親が無ければ何も作らずに断る（`create_dir_all`はやめた）。親を持ったまま、その場で FSCTL
  でジャンクションに変えると、相対の作成は`ERROR_CANT_RESOLVE_FILENAME`（1921）で失敗し、
  外には何もできない（手元・非管理者で測定）。chronogazer-elev の`preflight_root`・
  `grant_dir`も同じ型（パスで作ってから確かめる）で、乗り換えは別の issue にする（おすすめ
  どおり）。→ #642 で乗り換えた（docs/chronogazer-service-design.md §4 の B3-3b の実装メモ
  「作る経路（#642）」）。
- **2（P2）: 条件付き・オブジェクトの ACE の SID を読み落としていた**。`read_dacl`は種類
  5・6・9・10・11・12 の SID も読む（winnt.h の並び、`AceSize`の範囲を確かめる）。
  `plan_carry_over`は拒否の種類（1・6・10・12）を拒否として数え、条件付き・オブジェクトの
  許可（5・9・11）は`ConditionalAllow`で引き継がず、種類の分からない ACE・SID を読めない
  ACE は`CarryOver::blocked`に入れ、Hub は**引き継ぎを決めずに断る**（1）。ChronoGazer と
  `FileSecretStore`の`check_dacl`は許可（種類 0）・`OICI`以外を全部食い違いとして断るので、
  読み方が変わっても安全側は崩れない。
- **3（Codex P2）: 使用中の確かめが一度きりだった**。`banto_instance_lock::try_hold_named_mutex`
  （Hub の`try_acquire`と同じ`CreateMutexW`・既定のセキュリティ記述子で、診断ファイルを
  書かない）で profile の mutex を**取って、書き直しが終わるまで持つ**。持っている間は Hub が
  `AlreadyHeld`（または拒否）で起動に失敗する。全部のハンドルが閉じれば mutex は消えるので、
  昇格した管理者が作った mutex が後の Hub（LocalSystem・利用者）に残ることはない。
  `service-install`の中で使用中なら飛ばす扱い（B-1）は変えていない。
- 反証（手元・非管理者）: ルートの親の確かめを外す → 偽物 1・実物 1 が落ちる。作成をパスに
  戻す（`create_dir_all` + `pin_dir`）→ その場で親をジャンクションにする実物のテストが
  落ちる。拒否の種類を 1 だけに戻す → 共有 2・実物（`XD`を付けて読み戻す）1 が落ちる。
  Hub の`blocked`の確かめを外す → 1。mutex をすぐ放す → 3 が落ちる。

**実機確認の結果（2026-10-17、オーナー昇格、main=f8e588ee のリリースビルドの `banto-hub-elev`）**:
試験用のルートは、マシンの `BANTO_HUB_ROOT` を一時的に `D:\bh626\root` に向けて使い、終わってから値を消した。

- M5（確かめる前の副作用）: 利用者 `Everyone`・`Users`・`SYSTEM`・無い名前・`S-1-1-0`・小文字の `s-1-…`、profile-id `C:`・`x:y`・`nul`・`default.`・`..`・空白入り、ルート `%SystemDrive%\x626`・`C:\`・`C:\Program Files\x626`・相対パス・親が無いルートで、どれも終了コード 1 と理由の行。`D:\bh626` の中身は前と同じで、`C:\x626`・`C:\Program Files\x626`・無い親も作られなかった。
- M6a（ルートから作る）: 0。ルートと `profiles` は親から継承する形（所有者 Administrators）、profile は `D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;0x1301bf;;;<利用者>)`・所有者 Administrators。2 回目も 0。
- M1（途中の要素）: `profiles` がジャンクションのときも、ルートそのものがジャンクションのときも、「reparse point です…止めます」で 1（書く前に断る）。
- M2（末尾のリンク）: profile そのものがジャンクションなら 1。
- M4（自前の再帰）: 0。中のジャンクション・ハードリンク・Administrators を拒否した子は、それぞれ「飛ばしました」の警告の行（3 件のまとめの行も出る）。普通の子は、フォルダが `OICI` の 3 ACE、ファイルが継承フラグ無しの 3 ACE、所有者 Administrators。
- M7（稼働中）: サービスが動いている間は「使用中です（Global\BantoHub.default）」で 1、何も変えない。
- M6b（本来の場所の `default`）: 0。継承されていた `CREATOR OWNER`・`Users` の ACE は「引き継ぎません」の行を出して外れ、3 ACE だけになった（DB も同じ）。その後サービスを起動して WAL に書き込み、新しくできた `-wal` も継承で 3 ACE になった。昇格していないインストール利用者が `config` にファイルを書いて消せることも確かめた。
- M1・M2・M4 のリンクの先（外の対象の DACL）が変わらないことの比較は、確認のスクリプトの不備で結果が出なかった。M1・M2 は書く前に断るので外へ書く経路は無く、M4 の先が変わらないことは CI（管理者）の `an_inner_junction_target_is_not_rewritten`・`a_hardlink_target_is_not_rewritten` で確かめている。
- M3（旧バイナリとの比べ）は、旧実装でもジャンクションの先は変わらないことを PR-B の監査で追試済みなので、監査の追試で代えた（実機では測っていない）。ディレクトリのシンボリックリンクは測らない（上の「テスト」の項）。
- M8（上書きインストール）・M9（owner でない利用者のシェル）・M10（over-the-shoulder）と、レビューの Should 1（書き直しの間に起動した Hub の失敗の文言）は、B3-4 のインストーラの実機確認とまとめて行う（2026-10-17 オーナー了承）。追跡は #392（B3-4）のコメントに TODO として書いた。
- 気づき: 断ったときの最後の行の接頭辞が二重（`banto-hub-elev: banto-hub-elev: …`）。#642 で直した（下の「#642 の追補」）。

**#642 の追補（2026-10-17、#641 のオーナーレビューの Nit 1〜5 と実機確認の Nit 6。どれも実害なし）**:

- **Nit 5（作ろうとしたら既にあった段）**: `open_level`は、`create_child`が`Ok(None)`（確かめて
  から作るまでの間に誰かが作った）の段を、パスではなく**親のハンドルから相対で**辿らずに開き
  直す（`HubElevOps::open_child` → 共有に足した`PinnedDir::open_child_dir`。`create_child_dir`と
  同じ`NtCreateFile`の`RootDirectory`で`FILE_OPEN | FILE_OPEN_REPARSE_POINT`。ファイル・
  ジャンクションでもそのものを開くので、開いた後の本当のパス・属性・所有者の確かめは今どおり）。
- **Nit 2（相対で作る・開く名前）**: 共有の`guard::is_plain_child_name`で、区切り・`:`・`.`/`..`に
  加えて末尾の`.`と空白・`* ? < > | "`・予約デバイス名も断る（`NtCreateFile`は Win32 の正規化を
  通らないため）。
- **Nit 4（`COM0`・`LPT0`）**: 予約デバイス名の判定を共有の`guard::is_reserved_device_name`へ移し
  （`profile_paths::is_reserved_device_name`はその再公開）、`COM0`・`LPT0`も断る。影響: elev の許可
  リスト（`validate_strict_profile_id`）に加えて、**全体の`validate_profile_id`**も`com0`・`lpt0`
  （`lpt0.x`なども）を断るようになる。ホスト（console・service・shell）は`BANTO_HUB_PROFILE`が
  断られると警告を出して`default`を使い、`http_hub_health`の照合は「別の profile」として扱う。
  ChronoGazer の比べ用のハーネス（`p6-<pid>-<時刻>`）・既定の`default`には当たらない。
- **Nit 6（最後の行の接頭辞）**: `ElevatedError`の文言はほとんどが`banto-hub-elev: `で始まるのに、
  bin が`banto-hub-elev: {err}`で出していた（`grant-profile-acl`に限らず、頭のある失敗はすべて
  二重）。`service_elevated::failure_line`（純関数、golden）が頭の無い文言（`transparent`で包んだ
  `ProfileIdError`・`banto-hub: `で始まるサービスの文言）にだけ 1 回付け、bin はそれを出す。
- **Nit 1・3**: `banto-instance-lock`のテストの doc コメントの位置、`profile_grant.rs`の「論点 1〜11」。
- 共有: ルートの親の確かめの判断を`guard::check_root_parent`（型の付いた理由）へ移し、
  `root_parent_problem`は文言だけにした（chronogazer-elev と共有するため。文言は不変）。
