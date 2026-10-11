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
（実装済み。両アプリの npm が v4.0.0 に揃い、移行中のずれは解消）を追記。2026-10-06: §5 に
両アプリの監査ログ画面を banto の `SnapshotListResource` に寄せたこと（実装済み）を追記。同日: §5 に
banto v5.0.0 追従（監査ログ画面の回避策の撤去、実装済み）を追記。2026-10-07: §5 に ChronoGazer の
イベント一覧（`/events`）も `SnapshotListResource` に寄せたこと（実装済み）を追記。同日: §5 に ChronoGazer の
未保存の入力の確認（`beforeNavigate`、#508、実装済み）を追記。
2026-10-08: #517 の振り分けで、§2 に旧「譲渡前に昇格」の運用を残さないこと、§5 に AI 対話による
機能作成（記録計）の方向の採用（要件への追補は #524、未実装）を、オーナー決定として追記。
同日: §5 の同項に、要件の草案（recorder-requirements.md §3.7、未実装・未決あり）への参照を追記。同日: §5 の同項に、PLC 接続・収集グループ・タグ定義（I1）も対象に含めること、目的（ユーザーの負荷を極力減らす）、適用の動作（手動と同じ）のオーナー決定を追記。同日: §3.7 の未決（9 項目）がオーナー決定で確定したことを §5 の同項に反映。
同日: §5 に、しきい値は使う側（記録計・SCADA）の設定であり Hub は持たない・判定しないオーナー決定（#532・#533）を追記し、AI 対話の項の「しきい値を含む」を記録計の側の設定に直した。
同日: §5 の同項に、#532（ChronoGazer の記録計の側のタグごとの設定への移行）を実装したことを追記（#533 は未着手）。
2026-10-09: §5 の同項に、#533（Hub と `banto-tags` からしきい値を外す）を実装したことを追記。
2026-10-10: §5 に #383 段階3（Hub を接続種別として扱い、外部ソース接続タスクを `banto-collect` に足す）と
#392（収集エンジンを UI から独立させる段階移行）のオーナー決定（H1〜H11）を追記し、§4 のドライバ段落を更新（決定のみ、未実装）。
2026-10-11: §5 の #383 段階3 に、P2（`banto-tags` の `hub`、#566）と P3（`banto-collect` の Hub サンプラー）を
実装したことを追記。同日: P4（タグを「Hub から追加」で登録する導線）を実装し、P4 の細部（オーナー決定）を追記。
同日: P5（監視・履歴・しきい値の結合）を実装し、Hub の `value_source` を解釈しない（simulation 系・未知の出所も Good なら
記録する）オーナー決定を H4・P3 の細部の置き換えとして追記。
同日: P6（実機確認）を実施（R08ENCPU、Hub 経由 SLMP と直結 SLMP を別ポートで比較。ハーネスの判定は全段 PASS、
記録は [real-machine-test-2026-10-p6.md](real-machine-test-2026-10-p6.md)）。
同日: §5 の #392 に A2（ChronoGazer のトレイ常駐。ウィンドウを閉じても収集を止めない）を実装したことと、その実装の細部を追記
（B1 以降は未）。
2026-10-12: §5 の #392 に A2 のオーナー決定（トレイの左クリックを両アプリで「窓を戻す」に揃える、banto-hub にも終了の
二重防止 `QuitGate`、OS 終了で banto-hub が `RunningHub::shutdown` を通らない件は #586、ログオン時の自動起動は B）と、
#383 段階3 の P6 で分かったこと（Hub 側の収集だけが止まった場合のイベントは Hub 側に出る。このままでよい）を追記。
同日: §5 の #392 A2 の実装の細部に、トレイの「終了」の確定を収集の開始と直列化したこと（終了の受付の印、#392 A2 レビュー）と、再レビューでの追加（印を一時的・取り消せないの 2 種類に分けた、ChronoGazer で終了の `stop` を終端の操作にした）を追記。
同日: §5 の #392 A2 のオーナー決定 3 に、#586（banto-hub のデスクトップシェルが OS のサインアウト・シャットダウンでも
`RunningHub::shutdown` を通る。実装済み、Windows 実機の OS 終了は未確認）を直したことと、残件（ChronoGazer の後始末の 1 回を共有 crate に寄せるか）を追記。
同日: 同項に、#587 のレビューでホスト切替そのものを終了の追跡に入れたこと（SCM を呼ぶ前なら取りやめ、呼んだ後は完走させて終了が待つ）を追記。
T13〜T18 の詳細と最新の全体像は
[banto-hub-remaining-plan.md](banto-hub-remaining-plan.md) と
[banto-hub-desktop-plan.md](banto-hub-desktop-plan.md) を正とする（本文 §4c 表は
T13-1 までの粒度で、以降は同書へ移管）。Hardening（H1〜H10）は H7 の① 実機 soak
のみ残（詳細は improvement-plan.md）。docs 全体の
地図は [README.md](README.md)**（2026-10-11 更新。本文の T 系表は 2026-08-08 時点の
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
- 旧計画（banto の旧 industrial-plan）の「汎用機能は譲渡前に banto-industrial へ昇格する」
  運用は残さない（2026-10-08 オーナー決定、#517。MIT 公開のため前提が無い）

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
**段階1（Hub 購読）・段階2（SLMP / Modbus TCP 直結 = R1-B/R1-C）は実装済み。段階3（Hub 経由タグの合流・
保存・表示）の方針は 2026-10-10 に決定済みで、2026-10-11 に P2〜P4（種別・収集と保存・「Hub から追加」の
登録の導線）に加えて P5（監視・履歴・しきい値の結合）まで実装し、同日 P6（実機確認）も実施した（§5。ハーネスの判定は全段 PASS）**。P4 から、Hub 経由で購読する
のは「タグ設定」で登録したタグで、設定画面でのタグ選択は無くなった（§5「P4 の細部」）。

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

- **ChronoGazer の未保存の入力の確認（`beforeNavigate`、2026-10-07、実装済み。#508、経路 B）**: admin-template の
  `unsavedChanges.ts`（`@banto/forms` の `guardUnsavedChanges` / `UnsavedChangesNotice`）を写し、タグ設定・ユーザー管理・
  設定の入力のある画面に適用した（banto-hub は #214 より前に独自の仕組みを持つので対象外）。ログイン画面への移動は
  確認しない。E2E は `e2e/tests/user-unsaved-changes.spec.ts`。**Tauri のウィンドウを閉じるときの確認（上流の
  `windowCloseGuard.ts`）は入れていない** - close-requested を JS が握るとネイティブの閉じる動作が JS 任せになるため、別に決める。

- **監査ログ画面を banto の `SnapshotListResource` に寄せる（2026-10-06、実装済み。banto #248、
  「重複実装は banto に寄せる」の続き）**: 両アプリの `/audit-log` は、ブロックの遅延取得・世代の
  スナップショット境界（`asOfId`）・剪定による失効・ブロック単位の失敗・処理中の中断を自前で持っていた
  （ChronoGazer #410 の `auditBlocks.ts` + `#lib/blockCache.ts`、banto-hub #428 のその複製）。banto v2 以降の
  `@banto/admin-core` の `createSnapshotListResource` が同じ判断を持つので、画面を admin-template の
  `/audit-log` の形に写して置き換え、`auditBlocks.ts`（+ テスト）と banto-hub の `blockCache.ts`（+ 複製の
  一致を見ていた `blockCache.sync.test.ts`）を削除した。ChronoGazer の `blockCache.ts` は `/events` が使うので
  残した（2026-10-07 に `/events` も寄せて削除。下の「イベント一覧」の段落）。サーバー側は I2a / I3' で既に banto の `audit_log_router`（Tauri は `list_as_of`）なので変更なし。
  挙動の差: 応答の `deletionEpoch` でも失効を見る（件数が変わらない剪定 + 遅れたコミットも検出）、
  「再読み込み」は処理中でも押せる（処理中の要求を中断して新しい世代に）、選択中の行を強調する。
  文言（日本語の直書き・「0件の記録があります。」）と 15 秒の読み取りの上限は残した（banto の上限の
  文言は英語の固定文なので、`auditLogAdmin.ts` の `createAuditLogFetcher` がこちらの上限を先に掛ける）。
  **banto v5.0.0 追従（2026-10-06、実装済み）**: この回避策（取得関数の側の二重の上限 `runWithLimit` と、
  境界の食い違いの英語の固定文との文字列比較 `auditErrorText`）を外した。15 秒はリソースの
  `requestTimeoutMs` に、上限切れ・境界の食い違い・応答の形の不正の日本語の文言は `messages` に渡し
  （`createAuditLogResource`）、画面の失敗表示は `failures` の最も前の `'error'` から出す（admin-template v5 と
  同じ）。banto-hub の `runWithLimit.ts` は削除（ChronoGazer の `hubAdmin.ts` のものは収集・Hub の設定画面の
  ポーリングが使うので残す）。

  **ChronoGazer のイベント一覧（`/events`）も同じリソースに（2026-10-07、実装済み。オーナー決定 2026-10-06）**:
  `/events` に残っていた自前のブロックキャッシュ（`routes/(app)/events/eventBlocks.ts` と `#lib/blockCache.ts`、
  #409/#410）を `createSnapshotListResource` に置き換え、両ファイル（+ テスト）を削除した。継ぎ目は
  `collectAdmin.ts` の `createCollectEventsResource`: 取得関数が `Readout` の `unavailable` / `notRunning` を
  `EventsReadoutError`（`ProviderError` の派生）として投げ、リソースが同じオブジェクトのまま `failures` に残すので、
  画面（`eventsView`）は「読めなかった」（注記「読み取れませんでした（0件ではありません）」）と往復の失敗・
  上限切れ（赤字）を言い分け、**両方を同時に出せる**（従来どおり。どちらもいちばん前のブロックの失敗）。上限は
  4 秒（`COLLECT_READ_TIMEOUT_MS`）を `requestTimeoutMs` に渡し、文言は `messages` の日本語（取得関数の側の
  `runWithLimit` はやめた）。オーナー決定: **トーストは出さない**（`notify: false`）、「再読み込み」は**処理中でも
  押せる**（監査ログ画面・banto と同じ）。挙動の差: 最初のブロックが失敗した世代では次のブロックへ進まない、
  応答の形の不正も失敗として出す、境界の中の件数が変わったら失効として止める（`collect_events` には削除が
  まだ無いので今は起きない）。wire（Tauri `collect_events_list` / REST `GET /api/collect/events`）は変わらない。

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
- **AI 対話による機能作成（記録計）**: **目的はユーザーの負荷を極力減らすこと**。
  PLC 接続・収集グループ・タグ定義（I1）・記録計の側のしきい値の設定・トレンドのグループ表示構成・
  帳票テンプレートを再ビルドなしに変えられる宣言データ（DB）として持ち、AI アシスタントが
  対話で作れる範囲をこの宣言データの範囲と一致させる。生成した定義はバリデーション →
  プレビュー → 承認 → 保存（監査付き）を通し、承認した AI の変更は人が同じ変更を手動で
  適用したときと同じ動作をする。汎用画面エディタは入れない（§4 の護り）。旧 industrial-plan §5
  （2026-08-26 議論）の方向を **2026-10-08 オーナー決定で採用**（#517）。PLC 接続・収集グループ・
  タグ定義を対象に含めること、目的、適用の動作は同日のオーナー決定で追加。要件への追補は #524
  （[recorder-requirements.md](recorder-requirements.md) §3.7。運用の細目も同日オーナー決定で確定し、未決なし）、未実装
- **しきい値（HH/H/L/LL）は使う側（記録計・SCADA）が持つ設定**（2026-10-08 オーナー決定、
  #532・#533）。データ点（タグ定義）の性質ではない。Hub（`banto-tags`）はしきい値を持たず、
  警報も判定しない（データを集めて配る役に徹する）。
  - 警報の判定の正は PLC。PLC 経由の接続では、記録計のしきい値は表示と記録の補助。
    Modbus で計器に直接つなぐ接続は PLC の判定が無いので、記録計（または SCADA）の
    しきい値の判定が警報の役を担う。
  - SCADA は記録計の機能を含むので、同じ計器を両方で判定することは無い。それぞれが
    自分のしきい値を持つ。
  - しきい値は任意で、**既定は設定なし**（設定なしのタグは色・帯・しきい値イベントを出さない）。
  - しきい値のイベントには、**判定に使ったしきい値**を残す。
  - 作業: #532（ChronoGazer: 記録計の側のタグごとの設定へ移す。先。**2026-10-08 実装** -
    `recorder_tag_settings`・`/api/tag-thresholds`、しきい値イベントに判定値 `limit_value`。
    既存のタグの列の値は移さない）→ #533（Hub と `banto-tags` から外す。**2026-10-09 実装** -
    banto-tags の migration 0018 で `tags` のしきい値の列を落とし、Hub の画面・REST・MCP・CSV・
    設定パッケージから外した。値付きのしきい値は検証エラーで断る。Hub の収集は判定しない）。要件は [recorder-requirements.md](recorder-requirements.md)
    §3.1・§3.2・§3.5・§3.7 に反映済み。
- **#383 段階3（Hub 経由タグの合流・保存・表示）と #392（収集エンジンの UI 独立）（2026-10-10 オーナー決定「おすすめで」。#383 段階3 は P5 まで実装、#392 は A1・A2 を実装）**:
  - **#383 段階3**:
    - **H1 Hub は接続種別として表す**: `plc_connections.protocol` に `hub` を足し、Hub 由来のタグは
      `tags` の行にする（ペン・しきい値 `recorder_tag_settings`・履歴・除外が同じタグ ID で動く）。
      Hub は 1 つ。接続先とキーは `hub.record` のまま。
    - **H2 収集経路は `banto-collect` に「外部ソース」接続タスクを足す**: 日次の tstore ファイル・
      単一ライター・イベント・品質・stale を共有する。ChronoGazer 内に別の recorder を持って読み出し時に
      合流する案は、履歴・保持・イベントが二重になるため**却下**（#383 本文の初期の推奨は後者寄りだったが、
      本決定で置き換える）。
    - **H3 タグ定義は登録時に Hub のカタログから複製**（名前・データ型・単位）。同一性は外部名
      （scada-design.md §9.6、2026-09-30）。収集開始時にカタログと突き合わせ、消えた・型が変わったタグは
      外す（#414 と同じ）。
    - **H4 品質と記録**: Hub が Live でない間は全タグ `bad` で `null` を記録する。イベントは
      `plc_connected` / `plc_disconnected` を Hub の接続キーで再利用（新しい `EventKind` は足さない）。
      Hub の `stale` は Stale、`bad`・不明は Bad（good に丸めない）。**Hub の `value_source` は解釈しない**
      （`simulation`・`derived_simulation`・未知を含めて品質だけで扱う。2026-10-11 オーナー決定で置き換え: 以前は
      `simulation`・`derived_simulation` の値を記録しない（#413 と同じ）としていた。下の「P5 の決定」）。記録時刻は
      この PC の時計（R0 §4）。**購読は on-change**（Hub は値・品質が変わったときだけ送る）なので、Hub の `t` の
      経過時間で鮮度を判定しない: Live かつ good の定常値は、受信が長く途切れても最後の値を周期ごとに記録し続け
      Stale にしない。Stale・Bad の根拠は Hub の品質ラベルと Hub 接続の状態（Live でない）だけ（P3 の検証条件に
      「Live + good の定常値が複数周期続いても Stale にしない」を含める。2026-10-10 #564 のレビュー。当時の
      「`simulation`・`derived_simulation` をそれぞれ記録しない」は 2026-10-11 の決定で「どれも Good なら記録する」に替えた）。
    - **H5 Hub 経由の書き込みはしない**（R0 §7）。
    - 手順: P2（`banto-tags` に `hub` protocol）→ P3（`banto-collect` の Hub サンプラー）→ P4（タグを
      「Hub から」登録）→ P5（監視・履歴・しきい値の結合）→ P6 実機確認（Hub 経由の SLMP と直結 SLMP を別ポートで
      比較）。
    - **実装状況（2026-10-11）**: P2 は #566 で実装（スキーマと検証。収集は外していた）。P3（`banto-collect` の Hub サンプラー）は 2026-10-11 に実装: `banto-collect` に外部ソース （`ExternalSource`）の接続タスクを足し、ChronoGazer の Hub 購読（`HubService`）がそれを実装する。購読するタグは画面の選択 ∪ 収集の需要（開始のたびにレジストリの hub タグの外部名から作る。**P4 で「登録済みの hub タグ ∪ 需要」に替えた** - 下の「P4 の細部」）、開始時に Hub のタグ一覧と突き合わせて消えた・型が変わったタグを外す。Hub が Live でない間は全タグ Bad + null（イベントは Hub の接続キーの `plc_connected` / `plc_disconnected` / `plc_reconnected` - PLC 接続と同じ意味）、Live + good の定常値は周期ごとに記録して Good のまま、Hub の `stale` は Stale（記録は null - 新しい測定ではないため）、（`simulation`・`derived_simulation` と未知の出所は、この時点では表示するが記録しなかったが、**P5 で替えた** - 下の「P5 の決定」）。Hub を持たない実行形態（`banto-serve`）では `hubSourceUnavailable`、Hub 未接続では `hubNotConfigured` で hub 接続を外す（PLC 直結は動く）。詳細は `crates/banto-collect/src/external.rs` と `apps/chronogazer/core/src/collect.rs` のモジュール doc。P4（登録の導線）も 2026-10-11 に実装（下の「P4 の細部」、規則は `apps/chronogazer/core/src/hub_tags.rs`・購読は `hub.rs` のモジュール doc）。**P5（監視・履歴・しきい値の結合）も 2026-10-11 に実装**（下の「P5 の決定」）。**P6（実機確認）も 2026-10-11 に実施**: R08ENCPU を Hub（SLMP ポート 3101）と ChronoGazer の直結（同 3102）で同じ 25 タグ（i16・u16・i32・bit）を周期 1000 ms で読み、60 秒の定常区間で行数（62 / 62）・null（両側とも開始直後の 1 行だけ）・値（全組一致）が一致、Hub のプロセスの停止中は Hub 経由のタグだけが Bad + null（直結は影響なし）で `plc_disconnected` → `plc_reconnected`。Hub の収集だけを止めたときはリンクが Live のままなのでイベントは出ず、値が Bad + null になる（設計どおり）。値が変化する場合の追従は、PLC へ書き込まない条件のため未確認。ハーネスは `apps/chronogazer/core/examples/hub_p6_compare.rs`、記録は [real-machine-test-2026-10-p6.md](real-machine-test-2026-10-p6.md)。
    - **P3 の細部（2026-10-11 オーナー決定「おすすめで」）**: Hub が戻ったときのイベントは PLC と同じ `plc_reconnected`（最初は `plc_connected`）。~~未知の `value_source` は実値として扱わず記録しない~~（**P5 で置き換え**: 未知の出所も Good なら記録する。下の「P5 の決定」）。Hub の `stale` は null で記録する（古い値を新しい測定に見せない）。収集中に画面の選択タグを変えると購読の張り直しで数周期の切断・再接続と Bad が出る件は P4 で扱いを決める（→ P4 で選択を購読から外して解消。下）。開始時に Hub が答えないときは突き合わせをせずに始め（開始の待ちは最大 20 秒）、消えたタグは次の開始まで Bad のまま。
    - **P4 の細部（2026-10-11 オーナー決定「P4進めてください」= 推奨どおり。実装済み）**:
      1. **`hub` 接続の許可**: ChronoGazer の REST・Tauri は `hub` 接続の作成・更新・削除を受け付ける（banto-hub は
         引き続き断る）。接続先とキーは `hub.record` のままで、行は host/port を持たない。画面は作成フォームでは作らず、
         「Hub から追加」が最初の登録で暗黙に作り（名前「Hub」、使われていれば「Hub 2」…）、一覧では読み取り専用
         （「有効」の切り替えと削除だけ。グループを持つ間の削除は `banto-tags` が断る）。**「有効」の切り替え**（2026-10-11 オーナー追加決定）:
         PLC 接続と同じ更新・同じ未保存の確認（接続の行に版は無いので楽観ロックも無い - PLC 接続の編集と同じ）。無効にした Hub
         接続は、配下のグループ・タグごと PLC 接続と同じく収集から外れ（`banto-collect` の組み立てが無効な接続を計画しない。
         除外の一覧にも出さない）、**そのタグは購読もしない**（下の 4）。切り替えると購読を 1 回張り直す（許容）。**収集中に無効にしても購読から外す**（オーナーレビュー P2: 開始時の需要に名前が残ると和集合が変わらず購読が続いていた。レジストリを読み直すときに収集の需要も購読の対象の中に絞る - `Subscription::set_registered_from_registry`）。外れたタグは「収集を再起動」までは収集の計画に残り、購読していないので Bad + null で記録される（Hub 接続のタグが全部外れれば Hub の購読自体が止まり、Hub の接続キーで `plc_disconnected` も出る）。再起動で計画から外れる。
      2. **「Hub から追加」**: `/tags` のダイアログ（中央モーダル。banto-hub の `Modal` と層の約束を写した）で Hub の
         カタログ（`GET /api/hub/catalog`・`hub_catalog`、editor 以上、接続先・キー名は返さない）を検索・複数選択し、
         Hub 接続の下の既存グループか新しいグループ（名前・周期、既定 1000 ms、グループ作成の検証は `banto-tags`）に
         登録する（`POST /api/tags/from-hub`・`tags_register_from_hub`、editor 以上）。名前・データ型・単位・小数桁を
         複製、アドレス = 外部名、書き込み不可、スケーリングなし。数値と bit だけ選べ（文字列・構造体は理由付きで
         無効）、登録済みの外部名は「登録済み」で選べない。名前が登録先のグループで衝突したら**行ごとのエラー**
         （黙って改名しない）。1 回の登録は `BEGIN IMMEDIATE` の 1 トランザクションで全部か無し（暗黙の接続・グループも
         巻き戻す）、応答は banto-hub の `POST /api/tags/batch` と同じく常に 200 + `ok: false` で行ごとのエラー。
         2b. **外部名の一意性はすべての書き込みの経路で**（2026-10-11、Codex の指摘）: 「Hub から追加」だけでなく、普通のタグの
         作成・更新（REST・Tauri）とバッチでも、Hub 経由のタグの外部名はレジストリ全体で一意（`banto-tags` の配置検証が同じ実行者
         - プールか呼び出し側のトランザクション - で調べ、行ごと・項目 `address` のエラーにする。バッチ内の重複も行ごと）。`tags` は
           接続ではなくグループを参照するので UNIQUE 索引では表せず、migration `0020_tags_hub_external_name_unique.sql` のトリガで
           書き込みの文そのものでも断る（検査と書き込みの間に割り込まれた場合の最後の砦。後で `tags` を作り直す migration は
           このトリガも作り直すこと）。
      3. **Hub 経由のタグのスケーリング禁止**: `banto-tags` が恒等（`raw_lo == eng_lo` かつ `raw_hi == eng_hi`）以外の
         スケーリングを断る（作成・更新・バッチ。Hub は工学値を送るので二重換算になる）。タグの編集フォームは Hub 経由の
         タグでスケーリングの欄を出さない。
      4. **画面の選択を購読から外す**: 購読するタグ集合は**登録済みの hub タグ（レジストリの**有効な** hub 接続の下の
         すべてのタグ。グループ・タグの有効フラグには依らない）∪ 収集の需要**。ダイアログの「登録済み」の判定は無効な接続の下の
         タグも含む（2 本目を作らせない）。**Hub の接続が設定されている限り、収集を止めていても登録済みの
         タグを購読する**（設定画面が現在値を出せるように。需要は登録済みの部分集合なので、収集の開始・停止では集合が
         変わらず張り直さない）。設定画面の「選択したタグ」は読み取り専用の「登録済みのHubタグ」（現在値付き）に
         替えた。集合が変わるのは登録を変えたときだけで、レジストリを変えた REST・Tauri の後
         （`HubService::registry_changed`、張り直しのタスクは 1 本だけ）・突き合わせの頭・見張りの各周期でレジストリを
         読み直す。`banto-tagclient` を調べたが、購読の追加・削除を接続を保ったまま行う口は無い（`start(requests)` の要求は世代の間固定、`restart` は資格情報と接続先の差し替えだけ）。したがって**収集中に hub タグを
         追加・削除すると 1 回張り直し、数周期の Bad と切断・再接続のイベントが出る**（登録を変えたときだけなので許容）。
         **移行**: 既存の `hub.record.selected_tags` は自動ではタグにしない（グループと周期を持たないため）。
         「Hub から追加」が 1 度だけ事前にチェックして注記（「以前の Hub 接続の画面で選んでいたタグ」）を出し、最初の
         登録が成功したとき、または明示の「以前の選択を使わない」（`POST /api/hub/legacy-selection/dismiss`）で消す。
         登録の後の消去は**応答を待たせずに予約する**（消去は Hub の操作ロックを取るので、`connect` が握っていると最大 60 秒待つ。予約の間はダイアログ用の catalog が移行前の選択を空で返し、事前チェックは出直さない。応答の `legacySelectionClearScheduled`、`HubService::schedule_legacy_selection_clear`。2026-10-11 の P4 の監査）。古い記録は読める（後方互換）。選択の保存の口（`PUT /api/hub/selected-tags`・`hub_set_selected_tags`）は外した。
      5. **P3 の持ち越し**: 開始時の突き合わせで外したタグ（`hubTagNotFound` / `hubTagTypeChanged`）は収集の需要に
         残さない（`CollectorConfig::external_names` = 組み立ての後の計画で需要を絞る。Hub への往復は増えない）。
    - **P5 の決定（2026-10-11 オーナー決定。実装済み）**: 「Hub 側のシミュレーション値は記録して構わない。アプリケーションごとに責務は完全に分ける。むしろシミュレーション値でちゃんと動作できているか確認できる方が望ましい」。
      1. **ChronoGazer は Hub の `value_source` を解釈しない**。Hub 経由の値は `value_source`（`real`・`computed`・
         `internal`・`db`・`simulation`・`derived_simulation`・未知）に依らず、**品質だけ**で扱う（Good なら現在値に出して記録、
         Hub の `stale` は従来どおり Stale + null、bad・不明な品質は Bad、Hub が Live でない間は全タグ Bad + null）。
         `banto_collect::ExternalValueSource` と `ExternalReading::source` は廃止した（`banto-tagclient` の型は変えていない）。
         これは H4 の「`simulation`・`derived_simulation` は記録しない（#413）」と P3 の細部の「未知の `value_source` は記録しない」
         を**置き換える**。ChronoGazer 自身の接続単位のシミュレーション（`plc_connections.simulation`、PLC 直結のシミュレーション）
         は従来どおり記録しない（#413 のまま）。
      2. **監視・履歴・しきい値**: Hub 経由のタグは PLC 直結と同じ現在値・日次ファイル・イベントの口に乗っているので、監視画面・履歴・
         しきい値（`recorder_tag_settings`、シミュレーション由来の値でも判定してイベントを出す）はそのまま使える。モック Hub 相手の
         統合テスト（切断→再接続のイベント、bit タグの記録、しきい値）と E2E（Hub 経由のタグを含む表示グループが監視画面で値を出し、
         Hub を止めると通信エラーになる）で固定した。
      3. **接続の表示を Hub にも通る語にした**: イベントの種別は「接続 / 切断 / 再接続」（種別 `plc_*` と API は不変）。イベント一覧と
         設定 > 収集の接続の表は `conn:<id>` / `tag:<id>` ではなく接続・タグの名前（見つからなければキーのまま）。`/tags` の
         接続の節・収集グループの接続の欄は「接続」とし、Hub 経由の選択肢は `Hub（Hub 経由）`。Hub 経由のグループを選んだタグの
         作成フォームは編集と同じ Hub 用（外部名・スケーリング欄なし）。
      4. **Hub 経由の再接続は回数を数えない**: 外部接続の `Reconnecting` は `attempt = 0`（`EXTERNAL_RECONNECT_ATTEMPT`）で、
         画面は「再接続中」とだけ出す。tick ごとの標本は再試行の単位ではなく、購読が自分で張り直すため（以前は「再接続中（1回目）」
         のまま固まって見えた）。
      5. **除外の案内**: 除外の「PLC接続」を「接続」にし、Hub 系の理由ごとに案内を出す（`hubNotConfigured` = 設定 > Hub接続、
         `hubSourceUnavailable` = この実行形態では収集できない、`hubTagNotFound` = Hub 側の名前・キーの見え方を確認し、改名ならタグ設定でそのタグの
         アドレス（外部名）を編集、`hubTagTypeChanged` = タグ設定でデータ型を Hub に合わせて編集し、どちらも「収集を再起動」。
         削除・再登録は勧めない: 削除は記録計のしきい値も消し、表示ペンで使用中なら拒否され、旧 ID の履歴も取れなくなる。編集なら ID・しきい値・
         表示・履歴が保たれる）。タグ設定で直すものが無いとき（Hub 未接続など）は「不正」と言わない。
      6. **E2E 用の入口**: `banto-serve` は `BANTO_HUB_MEMORY_KEYS=1`（開発・E2E 用）のときだけ Hub の API キーをプロセス内に置き、
         収集にも Hub を繋ぐ（既定は変えない）。
    - **P6 で分かったことの決定（2026-10-12 オーナー決定）**: Hub 自体は動いていて **Hub 側の収集だけが止まった**
      ときは、ChronoGazer のイベント一覧には出さない（記録は Bad + null のまま）。**このままでよい** -
      停止は Hub 側のイベントに出るため（ChronoGazer が Hub の収集の状態まで写して二重に出さない）。
  - **#392 収集エンジンの UI 独立**:
    - **H6 scada-design.md §13.2 の 3 host モデルへ段階的に移る**: まず A（単一インスタンス・プロファイル排他 +
      トレイ常駐。ウィンドウを閉じても収集を止めない）、次に B（`banto-serve` から本番用の headless host、
      シェル起動の決定は banto-hub の T16-2 と同様、Windows サービス + インストーラ）で R0 §4 を満たす。
    - **H7 UI とエンジンは localhost REST で話す**（banto-hub と同じ。新しい IPC は作らない）。
    - **H8 サービス運用ではデスクトップの自動ログインを loopback grant に置き換える**（banto-hub の
      試運転 grant と同じ型）。
    - **H9 データの置き場所は `%APPDATA%` から ProgramData へ移す**。既存データを移行するか捨ててよいかは
      **未決**（サービス化の手順に入る前にオーナーが決める）。
    - **H10 サービスは Hub の API キーを machine スコープの DPAPI でファイルに保存する**。
    - **H11 更新は「インストーラがサービスを停止 → 置換 → 開始」**（banto-hub の手順を流用）。
    - 手順: A1（プロファイルロックの共有 crate。デスクトップと `banto-serve` の両方）→ A2（トレイ）→
      B1（headless host + KeyStore）→ B2（シェル決定）→ B3（サービス + インストーラ + 実機 24h/72h）。
    - **実装状況**: A1（2026-10-10、#565）・**A2（2026-10-11）実装済み**。B1 以降は未。
      A2 で、ChronoGazer の**ウィンドウの × はアプリを終了せずに隠す**（収集・Hub 購読・LAN サーバーは
      動き続ける）ようになり、トレイ（左クリック・「開く」で窓を戻す、右クリックで「収集の状態・開く・終了」）に
      常駐する。収集が止まるのは、トレイの「終了」（確認の後、既存の終了処理 = LAN サーバー → 収集の停止と
      最終 flush・`collection_stopped` → Hub 購読 → DB を通す）と、OS のシャットダウン・ログオフ
      （同じ終了処理を通す）だけ。
      **オーナー決定（2026-10-12）**:
      1. **トレイの左クリックの挙動を両アプリで揃える**: Windows の慣習どおり**左クリック = メインの窓を戻す
         （前面化）、右クリック = メニュー**。banto-hub（これまで左クリックでメニュー）を ChronoGazer に
         合わせた。メニューの中身（ホストの種類で変わる）は変えない。左クリックの扱い
         （`show_menu_on_left_click(false)` と左ボタンを離したときの判定）は `banto-desktop-tray` の
         `restore_window_on_left_click` に寄せた。banto-hub のメインの窓は `tauri.conf.json` で定義済みで、
         どのホスト（デスクトップ／サービス接続／fallback）でも存在し、× でも破棄されず隠れるだけなので、
         戻す窓が無い状態は無い（万一無ければ何もせず、メニューは右クリックで出る）。
      2. **banto-hub にも `QuitGate` を入れる**: 「アプリを終了」の確認中・終了処理中の 2 回目は無視する
         （確認ダイアログを 2 つ出さない）。banto-hub の確認文は収集中かどうかで変わるので、ChronoGazer と同じく
         「収集していない」文言で同意を取った後に収集が始まっていたら、黙って終了せず「収集中」の文言で
         もう一度確かめる。確認文と終了処理（`RunningHub::shutdown`）の中身は変えていない。
      3. **OS のサインアウト・シャットダウンで banto-hub のデスクトップシェルが `RunningHub::shutdown` を
         通らない件は別 issue（tyaro/banto-industrial#586）**。この PR では直さない（ChronoGazer は
         OS の終了でも同じ終了処理を通す）。
         → **#586 で直した（2026-10-12、実装済み。Windows 実機での OS のサインアウト・シャットダウンは
         未確認）**。受入条件はオーナーのレビュー由来: (1) 後始末の「1 回」は完了待ちまで含める
         （共有 crate の `ExitCleanupOnce`。トレイの終了と `RunEvent::Exit` で共有し、後から来た方は先行の
         完了を待つ）、(2) 予算はロックの取得・進行中の後始末の待ちも含めて上限を持つ（トレイ 15 秒・
         OS の終了 5 秒、予算切れは正常終了と別の文言でログ。OS の経路ではダイアログを出さない）、
         (3) 終了の開始後はホスト切替・「再試行」で新しい Hub を起動・適用しない。切替が止めている・
         起動している最中の Hub も同じ予算で待つ（Hub の置き場所 `HubSlot` が進行中の数と閉じた印を持つ）。
         #587 のレビューで、**ホスト切替そのもの**も spawn の前に登録して終了が待つようにした: 最初の SCM
         操作の前に終了が閉じた切替は SCM を呼ばずに終わり、操作を始めた切替は打ち切らずに完走させる
         （途中で止めると、サービスもアプリも止まった状態に落ちうるため）。Service→Desktop の途中の終了では、
         サービスは止まりアプリも終わる（終了はサービスを開始し直さない）。
         1 回に限るのは終了の後始末だけで、切替の停止・起動は何度でもできる、(4) サービス接続中・fallback の
         終了は SCM を呼ばない。閉じた後は新しい controller が作られないので、#585 の残件「切替で作られた
         新しい controller に終了の印が引き継がれない」も起こらなくなった。詳細は
         [banto-hub-t16-design.md](banto-hub-t16-design.md) §4.1 の #586 追記。
         ChronoGazer の `exit_cleanup_once`（`tokio::sync::OnceCell`）を共有 crate の `ExitCleanupOnce` に
         寄せるかは未（#392 の残件）。
      4. **ログオン時の自動起動は A の範囲外のまま**（B で扱う）。

      実装の細部（オーナー決定ではない。banto-hub の既存の型に合わせた）:
      - 窓の前面化・× で隠す・初回の通知・終了の確認ダイアログは、banto-hub と共有の crate
        `crates/banto-desktop-tray` に寄せた（banto-hub 側も同じ crate を使うように置き換えた。挙動は不変）。
        トレイのメニュー・状態の文言・終了処理の中身はアプリごとに持つ。
      - 2 つ目の起動は `tauri-plugin-single-instance` が既存の窓を前に出して終わる（DB にも A1 の排他にも
        触れない）。A1 の排他は、別のログオンセッションや `banto-serve` との衝突を受け止める二段目として残る。
      - 「終了」は収集中（開始中を含む）なら「収集を停止してから終了」の文言で確認する。確認中に収集が
        始まったら、黙って止めずにもう一度確かめる。確認中・終了処理中の 2 回目の「終了」は無視し、終了処理は
        1 回に限る。終了処理が失敗（最終 flush の失敗・5 秒の予算切れ）したら、その旨をダイアログで出してから
        終了する（プロセスは残さない。banto-hub の「アプリを終了」も停止の結果にかかわらず終了する）。
      - **トレイ常駐でも、ログオン時の自動起動は無い**（A の範囲外。プロセスを起動しない限り収集しないのは
        従来どおり）。UI からの独立（R0 §4 の「UI と独立」）は B で満たす。
      - **終了の確定と収集の開始の直列化**（#392 A2 レビュー、2026-10-12。両アプリ共通）: 「収集していない」
        文言で同意した後、答えのコールバックが状態を読んでから終了処理へ進むまでの間に、別のタスクが収集を
        開始できると、新しく始まった収集を再確認なしで止めて終了してしまう（`QuitGate` の Mutex は確認の段階
        しか守らない）。そこで**収集の開始を受け付ける側**に「終了の受付」の印を持たせた:
        banto-hub は `CollectionController`（印は `state` ロックの中。`Starting` を書く区間で印を見る）、
        ChronoGazer は `CollectorService`（印と「受け付け済みでまだ処理し終えていない開始の数」を 1 本の
        ロックで持ち、開始はキューへ投入する前に印を見る。数はコマンドに持たせた札で、状態を公開した後に
        減る）。トレイの答えは `QuitGate::answer` が `QuitAdmission::begin_quit_confirm`（印を立てて、同じ
        ロックの中で忙しいかを読む）を 1 回だけ呼び、確かめ直す（`AskAgain`）ときは印を下ろす。キャンセルでは
        立てず、終了処理に入ったら下ろさない。印が立っている間の開始は**明示的に断る**（banto-hub: REST の
        `start`・`start-all-simulation`・走行中の `mode` 切り替えは 409、MCP はツールエラー。ChronoGazer:
        REST・Tauri の `start`/`restart` と自動開始は「終了処理中のため、収集を開始できません」のエラー）。
        停止は断らない。終了処理（banto-hub の `RunningHub::shutdown`、ChronoGazer の `shutdown_app_state`・
        `banto-serve` の終了）も先頭で印を立てる（Windows サービスの停止・OS のシャットダウンの経路でも、
        終了処理の途中で開始されない）。
        **再レビューでの追加（2026-10-12）**:
        - **印を 2 種類にした**（P2-2）: トレイの確認のための**一時的な印**（`begin_quit_confirm` で立て、
          `AskAgain` の `cancel_quit_confirm` で下ろす）と、終了処理の**取り消せない印**（`begin_shutdown`。
          誰も下ろさない）。OS のシャットダウンの経路は `QuitGate` を `Quitting` にしないので、トレイの答えが
          後始末の開始より遅れて届きうる。`cancel_quit_confirm` は一時的な印だけを下ろすので、遅れた
          `AskAgain` が終了処理の印を解除して開始を通すことは無い。開始はどちらかが立っていれば断る
          （banto-hub も同じ構造にした。banto-hub のデスクトップの OS 終了経路は #586 で接続した。
          `RunningHub::shutdown` が立てる印は `AskAgain` で下ろせない）。
        - **ChronoGazer の受付から投入までの隙間**（P2-1）: 札を取る受付とキューへの投入は別の区間なので、
          その間に終了処理が印を立てて終了の `stop` を先に完了させると、元の開始が後から投入されて最後の
          停止の後に収集が立ち上がりうる。ライフサイクルタスクが `Start`/`Restart` を処理する**直前に**
          取り消せない印を確かめて断るようにした。終了処理は印を立ててから `stop` を投入し、タスクは
          キューを順に処理するので、**終了の `stop` が終端の操作になる**（前に並んだ開始はそれが止め、後ろに
          並んだ開始は断られる）。終了側が未投入の札が 0 になるまで待つ形は、投入の待ち（5 秒）が終了の
          予算（5 秒）と競合し、待ち合わせの順序を新たに決める必要が出るため採らなかった（タスク側で断れば
          誰も待たない）。一時的な印はタスク側では見ない（それより前に受け付けた開始は「忙しい」として
          数えられ、トレイが確かめ直すので実行してよい）。
  - **順序**: #383 段階3 を先に進め、#392 の A1 は並行。B1 は P3 の後。
  - 要件側は [recorder-requirements.md](recorder-requirements.md) §3.1・§4 に反映済み。

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
