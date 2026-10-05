# banto-industrial + 記録計アプリ 計画

作成日: 2026-07-12（過去アプリ棚卸しと記録計構想の議論に基づく）
状態: **進行中。I系（I0〜I6）実装済み（I6 = banto-broker として抽出済み）、
W系（relay-wright）は W5 まで実装済みだったが **2026-09-17 に凍結**（構想の練り直し、§4b）、
**2026-09-24 に main から外してタグ `archive/relay-wright-2026-09-24` に退避した**。T系（タグサーバー、
§4c）は T0〜T21 実装済み（試運転モード/ロックダウン・収集開始停止 UI・タグ名
一意性の収集グループ内一意への緩和・T19 UX 群・T20 文字列/構造体/レシピ/ビット・
T21 構成補助 MCP 管理面を含む、2026-09-06）。残 T18-5c/d（Windows
実機往復・72h soak = 実機必須）と実機・需要待ちの #210/#211/#123/#201。2026-09-24: I3a に記録時刻と
ファイル日付の契約（#424 案 A、§5）を追記。2026-10-02: §5 に両アプリの DB スキーマの整理（admin-template の
migration のコピー・旧形式の DB の拒否、オーナー決定・実装済み）を追記。2026-10-04: §5 に
ChronoGazer のサービスと REST ルーターを banto のものに置き換えたこと（実装済み）と、
それに伴うオーナー決定 2 点（#280 の旧バックアップは警告のみ・閲覧公開は画面に出さない）を追記。
同日: §5 に LAN 設定の適用を banto に寄せたこと（I2b、実装済み）と、閲覧公開を使うオーナー決定
（前の決定を変更）を追記。同日: §5 に起動時の環境判定を banto に寄せたこと
（I2c、実装済み。一時的に届かないときは demo に落とさず起動待ちで再試行）を追記。同日: §5 に
banto-hub のサービスと REST ルーターを banto のものに置き換えたこと（I3'、実装済み）を追記。
2026-10-05: §5 の起動時の環境判定に、banto v3.0.1（#321）で保護画面を直接開いたときの
真っ白が解消したこと（実装済み）を反映。同日: §5 に ChronoGazer の SvelteKit 3 移行（banto v4.0.0、
実装済み。banto-hub の npm は v3.0.1 のままの移行中）を追記。同日: §5 に banto-hub の SvelteKit 3 移行
（実装済み。両アプリの npm が v4.0.0 に揃い、移行中のずれは解消）を追記。
T13〜T18 の詳細と最新の全体像は
[banto-hub-remaining-plan.md](banto-hub-remaining-plan.md) と
[banto-hub-desktop-plan.md](banto-hub-desktop-plan.md) を正とする（本文 §4c 表は
T13-1 までの粒度で、以降は同書へ移管）。Hardening（H1〜H10）は H7 の① 実機 soak
のみ残（詳細は improvement-plan.md）。docs 全体の
地図は [README.md](README.md)**（2026-10-05 更新。本文の T 系表は 2026-08-08 時点の
まま — 実装状況の正は banto-hub-remaining-plan.md/banto-hub-desktop-plan.md）
最終検証日(コード照合): 2026-09-01

AI コードレビュー指摘の改善計画（H系）は
[improvement-plan.md](improvement-plan.md) で管理する（2026-08-08）。

banto-hub のアプリ／サービス運転計画（T14〜T18: デスクトップシェル・全 PLC
シミュレーション・サービス管理・タグ登録 UI/UX 改善）は
[banto-hub-desktop-plan.md](banto-hub-desktop-plan.md) で管理する
（2026-08-09 計画確定 + 上位モデル検証反映）。

本計画は banto テンプレートの外側 — **別リポジトリ banto-industrial** —
の計画である（2026-07-12 決定）。テンプレートのスコープ方針
（banto リポジトリの docs/template-scope.md §1 の4条件。本リポジトリには
実体がない上流ドキュメントのため、参照のみでリンクは張らない）によりドメイン寄りの
資産はテンプレートに入れないが、複数アプリが共有する（Rule of Three 充足済み）
ため案件内に書き捨てず、自社資産として独立リポジトリに蓄積する。
banto の `@banto/*` パッケージ（GitHub Packages）とクレート（git タグ参照）を
消費する側に立つ — これが banto M18 の配布整備を前提とする理由。

## 1. 背景: 過去アプリ棚卸し（2026-07-11）

時系列CSV出力 / 時系列帳票 / MELSECレシピDL / 簡易SCADA / 日報 /
MQTT発信 / 写真帳 / メニュー表示 / 倉庫管理 / 生産管理 の10本を分析した
結果、工場系で繰り返し必要になる資産は:

| 資産                                         | 出現   | 行き先                                     |
| -------------------------------------------- | ------ | ------------------------------------------ |
| タグ定義（名前・アドレス・型・スケーリング） | 3本    | banto-industrial                           |
| PLC通信（MC/SLMP）・MQTT                     | 3本    | banto-industrial                           |
| 外部時系列DB読み出し・保存                   | 3〜4本 | banto-industrial                           |
| 帳票/印刷・添付/画像・バーコード             | 4〜5本 | **banto 本体**（M19〜M21、横断機能のため） |

## 2. ライセンスと収益モデル

- banto-industrial は**MIT ライセンス**で公開する（ライブラリ・アプリとも）。
  誰でも自由に利用・改変・再配布・**販売**できる
- 収益化は**コードのライセンス課金ではなく**、導入支援・カスタム開発・保守、
  および tyaro 自身がアプリを**システムの一部として有償提供**することで行う
- MIT のため、受託契約に「汎用モジュールは受託者に留保する」といった
  権利留保条項は不要になった。案件固有コード（画面・業務ロジック）の扱いは
  各案件契約で個別に定める

## 3. banto-industrial マイルストーン（I系）

| #   | 内容                                                                                                                                                 | 依存        | 備考                                                                                                                |
| --- | ---------------------------------------------------------------------------------------------------------------------------------------------------- | ----------- | ------------------------------------------------------------------------------------------------------------------- |
| I0  | リポジトリ起票（workspace、banto 参照配線、CI）                                                                                                      | banto M18   |                                                                                                                     |
| I1  | タグレジストリ（タグマスタ CRUD、型・スケーリング・単位）                                                                                            | I0          | 3アプリが同一定義を消費                                                                                             |
| I2  | PLC プロトコルクライアント（**読み取り専用**、一括読み出し）: 共通 trait + **Modbus TCP 先行** → MC/SLMP 続行（2026-07-12 決定、デバッグ容易性優先） | I0          | 書き込みは記録計スコープ外。レシピDLアプリで必要になった時に別途                                                    |
| I2a | MC/SLMP 読み取りクライアント（banto-plc の `PlcClient` trait への2本目の実装。承認済み外部 `slmp` クレートをラップ、シミュレータ同梱、実装済み）     | I2          | 実PLC互換の最終検証は W5 の残項目（§4b）                                                                            |
| I3a | 時系列ストレージ（banto-tstore。日次ファイル + スキーマ凍結、実装済み）                                                                              | I0          | 下記 §5 参照。レジストリ（I1）非依存・自己記述ファイル                                                              |
| I3b | 収集エンジン（banto-collect。PLC 定期読み出し→I3a 書き込み、現在値キャッシュ + イベント2系統、実装済み）                                             | I1, I2, I3a | 下記 §5 参照                                                                                                        |
| I4  | 時系列クエリ層（期間クエリ + サーバ側 min/max 間引き、実装済み）                                                                                     | I3a, I3b    | banto M13 チャートへ直結。ハイブリッド合成の土台                                                                    |
| I5  | PLC 書き込みクレート（**banto-plc-write**。SLMP 一括書き込み、read 側と分離した専用 trait、実装済み）                                                | I2a         | 番号は I5 だが `crates/` 配下のクレート。消費者は relay-wright（W系）のみ — 読み取り専用アプリは書き込みAPIを見ない |
| —   | MQTT クライアント（rumqttc + タグバインド）                                                                                                          | I1          | 記録計に不要。→ タグサーバー T3（§4c）で回収予定                                                                    |

## 4. 記録計アプリ **ChronoGazer**（R系）— banto-industrial 内の最初の製品

デジタル記録計（ペーパーレスレコーダのソフトウェア版）:
PLC通信 + タグデータ保存 + リアルタイム/ヒストリカル/ハイブリッド
トレンド + 計器表示。**既設PLC + 現場PC でチャネル数自由**という
実機（横河GX/GP等、数十万円・チャネル固定）に対する価格・柔軟性の優位を狙う。
デモが営業資産を兼ねる。

| #   | 内容                                                     | 依存          | banto 資産                                      |
| --- | -------------------------------------------------------- | ------------- | ----------------------------------------------- |
| R0  | 要件定義（グループ表示仕様・設定項目・非スコープ確定）   | —             |                                                 |
| R1  | リアルタイムトレンド + 計器/バー/デジタルのグループ表示  | I1〜I3        | M13ストリーミング、Gauge、M11キオスク           |
| R2  | ヒストリカルトレンド                                     | I4            | M13ズーム/パン + サーバ側間引き                 |
| R3  | ハイブリッドトレンド + 期間CSV出力 + 日報帳票            | R2, banto M19 | M15 CSV、M19 report（過去アプリ①②をここで回収） |
| R4  | しきい値イベント一覧・ソークテスト・配布（インストーラ） | R1〜R3        | M13 bands/markers、M17バックアップ              |

**データ源のドライバ（2026-09-17 オーナー決定）**: ChronoGazer は**単体で動く**ことを前提とし、
データ源のドライバを3種持つ: **SLMP**（`banto-plc::slmp`）・**Modbus TCP**（`banto-plc::modbus`）・
**banto-hub 経由**（`banto-tagclient` の購読）。Hub 経由は #332 で接続設定とタグ選択まで入り、
**選んだタグの購読（値の受信）まで実装済み**（#383 段階1。世代の同一性は「接続先 + タグ集合」で、
`status()` では張り直さない。トレンド表示・保存は段階3）。banto-hub 側も今後接続ドライバが増えていく
想定なので前2者は Hub と機能が被るが、**現場 PC 1 台だけでも ChronoGazer が成立すること**を
優先し、この重複は許容する。段階（Hub 経由 → 直結 → 合流）と設計の論点は #383。

**スコープの護り（SCADA化の誘惑対策、v1 で入れない線）**:
PLC への書き込み / 汎用画面エディタ（表示は固定グループパターンのみ）/
アラーム状態遷移管理（ACK 等。しきい値の表示とイベント一覧まで）/
多段通知（メール等）。

## 4b. 自動書き込みアプリ **relay-wright**（W系）— 凍結・main から退避済み

> **凍結（2026-09-17 オーナー決定）**: デバッグ用途として位置づけていたが、構想を練り直すため
> しばし凍結する。W5 の実機検証も、#332（Hub 自動接続）の relay-wright への配線も止める
> （配線の途中までの作業はローカルブランチ `feat/332-hub-bootstrap-relay-wright` に WIP として
> 残してある。push はしていない）。
>
> **main から退避（2026-09-24 オーナー決定）**: 凍結が続いているため、`apps/relay-wright`・
> 専用の E2E（`e2e/relay-wright.playwright.config.ts` 等）・CI の relay-wright 手順を main から
> 外した。外す直前の main はタグ `archive/relay-wright-2026-09-24`、#332 配線の途中の作業は
> ブランチ `archive/relay-wright-332-wip` にそれぞれ退避してある。復元は
> `git checkout archive/relay-wright-2026-09-24 -- apps/relay-wright ...` で行う。
> `crates/banto-broker`・`crates/banto-plc-write`・`crates/banto-tags` への依存はコメントのみで
> コードの依存は無かったため、退避は他アプリに影響しない。

条件付き PLC 自動書き込みアプリ（Tauri + banto テンプレート由来、詳細計画は
セッション初期に作成したローカルの計画メモ `luminous-discovering-goblet.md`
— Claude plan mode のローカル成果物でリポジトリには未収録。当時の Rule of
Three 判断の経緯は本節と W1〜W5 実装（下表）に集約済み）: タグレジストリの読み取り値を条件に、
設定ルールに従って別の PLC デバイスへ値を自動書き込みする**構想だった**。ChronoGazer が
「v1 で入れない」と護った書き込みを、**専用アプリ + 専用クレート（I5）+
安全機構**（アーミング・dry-run・レート制限ブレーカ・log-before-write・
サイクル検出）に隔離して引き受ける設計。安全上の注意は退避タグの
`apps/relay-wright/README.md` 参照。

| #     | 内容                                                                                                                             | 依存    | 備考                                                                                                                               |
| ----- | -------------------------------------------------------------------------------------------------------------------------------- | ------- | ---------------------------------------------------------------------------------------------------------------------------------- |
| W1    | 骨格（Tauri アプリ雛形・レジストリ配線・`write_audit_log` 等スキーマ、実装済み）                                                 | I1, I5  | chronogazer と同型の banto テンプレート由来                                                                                        |
| W2    | レジストリ・ルール CRUD（書き込み対象・条件付きルール + 書き込みループのサイクル検出、実装済み）                                 | W1      |                                                                                                                                    |
| W3-A  | PLC アクセスブローカー（接続毎1タスク・read/write 単一セッション・再接続バックオフ、実装済み）                                   | W2, I2a |                                                                                                                                    |
| W3-B1 | エンジン中核（poller / rule_engine / writer / rate_limiter / arming、実装済み）                                                  | W3-A    | 安全不変条件（log-before-write・レート制限トリップで自動ディスアーム等）はここ                                                     |
| W3-B2 | 配線（Engine 起動/停止、REST・Tauri 両経路の制御、実装済み）                                                                     | W3-B1   |                                                                                                                                    |
| W4    | 監視・操作画面（エンジン制御・書き込み監査ログ・ナビゲーション、実装済み）                                                       | W3-B2   |                                                                                                                                    |
| W5    | 安全性強化（サイクル検出網羅テスト・レート制限の決定論テスト・フレークテスト決定論化・ドキュメント整合・インストーラ確認、本PR） | W1〜W4  | **凍結中（2026-09-17）**: 残っていた実機検証（実機 SLMP 互換・同時セッション数上限・`TypedDevice`/`PLCString`(Shift-JIS)）も止める |

## 4c. タグサーバーアプリ banto-hub（T系）— T0〜T21 実装済み・残るは T18-5c/d・実機依存項目（#210/#211/#123/#201）のみ

（詳細と最新の全体像は [banto-hub-remaining-plan.md](banto-hub-remaining-plan.md)・
[banto-hub-desktop-plan.md](banto-hub-desktop-plan.md) を正とする。以下の T13 以降を含む表は
2026-08-08 時点の粒度のまま保持し、実装状況は更新しない）

FA-Server 型の独立タグサーバー **banto-hub**（2026-08-04 起案・命名、設計詳細は
[tag-server-design.md](tag-server-design.md)）: I 系クレートを束ねた
ヘッドレス axum アプリで、タグ空間（= banto-collect の現在値キャッシュ）を
**REST / WebSocket / MQTT publish / gRPC** で外部公開する（OPC UA は後回し）。
外部システム連携（MES・クラウド・自作画面）と、同一 PC 同居時の PLC
セッション多重化（W5 実機検証の同時セッション数上限の結果次第で必須化）が
狙い。書き込みは per-tag opt-in + 監査 + レート制限のパススルーのみ
（ルールエンジンは持たない - relay-wright が構想していた領域で、同アプリは
2026-09-24 に main から退避済み）。**タグ定義の一次ソース**
として位置づけ、今後の関連アプリ（SCADA 等）は自前でアドレスを定義せず
タグサーバーの catalog をバインドして使う（同 doc §4.1/§7、2026-08-04 決定）。
**2026-09-17 追記**: これは「Hub があるときは Hub がタグ定義の一次ソース」という位置づけの
意味に読み替え、**ChronoGazer に Hub を必須にはしない**（§4 のドライバ決定、#383）。
演算タグ・内部タグもサーバー側で一元実装（同 doc §4.2）。**オンライン
動的変更**（稼働中のタグ定義変更、影響半径 = 触った接続のみ）を FA-Server との
差別化要件とする（同 doc §4.3、I7 = banto-collect の接続単位部分再構成が対応
バックログ）。ロガー/日報/帳票機能は作らない（記録は ChronoGazer の専管）。
I 系シミュレータで T0〜T6 が実機なしで進められる。
2026-08-04 決定分: 書き込み受付は REST / gRPC の2経路のみ（WS は購読専用・
MQTT set なし）。I1 `tags` 列追加1回（`writable`/`tag_kind`/`expression`/
`retain`）、I6 = broker 共有クレート抽出、OpenAPI は utoipa、ローカル記録は
tstore 保持7日。既定ポート: banto-hub = **8722**（UI/REST/WS）+ gRPC 50051。
※既存2アプリの LAN モード既定はどちらも 8721 で重複（両方既定無効のため
実害未発生）— どちらかの既定変更を W 系/M 系バックログとする。
※T0 実装時の発見（2026-08-05）: banto-collect の `build_config` は当時
modbus-tcp のみ対応だった（slmp 接続は構成エラー）→ **I8 = banto-collect の
SLMP 対応として T2-0 で実装済み**（2026-08-05。既知の制約: SLMP の CPU
種別・アクセスルート・word order はレジストリから設定できず banto-plc の
既定値 — 必要になったら I1 列追加）。
※T2 安全設計レビュー（2026-08-05 オーナー承認）: **I8 は T2 の前提スライス
（T2-0）として実施**。書き込み第一弾は SLMP（書き込みスタックが SLMP 専用
のため）。**I9 = Modbus 書き込み**（banto-plc-write への FC5/6/15/16 追加 +
broker のプロトコル抽象化）を新規バックログとする。broker 統合方式は
「SLMP 接続のみブローカー管理・PlcClient アダプタ注入・broker は
CollectorManager の外で生存」（tag-server-design.md §6-5）。

2026-08-09 決定分（T14〜T18、詳細は
[banto-hub-desktop-plan.md](banto-hub-desktop-plan.md)）: banto-hub を共通 Hub
ランタイムから「アプリホスト」「サービスホスト」の2ホストで駆動し、初回起動は
収集を開始せず、タグ登録→全 PLC シミュレーション→実機試運転→サービス常時運転化
までを1つの製品 UI で完結させる。**運転中の構成編集は方針改定済み**（2026-08-09
時点の「一律ロック」から、2026-08-11 追補で「編集はキュー化、反映は手動適用、
適用前キャンセル可」へ変更）。§4.3 のオンライン部分再構成基盤は内部維持し、
I1 CRUD の rebuild 失敗握り潰しは全構成 preflight へ置き換える。下表の T13-1 の
続きは同計画の T14〜T18 が引き継ぐ。

| #     | 内容                                                                                         | 依存    | 備考                                                                                                                                                                                                                                                                                                                                                                                                             |
| ----- | -------------------------------------------------------------------------------------------- | ------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| T0    | 骨格（ヘッドレス axum + レジストリ配線 + banto-collect 組み込み + REST 読み取り + API キー） | I1〜I3b | Tauri なし構成の初例。**実装済み**                                                                                                                                                                                                                                                                                                                                                                               |
| T1    | WebSocket 購読（on_change / interval、初期スナップショット）                                 | T0      | **実装済み**                                                                                                                                                                                                                                                                                                                                                                                                     |
| T2    | 書き込み経路（writable フラグ = I1 拡張、監査、ブレーカ、ブローカー統合 = I6 判断）          | T0, I5  | 安全設計レビューを実装前に。**実装済み**                                                                                                                                                                                                                                                                                                                                                                         |
| T3    | MQTT publish（rumqttc、retain、LWT）                                                         | T0      | §3 保留の MQTT 行を回収。**実装済み**                                                                                                                                                                                                                                                                                                                                                                            |
| T4    | gRPC（tonic、proto 版管理）                                                                  | T1      | **実装済み**                                                                                                                                                                                                                                                                                                                                                                                                     |
| T5    | 配布・運用強化（サービス化検討・ソーク・インストーラ）                                       | T0〜T4  | 実機検証（多重クライアント）含む。T5-1(Windows サービス化)/T5-2(NSIS インストーラ)/T5-3(運用ガイド)/T5-4(72h ソークハーネス実装)は実装済み、残るは T5-5(実機での72h soak 実行 + 実機最終サインオフ)。採番は docs/t5-handoff.md の定義（T5-4=ハーネス／T5-5=実機サインオフ）に統一（2026-09-01、banto-hub-desktop-plan.md §16.5 の残件を解消）。docs/banto-hub-operations.md §10/§11/§12、docs/t5-handoff.md 参照 |
| T6    | 演算タグ・内部タグ（タグ種別 = I1 拡張、式評価器、DAG 検証）                                 | T0      | SCADA 計画次第で前倒し可。**実装済み**                                                                                                                                                                                                                                                                                                                                                                           |
| T7    | オンライン部分再構成（接続単位の入れ替え = I7）                                              | T0, I7  | 外部契約不変のため後入れ可能。**実装済み（T7-1/T7-2、2026-08-05）**                                                                                                                                                                                                                                                                                                                                              |
| T8    | ワードデバイスのビットアクセス（アドレス側 `.N` 記法、書き込みは RMW + 確認読み）            | T2, T0  | tag-server-design.md §6.1。**実装済み（T8-1/T8-2、2026-08-06）**                                                                                                                                                                                                                                                                                                                                                 |
| T9    | 接続単位のシミュレーションモード（実機なしでの開発・検証、ランプ波生成）                     | T0      | ux-plan.md §1。**実装済み（T9-1/T9-2、2026-08-07）**                                                                                                                                                                                                                                                                                                                                                             |
| T10   | ライブタグモニタ（管理 UI の現在値一覧、WS 購読ベース）                                      | T1      | ux-plan.md §2。**実装済み（2026-08-07）**                                                                                                                                                                                                                                                                                                                                                                        |
| T11   | タグの一括登録（連続登録 + CSV インポート/エクスポート）                                     | T0      | ux-plan.md §3。**実装済み（T11-1/T11-2、2026-08-07）**                                                                                                                                                                                                                                                                                                                                                           |
| T12   | PLC 接続テストボタン（保存前の疎通確認）                                                     | T0      | ux-plan.md §4。**実装済み（2026-08-07）**                                                                                                                                                                                                                                                                                                                                                                        |
| T13-1 | 画面レイアウト刷新: 汎用部品（Drawer/SplitPane/Tree）+ tags ページの master-detail 化        | T0, T11 | ux-plan.md §4b。**実装済み（2026-08-08）**。残 T13-2/T13-3                                                                                                                                                                                                                                                                                                                                                       |

## 5. 技術課題メモ（議論済みの設計方向）

- **収集エンジン**: 24/365 前提。PLC断→自動再接続、品質フラグ
  （Good/Bad/Stale、roadmap §5 踏襲）、PC/PLC時刻ずれの扱い、電源断復帰。
  **ソークテスト（数日連続稼働）を検証項目に含める** — CRUDアプリと違う規律
- **時系列ストレージ（I3a: banto-tstore、実装済み）**: SQLite + WAL +
  バッチINSERTから開始。ワイドテーブル（`samples_<n>(ptime, c1..cN)`、
  グループ単位収集と親和）+ **日単位のファイル分割 + スキーマ凍結**
  （ファイルは作成時にスキーマ確定・以後不変、構成変更は連番ローテーション
  で吸収。チャート紙ロール交換に相当する運用感覚、M17バックアップと整合）。
  保持期間と自動間引き（`prune_files`）。負けたら PostgreSQL /
  TimescaleDB へ昇格（banto バックログの条項を発動）
  - **2026-09-24 オーナー決定（#424 案 A、実装済み）**: 行の記録時刻は
    ファイルの日付から ±24h 以内、を契約にする。`TsWriter::append` は、
    読み出し（banto-tsquery の候補ファイル選び）と同じ述語・同じ余白の定数
    （banto-tstore の `ptime_is_findable_in` / `candidate_date_range` /
    `MAX_OFFSET_PAD_MS`）で、その時刻だけを含む狭い検索範囲だとファイルが
    候補から外れて取りこぼす行をエラーで拒否する（ファイルの日付も含む広い
    範囲なら読めるが、範囲の取り方で結果が変わる）。受け付けた行は、その
    時刻を含むどの検索範囲でもファイルの選択から漏れない。
    保持期間の削除（`prune_files` / `plan_prune`）もこの契約に依存する。
    時計が 24h 規模で飛んだ瞬間の 1 サンプルは捨て、収集は既存の書き込み
    失敗として記録する。ファイルに時刻の最小・最大を持たせる案（案 B）は
    今回やらない（詳細は recorder-requirements.md §3.4）
- **アプリの DB スキーマ（2026-10-02 オーナー決定、実装済み）**: banto-industrial の
  独自実装は banto に寄せる（移植で直さない）。その土台として、両アプリ（chronogazer・
  banto-hub）の自前テーブル（settings・users・audit_log）を **banto の admin-template と
  同じ形**にした。決定は 3 点:
  - **既存の DB は壊してよい**（アルファ版。互換の移行は作らない）。ただし古い DB を
    開いたら黙って動かさず、**起動を拒否**して DB のパスと「削除または退避して起動し直す」
    を出す（判定: アプリの migration 記録テーブルが無いのに `users` か `settings` がある）。
  - **スキーマの置き場は、当面上流 admin-template の SQL を byte 等価でコピー**する
    （banto v2.1.1 `apps/admin-template/core/migrations-sqlite/` の 0002・0003・0004・0005・
    0007 → `apps/*/core/migrations-sqlite/`。banto 側には手を入れない）。banto-hub 固有の
    テーブルは `0101_*` 以降に最終形で置く。
  - **banto-hub の旧 DB 拒否はログと手順書だけ**（インストーラ等に作り直しの操作は
    作らない）。手順は [banto-hub-operations.md](banto-hub-operations.md) §1
    「旧形式の DB で起動を拒否されたとき」、chronogazer は
    [apps/chronogazer/README.md](../apps/chronogazer/README.md)。

  実装: sqlx 0.9 の `Migrator::dangerous_set_table_name` で記録テーブルを migrator ごとに
  分け（`_sqlx_migrations_chronogazer` / `_sqlx_migrations_banto_hub` /
  `_sqlx_migrations_banto_tags`。`banto-collect` は冪等 DDL のまま）、手書きの冪等 DDL を
  `sqlx::migrate!` に戻した。経緯と理由は各アプリの `core/src/db.rs` のモジュール doc と
  [r1a-readme-gaps.md](r1a-readme-gaps.md)

- **ChronoGazer のサービスと REST ルーターを banto のものに（2026-10-04、実装済み）**:
  banto に寄せる作業の段階名では I2a（§4 の I2a = SLMP 読み取りクライアントとは別物）。
  上のスキーマの整理を土台に、ChronoGazer core が持っていた admin-template のコピー
  （`users`・`settings`・`audit`・`backup` の 4 サービスと、`rest.rs` の users/auth/ui-settings/
  audit-log/backups の各ルーター・RBAC と監査のヘルパ）を削除し、banto v3.0.0 の
  `banto-admin-services` と `banto_server::routes` のものを使う。ChronoGazer に残るのは固有の
  設定キー（`data.dir`・`retention.days` の `StoreSettings`、`hub.*`）の型付きラッパと、Hub・
  収集・タグレジストリの口だけ。これで自前のコピーに入っていなかった banto v2.1.0 のセキュリティ
  修正（#277 初回セットアップの原子化、#278 未認証ログアウト・失敗ログインの監査の増幅、#280
  バックアップ保存先の DB ごとの分離）が入った。オーナー決定は 2 点:
  - **#280 の旧バックアップは起動時の警告のみ**（banto の既定どおり。共有領域
    `<DB のフォルダ>/backups/` 直下に残ったファイルを移す処理は作らない）。
  - ~~**閲覧公開（`server.viewer_public`・`POST /api/auth/grant/publicViewer`）は設定画面に
    出さない**（既定 OFF）~~ → **同日のオーナー決定で変更**（下の I2b）。I2a の時点では
    `GrantRegistry` を空にし、汎用の `settings_set` から `server.viewer_public` を書かせず、
    LAN 公開の保存では常に OFF を書いていた（暫定の塞ぎ。I2b で外した）。

- **LAN 設定の適用を banto に寄せ、閲覧公開を使う（2026-10-04、実装済み）**: banto に寄せる
  作業の段階名では I2b。
  - **適用の順序**: ChronoGazer の `server_apply`（`src-tauri`）は「保存 → 停止 → 起動」で
    ロールバックが無く、使用中のポートで適用すると新しい値が保存されたまま旧サーバーも
    止まっていた。admin-template v3.0.0 の形（banto v2.1.0 の #287・#288・#294）に揃えた:
    検証 → 旧サーバー停止 → `banto_server::bind` → 保存（`auth_config_lock` の下）→
    `BoundServer::serve`。bind か保存に失敗したら何も保存せず旧設定でサーバーを起こし直し、
    `settings_change` / `failed` を監査する。起動時の自動開始の判定も banto の
    `auth_server_combination_allowed` に揃えた。
  - **オーナー決定（2026-10-04）: ChronoGazer でも閲覧公開を使う**（同日 I2a の「画面に
    出さない」を変更）。手元の端末はログイン不要モード、LAN の相手には閲覧だけ、という
    banto の使い方をする。REST（`core/src/rest.rs`・`banto-serve`・デスクトップの組み込み
    サーバー）は `GrantSpec::public_viewer` を登録し、設定の「接続」に admin-template と
    同じ項目を出す。閲覧公開を OFF で適用すると保存の直後に発行済みの閲覧者のトークンを
    失効させる（ADR-0017 の順序）。「ログイン不要モード + LAN」は閲覧公開 ON のときだけ。
    閲覧者に見せる画面は監視・ヒストリカル・イベントで、タグ設定（PLC の接続先を含む）・
    ユーザー管理・監査ログ・設定は出さない（`navigation.ts` の `NavItem.publicViewer`）。
    I2a の暫定の塞ぎ（`settings_set` の拒否・常に OFF で保存）は外した。

- **起動時の環境判定を banto に寄せる（2026-10-04、実装済み）**: banto に寄せる作業の段階名では
  I2c（banto v2.1.0 の #286）。ChronoGazer の `setup.ts` の `isEmbeddedServer()` は
  `GET /api/auth/check` の fetch の失敗・例外を「サーバー無し」と読み、LAN/REST のビルドで一時的に
  届かないだけで demo（メモリ上の空データ）に落ちて戻らなかった。admin-template v3.0.0 の
  `environment.ts`（`probeBackend` の 3 値 server / none / unreachable、`isDemoBuild` =
  `VITE_BANTO_DEMO=1`）・`startup.ts`（`resolveStartupTarget`）・`startupState.svelte.ts`・
  `StartupSplash.svelte` を写し（文言は日本語の直書き）、届かないときは起動待ちで自動再試行
  （2 回）→「サーバーに接続できません」+「再接続」にした。demo になるのは `VITE_BANTO_DEMO=1` の
  ビルドと、同じオリジンが「`/api` は無い」と確定的に答えたときだけ。ChronoGazer には demo を
  静的に公開する経路が無いので、`VITE_BANTO_DEMO=1` を設定する場所は無い。banto-hub は `server`
  固定で probe しないので対象外。保護画面を直接開いたとき、届かない間は真っ白のまま
  だった既知の制約は、banto v3.0.1（#321）の追従で解消した（2026-10-05、`(app)` のガードが判定を待たず
  起動待ちの印付きの 503 で延期し、ルートのレイアウトがスプラッシュを出して起動後に同じ URL をやり直す。
  `startupGate.ts`）。

- **ChronoGazer を SvelteKit 3 に移行（2026-10-05、実装済み。banto v4.0.0 の #325・#326）**: Rust の
  `banto-*`（両アプリ共通）と ChronoGazer の npm `@banto/*` を v4.0.0 に上げ、`sv migrate sveltekit-3` の
  後に admin-template v4.0.0 の形を写した: `svelte.config.js` を廃止して `vite.config.ts` の
  `sveltekit({...})` に、`$lib` → `#lib`（`package.json` の `imports`、拡張子が必須）、`tsconfig.json` は
  `$app/tsconfig`、`invalidateAll()` → `refreshAll()`、`error(status, message, { … })`。ナビ・設定カテゴリの
  表の `path` は `AppPath`（`` `/${Path}` ``。存在しないルートは型エラー）にし、URL にするときは
  `resolveAppPath()` を通す。ChronoGazer は `base` を使わないので自動移行はガードを書き換えなかったが、
  閲覧公開のガード（`(app)/+layout.ts`）も上流の形（`url.pathname` と `resolveAppPath()` の結果を比べる）に
  揃え、上流で自動移行が入れた `resolve('')` の形に戻すと単体テストが落ちることを確かめた。#326 の
  `navigationSettled.svelte.ts` を写し、#321 の起動待ちのやり直しと配線①はナビゲーションが終わるまで
  `refreshAll()` を始めない。banto-hub の npm は v3.0.1・SvelteKit 2 のまま（移行中の一時的なずれ。
  [README.md](README.md)「現状ひとめ」。banto-hub の移行は次の PR）。

- **banto-hub を SvelteKit 3 に移行（2026-10-05、実装済み。banto v4.0.0 の #325・#326）**: banto-hub の npm
  `@banto/*` を v4.0.0 に上げ、ChronoGazer と同じ手順（`sv migrate sveltekit-3` → 手直し）で移行した。これで
  両アプリの Rust・npm が v4.0.0 に揃い、上の移行中のずれは解消。`AppPath` / `resolveAppPath()` を写し、アプリ内の
  `goto()`・`redirect()`・`href` はすべて `resolveAppPath()` 経由（存在しないルートは型エラー）。banto-hub には
  閲覧公開が無く、ガード（`(app)/+layout.ts`）にパスの比較は無いので、自動移行の `resolve('')` の罠は
  /login への redirect の行き先で確かめた（`resolveAppPath()` を `resolve('')` への連結に壊すとガード・
  `monitorHref` の単体テストが落ちる）。#326 の `navigationSettled.svelte.ts` を写し、配線①はナビゲーションが
  終わるまで `refreshAll()` を始めない（タグ画面の未保存の変更の確認 `beforeNavigate` を飛ばさせない）。
  単体テストの vitest は `$app/paths` を読むため `sveltekit()` プラグインに替えた。

  続き（未着手）: 初回セットアップ画面。手順と
  wire の変化は [apps/chronogazer/README.md](../apps/chronogazer/README.md)「アカウント・
  監査ログ・バックアップ」「LAN アクセスと閲覧公開」と `apps/chronogazer/core/src/rest.rs` の
  モジュール doc

- **banto-hub のサービスと REST ルーターを banto のものに（2026-10-04、実装済み）**: banto に
  寄せる作業の段階名では I3'。I2a と同じやり方で、banto-hub core が持っていた ChronoGazer 由来の
  コピー（`users`・`audit` の 2 サービス、`settings` の汎用部分、`rest.rs` の auth（status/setup/
  change-password）・users・audit-log の各ルーター、ログイン/ログアウトの監査、`record_write` などの
  ヘルパ）を削除し、`banto-admin-services` と `banto_server::routes` のものを使う。これで banto
  v2.1.0 の #277（初回セットアップの原子化）と #278（失敗ログインの名前の切り詰めとダミー検証、
  未認証・無効なトークンのログアウトを記録しない）が banto-hub にも入った。
  - **残した固有部分**: hub 固有の設定キー（`server.bind/port`・`data.dir`・`retention.days`・
    `mqtt.*`・`grpc.*`）の型付きラッパ（`HubSettingsExt`。hub の bind/port は `server.enabled` を
    持たず既定 8722 なので、banto の `server_config` と別名の `hub_server_config`）、#431 の
    `require_session` とその後ろの RBAC の床、`Sec-WebSocket-Protocol` の bearer、API キー・MCP・
    gRPC・書き込み監査・pending changes・sink・タグ空間の各ルーター。
  - **RBAC の床は banto の `RoleGuard` にしない**: banto のものはゲートがセッションを載せないと DB で
    照合し直すので、#431 の「DB が答えないときも緊急停止は通す」例外が床で 500 になる（入れ替えると
    #431 のテストが落ちることを確認）。`require_session` の後ろは自前の `SessionRoleGuard` のまま。
    banto のルーター（users・audit-log）は banto の `require_auth` + `RoleGuard` を自分で積む（どちらも
    #431 の例外が無い通常の操作で、判断は `require_session` の通常の操作と同じ）。
  - **wire・監査の語彙の変化**: `PUT /api/audit-log/config` の成功は `settings_change` / `settings`
    （以前は `update` / `audit_log_config`）、`/api/audit-log/config` の拒否の resource も `settings`。
    監査ログ一覧の応答に `deletionEpoch`（#248 の受け皿）、読めない `asOfId` は JSON の 400。試運転の
    grant での change-password は 403。詳細は [banto-hub-operations.md](banto-hub-operations.md)
    §1「初回セットアップの運用」・§9「監査ログに残るもの」と `apps/banto-hub/core/src/rest.rs` の
    モジュール doc。版（v0.2.0-alpha.28）は上げていない。

- **ハイブリッドトレンド**: メモリ上のローリング窓（直近）+ DB（過去、
  間引き済み）を、チャート viewport の参照位置で継ぎ目なく合成する
  クエリ層。M13 の「全域表示中のみ追従/ズーム中は窓維持」が前段
- **性能エスカレーション**: サーバ側集約（第1段）→ Canvas レンダラ（第2段）。
  フロント WASM は不採用（banto リポジトリの docs/template-scope.md §4.2。
  本リポジトリには実体がない上流ドキュメント）

## 6. 全体の依存関係

```
banto M18（E2E/lint + 配布整備）
  → I0〜I2（タグ・SLMP）
    → I3a（時系列ストレージ）→ I3b（収集エンジン）→ I4（クエリ層）
      → R1（リアルタイム）→ R2（ヒストリカル）
                              → R3（ハイブリッド+CSV/帳票）← banto M19（report）
                                → R4（イベント・ソーク・配布）
    → I2a（SLMP読み取り）→ I5（banto-plc-write 書き込みクレート）
      → W1〜W5（relay-wright 自動書き込みアプリ、§4b）
    → I1〜I3b → T0〜T5（タグサーバー、§4c）← I5（T2 書き込みのみ）
banto M20（添付）/ M21（バーコード）は記録計と独立（倉庫・生産系案件向け）
```
