# banto-hub T16 詳細設計（デスクトップシェル・タスクトレイ）

作成日: 2026-08-09
状態: **設計確定。P1〜P3 承認済み。T16-0（薄いシェル `banto-hub-shell`、
`apps/banto-hub/src-tauri`）・T16-1（トレイ状態表示）マージ済み。
次は T16-2（T17 依存のため後回し）。**
最終検証日(コード照合): 2026-08-09
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

## 6. 承認チェックリスト

- [x] P1: T0 再解釈（薄いシェル追加を許可、二重 UI は禁止のまま）— 2026-08-09
- [x] P2: T16 SCM fallback 受入を T17 後へ移す — 2026-08-09
- [x] P3: WebView は Hub localhost origin（frontendDist 二重配布なし）— 2026-08-09
