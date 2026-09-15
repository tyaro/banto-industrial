# docs 地図（まずここを読む）

banto-industrial のドキュメント全体の入口。「どの文書が何の**正**か」「今どこまで進んでいるか」を
1 画面で引くための地図。詳細は各文書へ辿る。

状態: **地図として現行**。索引に徹し、実装状況・設計判断の本体は各文書側で管理する。
最終更新: 2026-09-15（T19（UX-30〜48）・T20（文字列/構造体/レシピ/ビット .0〜.F）・T21（構成補助 MCP 管理面）完了、MCP 31 ツール実機検証、外部 DB 連携 S0〜S2b・S4・S5 完了、S3/S6/S7 残（MCP は 37 ツールに。追加分はローカル PostgreSQL で検証）に加え、v0.2.0-alpha.8: 書き込み受付の既定を「可」に変更し、再起動・収集操作での自動無効化を撤回（#340）、v0.2.0-alpha.9: 試運転中は構成 CRUD を収集中でも即時・無停止反映（#341）、v0.2.0-alpha.10: computed タグの catalog 公開・外部読み取り出力のシミュレーションゲート撤廃（#335）、v0.2.0-alpha.11: シミュレーションデバイスへの外部書き込みをシミュレータへ反映（#363）、v0.2.0-alpha.12: T15-3 テスト出力（test_output）機構を撤去（#362）、v0.2.0-alpha.13: PLC 到達不能中の plc_reconnected / plc_disconnected フラップを修正（#344）、
v0.2.0-alpha.14: banto-hub の設定画面をカテゴリ別ルートへ分割（#359 banto-hub 分）を反映、
chronogazer の設定画面をカテゴリ別ルートへ分割（#359 chronogazer 分）を反映、
relay-wright の設定画面をカテゴリ別ルートへ分割（#359 relay-wright 分、issue #359 は3アプリ分完了）を反映）。
最終検証日(コード照合): 2026-09-15

> この地図は索引に徹する。実装状況・設計判断の本体は各文書側にあり、状態の**正**は
> 常にリンク先の `状態:` 行と各表とする（CLAUDE.md H8 の状態欄同期規約）。

## 現状ひとめ（2026-09-15）

- **構成**: Rust workspace + SvelteKit/Tauri。アプリは **banto-hub**（タグサーバー）/ **chronogazer**
  （記録計）/ **relay-wright**。上流 `banto` は git tag / `@banto/*` を消費（Rust クレート・npm
  `@banto/*` とも現行 **v1.6.0** で揃っている。`Cargo.toml`/`package.json` を正とする）。
  Rust と npm は別マニフェストで独立に追従できるが、**上げるときは揃えて上げる運用**とする
  （2026-09-01、Issue #220 — npm 側だけ v1.2.0 に取り残されていたのを是正した教訓）。
- **I 系（基盤 I0〜I6）**: 実装済み（I6 = banto-broker として抽出済み）。
- **W 系（relay-wright）**: W5 まで実装済み（実機検証のみ残）。
- **T 系（banto-hub、T0〜T21）**: T0〜T18-6 に加え、**T19（UX-30〜48 の UI/UX 群、S1〜S5）・T20（文字列
  read/write・構造体タグ登録＋オフセットコピー・レシピ一括書き込み・ワードデバイスのビット .0〜.F）・
  T21（構成補助 MCP＝管理面ツール）まで完了**（2026-09-06）。**残るは T18-5c/d（Windows 実機往復・
  狭幅/倍率・72h soak = オーナー同席の実機検証）と、実機・需要待ちの #210/#211/#123/#201 のみ**。
  T18-5a は「全タグのクライアント保持（上限 10,000 タグ）」を正式仕様化（windowed 化はバックログ降格）。
- **MCP（機械/AI 向け外部 IF）**: `POST /mcp`（API キー認証）で **37 ツール** — データ面（値の
  read/write・レシピ・状態参照）＋管理面（接続/グループ/タグ CRUD・設定 gRPC/MQTT/retention・
  収集/write 制御・API キー発行/失効・lock_down・Sink グループ管理）。実機 R08ENCPU で 31 ツール検証済み（外部 DB 連携で追加した 6 ツールはローカル PostgreSQL で検証）
  （2026-09-06、実バグ0）。IF 詳細は [banto-hub-mcp-reference.md](banto-hub-mcp-reference.md)。
- **Hardening（H1〜H10）**: H1〜H6・H8・H10 完了。H9 は 2026-08-14 に完全完了。H5 は relay-wright の
  組み込みサーバーモード E2E を含め完了（2026-08-30、PR #193。Tauri 固有経路の E2E は WebDriver 課題と
  して別スコープに分離）。**残るは H7 の① 実機 soak のみ**（詳細は improvement-plan.md）。
- **v0.2.0-alpha.8（2026-09-14、#340）**: 書き込み受付（write_enabled）の既定を「可」に変更し、
  プロセス再起動・収集の開始/停止/モード変更での自動無効化を撤回（オーナー決定 2026-09-09）。
  トグルは運用者が手で止める非常停止スイッチに徹する。詳細は
  [tag-server-design.md](tag-server-design.md) §6-6。**実機確認済み（2026-09-14、R08ENCPU）**:
  起動直後 `writeEnabled: true`、収集開始後も変わらず enable 不要で書き込み → 読み戻し一致、
  disable → 再起動 → 無効のまま復元、enable → 再起動 → 有効で復元。
- **v0.2.0-alpha.9（2026-09-14、#341）**: 試運転中（未ロックダウン）は接続・グループ・タグの
  CRUD を収集中でも即時・無停止で反映し、ロックダウン後のみ pending queue + 明示適用にする
  （オーナー決定 2026-09-09 / 2026-09-14）。明示適用も収集を止めない。あわせて
  `banto_collect::Collector::apply_config` の writer 配布不具合（唯一の接続が replaced に
  なると新 writer が届かず履歴書き込みが列数不一致で全滅）を修正。詳細は
  [tag-server-design.md](tag-server-design.md) §4.3・
  [banto-hub-operations.md](banto-hub-operations.md) §19。**実機確認済み（2026-09-14、R08ENCPU、
  試運転モード）**: 収集中のタグ削除・作成・作成（writable）がいずれも 200 で即時反映され、
  `runId` 不変・既存タグは good のまま・pending 0 件・新タグは数秒で good/real。
  `POST /api/collection/reapply` も 200・`runId` 不変。ロックダウン後の queue + 無停止適用は
  admin ログインが要るためローカルでは未実施（CI の `chromium-locked-down` E2E と統合テストで担保）。
- **v0.2.0-alpha.10（2026-09-14/15、#335）**: API キー経由の catalog（`GET /api/v1/tags`）から
  computed タグと接続単位のシミュレーション設定済み PLC タグが常時隠れていた不具合を修正
  （`value_source` に新ラベル `computed` を追加し、`derived_simulation` は真にシミュレーション
  中のときだけの情報ラベルに変更）。さらに 2026-09-15 オーナー決定「外部出力を PLC への出力と
  勘違いしていた」で、外部への**読み取り**出力（REST/WS/gRPC/MQTT）はシミュレーションで一切
  ゲートしない契約へ改定（全 PLC シミュレーション中も catalog・値読み取り・購読・publish を
  常に配信）。T15-3 の `test_output` opt-in は効果を失い deprecated となったが、
  **撤去済み（2026-09-15、#362。v0.2.0-alpha.12）**: `TestOutputControl` の制御
  プレーンごと撤去した。詳細は [tag-server-design.md](tag-server-design.md) §4.2、
  [banto-hub-desktop-plan.md](banto-hub-desktop-plan.md) §6.3。
- **v0.2.0-alpha.11（2026-09-15、#363）**: シミュレーションデバイス（接続単位
  `simulation: true`・全シミュレーション運転中）のタグへの外部書き込みを拒否せず、PC 上の
  in-process シミュレータへ反映する契約へ改定（オーナー決定 2026-09-15）。`banto-plc` の
  シミュレータに Modbus FC5/6/15/16 と SLMP `0x1401` を実装し、ワイヤ経由で書いた番地は
  held としてランプ波の更新対象から外れるため読み戻せる。実機向けの護り（writable /
  write スコープ / レート制限 / 値検査 / log-before-write / write_enabled）はすべて据え置きで、
  撤去したのは「シミュレーション中は拒否」の 1 段だけ。**wire 変更**: エラーコード
  `simulation_write_rejected` は返らなくなった。監査 `detail` に `target: simulator|plc` を記録。
  詳細は [tag-server-design.md](tag-server-design.md) §6.5、
  [banto-hub-operations.md](banto-hub-operations.md) §4。 **ローカル確認済み（2026-09-15、alpha.11 ビルド）**: 実機 R08ENCPU への書き込みは
  監査 `target: plc`、収集中に live 追加した SLMP シミュレーション接続への D0 書き込みは 200 で
  1.5 秒後も保持（隣の D1 はランプ継続）・監査 `target: simulator`。
- **v0.2.0-alpha.12（2026-09-15、#362）**: alpha.10（#335）で外部への読み取り出力の
  シミュレーションゲートを撤廃して以降どの経路からも参照されなくなっていた T15-3
  「テスト出力」（`TestOutputControl`）機構を撤去した。`apps/banto-hub/core/src/test_output.rs`
  削除、REST `POST /api/test-output/{enable,disable}` 削除、`GET /api/v1/status`・
  `GET /api/status` の `test_output`/`testOutput` 削除、gRPC proto の
  `StreamValuesRequest.test_output`・`ValueBatch.simulation`/`run_id` を `reserved` 化。
  **wire 変更（破壊的）**。MQTT/WS は #364 までに完了済みで追加変更なし。詳細は
  [tag-server-design.md](tag-server-design.md) §4.2、
  [banto-hub-desktop-plan.md](banto-hub-desktop-plan.md) §6.3。
- **v0.2.0-alpha.13（2026-09-15、#344）**: PLC 到達不能の間、`plc_reconnected` と
  `plc_disconnected` が収集周期ごとに 1 組ずつフラップし続ける不具合を修正した（実測で
  1000ms 周期・2 接続で約 13,000 件/時）。hub の `BrokerReadClient::connect()` が broker
  セッションの実状態を見ずに常に即 `Ok` を返していたのが原因。以後 `connect()` は broker の
  `status_watch` を見て `Reconnecting` なら 3 秒待って復帰しなければ失敗を返し、
  `plc_reconnected` は「切断後、最初の読み取りが成功した時点」で 1 回だけ記録する。
  **wire 変更なし**（記録のタイミングと件数のみ変更）。詳細は
  [tag-server-design.md](tag-server-design.md) §6 項目5、
  [banto-hub-operations.md](banto-hub-operations.md) §10。 **ローカル確認済み（2026-09-15、alpha.13 ビルド）**: 到達不能 IP への SLMP 接続を
  収集中に追加し 30 秒観測 → 接続イベント 0 件（修正前は `plc_disconnected` 30 + `plc_reconnected` 30）、
  status は `reconnecting`、同居する実機接続の収集は影響なし。
- **v0.2.0-alpha.14（2026-09-15、#359 banto-hub 分）**: 上流テンプレート v1.6.0（#358 で追従）の
  設定画面カテゴリ別ルート分割を banto-hub に移植した。管理 UI の URL 構造が変わり、
  `/settings` は `/settings/{appearance,account,connectivity,data,security}` の実ルートに分かれる
  （旧 `/settings` は先頭の可視カテゴリへ 307 redirect するのでブックマークは引き続き有効）。
  コマンドパレット `config.import` の誘導先も `/settings#config-package` から
  `/settings/data#config-package` に変わった。挙動・API・DB は無変更。chronogazer・relay-wright への
  展開は別 PR（issue #359 は3アプリ分の作業）。
- **chronogazer の設定画面カテゴリ別ルート分割（2026-09-15、#359 chronogazer 分）**: banto-hub
  （#371、上記 v0.2.0-alpha.14）と同じ構成を chronogazer にも移植した。`/settings` は
  `/settings/{appearance,account,connectivity,data,security}` の実ルートに分かれ（旧 `/settings`
  は先頭の可視カテゴリへ 307 redirect）、`AuthSettings`（ログイン不要モード＋自動ログイン）を
  Account/Connectivity/Security の3カテゴリで共有する `authSettingsStore` を新設した。挙動・API・DB
  は無変更。relay-wright への展開は別 PR。
- **relay-wright の設定画面カテゴリ別ルート分割（2026-09-15、#359 relay-wright 分、issue #359 完了）**:
  banto-hub・chronogazer と同じ構成を relay-wright にも移植した。`/settings` は
  `/settings/{appearance,account,connectivity,data,security}` の実ルートに分かれ（旧 `/settings`
  は先頭の可視カテゴリへ 307 redirect）。relay-wright 固有のタグモニタ手動書き込み（H2）とアーム
  時限失効（H10）は、どちらも意図しない PLC 書き込みを防ぐ安全装置なので、6番目のカテゴリを増やさず
  認証と同じ `security` に同居させた。`AuthSettings` を Account/Connectivity/Security の3カテゴリで
  共有する `authSettingsStore` を新設した。挙動・API・DB は無変更。
- **出荷ゲート**: T5-5（実機での 72h soak 実行 + 実機最終サインオフ）のみ残（実機必須）。
- **banto-tagclient**: **S4a完了（2026-09-01）**。読み取り専用DTO、Endpoint/Secret境界、
  stable ID resolver、REST catalog/values transport、WS wire純粋解析、bounded publish gate、認証付き
  WebSocket handshake、on_change subscribe、単一世代workerとwatchによるatomic latest snapshot配信、
  公開Handle、worker所有権、明示shutdown、catalog起点の再接続・backoff・停止割り込み、rebinding、config_changed再解決、消費型restartによるcredential/endpoint置換を実装済み。S4b-1互換候補ではorigin/main 509bf0e（Banto v1.4.0）とのローカル統合でtokio-tungstenite 0.29系一本化を確認したが、未push・未mergeである。S4互換tag固定、実Hub/LAN統合検証、配布サイズ確認は残課題である。
  Hubのrelease tagは未確定。

## まず読む順

1. **CLAUDE.md**（ルート）— AI 作業規約・役割分担・開発規約。
2. **この地図**（docs/README.md）— 全体像と各文書の役割。
3. 目的別に下表の該当文書へ。banto-hub の現状を最短で掴むなら
   [banto-hub-remaining-plan.md](banto-hub-remaining-plan.md)（残作業の索引・最新の全体像）。

## 文書地図

### 現行（現状の正・参照先）

| 文書                                                               | 何の正か / 役割                                                                                                                                                                                                                                                                                                                                                   |
| ------------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| [plan.md](plan.md)                                                 | **全体計画の親**。I/R/W/T 系マイルストーンと依存の一覧。                                                                                                                                                                                                                                                                                                          |
| [tag-server-design.md](tag-server-design.md)                       | **banto-hub 設計の一次ソース**。タグ空間モデル・外部 IF・書き込み安全。実装状況は §9（T 系）表が正。                                                                                                                                                                                                                                                              |
| [banto-tagclient-design.md](banto-tagclient-design.md)             | **banto-tagclient の実装前設計**。読み取り専用SDKのREST/WS、binding、再接続、停止、テストゲートの正。                                                                                                                                                                                                                                                             |
| [banto-rtsp-design.md](banto-rtsp-design.md)                       | RTSP 映像取り込み設計（Draft、Phase 1 実装済み・実機/配布確認待ち）。                                                                                                                                                                                                                                                                                             |
| [banto-hub-desktop-plan.md](banto-hub-desktop-plan.md)             | **banto-hub 運転計画（T14〜T18）・UI/UX 決定台帳**。§9.3〜9.5 が T18 タグ登録 UX の受け入れの正。                                                                                                                                                                                                                                                                 |
| [banto-hub-operations.md](banto-hub-operations.md)                 | **banto-hub 運用ガイド**（起動・ポート・API/MQTT/gRPC・サービス化・soak 手順）。現状の運用を引く入口。                                                                                                                                                                                                                                                            |
| [banto-hub-t16-design.md](banto-hub-t16-design.md)                 | T16 詳細設計（デスクトップシェル・タスクトレイ）。                                                                                                                                                                                                                                                                                                                |
| [banto-hub-t17-design.md](banto-hub-t17-design.md)                 | T17 詳細設計（SCM 管理・profile・UAC・インストーラ）。                                                                                                                                                                                                                                                                                                            |
| [banto-hub-installer-design.md](banto-hub-installer-design.md)     | **一体インストーラ設計（Draft・オーナー決定待ち）**。シェル・Hub・elev・サイドカーを 1 本の NSIS で配置し、サービス登録と Operators 権限まで行う。方式は既存生成ツールの複数バイナリ化（案 A）を推奨。決定項目は §7。                                                                                                                                             |
| [banto-hub-t18-design.md](banto-hub-t18-design.md)                 | T18 詳細設計（タグ登録 UI/UX・性能検証）の実装分解索引（受け入れは desktop-plan §9.4 が正）。                                                                                                                                                                                                                                                                     |
| [banto-hub-t19-design.md](banto-hub-t19-design.md)                 | T19（UX-30〜48 の UI/UX 群）設計・決定台帳。S1〜S5 完了。                                                                                                                                                                                                                                                                                                         |
| [banto-hub-t20-design.md](banto-hub-t20-design.md)                 | **T20 計画**（文字列 read/write・構造体タグ登録・レシピ一括書き込み・ワードデバイスのビット .0〜.F）。                                                                                                                                                                                                                                                            |
| [banto-hub-mcp-reference.md](banto-hub-mcp-reference.md)           | **banto-hub MCP リファレンス**。データ面（読み書き・レシピ原子性）＋構成補助（管理面・T21）の全ツール I/F・スコープ・ロックダウン（MES/ゲートウェイ実装者向け）。                                                                                                                                                                                                 |
| [banto-hub-t21-design.md](banto-hub-t21-design.md)                 | **T21（完了）** 構成補助 MCP（管理面）。接続/グループ/タグ・設定・収集/write 制御・API キー・lock_down を MCP から。安全境界の設計・決定台帳。                                                                                                                                                                                                                    |
| [banto-hub-external-db-design.md](banto-hub-external-db-design.md) | **外部 DB 連携（#228 Source / #229 Sink）の設計**。S0〜S2b・S4・S5 完了（Source エンジン・Sink の Hub 側とサイドカー）、S3（Source UI）/S6（Sink UI）/S7（実 DB 検証）残。DB 接続を Source/Sink で共有し、Source は既存の接続→グループ→タグ 3 階層を流用（案 A）して Hub 内で動き、Sink は設定と監視を Hub に置いた別プロセスのサイドカー `apps/banto-hub-sink`。 |
| [external-db-test-2026-09.md](external-db-test-2026-09.md)         | 外部 DB 連携の検証手順（S3 の手動 smoke A-1〜A-14、Sink の smoke と S7 実 DB 検証 B-1〜B-13）。結果は §5 に記録。                                                                                                                                                                                                                                                 |
| [banto-hub-remaining-plan.md](banto-hub-remaining-plan.md)         | banto-hub **残作業の優先順位・着手順の索引**（他文書を正とする）。銘柄横断で現状を掴む最短路。                                                                                                                                                                                                                                                                    |
| [improvement-plan.md](improvement-plan.md)                         | Hardening（H1〜H10）の設計・進捗ログ。                                                                                                                                                                                                                                                                                                                            |
| [recorder-requirements.md](recorder-requirements.md)               | 記録計（chronogazer）R0 要件定義。R1〜R4 スコープの正。                                                                                                                                                                                                                                                                                                           |

### トピック別の「正」（重複時はここを見る）

- **T18 タグ登録 UX の受け入れ**: [banto-hub-desktop-plan.md](banto-hub-desktop-plan.md) §9.3〜9.5。
  実装分解は [banto-hub-t18-design.md](banto-hub-t18-design.md)、運用手順は [banto-hub-operations.md](banto-hub-operations.md)。
- **pending queue（運転中編集のキュー化）**: [banto-hub-desktop-plan.md](banto-hub-desktop-plan.md) §9.3 TAG-P0-3。
- **SLMP 構造化エラー（H9）**: [h9-slmp-structured-error-spec.md](h9-slmp-structured-error-spec.md)。
- **タグ定義の single source of truth**: [tag-server-design.md](tag-server-design.md)。

### アーカイブ（役目終了・経緯として保存。現行仕様ではない）

各文書の冒頭に**アーカイブ・バナー**を付与済み。リンクは生きている（過去の rationale として参照可）。

| 文書                                                         | 状態                                                                                                            |
| ------------------------------------------------------------ | --------------------------------------------------------------------------------------------------------------- |
| [ux-plan.md](ux-plan.md)                                     | UX 改善計画（T9〜T13）。T13-2/3 は desktop-plan/T18 へ移管済み。T9〜T12 の rationale として参照のみ。           |
| [h10-3-read-scope-proposal.md](h10-3-read-scope-proposal.md) | H10③ read スコープの比較検討（案 B 採用、PR #75 で決着済み）。                                                  |
| [t5-handoff.md](t5-handoff.md)                               | T5 セッション引き継ぎメモ（内容は operations / desktop-plan に反映済み）。                                      |
| [banto-hub-t14-design.md](banto-hub-t14-design.md)           | T14（ランタイム状態管理・制御面分離）は実装完了。現行の設計判断は desktop-plan / tag-server-design へ吸収済み。 |
| [r1-plan.md](r1-plan.md)                                     | 記録計 R1 実施計画。実施完了、記録として保存。                                                                  |
| [real-machine-test-2026-09.md](real-machine-test-2026-09.md) | #130/#131/#123 の一回性実機検証記録。結果は tag-server-design.md §6.2 等に吸収済み。                            |
| [real-machine-mcp-2026-09.md](real-machine-mcp-2026-09.md)   | T19 S5 の MCP 実機検証（2026-09-04）の一回性記録。現行 IF・実機検証索引は banto-hub-mcp-reference.md。          |
| [r1a-readme-gaps.md](r1a-readme-gaps.md)                     | 上流 banto の README 手順の穴（外部フィードバック用チェックリスト。本リポの仕様ではない）。                     |

## 補足: なぜ状態ヘッダが厚くなるか

CLAUDE.md は「設計判断はオーナー決定として日付付きで docs に記録」「実装状況が変わる PR で `状態:` 行を更新」
（H8）を定めており、各文書の状態ヘッダに日付付きの決定・進捗が積層する。経緯は意図的に保存する方針のため、
現状だけを素早く引きたいときは本地図と各文書の**表**（§9 表など）を先に見るとよい。
