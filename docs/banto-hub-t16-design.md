# banto-hub T16 詳細設計（デスクトップシェル・タスクトレイ）

作成日: 2026-08-09
状態: **設計確定。P1〜P3 承認済み。T16-0（薄いシェル `banto-hub-shell`、
`apps/banto-hub/src-tauri`）・T16-1（トレイ状態表示）マージ済み。
T16-2 第一スライス（サービス検出・接続・native fallback・Operators
ゲート）実装済み（下記 §3 T16-2 実装メモ参照）。2026-08-10 Windows
実機でサービス接続／Desktop 起動の主要経路を確認済み（同メモ末尾）。
同日発見の既知ギャップ「LocalSystem 作成 profile の ACL」（下記 §5）は
T17 側（`profile_acl.rs`、docs/banto-hub-t17-design.md §12）で解消済み。
T16-2 第二スライス（トレイ開始/停止の`HostSwitchEngine`完了待ち・
Desktop 引き継ぎの安全化・`BANTO_BIND`対応 navigate/probe・Administrators
ゲート緩和）実装済み（下記 §3 第二スライス実装メモ、§5 参照）。
**同日 Windows 実機でトレイ「サービスを停止」→Desktop 引き継ぎと
「サービスを開始」→Service 接続を確認済み**（§3 第二スライス実機検証）。
T16-2 第三スライス（openapi 応答への profile-id 埋め込み・
`HttpHubHealthProbe`のワイヤ確認）実装済み（下記 §3 第三スライス実装メモ、
§5 参照）- これで T16-2 第一スライスの既知の gap は全て解消済み。
**切替ウィザード UI**（`/status` の Windows サービスカード＋シェル最小
invoke・実`ShellDesktopControl`・自動起動 UAC）実装済み（下記 §3
切替ウィザード実装メモ）。**2026-08-31: Windows 実機で検証し、実装時には
見えていなかった3層の不具合（remote origin ケイパビリティ・ACL 未宣言・
管理 UI の `/api/v1/*` 誤用）を発見・修正した上で Desktop→Service 切替の
成功を確認済み**（下記 §3 切替ウィザード実機検証メモ）。**Service→Desktop
の逆経路・自動起動トグルの UAC・UAC キャンセル時の挙動も 2026-09-01 に実機で
確認済み（オーナー実施）。**切替ウィザードの実機検証は完了。**
最終検証日(コード照合): 2026-08-11
最終検証日(Windows 実機): 2026-09-01（切替ウィザード UI の全経路 - Desktop→Service/Service→Desktop・自動起動 UAC・UAC キャンセル。2026-08-31 は Desktop→Service
経路、下記 §3 切替ウィザード実機検証メモ）。T16-2 シェル第一・第二スライス
自体は 2026-08-10 に検証済み（Operators 対話ユーザー、
`HostSwitchEngine`完了待ちを含む）。**Service→Desktop 逆経路・自動起動
トグルの UAC・UAC キャンセル時の挙動は 2026-09-01 に確認済み。**
基準コミット: `396e927`（main、T16-1 マージ後）。T16-1 の実装は本設計と
同じ PR（`cursor/t16-1-tray-status-e3cb`、#101）で追加。

関連: [banto-hub-desktop-plan.md](banto-hub-desktop-plan.md)
（§8 シェル / §9.9 トレイ / §10 T16 / §16.3 T16・T17）、
[tag-server-design.md](tag-server-design.md) §3.1、
[banto-hub-t14-design.md](banto-hub-t14-design.md)。

## 0. スコープと位置づけ

本書は desktop-plan §16.3「T16/T17」の未決事項のうち、**T16 着手に必要な
判断だけ**を現行コード照合に基づき詳細設計へ落としたものである。T17
（SCM 管理・プロファイル・インストーラ再設計）本体は別設計とする。

対象:

1. T0「Tauri なし」決定と desktop-plan の薄いシェル方針の整合
2. T16 のサブスライス分割（T16-0 は T17 SCM に依存しない）
3. T16-0 の composition（HubRuntime 埋め込み、WebView の origin、トレイ、
   単一インスタンス）

対象外（T16-1 以降 / T17）:

- desktop⇔service 切替の中間状態・ticket プロトコル
- native fallback のサービス開始／安全停止
- `BantoHub Operators` / UAC helper / ACL
- コード署名・WebView2 Fixed Version 同梱

## 1. 主要判断（2026-08-09 承認済み）

| ID  | 論点                     | 決定                                                                                                                               | 主な代替                                          | 影響                                        |
| --- | ------------------------ | ---------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------- | ------------------------------------------- |
| P1  | T0「Tauri なし」との整合 | T0 を「ヘッドレス exe を一次形態として残す」に再解釈し、**薄いデスクトップシェルを二次ホストとして追加**する（二重 UI は作らない） | T0 を維持しシェルを別プロセスのブラウザだけにする | tag-server-design §3.1 / installer コメント |
| P2  | T16⇔T17 依存             | **T16 の SCM fallback 受入条件は T17 後へ移す**。T16-0 はサービス非依存のシェルのみ                                                | 最小 SCM を T17-0 として先出し                    | T16 受入条件の読み替え                      |
| P3  | WebView のコンテンツ起源 | Hub の `http://127.0.0.1:<port>` を WebView で開く（axum が UI+API を同一 origin 提供）。`frontendDist` 二重配布は採らない         | chronogazer 型の invoke 二重 UI                   | CORS/API URL 分岐を増やさない               |

根拠: desktop-plan §8.1 / §16.3 の既存方針と、2026-08-09 の進行指示
（T15 完了後に T16 へ進む）により P1〜P3 を確定。**T16-0 着手可**。

## 2. T0 再解釈（P1）

### 2.1 現行記録

- `docs/tag-server-design.md` §3.1: 「Tauri は使わない — 管理 UI は静的ビルドを
  axum が配信」
- `apps/banto-hub/installer` モジュール doc: 「フル Tauri アプリ化はしない
  （オーナー決定 2026-08-06）」
- `docs/banto-hub-desktop-plan.md` §8.1: 「薄いデスクトップシェルは Hub の
  localhost URL を WebView で開く」

### 2.2 決定の意味

T0 が守ろうとしていたのは次である。

- 収集ランタイムを UI フレームワークに埋め込まないこと（寿命の分離）
- 管理 UI をデスクトップ専用に二重実装しないこと
- ヘッドレスサービス運用を一次形態として残すこと

desktop-plan のシェルはこれらを壊さない。

- `HubRuntime`（T14-1）はライブラリのままで、コンソール／サービス／シェルが
  薄いホストになる
- UI は現行 SvelteKit 静的ビルドを axum が配信したものを WebView が開くだけ
- `banto-hub.exe`（ヘッドレス）とサービス経路は残す

したがって T0 を「フル Tauri アプリ（独自 frontendDist + invoke 面）にはしない」
と読み替え、**薄いシェル追加を日付付きで許可する**。T16-0 PR で
tag-server-design §3.1 と installer コメントを更新する。

## 3. サブスライス分割（P2）

| スライス | 内容                                                                                            | 依存         | 受入の要点                                                             |
| -------- | ----------------------------------------------------------------------------------------------- | ------------ | ---------------------------------------------------------------------- |
| T16-0    | 薄いシェル: HubRuntime 埋め込み、WebView→localhost、単一インスタンス、×→トレイ、終了で shutdown | T14, P1〜P3  | 通常起動で管理画面。収集は stopped。二重起動は既存窓再表示。SCM 非対応 |
| T16-1    | トレイ状態表示（収集停止／設定どおり／全 PLC SIM／異常）とホスト別メニュー文言                  | T16-0, T15   | §9.9 の状態識別。開始／再起動はトレイに置かない                        |
| T16-2    | サービス検出時は Desktop Collector を起動せずその UI へ接続 + native fallback UI                | T16-0, T17   | §9.9 fallback。profile/mutex/port/version 検証できない場合は開始を隠す |
| T16-3    | 共通運転バー／運転画面（Web UI）との最終結合と Windows 実機受入                                 | T16-1, T18-1 | キーボード導線・色以外の識別。OS 終了時 flush は別途 UX-7 決定が必要   |

**T16-0 は T17 を待たない。** §10 T16 受入のうち「Hub 停止中の native
fallback からサービス開始／安全停止」は T16-2（= T17 後）へ移す旨を
desktop-plan §10 に日付付きで追記する（T16-0 PR で実施）。

> **T16-2 第一スライス実装メモ（2026-08-10）**: T17（`docs/banto-hub-t17-design.md`
> §4 の引き渡し契約）が確定したことを受け、`apps/banto-hub/src-tauri/src/lib.rs`
> を以下のとおり配線した。フルの `HostSwitchEngine`（T17-3）UI/切替ウィザードは
> 今回のスコープに含めず、「サービス検出→接続、それ以外はデスクトップ起動、
> 失敗したら native fallback」という単純な決定木のみを実装した（次スライスへの
> 引き継ぎは §5 参照）。
>
> - **サービス検出・接続**: `#[cfg(windows)]`で
>   `banto_hub_core::service_manager::WindowsServiceManager::query_status()`
>   を問い合わせ、`Running`かつ
>   `banto_hub_core::http_hub_health::HttpHubHealthProbe`（後述）が`Healthy`を
>   返した場合のみ、`HubRuntime::start`を呼ばずにメインウィンドウをサービスの
>   `http://127.0.0.1:{port}/`へ`navigate`する（実装指示 1.）。`Stopped`/
>   `NotInstalled`なら従来どおりデスクトップホストとして起動を試みる。
>   `StartPending`/`StopPending`/その他の遷移中状態は「デスクトップも起動
>   しない」安全側で fallback へ回す（サービスが port/profile lock を握って
>   いる可能性があるため）。
> - **実 HTTP `HubHealthProbe`**（実装指示 3.、T17-3 で deferred だった分）:
>   `apps/banto-hub/core/src/http_hub_health.rs`に`HttpHubHealthProbe`を追加。
>   新規クレート依存を増やさず`std::net::TcpStream`で素朴な HTTP/1.1
>   リクエストを組み立てて`GET /api/v1/openapi.json`を叩き、`serde_json`で
>   本文を解析する。接続不可は`Unreachable`、200 以外または非 openapi 応答は
>   `PortConflict`、`expected_profile`が不正または期待 profile の
>   `profile.lock`が読めない場合は`WrongProfileOrVersion`/`MutexOwnerUnknown`、
>   それ以外は`info.version`を添えて`Healthy`として分類する。
> - **native fallback UI**（実装指示 2.）: `ui/index.html`のプレースホルダ
>   （`#banto-hub-status`）はそのまま流用し、SCM 状態・health 診断・
>   起動エラー・Operators 可否を日本語の複数行文言として`window.eval`で
>   書き込む（`lib.rs::fallback_message`、`invoke`面は新設しない）。操作系
>   （サービス開始／停止／再試行）は webview 側のボタンではなくタスクトレイ
>   メニューに置いた（「二重 UI を作らない」方針、§4.1 と同じ理由）。
> - **Operators ゲート**（実装指示 4.）: `banto_hub_core::service_operators::is_current_process_operator()`
>   の結果をプロセス起動時に一度確定し（`AppState::can_operate_service`）、
>   トレイの「サービスを開始」（`Stopped`かつ Operators のときだけ）・
>   「サービスを停止」（`Running`かつ Operators のときだけ）の表示可否に使う。
>   「サービスを開始」は health が`WrongProfileOrVersion`/`MutexOwnerUnknown`/
>   `PortConflict`、または SCM が`StartPending`の間は隠す（実装指示 3.）。
>   純粋な判定ロジックは`tray_status.rs`の`show_start_service_action`/
>   `show_stop_service_action`に切り出し、Tauri を起動せずテストしている。
> - 非 Windows: SCM 判定自体をコンパイル対象から外し（`#[cfg(windows)]`）、
>   常にデスクトップホストとして起動を試みる（従来どおり）。
>
> 既知の gap（次スライスへの引き継ぎ）は §5 参照。
>
> **Windows 実機検証（2026-08-10、`_verify_t17/`、Operators 対話ユーザー）**:
>
> | シナリオ                                                        | 結果                                                                                                                                                      |
> | --------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------- |
> | サービス Running + shell 起動                                   | shell は Desktop Hub を起動せず生存。`profile.lock` は `host_kind: service` のまま。openapi 200。プロセスは `banto-hub`(service) + `banto-hub-shell` のみ |
> | サービス Stopped + shell 起動（profile DB が Users 書き込み可） | `profile.lock` が `host_kind: shell`、openapi 200（Desktop Hub 埋め込み）                                                                                 |
> | Operators の `sc start`/`sc stop`                               | OK（既存 T17-2 ACL）                                                                                                                                      |
>
> **発見した運用上の注意**: LocalSystem サービスが先に作った
> `%ProgramData%\BantoHub\profiles\...\config\*.sqlite3` は Users が
> 書き込めず、Desktop/shell ホスト起動が
> `attempt to write a readonly database` で失敗し fallback になる。
> 検証時は config を消すか Users に Modify を付与して Desktop 経路を確認した
> （§5「LocalSystem 作成 profile の ACL」参照）。
> **その後の対応（同日）**: `grant-profile-acl`（profile owner への継承付き
> Modify、`Users`全体には付与しない）で解消済み。

> **T16-2 第二スライス実装メモ（2026-08-10）**: 第一スライスの
> `decide_startup`（起動時の決定木）自体は変更せず、下記 §5 の既知の gap
> のうち「トレイ操作の完了待ち」「Desktop 引き継ぎの安全性」「navigate/probe
> 先ホスト」「Administrators のトレイ操作可否」の4点に対応した。
>
> - **トレイ「サービスを開始/停止」の完了待ち**（既知の gap
>   「`ServiceManager::start`/`stop`の完了待ちをしていない」への対応）:
>   発行のみの fire-and-forget から
>   `banto_hub_core::host_switch::HostSwitchEngine`（T17-3 で設計済みの状態
>   機械）による完了待ちへ置き換えた。`lib.rs::run_host_switch`が現在の
>   `ShellView`から engine の初期状態を求めて1回だけ構築し、
>   `std::thread::spawn`した背景スレッドで`HostSwitchEngine::step`を
>   `Waiting`の間`std::thread::sleep`しながら繰り返し呼ぶ
>   （`lib.rs::drive_host_switch`）。トレイのメニューイベントハンドラ自体は
>   即座に返るためクリックの応答性は保たれる。フルの切替ウィザード UI は
>   引き続き作らず、トレイの「開始/停止」という単一の入り口だけを engine
>   経由にした（実装指示「without rewriting the whole first-slice decision
>   tree」）。
> - **サービス停止後の Desktop 引き継ぎの安全化**（既知の gap「たまたま
>   そうなる」動作への対応）: `HostSwitchEngine`の`Service→Desktop`遷移が
>   持つ不変条件（SCM `Stopped`到達**かつ**旧 health `Unreachable`到達を
>   確認してから初めて Desktop 起動を許可する）をそのまま利用する形になった
>   - シェル側で個別に待ち合わせロジックを重複実装していない。
> - **navigate/probe 先ホストの`BANTO_BIND`対応**（既知の gap「navigate 先を
>   `127.0.0.1`固定にしている」への対応）: `lib.rs::resolve_navigate_host`が
>   `BANTO_BIND`（console/service ホストと同じ env）を読み、値があればそれを
>   使う。ただし空文字列・`0.0.0.0`・`::`（全インターフェース bind）は
>   「このプロセス自身が接続する」用途では意味を持たないため`127.0.0.1`へ
>   読み替える（loopback-safe default）。`apps/banto-hub/core/src/http_hub_health.rs`
>   の`HttpHubHealthProbe`に`host`フィールドを追加し
>   （`with_host`/`with_host_and_timeout`）、`decide_startup`・
>   `attempt_desktop_start`・`run_host_switch`が同じ解決結果
>   （`ProbeTarget::host`）を probe・navigate の両方に使う。あわせて、
>   `host`が複数アドレスに解決される場合（`localhost`が環境によって
>   `::1`を先に返すが listen していない、等）に備え、解決できた全アドレスへ
>   順に接続を試みるよう`fetch_openapi`を修正した（1つ目のアドレスだけを
>   試して誤って`Unreachable`と判定しないようにするため）。
> - **Administrators のトレイ操作可否**（nice-if-small 実装指示 4.）:
>   `apps/banto-hub/core/src/service_operators.rs`に
>   `is_current_process_admin()`（ローカル`Administrators`グループの
>   メンバーシップ判定、`is_current_process_operator()`と同じ
>   `windows_impl::is_current_process_member_of`を共有）を追加し、
>   `AppState::can_operate_service`を「Operators **または** Administrators」
>   に緩和した（desktop-plan §8.3 の意図に合わせ、ローカル管理者を不必要に
>   締め出さない）。**既知の限界**: UAC の split token 環境では、シェルが
>   非昇格プロセスとして動いている場合`Administrators`メンバーシップの
>   判定が実際の昇格状態と一致しないことがある（Windows の一般的な制約 -
>   このスライスでは対応しない、低リスクと判断）。
> - out of scope（当時）: openapi 応答への profile-id 埋め込み（ワイヤ確認）は
>   第三スライスで解消、切替ウィザード UI・NSIS/installer 変更は引き続き未着手。
>
> 検証: `cargo fmt` / `cargo clippy -p banto-hub-core -p banto-hub-shell
--all-features -- -D warnings` / `cargo test -p banto-hub-core`
> （301 passed, 4 ignored — 既存の Windows 実機限定テストのみ ignore）/
> `cargo test -p banto-hub-shell`（16 passed）。
>
> **Windows 実機検証 — 第二スライス トレイ操作（2026-08-10、`_verify_t17/`、
> Operators 対話ユーザー `TKent`）**:
>
> | シナリオ                                                           | 結果                                                                                                                                                                                                    |
> | ------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
> | サービス Running + shell 起動                                      | `profile.lock` `host_kind: service` のまま。openapi 200。プロセスは `banto-hub`(service) + `banto-hub-shell` のみ                                                                                       |
> | トレイ「サービスを停止」（`HostSwitchEngine` Service→Desktop）     | 約 2〜3 秒で SCM `Stopped`、サービス process 消失、続けて shell が `host_kind: shell` を取得、openapi 200（Desktop Hub）。Stopped 到達と旧 health 消失を待ってから Desktop 起動していることを観測で確認 |
> | Fallback + Stopped からトレイ「サービスを開始」（Offline→Service） | profile mutex を一時保持して Desktop 起動を失敗させ Fallback にしたうえでクリック。SCM `Running` + `host_kind: service` + openapi 200。shell process は同一のままサービスへ接続                         |
>
> 前提: `banto-hub-elev.exe service-install`（Demand）+ `grant-profile-acl`、
> バイナリは `_verify_t17/`。

> **T16-2 第三スライス実装メモ（2026-08-10）**: 上記 §5「T16-2 第一スライス
> の既知の gap」に残っていた最後の1点「profile-id のワイヤ確認をして
> いない」に対応した小規模な follow-up。
>
> - **`GET /api/v1/openapi.json`への profile-id 埋め込み**
>   （`apps/banto-hub/core/src/rest.rs`）: utoipa の`#[openapi(info(...))]`
>   は任意拡張フィールドを直接生成できないため、`ApiDoc::openapi()`が生成
>   した`utoipa::openapi::OpenApi`を`serde_json::Value`へ変換したうえで
>   `info.x-banto-hub-profile-id`へこの Hub インスタンス自身の profile-id
>   （`HubConfig::profile_id`）を差し込む方式にした（utoipa の
>   `Extensions` API とは戦わない最小実装）。`openapi_json`ハンドラの状態を
>   `OpenApiState { profile_id: String }`に変え、`openapi_router`・
>   `api_router_with_controller_mode`・公開関数`api_router_with_controller`/
>   `api_router`まで`profile_id: String`引数を素通しした
>   （`HubRuntime::start`が`HubConfig::profile_id`をそのまま渡す）。
> - **`HttpHubHealthProbe`のワイヤ確認**
>   （`apps/banto-hub/core/src/http_hub_health.rs`）: openapi 応答から
>   `info.x-banto-hub-profile-id`を読み、`expected_profile`と直接比較する
>   ように変更。欠落（旧バージョンの Hub 等）・型不一致・不一致のいずれも
>   区別せず`HealthOutcome::WrongProfileOrVersion`に倒す（呼び出し元の対処が
>   変わらないため）。一致した場合のみ、従来どおり`profile.lock`の読み取り
>   確認（`MutexOwnerUnknown`/`Healthy`の分岐）へ進む - lock ファイル確認は
>   ワイヤ確認の**後**に行う二次確認という位置づけに変わった。
> - 呼び出し元（`apps/banto-hub/src-tauri`）は無変更 -
>   `HttpHubHealthProbe::with_host`等のコンストラクタのシグネチャは変わって
>   いない。
> - テスト call site（`api_router`/`api_router_with_controller`を呼ぶ既存の
>   単体・結合テスト、計17箇所）は全て
>   `banto_hub_core::profile_paths::DEFAULT_PROFILE_ID`（`"default"`）を
>   渡すよう更新した。
>
> 検証: `cargo fmt --all -- --check` / `cargo clippy -p banto-hub-core -p
banto-hub-shell --all-features -- -D warnings` / `cargo test -p
banto-hub-core --lib`（305 passed, 4 ignored）/ 影響する結合テスト
> （`integration`/`t7_partial_reconfig`/`t8_bit_access`/`t9_simulation`/
> `t11_batch_tags`/`computed`/`write`/`soak`/`grpc`/`mqtt`/`stream`/
> `t12_connection_test`/`t15_write_peek`/`t15_simulation_coverage`）が
> `--test-threads=1`で全て pass することを確認。**既知の注意点**:
> `http_hub_health`/`profile_lock`の`"default"` profile を使うテストが
> 複数あり、Windows の named mutex（`Global\BantoHub.<profile-id>`）が
> profile-id 単位のプロセス超えグローバル排他であるため、デフォルトの
> 並列テスト実行では稀に`AlreadyHeld`で衝突することがある
> （`profile_lock.rs`の`different_profile_ids_can_both_acquire`のコメント
> が同種の制約に既に触れている）。本スライスで新規追加した2テストは
> ワイヤ確認だけで結果が決まる（lock を一切取得しない）よう設計し、この
> 衝突面を増やさないようにした - 既存テスト間の衝突は本スライス以前からの
> 既知の制約であり、このスライスのスコープ外として残す。

> **切替ウィザード UI 実装メモ（2026-08-11）**: desktop-plan §9.7 の
> 「Windows サービス」カードを Hub 管理 UI（`/status`）に追加し、シェルへ
> 最小の Tauri invoke を配線した（運転 API の二重実装はしない）。
>
> - **`ShellDesktopControl`**: Desktop→Service で`RunningHub::shutdown`を
>   実行し、`is_stopped`は hub 未保有で`true`（ダミー実装を置き換え）。
> - **invoke**: `host_switch_status` / `switch_to_service` /
>   `switch_to_desktop` / `set_service_autostart`（`host_switch_ipc.rs`）。
>   進捗は`host_switch_progress`イベント。トレイ開始/停止と単一飛行フラグを
>   共有。自動起動は`banto-hub-elev.exe`を`ShellExecuteExW`（verb `runas`）で
>   起動。
> - **UI**: `hostSwitchShell.ts` / `hostSwitchGate.ts`。ゲートは Hub Admin +
>   `can_operate` + `last_config_error == null` + revision 取得済み。非シェル
>   では「ローカルシェルが必要」と表示して無効化。
> - out of scope: `/operation` ナビ再編・共通運転バー（T18）、遠隔ブラウザ SCM、
>   NSIS。
>
> 検証: コード照合時点では単体（ゲート vitest）・`cargo`/型チェックを実施。
> **Windows 実機での Desktop→Service UI 経路・自動起動 UAC は未検証**。

> **切替ウィザード実機検証メモ（2026-08-31）**: Windows 実機で `/status`
> の Windows サービスカードから Desktop→Service 切替を検証したところ、
> コード照合だけでは見えていなかった3層の不具合が続けて見つかった
> （後から同じ罠を踏まないための記録）。
>
> 1. **リモートオリジンがケイパビリティ未許可**: このシェルは自分自身が
>    配信する `http://127.0.0.1:{port}/` を `window.navigate` で読み込む
>    設計だが、Tauri v2 の既定は `capabilities/default.json` に
>    `remote.urls` が無いローカルアセット由来（`URL: local`）にしか
>    `core:default`（invoke・`event.listen`）を許可しない。
>    `remote.urls: ["http://127.0.0.1:*/*"]` を追加して解消した
>    （`2539858`）。
> 2. **自前コマンドが ACL 未宣言**: `generate_handler!` はランタイムの
>    ディスパッチ登録に過ぎず ACL には現れない。tauri 2.11.5 の ACL
>    マニフェストにアプリ自身のコマンド（`APP_ACL_KEY`）のエントリが
>    無いと `host_switch_status not allowed. Plugin not found` を返す。
>    `tauri-build` の `Attributes::app_manifest(AppManifest::commands)` で
>    4コマンド（`host_switch_status`/`switch_to_service`/
>    `switch_to_desktop`/`set_service_autostart`）を ACL へ宣言し、自動
>    生成された `allow-<command>` permission を `capabilities/default.json`
>    に追加して解消した（`db20794`）。
> 3. **管理 UI の状態取得が `/api/v1/*` を使っていた**: `hubStatus.ts` が
>    `/api/v1/status`・`/api/v1/values` を叩いており、これらは
>    `require_tag_space_auth`（API キー認証）固定で試運転モードのバイパス
>    対象外（tag-server-design.md §5.6「管理 UI と `/api/v1/*` の境界」
>    参照）のため 401 になり、`hostSwitchGate.isPreflightOk` が構成
>    revision の取得失敗で連鎖的に切替ゲートを閉じていた。管理系
>    `GET /api/status`・`GET /api/values` を新設して解消した（`0c6c3e7`）。
>
> **2026-09-01 追記: 残っていた項目もすべて実機で確認済み（オーナー実施）。** 逆経路
> （Service→Desktop）、自動起動トグルの UAC、UAC をキャンセルした場合の挙動は
> いずれも問題なく動作した。**これで切替ウィザードの実機検証は完了**とする。

## 4. T16-0 設計（P3）

### 4.1 Composition

```text
banto-hub-shell (Tauri v2, Windows 優先)
  └─ setup:
       HubRuntime::start(config) -> RunningHub   // collection stays Stopped
       open WebviewWindow at http://127.0.0.1:{port}/
       TrayIcon + CloseRequested -> hide
       Exit -> RunningHub::shutdown() -> app.exit
```

- chronogazer/relay-wright の `src-tauri` 骨格（Cargo.toml / build.rs /
  capabilities 最小 / icons）は再利用する
- invoke によるタグ／運転 API 二重実装はしない。capabilities は
  `core:default` 程度に留める
- ポート競合や profile lock 失敗時はサービス接続モードへ逃がさず、
  「起動できません」診断を表示して終了操作だけ提供する（T16-2 で拡張）
- クレート名: `banto-hub-shell`（ヘッドレス bin `banto-hub` と衝突させない）

### 4.2 単一インスタンス

- GUI 二重起動: 第二インスタンスは既存ウィンドウを前面化し自身は終了
  （`tauri-plugin-single-instance` または同等）
- profile 排他: desktop-plan §16.2 の `Global\BantoHub.<profile-id>` +
  ファイルロックは T17 の profile path 実装と合わせて本採用する。T16-0 は
  HubRuntime の bind 失敗を当面の衝突検知とする

### 4.3 トレイ（T16-0 最小）

| 操作         | 動作                                                       |
| ------------ | ---------------------------------------------------------- |
| 画面を開く   | ウィンドウ再表示                                           |
| アプリを終了 | 確認なし（T16-0）→ shutdown + exit。確認ダイアログは T16-1 |
| ×            | トレイ格納。初回だけ継続通知（可能なら）                   |

開始／再起動／サービス操作はトレイに置かない（§8.2）。

> **T16-1 実装メモ（2026-08-09）**: 上表の「確認なし（T16-0）」「初回だけ
> 継続通知（可能なら）」は本 PR で以下のとおり実装した（詳細は
> [banto-hub-desktop-plan.md](banto-hub-desktop-plan.md) §10「T16」の
> T16-1 実装メモ）。
>
> - 「アプリを終了」は `tauri-plugin-dialog` によるネイティブ確認ダイアログを
>   経由するようになった。収集中は「収集を停止し、履歴を flush してから
>   終了します」、停止中は短い確認文言。キャンセル時は shutdown/exit を
>   一切呼ばない。
> - 初回継続通知は `tauri-plugin-notification` で OS 通知を出し、既読フラグを
>   `app_data_dir` 配下のファイルへ永続化する（プロセス内フラグではなく実際に
>   永続化できた）。
> - あわせて、トレイの状態ラベル・tooltip・「収集を停止」メニュー項目
>   （`CollectionController::subscribe_status()` 購読で状態変化に追従）を
>   §3「T16-1」・desktop-plan §9.7/§9.9 のとおり実装した。

### 4.4 作成予定ファイル

- `apps/banto-hub/src-tauri/{Cargo.toml,tauri.conf.json,build.rs,src/{main,lib}.rs,capabilities/default.json,icons/,ui/index.html}`
  （`ui/index.html` は §4.1 の起動プレースホルダ - `frontendDist` はこの
  静的1ファイルのみを指し、SvelteKit ビルドは参照しない）
- root `Cargo.toml` workspace members へ追加
- `docs/tag-server-design.md` §3.1 日付付き注記
- `docs/banto-hub-desktop-plan.md` §10 T16 受入の T17 後送り注記

### 4.5 検証

- CI（既存 Windows rust job）: `cargo clippy/test -p banto-hub-shell` で
  コンパイルと「起動直後 collection=stopped」単体（可能な範囲）
- 手動（Windows）: 起動→管理画面表示、×→トレイ、二重起動で既存窓、終了でプロセス消失

## 5. T16-1 以降で別途設計するもの

- ticket プロトコル、Operators、仮想サービスアカウント
- desktop⇔service 切替状態表
- WebView2 Fixed Version / コード署名
- OS シャットダウン時 flush（UX-7）

これらは T16-2 / T17 着手前に、本書へ追記するか `banto-hub-t17-design.md`
を新設する。

### T16-2 第一スライスの既知の gap（次スライスへの引き継ぎ）

- ~~**profile-id のワイヤ確認をしていない**~~: **2026-08-10 第三スライスで
  解消**。`GET /api/v1/openapi.json`の`info.x-banto-hub-profile-id`拡張
  フィールドに、応答している Hub インスタンス自身の profile-id を埋め込み、
  `HttpHubHealthProbe`がそれを`expected_profile`と直接比較するようにした
  （下記 §3 第三スライス実装メモ、`http_hub_health.rs`のモジュール doc
  参照）。`profile.lock`確認は不一致がないことを確認した後の二次確認として
  残している。
- ~~**`ServiceManager::start`/`stop`の完了待ちをしていない**~~: **2026-08-10
  第二スライスで解消**。トレイ「サービスを開始/停止」は
  `banto_hub_core::host_switch::HostSwitchEngine`による完了待ちに置き換えた
  （`lib.rs::run_host_switch`/`drive_host_switch`、上記 §3 第二スライス実装
  メモ参照）。
- ~~**navigate 先を`127.0.0.1`固定にしている**~~: **2026-08-10 第二スライスで
  解消**。`BANTO_BIND`があればそれを解決して navigate/probe 双方に使う
  （`lib.rs::resolve_navigate_host`、`HttpHubHealthProbe::with_host`、上記
  §3 参照）。全インターフェース bind は引き続き loopback へ読み替える。
- ~~**サービス停止後の自動デスクトップ引き継ぎは「たまたまそうなる」動作**~~:
  **2026-08-10 第二スライスで解消**。トレイ「サービスを停止」は
  `HostSwitchEngine`の`Service→Desktop`遷移を使うようになり、SCM
  `Stopped`到達**かつ**旧 health `Unreachable`到達を確認してから初めて
  Desktop 起動を試みる（上記 §3 参照）。なお「再試行」ボタン
  （`retry_startup`）は引き続き単純な決定木の再評価のみで、この安全策は
  トレイの明示的な「サービスを開始/停止」操作にのみ適用される。
- **Windows 実機での`WindowsServiceManager`経路**: 2026-08-10 に
  `_verify_t17` でサービス Running 時の shell 接続・Stopped 時の Desktop
  起動を確認済み（§3 実装メモ末尾）。**同日、第二スライスのトレイ
  「サービスを停止/開始」と`HostSwitchEngine`完了待ちも実機確認済み**
  （§3 第二スライス実機検証表）。
- **Administrators のトレイ操作可否**: 2026-08-10 第二スライスで
  `is_current_process_admin()`を追加し、Operators または Administrators
  なら操作可能にした（上記 §3 参照）。UAC split token 環境での挙動は
  既知の限界として残る（低リスクと判断、対応は見送り）。
- **LocalSystem 作成 profile の ACL**: サービス先行作成の DB が対話ユーザー
  から readonly になり Desktop 起動が失敗し得る（§3 実機メモ）。**2026-08-10
  対応済み**: `profile_acl.rs`＋固定アクション`grant-profile-acl`
  （docs/banto-hub-t17-design.md §12）で profile owner への DACL 付与
  （`Users`全体には付与しない、desktop-plan §11）を実装した。
  `service-install`が新規インストール時に自動実行するため、通常は手動
  操作不要。既存インストールで本エラーに遭遇した場合は
  `banto-hub-elev.exe grant-profile-acl`で手動解消できる
  （docs/banto-hub-operations.md §10「profile ディレクトリの権限
  （ACL）」）。2026-08-10 実機で `grant-profile-acl`（UAC）による
  owner Modify 付与と書き込み復旧を確認済み（t17-design §12）。
  サービス先行作成→Desktop Hub 起動までのフル E2E は任意の追加確認。

## 6. 承認チェックリスト

- [x] P1: T0 再解釈（薄いシェル追加を許可、二重 UI は禁止のまま）— 2026-08-09
- [x] P2: T16 SCM fallback 受入を T17 後へ移す — 2026-08-09
- [x] P3: WebView は Hub localhost origin（frontendDist 二重配布なし）— 2026-08-09
