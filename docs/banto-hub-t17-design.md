# banto-hub T17 詳細設計（SCM 管理・profile・UAC・インストーラ再設計）

作成日: 2026-08-10
状態: **設計確定。主要判断 P1〜P6 は 2026-08-10 オーナー承認済み。
T17-0（SCM 状態取得＋start/stop/restart/autostart API 抽出）から着手可。**
T16-0（薄いシェル）・T16-1（トレイ状態表示）はマージ済みで本書の前提。
T16-2（サービス検出・native fallback）は本書 §4 の引き渡し契約（P5）に
従い、T17-0 以降が提供する API を消費する形で着手する。
最終検証日(コード照合): 2026-08-10
基準コミット: `91ec221`（main、T17 詳細設計草案 #114 マージ後）。

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

- **`Global\` named mutex の Session 0 越え挙動**: `Global\` 名前空間が
  実際にサービス（Session 0）とユーザーセッション間で同一オブジェクトを
  指すこと自体は Win32 の文書化された仕様だが、CI ランナー環境や
  `windows-service` クレートの実行コンテキストでどう振る舞うかは未検証。
  要 Windows 実機スパイク。
- **UAC split-token の管理者判定落とし穴**（desktop-plan §16.3 既指摘）:
  `CheckTokenMembership` が UAC 昇格前トークンでは Administrators を
  deny-only として偽の非管理者判定を返し得る。`TokenLinkedToken` を
  使うかどうかの実装方針は要 Windows 実機スパイク。
- **サービス Security Descriptor（SDDL）への `BantoHub Operators`
  SID 付与の正確な API 呼び出し**（P3）: `SetServiceObjectSecurity` の
  引数構成、`windows-service`/`windows-sys` クレートでの表現方法は本書
  では確定しない。要 Windows 実機スパイク。
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
  警告が出る。証明書調達の有無はオーナー判断待ちで、本書では扱わない。
- **上書きインストール時の既存サービス設定保持**（desktop-plan §16.3
  「上書きインストール時は既存サービスの起動種別・自動起動設定を保持」）:
  P4 で新規インストールの既定を `Demand` にする際、既存インストールの
  現在値をどう検出して保持するかの具体実装は T17-4 着手時に確定する
  （本書では方針のみ）。
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

**上記すべて 2026-08-10 オーナー承認済み。T17-0 から実装着手可。**
