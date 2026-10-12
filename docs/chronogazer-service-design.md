# ChronoGazer のサービス化（#392 段階 B）設計・計画

作成日: 2026-10-12
状態: **計画確定（2026-10-12 オーナー決定「おすすめで」）、B1-1（engine 抽出）・B1-2（デスクトップの乗り換え）と起動時の対処の窓（B1-2 の追補）実装済み・B1-3 以降は未実装**。#588 は実装済み。B1-1 で `chronogazer_core::engine`（`EngineRuntime`）を新設し `banto-serve` を乗り換え、B1-2 でデスクトップ（`src-tauri`）も乗り換えた。起動に失敗したときは panic せず、エラーの窓で対処を選ばせる（§4 の追補）。
最終更新: 2026-10-12（起動時の保存先の試し書きと「もう一度確かめる」を §4 の追補に追加。同日の追加のオーナー決定）。2026-10-12（起動時の対処の窓のオーナー決定と実装: §4 の B1-2 の実装メモの後に追補）。2026-10-12（B1-2 を実装: §4 の B1-1 の実装メモの後に B1-2 の実装メモ。§1.2 の `ExitCleanupOnce` の行は済み）。2026-10-12（B1-1 を実装: §4 の表の後に実装メモ）。2026-10-12（初版。調査の基準は main=970993e。論点 1〜14 のオーナー決定と H9 の決定（既存の `%APPDATA%` のデータは移行しない）を記録）

[plan.md](plan.md) §5 の #392（H6〜H11）の段階 B を、実装できる粒度に落とした計画書。R0 の「UI と独立」
（[recorder-requirements.md](recorder-requirements.md) §4）を B で満たす。banto-hub 側の前例は
[banto-hub-t16-design.md](banto-hub-t16-design.md)。

**オーナー決定（2026-10-12）**:

- **H9（データの置き場所を ProgramData へ）: 既存の `%APPDATA%` のデータは移行しない（不要）**。
- §5 の論点 1〜14 は**すべて推奨どおり**（「おすすめで ok」）。
- 順序: #588 を先に行い、その後 B1-1 から。論点 13 は issue #589 として起票済みで、B3-1 で直す。

## 0. 結論（要約）

- **B1**: 本番用の headless host は `banto-serve` の本番化ではなく **新しい bin `chronogazer-server`**。`banto-serve` は E2E 10 本以上が使う開発用の入口なので、既定（`0.0.0.0`、`./banto-dev.sqlite3`）を変えずに残す。先に Tauri（`src-tauri/src/lib.rs` の `setup`）と `banto-serve` の `main` に重複する組み立て（約 300 行）を `chronogazer_core` のライブラリ境界 **`EngineRuntime`** に寄せる（banto-hub の `HubRuntime` と同じ型）。3 つの host（デスクトップ／headless／サービス）はすべてこれを呼ぶ。
- **KeyStore（H10）**: DPAPI（`CRYPTPROTECT_LOCAL_MACHINE`）でファイルに保存する実装を **新しい共有 crate**（仮称 `crates/banto-secret-store`）に置き、`banto_hub_bootstrap::KeyStore` を実装する。将来は sink の平文 API キーや Hub の MQTT パスワードにも使える。
- **B2**: 今の Tauri アプリを banto-hub の T16-2 と同じ判定木のシェルにする。サービスが Running かつ healthy ならローカルのエンジンを起動せず `http://127.0.0.1:<port>/` へ navigate、サービスが無い・Stopped ならプロセス内で起動、遷移中・不健康なら fallback 画面。**落とし穴**: navigate 先でも Tauri は `__TAURI_INTERNALS__` を注入するので、フロントの `isTauri()`（`apps/chronogazer/src/lib/banto/environment.ts:16`）が true になり invoke 経路を選んで壊れる。判定の修正が必須（§3.2）。
- **B3**: banto-hub の SCM・elev・ACL・NSIS の実装は大半が `banto-hub-core` に BantoHub 固定で入り、sink は写しを複製している。ChronoGazer で 3 つ目の複製を作らないよう共有 crate に抽出し、hub と sink も乗り換える。
- **別件（banto-hub の既存の欠け）**: Hub と sink のサービス本体は `ServiceControlAccept::STOP` しか受けていない（`apps/banto-hub/core/src/bin/banto_hub/win_service.rs:314-318`、`apps/banto-hub-sink/src/service.rs:324`）。OS のシャットダウン時に SHUTDOWN/PRESHUTDOWN 通知が来ず最終 flush を通らない可能性が高い（#586 のサービス版）。→ issue #589。ChronoGazer は最初から PRESHUTDOWN を受ける。

## 1. banto-hub の 4 形態と、流用・共有化の判断

### 1.1 実体

| 形態                     | 実体                                                                                                         | 主なコード                                                                                                                                               |
| ------------------------ | ------------------------------------------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------- |
| コンソール               | `banto-hub.exe`（引数なし）                                                                                  | `apps/banto-hub/core/src/bin/banto-hub.rs`                                                                                                               |
| サービス                 | 同じ exe を `run-service` で起動（LocalSystem・OwnProcess）                                                  | `bin/banto_hub/win_service.rs`（dispatcher、`StoppedGate`、`describe_start_failure`、ログ）、`service_install.rs`（OnDemand で登録、既存なら何もしない） |
| デスクトップ（シェル内） | `banto-hub-shell.exe` が `HubRuntime` をプロセス内で起動し自分の HTTP UI へ navigate                         | `apps/banto-hub/src-tauri/src/lib.rs`（`decide_startup`、`attempt_desktop_start`）、`hub_slot.rs`                                                        |
| サービス接続中のシェル   | SCM が Running かつ `HttpHubHealthProbe` が Healthy なら navigate だけ                                       | `service_manager.rs`、`http_hub_health.rs`（`/api/v1/openapi.json` の `x-banto-hub-profile-id` と `profile.lock`）                                       |
| fallback                 | SCM が遷移中・不健康、またはデスクトップ起動の失敗。診断の静的ページ、トレイから「サービスを開始」「再試行」 | `lib.rs` の `render_fallback`                                                                                                                            |

- **切替**: `host_switch.rs` の `HostSwitchEngine`（不変条件「両方を動かしたまま失敗で終わらない」）。終了との競合は `HubSlot` と `ExitCleanupOnce`（#586/#587）。
- **profile**（T17 P1）: `%ProgramData%\BantoHub\profiles\<id>\{config,data,logs}` を 3 host で共有。上書きは `BANTO_HUB_ROOT`。
- **排他**: `Global\BantoHub.<id>` の named mutex と `profile.lock`（`banto-instance-lock`）。
- **権限**（T17 P3）: `BantoHub Operators` にサービス DACL で start/stop/query だけ付与。install/uninstall・自動起動・ACL は固定 9 アクションの UAC ヘルパ `banto-hub-elev.exe`。profile の ACL は SYSTEM・Administrators が Full、インストール利用者が Modify、Users なし。
- **loopback grant の前例**: `commissioning.rs`（`GrantSpec { require_loopback_peer: true, max_sessions: 16 }`）。
- **インストーラ**: `apps/banto-hub/installer/`（tauri-bundler を直接呼ぶ xtask、NSIS、PerMachine）と `hooks/service-hooks.nsh`（PREINSTALL: Running なら止めて記録・シェルを taskkill／POSTINSTALL: `elev service-install`、止めたものだけ再開／PREUNINSTALL: 登録解除、ProgramData は残す）。既定の起動種別は OnDemand（T17 P4）。

### 1.2 ChronoGazer での扱い

| 部品                                                                                                         | 判断                                                                                                                 | 理由                                                                            |
| ------------------------------------------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------- |
| `ServiceManager`・`ScmState`・`TransitionHandle`・`MockServiceManager`・`WindowsServiceManager::for_service` | 共有 crate へ抽出（仮称 `crates/banto-service-control`）                                                             | 既にサービス名を引数で受ける。chronogazer-core を banto-hub-core に依存させない |
| `service_install::install/uninstall`（sink も写し）                                                          | 抽出して引数化（`ServiceSpec { name, display, exe, args, start_type }`）                                             | 3 つ目の写しを作らない                                                          |
| `run_service_dispatcher`・`StoppedGate`・状態報告                                                            | 抽出（汎用のサービス本体ランナー。STOP と PRESHUTDOWN、停止中は `StopPending` と wait_hint）                         | PRESHUTDOWN をここで 1 回だけ実装                                               |
| `service_operators`                                                                                          | 抽出して引数化                                                                                                       | ChronoGazer 用のグループ名                                                      |
| `service_elevated` の Win32 部分                                                                             | ライブラリとして抽出、昇格する exe は製品ごと（`chronogazer-elev.exe`）                                              | 昇格の境界は製品ごとに監査                                                      |
| `hub_health` の trait・`host_switch` の engine                                                               | 抽出（汎用化。probe の HTTP 実装はパスと同一性検証を引数に）                                                         | ChronoGazer は `/api/host` を見る                                               |
| `hub_slot::HubSlot<T>`                                                                                       | B2 で切替を入れるなら共有へ                                                                                          | 入れないなら不要                                                                |
| `ExitCleanupOnce`                                                                                            | ChronoGazer の `exit_cleanup_once` を `banto_desktop_tray::ExitCleanupOnce` に寄せる（#392 の残件。**B1-2 で済み**） | B2 で終了経路を触る                                                             |
| `hub_log`                                                                                                    | 抽出。ChronoGazer は stderr もログファイルへ向け直す（`SetStdHandle`）                                               | core と依存 crate が `eprintln!` を直接使う（30 か所以上）                      |
| NSIS の `StopServiceIfRunning`                                                                               | 共通 `.nsh` に切り出し両インストーラから `!include`                                                                  | H11 を同じ手順に                                                                |
| profile の概念                                                                                               | 持ち込まない（1 台 1 インスタンス、A1 の排他単位は DB の正規化パスのまま）                                           | YAGNI。ルート上書きの env だけ                                                  |

抽出だけの PR（挙動不変、hub と sink の既存テストが green）を先に出し、ChronoGazer が使う PR は後。

## 2. ChronoGazer の現状

- 収集はデスクトップの Tauri プロセス内（`src-tauri/src/lib.rs` の `setup`）。順序: A1 ロック → リストア適用 → `init_db` → 窓 → services → `HubService::new(KeyringKeyStore)` → `hub.resume` → `collect.attach_hub` → `autostart` → retention → LAN サーバー（`server.enabled` のときだけ）→ `AppState` → 監査 flusher → トレイ。画面は invoke（約 80 個の Tauri コマンド）で、REST は使っていない。
- 終了: `shutdown_app_state` が 取り消せない印 → LAN サーバー → `collect.stop`（最終 flush）→ `hub.shutdown` → `pool.close` を予算 5 秒で。
- `banto-serve` は開発・E2E 用。本番との差: 既定 bind `0.0.0.0`・DB `./banto-dev.sqlite3`、`HubService::shutdown`・`pool.close` を呼ばない、起動失敗を `expect`/`exit(1)`、ログはコンソールだけ、SCM 連携なし、KeyStore は `UnavailableKeyStore`（既定）、終了は Ctrl-C だけ。**本番に使える度合いは起動配線の参考まで**。
- `BANTO_HUB_MEMORY_KEYS=1`: 開発・E2E 専用（`MemoryKeyStore`＋`attach_hub`）。`e2e/tests/user-simulator-monitor-hub.spec.ts` が依存。
- 自動ログイン: ログイン不要モード → 自動ログイン（OS キーリングのパスワード）→ ログイン画面。
- データ: `%APPDATA%\dev.tyaro.chronogazer\`（`chronogazer.sqlite3`、`data\`、`backups\`、`audit-spool\`、`tray-hint-shown`）。`data.dir` は絶対パスも可。
- A1: 排他単位は DB の正規化パス、mutex `Global\ChronoGazer.<hash>`、host 種別は `Desktop` と `Serve`。A2: トレイ常駐と単一インスタンス。ログオン時の自動起動なし。
- REST に無い Tauri 専用の操作: LAN サーバー設定、認証モード、自動ログイン、Acrylic、バックアップフォルダを開く、汎用の `settings_get/set`、初回セットアップ。
- 配布: `tauri.conf.json` の bundle は既定（NSIS currentUser）、専用インストーラなし。

## 3. 設計案

### 3.1 B1: headless host と KeyStore

**(a) `chronogazer_core::engine`（新設）**: `EngineConfig { db_path, data_base, key_store, host, listen, allow_setup, seed_viewer_public }` → `EngineRuntime::start(cfg) -> Result<RunningEngine, EngineStartError>`。A1 ロック（DB に触れる前）→ リストア → `init_db` → services → hub（`attach_registry`、`attach_hub`）→ resume/autostart/retention → listener → 監査 flusher。失敗は `Result`。`RunningEngine` は各 service のハンドルと `shutdown(budget) -> ShutdownReport`（今の順序、最初に `begin_shutdown`）。LAN の listener は入れ替えられる `ServerSlot`。最初の PR は `banto-serve` だけを乗り換え、Tauri は別 PR。

**(b) 新しい bin `chronogazer-server`**: B1 はコンソールモードだけ（`run-service`/`install`/`uninstall` は B3、1 つの exe）。

- データ: 既定 `%ProgramData%\ChronoGazer\`、`CHRONOGAZER_ROOT` で上書き（空文字は無視、純関数で非 Windows でもテスト）。

```
%ProgramData%\ChronoGazer\
  chronogazer.sqlite3, chronogazer.sqlite3.instance.lock
  data\
  backups\chronogazer.sqlite3\  audit-spool\chronogazer.sqlite3\
  logs\chronogazer-server-YYYY-MM-DD.log
  secrets\hub-keys\<sha256(account)>.bin
  run\endpoint.json   （port・pid・scope・version。シェルの probe 用。Users に読み取り可）
```

- **H9: 既存の `%APPDATA%` は読まないし移さない**（消さない）。旧データの案内（決定 3）は **このエンジン（サービスでは LocalSystem で動き、利用者の `%APPDATA%` を指さない）ではなく、利用者として動くシェル（B2-4）が行う**（2026-10-12、#590 のレビュー）: シェルが対話ユーザーの `%APPDATA%\dev.tyaro.chronogazer\chronogazer.sqlite3` の存在を調べ、画面で 1 回だけ案内する。案内済みの印は**利用者ごと**（その利用者の `%LOCALAPPDATA%` 配下）に持つ。インストーラも、インストールした利用者の旧データを見つけたら完了画面で案内する。
- `ChronoGazerHost` に `Server` と `Service` を追加。
- 待ち受け: **loopback の listener は常に開く**（`127.0.0.1:<server.port>`）。`server.enabled` のときだけ LAN 用を足す（`0.0.0.0` なら 1 本にまとめる）。既定 port 8721（Hub の 8722 と衝突しない）。認証無効と LAN の不整合の判定はデスクトップと同じ。
- `GET /api/host`（未認証、新設）: `{ product: "chronogazer", hostKind, version, scope }`。シェルの probe が別製品・別 DB・port 競合を見分ける。
- KeyStore は常に `DpapiFileKeyStore`、`attach_hub` も常に。**`BANTO_HUB_MEMORY_KEYS` はこの bin では読まない**（`banto-serve` 専用のまま）。
- 終了: Ctrl-C・Ctrl-Break は `shutdown`（予算 15 秒）。**コンソールを閉じたとき（`CTRL_CLOSE_EVENT`）は Windows が待つ時間が約 5 秒しかない**ので別の経路・別の予算にし、OS の猶予内（例 4 秒）で最終 flush まで終える（2026-10-12、#590 のレビュー）。B1-5 の受入条件に「コンソールの × で閉じても `collection_stopped` と最後の行が残る」を実機で含める。
- ログ: `logs\` に日付つきファイル、stderr も向け直す。保持日数は設定値（初期案 30 日）。

**(c) `crates/banto-secret-store`（新設、H10）**: `Protector` trait、Windows 実装 `DpapiMachine`（`CRYPTPROTECT_LOCAL_MACHINE | CRYPTPROTECT_UI_FORBIDDEN`、`LocalFree`、平文は `zeroize`）、テスト用 `PlainProtector`。`FileSecretStore<P>`: 1 アカウント 1 ファイル（名前は `sha256(account)`）、中身はマジック `BSS1`・版数・アカウント・blob、entropy は `"banto-secret-store/v1:" + account`（ファイルを入れ替えても復号できない）、書き込みは一時ファイル → `MoveFileExW(REPLACE_EXISTING|WRITE_THROUGH)`、削除は「無ければ Ok」、プロセス内は `Mutex`。ディレクトリは継承しない保護された DACL（SYSTEM と Administrators が Full、**インストール利用者の SID に Modify**。決定 4 - 非昇格のデスクトップ host が同じキーを読み書きするため。Users や Operators 全体には付与しない）。作成・更新のたびにこの DACL を保つ（呼び出し側が指定したインストール利用者の SID を受け取る）。`impl KeyStore`、失敗は秘密を含めない。非 Windows は `UnsupportedPlatform` で fail closed。

**(d) テスト方針**: engine は既存の `collect_roundtrip.rs` と E2E が green、start の失敗が `Result`、shutdown の順序、ロック競合で DB に触れない。`chronogazer-server` はルート解決の全分岐、`ListenPlan`、`/api/host`、一時ルートで dev_plc から収集する統合テスト。secret-store は形式・原子的置換・entropy 取り違えの拒否・削除の冪等性（全 OS）、DPAPI の往復（Windows）、再起動後に Hub の resume が張れること（Windows、2 回起動）。

### 3.2 B2: シェルの決定（概略）

- 起動判定は T16-2 と同じ（Windows のみ）: Running かつ Healthy → navigate（`ShellView::Service`）、Running だが不健康・遷移中 → fallback、Stopped・NotInstalled・非 Windows → プロセス内で `EngineRuntime`（`ShellView::Desktop`）。**SCM が遷移中なら絶対にローカルで起動しない**（A1 は最後の防衛線）。
- **デスクトップ host も ProgramData を使う**（hub の T17 P1 と同じ）。そうしないとサービスとデスクトップで DB が分かれ、同じ PLC を二重にポーリングし履歴も割れる。
- **フロントの判定の修正（必須）**: `isTauri()` を「Tauri の内部オブジェクトがある かつ origin が Tauri 自身のアセット」に。サービス接続時は HTTP の `server` モード。capabilities の `remote.urls` は付けない。
- **H8: loopback grant**（B2-1）: kind `localDesktop`、`require_loopback_peer: true`、`max_sessions` 小さく、**Host がループバックであることも確かめる**（DNS rebinding 対策）。未初期化（ユーザー 0 人）の間は admin、初期化後は設定 `auth.local_grant`（off / viewer / editor / admin）、既定 off。フロントは `status.grants.localDesktop` が true ならログイン画面の前に `enterGrant`。
- REST の差分を埋める: LAN サーバー設定と認証モードの GET/PUT（admin）。Acrylic・フォルダを開く・自動ログインはサービス接続時に非表示。
- トレイ: サービス接続時は「サービス: 実行中」「開く」「（Operators のみ）サービスを停止」「シェルを終了」（シェルを終了してもサービスは止めない）。デスクトップ時は A2 のまま。

### 3.3 B3: サービス・インストーラ・実機確認（概略）

- サービス: `chronogazer-server.exe run-service`、サービス名 `ChronoGazer`、表示名「ChronoGazer 記録エンジン」、LocalSystem・OwnProcess。共有ランナーで STOP と **PRESHUTDOWN**（タイムアウト例 30 秒、`StopPending` と wait_hint）。起動失敗の終了コード（ロック競合 = 2、その他 = 3）。
- 起動種別: 新規は OnDemand、UI／elev で「Windows 起動時に自動開始（遅延）」を ON。R0 §4 の 24/365 は自動起動 ON の運用で満たすと明記。
- elev: `chronogazer-elev.exe`（requireAdministrator）の固定アクション: `setup-operators`、`grant-service-acl`、`grant-data-acl`、`service-install`、`service-uninstall`、`autostart-enable/disable`。
- ACL: データのルートは SYSTEM・Administrators が Full、インストール利用者が Modify、Users なし。`secrets\` は継承を切り、SYSTEM・Administrators が Full、**インストール利用者の SID に Modify**（決定 4。Users・Operators 全体には付与しない）。
- インストーラ: `apps/chronogazer/installer/`（hub と同じ xtask、NSIS、PerMachine、`+crt-static`）。同梱は `ChronoGazer.exe`（シェル）・`chronogazer-server.exe`・`chronogazer-elev.exe`。フックは H11 の順序（PREINSTALL: Running なら止めて記録、シェルは**正常終了を頼んで後始末の完了を待つ**（下）／POSTINSTALL: `elev service-install`、ProgramData と ACL、止めたものだけ再開／PREUNINSTALL: 登録解除、ProgramData は残す）。
- **インストーラによるシェルの停止**（2026-10-12、#590 のレビュー）: サービスが止まっている間はシェル自身が収集エンジンを持っている（デスクトップ host）ので、`taskkill /F` では `RunningEngine::shutdown` を通らず最終 flush と `collection_stopped` を失う。WM_CLOSE は A2 の close handler が窓を隠すだけでプロセスが残る。→ インストーラ専用の**正常終了の要求**（例: シェルの single-instance 経由の引数 `--quit-for-update`、またはシェルが持つ名前付きイベント）を送り、シェルは確認ダイアログを出さずに `exit_cleanup`（後始末を 1 回）を通して終了する。インストーラはプロセスの終了を上限（例 20 秒）まで待ち、**上限を超えたときだけ**「最後の数秒の記録が失われる可能性があります」と警告してから強制終了する。B3-4 の受入条件に「デスクトップ host で収集中に上書きインストール → `collection_stopped` と最後の行が残る」「応答しないシェルは警告の後に強制終了される」を実機で含める。
- 実機確認: 新規・稼働中の上書き・アンインストールして再インストール、OS の再起動（OnDemand と自動起動）、サインアウトしても収集が続く、シャットダウンで最終 flush と `collection_stopped`、Hub と同居、**24h → 72h の soak**（サービスの working set、行の抜け、`null`）。

## 4. PR の分け方と受入条件

| PR                                      | 内容                                                                                                             | 受入条件                                                                                                                                                                                                                                                                |
| --------------------------------------- | ---------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| B1-0 docs                               | この計画と決定の記録                                                                                             | オーナー承認、H9 の決定の反映                                                                                                                                                                                                                                           |
| B1-1 engine 抽出                        | `chronogazer_core::engine`、`banto-serve` を乗り換え、shutdown を本番と同じ順序に                                | 既存の Rust テスト・E2E・mini_soak が green、`banto-serve` の env と既定値は不変、shutdown 順序の単体テスト                                                                                                                                                             |
| B1-2 Tauri を engine に                 | `setup`・`shutdown_app_state` が `EngineRuntime` を使う、`exit_cleanup_once` を共有 `ExitCleanupOnce` に         | Windows CI green、手動で起動・トレイ終了・サインアウトで flush、挙動不変                                                                                                                                                                                                |
| B1-3 `banto-secret-store`               | 共有 crate、DPAPI machine、ファイル形式、原子的置換、KeyStore adapter                                            | 全 OS で形式・置換・entropy、Windows で DPAPI の往復、秘密がログ・エラーに出ない                                                                                                                                                                                        |
| B1-4 パスと host 種別                   | `chronogazer_core::paths`、`ChronoGazerHost::{Server,Service}`、`GET /api/host`、`run\endpoint.json`             | ルート解決の全分岐、`/api/host`                                                                                                                                                                                                                                         |
| B1-5 `chronogazer-server`（コンソール） | ListenPlan、DPAPI KeyStore、`attach_hub` 常時、ログ、コンソールの Ctrl                                           | dev_plc から収集する統合テスト、Hub の再起動後 resume、デスクトップと同じ DB で A1 拒否、%APPDATA% に触れない                                                                                                                                                           |
| B2-1 loopback grant と REST の差分      | `localDesktop` grant、LAN と認証設定の REST                                                                      | 非 loopback の peer・Host・LAN 経由は 403、未初期化の間だけ admin、off なら 403、監査に `via: localGrant`                                                                                                                                                               |
| B2-2 フロントのサービス接続モード       | `isTauri()` の origin 判定、server モード、grant に自動で入る、デスクトップ専用設定を非表示                      | vitest、E2E（`chronogazer-server` 相手に grant で入る → 監視画面）                                                                                                                                                                                                      |
| B2-3 共有 crate の抽出（制御側）        | `banto-service-control`、hub は re-export で挙動不変                                                             | hub-core・hub-shell のテスト全部 green                                                                                                                                                                                                                                  |
| B2-4 シェルの起動判定                   | T16-2 の判定木、navigate、fallback、トレイ 3 態、デスクトップ host を ProgramData へ、KeyStore を決定 4 のとおり | 判定の純関数テスト（SCM × health の全組合せ）、実機で Running → 接続・Stopped → プロセス内・StartPending → fallback                                                                                                                                                     |
| （任意）B2-5 切替                       | `HostSwitchEngine` をトレイに配線                                                                                | hub と同じ不変条件、実機で往復                                                                                                                                                                                                                                          |
| B3-1 サービスランナーの共有化           | 共有ランナー（STOP と PRESHUTDOWN）、install/uninstall の引数化、hub と sink を乗り換え（#589 を直す）           | 純ロジックのテスト、hub と sink の既存テスト green                                                                                                                                                                                                                      |
| B3-2 ChronoGazer のサービス化           | `run-service`/`install`/`uninstall`、PRESHUTDOWN の予算、ログ                                                    | 実機で install → Start-Service → Session 0 で収集 → Stop-Service で最終 flush と `collection_stopped`、ロック競合で終了コード 2                                                                                                                                         |
| B3-3 elev と ACL                        | `chronogazer-elev.exe`、Operators、サービス ACL、データと secrets の ACL                                         | 実機で UAC、`sc sdshow`、非管理者の Operators で start/stop、Users に書き込み ACE なし、`secrets\` はインストール利用者以外の非管理者から読めない、**非昇格のインストール利用者で Desktop → Service → Desktop と切り替えても同じ Hub のキーを使い続けられる**（決定 4） |
| B3-4 インストーラ                       | `apps/chronogazer/installer`、共通 nsh、`build-release.ps1`                                                      | 実機で新規・稼働中の上書き・アンインストール（ProgramData は残る）・再インストール                                                                                                                                                                                      |
| B3-5 実機確認と docs                    | OS 再起動・サインアウト・シャットダウン・Hub と同居・24h → 72h soak                                              | R0 §4（72h で欠測ゼロ、メモリ増加なし）、結果を docs に                                                                                                                                                                                                                 |

**B1-1 の実装メモ（2026-10-12、実装済み）**:

- 起動は `EngineRuntime::open`（A1 の排他 → リストア予約の適用 → `init_db`）と `OpenedEngine::start`（サービス → Hub → 収集 → listener → 監査の定期の流し込み）の 2 段。デスクトップは今どおり DB を開けた後・サービスの前に窓を作るため、B1-2 では間で窓を作る。B1-2 の呼び方の見通しは `apps/chronogazer/core/src/engine.rs` のモジュール doc。
- LAN の listener は **bind（ポートの確保）だけを自動開始より前**に行い、serve は後にした（§3.1(a) の順序からの変更）。`EngineListen::Required` で bind できなければ、収集・Hub の張り直し・保持期間の掃除を始めずに `Err` で戻る（失敗して戻る経路にタスクを残さない）。デスクトップの LAN の自動起動は `EngineListen::BestEffort`（失敗しても起動を続ける）。
- 終了の予算は呼び出し側が渡す（デスクトップ 5 秒、`banto-serve` 15 秒）。済んだ手順は `ShutdownReport::completed` に順に残る。A1 の排他は `shutdown` では返さず、`RunningEngine` の drop で返す。
- `banto-serve` の環境変数と既定値は不変。変わったのは終了の順序（Hub の購読の停止と pool を閉じる手順が加わった）と、起動の失敗（Hub の初期化・ポートの使用中）が panic ではなく終了コード 1 になったこと。

**B1-2 の実装メモ（2026-10-12、実装済み）**:

- `src-tauri` の `setup` は `EngineRuntime::open` → 窓（今までどおり DB を開けた後）→ `opened.events()` の webview への転送 → `lan_autostart_allowed` で `with_listen(BestEffort|Disabled)` → `start` → 自動ログイン（`initial_desktop_session` に切り出した）・Acrylic → `AppState` → トレイ。A1 の排他・旧形式の DB はエラーの窓（今までどおり）、それ以外の起動の失敗は今までどおり panic。
- `AppState` はセッションまわり（`auth`・`auth_io`・`auth_config_lock`）と `services: EngineServices`・`server: Arc<ServerSlot>`（エンジンの slot）だけを持つ。`server_apply` は `ServerSlot::lock()`、LAN のルーターは `EngineServices::router(false)`。重複していたサービスの組み立て・`start_embedded_server`・監査の流し込みのハンドル・A1 のガードの管理状態は消した。Tauri のコマンドの中身は `state.x` → `state.services.x` の機械的な置き換えだけ。
- 終了: `shutdown_app_state` は `RunningEngine::shutdown` を `ExitCleanupReport` に写すだけ。1 回化は `banto_desktop_tray::ExitCleanupOnce` に寄せ、banto-hub の `run_exit_cleanup` と同じ形（本体の結果の記録はワーカーの中で公開の前に 1 回、待ちの打ち切りは各呼び出しが記録）。予算は今までどおりトレイの終了・`RunEvent::Exit` とも 5 秒。待ちが期限で切れたときもトレイには `timed_out` を返し、#585 の失敗のダイアログの情報を落とさない。トレイの終了は条件変数で待つので tokio のワーカーではなく専用のスレッドから呼ぶ。
- engine に `EngineConfig::store_config_required` を足した（下の差 (1)）。
- B1-1 の engine の doc が挙げた、B1-1 より前のデスクトップとの差の扱い:
  1. 保存設定（`data.dir`）を読めないとき: engine は既定値で続行していたが、**デスクトップは今までどおり起動を止める**（`store_config_required: true`、`EngineStartError::StoreConfig` で panic。既定値で続けると別の場所を設定していた利用者の時系列データを違う場所へ書き始めるため）。`banto-serve` は既定値で続行のまま。
  2. 起動時にリストアを適用したときの標準出力の 1 行: 受け入れた（リリースビルドのデスクトップは標準出力が見えない。監査の `restore_applied` は今までどおり）。
  3. 自動ログインが Hub と収集の組み立ての後: 受け入れた（どちらも spawn して投げっぱなし、`AppState` はその後に作るので Tauri のコマンドはまだ受けない。監査の `login` の記録の時刻が数ミリ秒遅れるだけ）。
  4. LAN の bind が自動開始の spawn より前: 受け入れた（serve は後。bind の失敗は今までどおりログだけで起動を続ける）。
  5. 保持期間の掃除を終了で abort: 受け入れた（以前はプロセスの終わりまで放っておいた。消すのは古い日付のファイルだけで、途中で止めても次の起動で続きを消す）。
- ほかの観測できる差: 終了の後始末の結果を標準エラー出力に 1 行出す（「終了の後始末が完了しました（トレイの終了）」など。banto-hub と同じ）。
- 検証: Windows の実機で、試験専用の identifier（`dev.tyaro.chronogazer.b12check`）のビルドを開発用 PLC 相手に動かし、(a) トレイの「終了」・(b) `WM_QUERYENDSESSION` → `WM_ENDSESSION` の模擬の両方で `collection_stopped` と終了直前の行が残り `-wal`/`-shm` が畳まれること、(c) `banto-serve` が同じ DB を使用中のときのエラーの窓と、2 つ目の起動でその窓が前に出ること、(d) 保存済みの LAN のポートが塞がっていても起動を続けること・Tauri の `server_apply` での入れ切り・2 つ目の起動で隠した窓が前に出ることを自動で確かめた。OS の実際のサインアウトはしていない（B3-5）。

**追補: 起動に失敗したときのエラーの窓と対処の選択（2026-10-12 オーナー決定・実装済み）**:

オーナー決定（2026-10-12）: 起動時に止まる失敗で、**panic で落とさず、エラーの窓を出して利用者に対処を選ばせる**。
どの選択肢でも**今の DB や記録を消さない**。

| 起きたこと                                                                                          | 選べる対処                                                                                                                                                                       |
| --------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| データの保存先（`data.dir`、`EngineStartError::StoreConfig`）が読めない                             | ① 保存先のフォルダを選び直す（フォルダ選択ダイアログ、設定に保存して起動を続ける）② 既定の場所に戻して起動する（記録は元の場所に残り画面からは見えなくなる旨を先に伝える）③ 終了 |
| DB そのものが開けない・壊れている（`Database` のうち旧形式以外、`Hub` の初期化の失敗など）          | ① バックアップから戻す（一覧から選ぶ。今のリストアと同じ仕組み・安全策）② 新しく作り直す（今の DB は `chronogazer.sqlite3.broken-<日時>` へ名前を変えて残す）③ 終了              |
| 保存先に書き込めない（USB の取り外し・権限・ドライブが無い。起動時の試し書き、2026-10-12 追加決定） | もう一度確かめる（差し直した等の後に同じ保存先でやり直す）・① 選び直す・② 既定に戻す・③ 終了                                                                                     |
| 既に起動中、旧形式の DB                                                                             | 今のエラーの窓のまま                                                                                                                                                             |

- ① ② のうち作り直す・戻す・既定に戻すは確認を 1 回はさむ。対処が失敗したら（フォルダに書けない・バックアップが壊れている・名前を変えられない）同じ窓に理由を出して選び直せる。
- DB が開けない理由が権限・他のソフトのロックでも「作り直す」は出す（2026-10-12 オーナー決定: ロック中なら名前の変更が失敗して理由が出るだけで、何も消さない）。
- 判断（失敗 → 窓と選択肢）は純関数にして Tauri から切り離す（B2 のシェルでも使う）。表に当てはまらない失敗は「③ 終了」だけの窓（理由とログの場所）。

実装メモ（2026-10-12）:

- **判断と対処は `chronogazer_core::startup_recovery`**（Tauri に依存しない）。`classify`（`EngineStartError` → `FailureKind`）と `plan`（`FailureKind` → `WindowPlan`）は純関数で、表のテストで総当たりにした。対処は `set_aside_broken_db`（作り直す）・`restore_from_backup`（戻す）・`apply_store_dir`（選び直す・既定に戻す）。窓は `src-tauri` の `startup` モジュールと `static/startup-recovery.html`（今までのエラーの窓 `legacy-db.html` と同じく、コマンドを呼ばない静的ページ）。
- **分類**: DB の失敗は SQLite の文言で分ける（`file is not a database`・`malformed`・`corrupt` → 壊れている、それ以外 → 開けない）。sqlx の `VersionMissing`（この版が知らない migration が当たっている = 新しい版の ChronoGazer が使った DB）は**「終了」だけ**にした（壊れていない DB を脇へ退けないため。その版で開くよう案内する）。`Hub` の初期化と、`start` の前の設定（認証・LAN）の読み取りの失敗は「設定 DB を読めない」として DB の窓。保存設定の失敗は、読めないのが `data.dir` そのもの（`StoreConfig::data_dir_unreadable`、engine が pool を閉じる前に確かめる）で DB が壊れていないときだけ保存先の窓、`data.dir` は読めるが保持期間などが読めないときは DB の窓（開けない）。
- **作り直す**: `-wal`・`-shm`・`-journal` を**先に**、本体を最後に `<DB 名>.broken-<日時>`（ローカル時刻の `YYYYMMDD-HHMMSS`、同名があれば `-1`…）へ移す（本体だけ移して古い WAL が元の名前に残ると、作り直した空の DB に当たりうるため）。途中で失敗したら移したものを元に戻す。Windows で閉じた直後のハンドルに備えて名前の変更は短く再試行する（最大約 3 秒）。
- **バックアップから戻す**: `BackupService::stage_restore_from_file`（整合性検査と必須テーブル）で予約 → 壊れた DB を作り直しと同じ形で退ける → 次の `EngineRuntime::open` の `apply_pending_restore_at_startup` が再検査して適用し、`restore_applied` を監査に残す（今のリストアと同じ経路）。退ける理由: リストアの適用は今の DB の本体だけを `pre-restore-*` に写して `-wal`・`-shm` を消すので、壊れた DB の WAL の記録を失いうる（その代わり `restore_applied` の `preRestoreBackupFileName` は空になる。`startup_recovery` の監査に退けた名前が残る）。退けられなければ予約を取り消す。`BackupService` は SQLite のハンドルを要求する（SQLite かどうかの確認だけに使う）ので、壊れた DB の代わりにメモリ上の空の pool を渡す。
- **保存先**: 書く前にフォルダに書き込めることを確かめ（作る → 小さなファイルを書いて消す）、だめなら設定を変えずに理由を返す。書くのは `data.dir` の 1 項目だけ（保持期間には触れない）。フォルダは絶対パスだけ受ける。
- **保存先の試し書き**（2026-10-12 追加決定）: `EngineConfig::store_dir_check_required`（デスクトップ `true`、`banto-serve` `false`）のとき、`start` が保存設定を解決した直後（収集を組む前）に解決済みの `data.dir` へ `check_writable_dir`（無ければ作る - 収集の開始も作るので同じ扱い - 一時ファイルを書いて消す。既存の中身には触れない）を行い、失敗なら Hub を止め pool を閉じて `EngineStartError::StoreDirUnwritable` を返す。窓は「書き込めない」（値を読めない窓と文言を分け、理由にフォルダと OS のエラーを出す）で、選択肢の先頭に「もう一度確かめる」（何も変えずに起動をやり直す）。`banto-serve` は確かめない（開発・E2E 用。今までどおり書けなければ収集の開始がエラーを状態に残す。E2E の前提を変えないため）。B3 のサービスは同じ判断（`classify`・`plan`）を使える。
- **やり直しの方式: 同じプロセスで `open`/`start` をやり直す**（`AppHandle::restart` にしない）。理由: 再起動は古いプロセスが終わりきる前に新しいプロセスが単一インスタンスのプラグイン・A1 の排他にぶつかりうること、選んだ対処を新しいプロセスへ渡す経路が要ること。失敗して戻った `open`/`start` は A1 の排他と pool を返している: `init_db` は失敗の経路でも pool を閉じてから返すようにした（以前は旧形式の判定と migration の失敗で閉じずに戻っていた）、`start` の失敗の経路は B1-1 から閉じる、`start` の前で止めるときは新設の `OpenedEngine::close`（pool を閉じるのを待ってから排他を返す）。
- **デスクトップの起動の組み立て**を `launch_desktop` に切り出し、`setup()` の最初の起動と対処の後のやり直しの両方が呼ぶ。**メインの窓は `start` の後（`AppState` を manage した後）に作るように移した**（B1-2 では `open` と `start` の間）: `start` の失敗でメインの画面を出してから対処の窓に切り替えないため。旧形式の DB で空の画面を先に出さない、という B1-2 の理由もそのまま満たし、画面のコマンドが `AppState` より先に呼ばれることも無くなる。
- **窓と Rust のやりとり**: 窓は capabilities の対象外（コマンドを呼べない）のまま、選んだ対処を `/__recovery/<対処>?file=<バックアップ>` への移動で知らせ、Rust が `on_navigation` で受け取って移動を取り消す。Rust は今出している選択肢だけを受け、処理中は二度押しを無視し、窓の × を止める（名前の変更の途中で終わらせない）。結果は同じ窓に描き直す。フォルダ選択は Rust 側の `tauri-plugin-dialog`。
- **監査**: 選んだ対処は DB を開けた後の `OpenedEngine::start` が `startup_recovery`（`resource: startup`、`detail.choice` と退けたファイル名・バックアップ名・保存先）として記録する（`OpenedEngine::with_recovery_note`）。
- **洗い出した panic と扱い**（`setup` の中）: `app_data_dir()`・`create_dir_all` の `expect` → 「終了」だけの窓。`init_db` の失敗の `panic!` → 分類して窓。`auth_config`・`server_config` の `expect` → DB の窓（`opened.close()` の後）。`start` の `Hub`・`StoreConfig`・その他の `panic!` → 分類して窓。メインの窓・トレイの作成の `?`（`setup` が `Err` を返すと `run()` の `expect` で panic）→ 「終了」だけの窓（エンジンは動いているので、窓を閉じると `RunEvent::Exit` の後始末が止める）。`tauri.conf.json` の main の窓の定義・既定アイコンの `expect` → エラーに変えた。**残した panic**: `run()` の `.build(..).expect("error while building tauri application")`（Tauri 自体・WebView2 を起動できない = 窓を出す手段が無い）と、内容が文字列だけの `serde_json::to_string` の `expect`（失敗しない）。
- **検証**: core の単体テスト 13 件（分類・窓の表、作り直し・付随ファイル・名前の衝突・失敗の巻き戻し（Windows）、バックアップからの戻しと壊れたバックアップの拒否、保存先の選び直し・既定に戻す・書き込めない場所の拒否）と src-tauri の 4 件（文言と確認の対応、JS へ渡す JSON、移動の受け付け、ページの名前）。反証: 付随ファイルを移さないようにすると 3 件、`data_dir_unreadable` を常に `false` にすると保存先のテストが落ちる。Windows の実機で試験専用の identifier（`dev.tyaro.chronogazer.recovcheck`）のビルドを CDP と Win32 メッセージで操作し、(a) `data.dir` を読めない値（BLOB）にして起動 → 窓 →「既定の場所に戻す」（確認の取り消しも）で起動を続ける、(a2) フォルダ選択ダイアログの取り消し → 選び直しで保存して起動を続ける、(b) DB を壊して起動 → 窓 → DB を別のプロセスが共有なしで開いている間の「作り直す」は同じ窓に理由（ファイルはそのまま）→ 開放後に作り直すと旧 DB と `-wal`・`-shm` が `.broken-<日時>` で残り空の DB で起動する、(c) 壊れたバックアップは同じ窓に理由 → 正しいバックアップから戻すとアカウントが戻り `restore_applied` と `startup_recovery` が残る、(d) 新しい版の DB は「終了」だけの窓でファイルに触れない、(e) 既に起動中は今までの窓のまま、を確かめた。

## 5. 論点と決定（2026-10-12 オーナー決定「おすすめで ok」= すべて推奨どおり）

論点 13 は issue #589（banto-hub・sink のサービスが OS のシャットダウン通知を受けない）として起票済み。

1. **Desktop⇔Service の切替**: B2 では起動判定と fallback だけ。切替（`HostSwitchEngine`）は後回し（任意の B2-5）。サービスだけにする案は採らない（評価用の単体配布やサービスを入れない現場で収集できなくなる。hub の決定 9 と揃える）。
2. **headless host の実体**: 新しい bin `chronogazer-server.exe`。`banto-serve` は開発・E2E 用として残す。
3. **既存の %APPDATA% のデータ（H9 で移行しない）**: 放置。初回にログと画面で 1 回だけ「旧データは使われません。不要なら手で消してください（場所）」と案内。自動では消さない。
4. **デスクトップ host の Hub API キーの置き場所**: デスクトップ host も DPAPI ファイルストアを使い、`secrets\` にインストール利用者の Modify を付ける。自動ログインのパスワードはキーリングのまま（サービス接続時は使わない）。
5. **サービスの起動種別**: インストール直後は OnDemand。UI／elev で「Windows 起動時に自動開始（遅延）」を ON。R0 §4 は自動起動 ON の運用で満たすと明記。
6. **Operators グループ**: `ChronoGazer Operators` を別に作る（製品ごとに最小権限）。
7. **名前とポート**: サービス名 `ChronoGazer`、表示名「ChronoGazer 記録エンジン」、データ `%ProgramData%\ChronoGazer\`、port 8721（設定で変更可）。
8. **loopback grant の権限**: 未初期化の間は admin、初期化後は設定 `auth.local_grant` = off / viewer / editor / admin、**既定 off**。identity は合成の `local`。ロールの変更は当面「サービスの再起動で反映」。
9. **サービスで LAN の公開設定を変えたとき**: その場で listener を入れ替える（`ServerSlot`）。loopback の listener は常時。
10. **リストアの適用**: UI に「サービスを再起動して適用」（Operators のみ。それ以外は案内文）。
11. **`data.dir` の制限**（2026-10-12 #590 のレビューで書き換え、オーナー承認）: サービス host が書き込む先は、**ACL で保護したデータのルート配下と、管理者が登録した特定のディレクトリだけ**（ドライブ単位の許可はしない - ドライブ単位では LocalSystem による任意パスへの書き込みを制限できない）。判定はパス文字列ではなく、**開いたハンドルで行う**: 各要素を reparse point（ジャンクション・シンボリックリンク）を辿らずに開き（`FILE_FLAG_OPEN_REPARSE_POINT`）、reparse point があれば拒否し、`GetFinalPathNameByHandleW` で得た最終パスが許可ルートの配下であることを確かめ、そのハンドル（またはその配下）にだけ書く（確認と書き込みの間のすり替え＝TOCTOU を防ぐ）。登録できるのは管理者（elev 経由）だけで、登録先には SYSTEM・Administrators・インストール利用者だけの ACL を付ける。ネットワークドライブは LocalSystem から見えないことも案内。B3-3 の受入条件に「許可ルートの中にジャンクションを置いても外へ書けない」「登録していない場所・ドライブ直下は拒否」を含める。
12. **Hub との同居**: インストーラは別々、SCM の依存関係は付けない（Hub が後から上がるのは resume で吸収）。同じ PC の Hub へ loopback で接続、キーは #332 の self-issue。
13. **Hub の SHUTDOWN/PRESHUTDOWN の欠け**: 別 issue（#589）にし、B3-1 の共有ランナーで一緒に直す。
14. **ファイアウォール**: インストーラは受信規則を作らない（LAN 公開は利用者の明示操作、案内だけ）。

順序（2026-10-12 オーナー決定）: #588（収集の停止の並行化）を先に行い、その後 B1-1 から。

## 6. リスク

**並行性と原子性**

- 起動時の競合: OS 起動直後でサービスが StartPending のときにシェルが起動 → 遷移中は fallback と「再試行」。最後は A1 の named mutex（Session 0 越えは `OpenMutexW` で確定済み）。
- 排他の単位は DB だけ: 解決した `data.dir` が同じだと二重書き込みを防げない既知の制約は残る（全 host が同じルートを使えば実害なし。決定 11）。
- KeyStore の書き込み: DB の `HubRecord` とキーファイルは 2 段の書き込み。片方だけ残ったときの既存 bootstrap の扱いを確認する。孤児キーは起動時に掃除しない（害が無い）。
- 停止の予算: サービスの停止は最終 flush を含む。SCM の既定の待ちに収まるよう PRESHUTDOWN と wait_hint を報告する。
- 上書きインストール中の欠測: 止めて置換して再開する間は収集が止まる（`collection_stopped` が残るので許容）。migration の失敗はサービスが起動失敗の終了コードで止まり、ログで案内。

**セキュリティ**

- **DPAPI machine スコープ**: 同じ PC 上のファイルを読めるすべてのプロセスとユーザーが復号できる。守れるのは別の PC へのコピーとオフラインの盗難だけ。**本当の境界はファイルの ACL**。VM の複製先でも復号でき、OS の再インストールでキーを失う（再発行が要る）。OS キーリング（ユーザースコープ）より広いことを明記する。
- **loopback grant の範囲**: peer が loopback なら誰でも発行を受けられる（多人数ログオン、同じ PC の他のアプリ）。**DNS rebinding** 対策で Host が loopback であることの確認を必須にする（hub の試運転 grant も同じ露出を持つので同時に見直す価値がある）。同じ PC の reverse proxy の前に置かない。
- **LAN 公開との関係**: サービスの既定は loopback だけ（`banto-serve` の `0.0.0.0` 既定を本番に持ち込まない）。LAN の listener からの要求には grant を出さない。閲覧公開とログイン不要モードの排他規則はサービスでも同じ判定。サービス host ではログイン不要モード自体を `local_grant` に置き換えることも検討。
- **LocalSystem による任意パスへの書き込み**: アプリの admin が `data.dir` やバックアップの保存先を変えると権限昇格の経路になる（決定 11）。
- 未署名の配布物: SmartScreen の警告は既知の制約（hub の 2026-08-12 決定と同じ）。

**運用**

- 既存の Tauri バンドル（currentUser）と新しい PerMachine インストーラが二重に入りうる。旧版のアンインストールを案内するか PREINSTALL で検出する。
- ログのローテーションと保持日数（72h soak で確認）。
- サービス接続時はシェルが REST にしか依存しないので、Tauri 専用の機能（Acrylic、フォルダを開く）は使えない（仕様として明記）。

## 主なファイル

- `apps/chronogazer/src-tauri/src/lib.rs`（`setup`、`shutdown_app_state`、自動ログイン）
- `apps/chronogazer/core/src/bin/banto-serve.rs`
- `apps/banto-hub/core/src/bin/banto_hub/win_service.rs`、`service_manager.rs`、`service_install.rs`、`service_elevated.rs`、`profile_acl.rs`、`host_switch.rs`、`http_hub_health.rs`
- `apps/banto-hub/src-tauri/src/lib.rs`（`decide_startup`）
- `apps/banto-hub/installer/hooks/service-hooks.nsh`
- `crates/banto-hub-bootstrap/src/state.rs`（`KeyStore`）、`crates/banto-instance-lock/src/lib.rs`、`apps/chronogazer/core/src/instance_lock.rs`
- `apps/chronogazer/src/lib/banto/environment.ts`（`isTauri()`）
