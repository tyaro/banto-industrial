# docs 地図（まずここを読む）

banto-industrial のドキュメント全体の入口。「どの文書が何の**正**か」「今どこまで進んでいるか」を
1 画面で引くための地図。詳細は各文書へ辿る。

状態: **地図として現行**。索引に徹し、実装状況・設計判断の本体は各文書側で管理する。
最終更新: 2026-09-18（#383 段階2a: chronogazer にタグレジストリ CRUD と
設定画面を追加、r1-plan.md の実施状況表記をアーカイブから運用中へ訂正。
以下は経緯。#383 段階1 の実機確認と、そこで見つかった 2 件の修正。以下は経緯。2026-09-17: #332 Hub 自動接続・#383 段階1（ChronoGazer が選んだ Hub タグを購読して値を受ける）・relay-wright 凍結を反映。以下は経緯。2026-09-16: T19（UX-30〜48）・T20（文字列/構造体/レシピ/ビット .0〜.F）・T21（構成補助 MCP 管理面）完了、MCP 31 ツール実機検証、外部 DB 連携 S0〜S2b・S4・S5 完了、S3/S6/S7 残（MCP は 37 ツールに。追加分はローカル PostgreSQL で検証）に加え、v0.2.0-alpha.8: 書き込み受付の既定を「可」に変更し、再起動・収集操作での自動無効化を撤回（#340）、v0.2.0-alpha.9: 試運転中は構成 CRUD を収集中でも即時・無停止反映（#341）、v0.2.0-alpha.10: computed タグの catalog 公開・外部読み取り出力のシミュレーションゲート撤廃（#335）、v0.2.0-alpha.11: シミュレーションデバイスへの外部書き込みをシミュレータへ反映（#363）、v0.2.0-alpha.12: T15-3 テスト出力（test_output）機構を撤去（#362）、v0.2.0-alpha.13: PLC 到達不能中の plc_reconnected / plc_disconnected フラップを修正（#344）、
v0.2.0-alpha.14: banto-hub の設定画面をカテゴリ別ルートへ分割（#359 banto-hub 分）を反映、
chronogazer の設定画面をカテゴリ別ルートへ分割（#359 chronogazer 分）を反映、
relay-wright の設定画面をカテゴリ別ルートへ分割（#359 relay-wright 分、issue #359 は3アプリ分完了）を反映、
v0.2.0-alpha.15: 演算タグの式チェック API（`POST /api/tags/expression/check`）・
エラー位置のインライン表示・ライブプレビューを追加（#342 段階A。MCP `check_expression`
ツールを追加し 38 ツールに）を反映、
v0.2.0-alpha.16: Drawer/Modal が未保存中は Esc・オーバーレイクリックで閉じないよう修正し、
接続 Drawer・収集グループ Drawer に無かった未保存破棄確認を追加（誤爆クローズ防止）を反映、
v0.2.0-alpha.17: タグの編集・連続登録を非モーダルの右ペインへ移動（#375）を反映、
v0.2.0-alpha.18: 演算タグの式欄に「一覧から挿入」を追加し、新規作成フォームも
非モーダルの右ペインへ移動（#342 段階C）を反映、
v0.2.0-alpha.19: 演算タグの式欄にセグメント補完を追加（#342 段階B。組み込み関数表を
`GET /api/tags/expression/functions` で配る。**これで #342 は A/B/C の3段階すべて完了**）、
v0.2.0-alpha.20: 狭幅（≤900px）でタグ登録・タグモニタの左ツリーをオフキャンバスへ退避
（#378。400px でグリッドが操作できなかった制約を解消。wire 変更なし）を反映）。
最終検証日(コード照合): 2026-09-16

> この地図は索引に徹する。実装状況・設計判断の本体は各文書側にあり、状態の**正**は
> 常にリンク先の `状態:` 行と各表とする（CLAUDE.md H8 の状態欄同期規約）。

## 現状ひとめ（2026-09-18）

- **構成**: Rust workspace + SvelteKit/Tauri。アプリは **banto-hub**（タグサーバー）/ **chronogazer**
  （記録計）/ **relay-wright**。上流 `banto` は git tag / `@banto/*` を消費（Rust クレート・npm
  `@banto/*` とも現行 **v1.6.0** で揃っている。`Cargo.toml`/`package.json` を正とする）。
  Rust と npm は別マニフェストで独立に追従できるが、**上げるときは揃えて上げる運用**とする
  （2026-09-01、Issue #220 — npm 側だけ v1.2.0 に取り残されていたのを是正した教訓）。
- **I 系（基盤 I0〜I6）**: 実装済み（I6 = banto-broker として抽出済み）。
- **W 系（relay-wright）**: W5 まで実装済みだが **2026-09-17 に凍結**（構想の練り直し、オーナー決定）。
  残っていた W5 実機検証も、#332 の Hub 自動接続配線も止める（plan.md §4b）。
- **T 系（banto-hub、T0〜T21）**: T0〜T18-6 に加え、**T19（UX-30〜48 の UI/UX 群、S1〜S5）・T20（文字列
  read/write・構造体タグ登録＋オフセットコピー・レシピ一括書き込み・ワードデバイスのビット .0〜.F）・
  T21（構成補助 MCP＝管理面ツール）まで完了**（2026-09-06）。**残るは T18-5c/d（Windows 実機往復・
  狭幅/倍率・72h soak = オーナー同席の実機検証）と、実機・需要待ちの #210/#211/#123/#201 のみ**。
  T18-5a は「全タグのクライアント保持（上限 10,000 タグ）」を正式仕様化（windowed 化はバックログ降格）。
- **MCP（機械/AI 向け外部 IF）**: `POST /mcp`（API キー認証）で **38 ツール** — データ面（値の
  read/write・レシピ・状態参照）＋管理面（接続/グループ/タグ CRUD・式チェック・設定
  gRPC/MQTT/retention・収集/write 制御・API キー発行/失効・lock_down・Sink グループ管理）。実機
  R08ENCPU で 31 ツール検証済み（外部 DB 連携で追加した 6 ツール・#342 の `check_expression` は
  ローカル/ローカル PostgreSQL で検証）（2026-09-06、実バグ0）。IF 詳細は
  [banto-hub-mcp-reference.md](banto-hub-mcp-reference.md)（#342 の `check_expression`
  ツール追加は本 PR の時点で同文書へ未反映 - 別途反映が要る）。
- **Hub 自動接続（#332、2026-09-17）**: Banto アプリが試運転中の Hub に対して自分用の `read` キーを
  自己発行し OS キーリングへ保存する共有 crate `crates/banto-hub-bootstrap` を追加し、chronogazer に
  設定カテゴリ「Hub 接続」を新設（chronogazer はこれまで Hub に接続していなかったため新規実装）。
  **banto-hub 側は変更ゼロ**。`admin`/`write:` はコードでホワイトリスト拒否、失効は自分が発行した
  id だけ。**relay-wright への配線は保留**（relay-wright は 2026-09-17 のオーナー決定で凍結、
  plan.md §4b）。選んだタグの購読は #383 段階1 で実装済み。詳細は
  [banto-hub-client-bootstrap.md](banto-hub-client-bootstrap.md)。
- **ChronoGazer の3ドライバ構成（#383、2026-09-17 オーナー決定）— 段階1 完了、段階2・3 は未着手**:
  ChronoGazer は単体で動く記録計として **SLMP / Modbus TCP / banto-hub 経由**の3ドライバを持つ、
  という方針を決めた。banto-hub 側も接続ドライバが増えていく想定で前2者は機能が被るが、現場 PC
  1 台での成立を優先して重複を許容する（plan.md §4、tag-server-design.md §7）。**段階1（Hub 経由
  ドライバ＝選んだタグを購読して値を受け、購読状態を設定画面に出す）は実装済み** — 世代の同一性は
  「接続先 + タグ集合」で `status()` では張り直さず、未解決タグは残りだけで購読して一覧に出し、
  購読の失敗は接続の 6 状態を汚さない（banto-hub-client-bootstrap.md §10）。**段階2（SLMP /
  Modbus TCP 直結）と段階3（3ドライバの合流・トレンド表示・保存）は未着手**。
  **段階2a（= r1-plan.md R1-B、レジストリ CRUD）は実装済み（2026-09-18）**:
  PLC接続/収集グループ/タグの3エンティティ CRUD を REST + Tauri 両経路・
  `/tags` 設定画面で公開（editor 以上、監査記録）。実際の SLMP/Modbus TCP
  直結収集（Collector のライフサイクル・現在値・イベント。r1-plan.md R1-C）は
  **まだ含まない** - 収集エンジンは当面 Tauri プロセス内で動かす方針
  （オーナー決定、recorder-requirements.md §4 追補）。
  **relay-wright は凍結**（構想の練り直し、plan.md §4b）。**段階1 は実機（R08ENCPU + 本物の banto-hub）で確認済み**
  （2026-09-17、banto-hub-client-bootstrap.md §11）。確認の過程で「タグが 1 つ消えると購読が二度と戻らない」など
  **モックでは出なかった欠陥 2 件**を発見し修正した（#388）。
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
- **v0.2.0-alpha.15（2026-09-15、#342 段階A）**: 演算タグの式入力 UX 改善の第1段。
  `POST /api/tags/expression/check`（editor 以上、常に 200 で `ok`/`resultType`/`refs`/
  `preview`/`error` を返す）と同ロジックを共有する MCP `check_expression` ツールを追加し、
  保存前に式を検証・試算できるようにした。フロント（`(app)/tags/+page.svelte`）は式欄
  （`#tag-expression`）に 300ms debounce のライブチェックを配線し、`pos`（バイト = 文字
  オフセット）でエラー位置に下線を出し、結果型・参照タグ一覧・試算値をプレビュー表示する。
  issue #342 原文の「キャレット自動移動」は打鍵の邪魔になるため見送り、エラーメッセージを
  クリックしたときだけ移動する挙動に変更した。段階 B（セグメント補完）・C（ツリーからの
  挿入）は別 PR。**wire 追加のみ**（新エンドポイント・新 MCP ツール、既存 API は無変更）。
  詳細は [tag-server-design.md](tag-server-design.md) §4.2、
  [banto-hub-operations.md](banto-hub-operations.md)。
- **v0.2.0-alpha.16（2026-09-15）**: オーナー報告「設定中に操作ミスで閉じてしまい最初から
  やり直しになる」の修正。`Drawer.svelte`/`Modal.svelte` に `dirty`/`onBlockedClose` を追加し、
  未保存の変更がある間は Esc・オーバーレイクリックで閉じないようにした（`×` 経由の確認は
  塞がない）。あわせて調査で判明した「接続 Drawer・収集グループ Drawer には未保存確認自体が
  無かった」不具合も修正し、タグ編集 Drawer と同じ `isFormDirty` ベースの破棄確認を追加した。
  フロントのみ、**wire 変更なし**。詳細は
  [banto-hub-desktop-plan.md](banto-hub-desktop-plan.md) §9.4 TAG-UX-C。
- **v0.2.0-alpha.17（2026-09-15、#375）**: オーナー報告「編集中に左ツリーと中央グリッドを
  見られない」の解消。タグ画面の「編集」「連続登録」をモーダルの Drawer から**非モーダルの
  右ペイン**へ移した（オーバーレイを持たないので編集中も左ツリーとグリッドを操作できる。
  Esc では閉じず、明示的な「閉じる」ボタンと登録成功後だけ閉じる）。狭幅（≤900px、サイドバーの
  オフキャンバスと同じブレークポイント）は従来どおりオーバーレイの Drawer、構造体展開と CSV
  取り込みは一過性のウィザードなのでモーダルのまま。フロントのみ、**wire 変更なし**。詳細は
  [banto-hub-desktop-plan.md](banto-hub-desktop-plan.md) §9.4 TAG-UX-C 2026-09-15 追補その2。
- **v0.2.0-alpha.18（2026-09-15、#342 段階C）**: 演算タグの式欄に**「一覧から挿入」**を
  追加した。トグルが ON の間だけタグ一覧の行クリックが「完全名を式欄のキャレット位置へ挿入」
  になり、編集対象は切り替わらない（自タグ・文字列型・循環になる参照は挿入せず理由をトースト
  で出す。**正は段階Aのサーバチェック**）。issue 原案のツリー3階層化は採らず、#375 で非
  モーダルになったグリッドをそのまま使う。あわせて**新規作成フォームも右ペインへ**移した
  （狭幅は従来どおり中央モーダル）。段階B（セグメント補完）は未着手。フロントのみ、
  **wire 変更なし**。詳細は [tag-server-design.md](tag-server-design.md) §4.2 と
  [banto-hub-desktop-plan.md](banto-hub-desktop-plan.md) §9.4 TAG-UX-C 2026-09-15 追補その3。
- **v0.2.0-alpha.19（2026-09-16、#342 段階B）**: 演算タグの式欄に**セグメント補完**を
  追加した。キャレット直前の字面から接続名 → 収集グループ名 → タグ名の候補を出し、
  接続・グループを確定すると名前 + `.` が入ってそのまま次の階層が開く。組み込み関数も
  第1セグメントの候補に混ざる（確定すると `name(`）。開くのはドット入力・2文字以上の入力・
  `Ctrl+Space`・`Ctrl+.`（IME 変換中は開かない）。**`Esc` はポップアップが開いている間は
  ポップアップだけを閉じ、「一覧から挿入」トグルは ON のまま**。候補の除外は段階Cの純関数を
  再利用し、補完では淡色ではなく非表示にする。**wire 追加**: `GET /api/tags/expression/functions`
  （組み込み関数表を `banto-expr` 単一ソースから配る。MCP には追加しない）。
  **これで issue #342 は段階 A（式チェック API）・B（セグメント補完）・C（一覧から挿入）の
  3段階すべてが完了した**。詳細は [tag-server-design.md](tag-server-design.md) §4.2。
- **v0.2.0-alpha.20（2026-09-16、#378）**: 狭幅（≤900px、サイドバーのオフキャンバスと同じ
  ブレークポイント）で**タグ登録・タグモニタの左ツリーをオフキャンバスへ退避**するように
  した。400px 幅では固定 280px のツリーに押されてグリッドに約 120px しか残らず行がクリック
  できなかった（#375 の E2E で実測した既存の制約）。ツールバーの「📁 ツリー」ボタンで開き、
  ノードを選ぶと自動で閉じる（`Esc`・バックドロップでも閉じ、フォーカスはボタンへ戻る）。
  退避は汎用部品 `SplitPane.svelte` に実装して2画面で共有し、狭幅判定は `narrow` prop で
  受ける（同部品のアプリ非依存規約は維持）。**広幅の DOM・CSS は不変**。フロントのみ、
  **wire 変更なし**。詳細は [banto-hub-desktop-plan.md](banto-hub-desktop-plan.md) §9.4
  TAG-UX-G 2026-09-16 追補。
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

| 文書                                                               | 何の正か / 役割                                                                                                                                                                                                                                                                                                                                                                                 |
| ------------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| [implementation-checklist.md](implementation-checklist.md)         | **実装・検証・PR のチェックリスト**。CLAUDE.md から `@` インポートされ毎セッション読み込まれる。実際に踏んだ事故だけを載せる表（検証順序・E2E の罠・docs 同期・設計の規律・マージ運用）。                                                                                                                                                                                                       |
| [plan.md](plan.md)                                                 | **全体計画の親**。I/R/W/T 系マイルストーンと依存の一覧。                                                                                                                                                                                                                                                                                                                                        |
| [tag-server-design.md](tag-server-design.md)                       | **banto-hub 設計の一次ソース**。タグ空間モデル・外部 IF・書き込み安全。実装状況は §9（T 系）表が正。                                                                                                                                                                                                                                                                                            |
| [banto-tagclient-design.md](banto-tagclient-design.md)             | **banto-tagclient の実装前設計**。読み取り専用SDKのREST/WS、binding、再接続、停止、テストゲートの正。                                                                                                                                                                                                                                                                                           |
| [banto-hub-client-bootstrap.md](banto-hub-client-bootstrap.md)     | **Banto クライアントの Hub 自動接続（#332）の正**。試運転中の `read` キー自己発行 → OS キーリング → `banto-tagclient` への供給。共有 crate `crates/banto-hub-bootstrap` の trait 境界・発行規則・6 状態・同一 PC 限定である理由。chronogazer 分のみ実装済み（relay-wright は凍結で保留、Hub 側は変更ゼロ）。§10 に #383 段階1 の購読（世代の同一性・未解決タグ・ポーリング口・起動時 resume）。 |
| [banto-rtsp-design.md](banto-rtsp-design.md)                       | RTSP 映像取り込み設計（Draft、Phase 1 実装済み・実機/配布確認待ち）。                                                                                                                                                                                                                                                                                                                           |
| [banto-hub-desktop-plan.md](banto-hub-desktop-plan.md)             | **banto-hub 運転計画（T14〜T18）・UI/UX 決定台帳**。§9.3〜9.5 が T18 タグ登録 UX の受け入れの正。                                                                                                                                                                                                                                                                                               |
| [banto-hub-operations.md](banto-hub-operations.md)                 | **banto-hub 運用ガイド**（起動・ポート・API/MQTT/gRPC・サービス化・soak 手順）。現状の運用を引く入口。                                                                                                                                                                                                                                                                                          |
| [banto-hub-t16-design.md](banto-hub-t16-design.md)                 | T16 詳細設計（デスクトップシェル・タスクトレイ）。                                                                                                                                                                                                                                                                                                                                              |
| [banto-hub-t17-design.md](banto-hub-t17-design.md)                 | T17 詳細設計（SCM 管理・profile・UAC・インストーラ）。                                                                                                                                                                                                                                                                                                                                          |
| [banto-hub-installer-design.md](banto-hub-installer-design.md)     | **一体インストーラ設計（Draft・オーナー決定待ち）**。シェル・Hub・elev・サイドカーを 1 本の NSIS で配置し、サービス登録と Operators 権限まで行う。方式は既存生成ツールの複数バイナリ化（案 A）を推奨。決定項目は §7。                                                                                                                                                                           |
| [banto-hub-t18-design.md](banto-hub-t18-design.md)                 | T18 詳細設計（タグ登録 UI/UX・性能検証）の実装分解索引（受け入れは desktop-plan §9.4 が正）。                                                                                                                                                                                                                                                                                                   |
| [banto-hub-t19-design.md](banto-hub-t19-design.md)                 | T19（UX-30〜48 の UI/UX 群）設計・決定台帳。S1〜S5 完了。                                                                                                                                                                                                                                                                                                                                       |
| [banto-hub-t20-design.md](banto-hub-t20-design.md)                 | **T20 計画**（文字列 read/write・構造体タグ登録・レシピ一括書き込み・ワードデバイスのビット .0〜.F）。                                                                                                                                                                                                                                                                                          |
| [banto-hub-mcp-reference.md](banto-hub-mcp-reference.md)           | **banto-hub MCP リファレンス**。データ面（読み書き・レシピ原子性）＋構成補助（管理面・T21）の全ツール I/F・スコープ・ロックダウン（MES/ゲートウェイ実装者向け）。                                                                                                                                                                                                                               |
| [banto-hub-t21-design.md](banto-hub-t21-design.md)                 | **T21（完了）** 構成補助 MCP（管理面）。接続/グループ/タグ・設定・収集/write 制御・API キー・lock_down を MCP から。安全境界の設計・決定台帳。                                                                                                                                                                                                                                                  |
| [banto-hub-external-db-design.md](banto-hub-external-db-design.md) | **外部 DB 連携（#228 Source / #229 Sink）の設計**。S0〜S2b・S4・S5 完了（Source エンジン・Sink の Hub 側とサイドカー）、S3（Source UI）/S6（Sink UI）/S7（実 DB 検証）残。DB 接続を Source/Sink で共有し、Source は既存の接続→グループ→タグ 3 階層を流用（案 A）して Hub 内で動き、Sink は設定と監視を Hub に置いた別プロセスのサイドカー `apps/banto-hub-sink`。                               |
| [external-db-test-2026-09.md](external-db-test-2026-09.md)         | 外部 DB 連携の検証手順（S3 の手動 smoke A-1〜A-14、Sink の smoke と S7 実 DB 検証 B-1〜B-13）。結果は §5 に記録。                                                                                                                                                                                                                                                                               |
| [banto-hub-remaining-plan.md](banto-hub-remaining-plan.md)         | banto-hub **残作業の優先順位・着手順の索引**（他文書を正とする）。銘柄横断で現状を掴む最短路。                                                                                                                                                                                                                                                                                                  |
| [improvement-plan.md](improvement-plan.md)                         | Hardening（H1〜H10）の設計・進捗ログ。                                                                                                                                                                                                                                                                                                                                                          |
| [recorder-requirements.md](recorder-requirements.md)               | 記録計（chronogazer）R0 要件定義。R1〜R4 スコープの正。                                                                                                                                                                                                                                                                                                                                         |

### トピック別の「正」（重複時はここを見る）

- **T18 タグ登録 UX の受け入れ**: [banto-hub-desktop-plan.md](banto-hub-desktop-plan.md) §9.3〜9.5。
  実装分解は [banto-hub-t18-design.md](banto-hub-t18-design.md)、運用手順は [banto-hub-operations.md](banto-hub-operations.md)。
- **pending queue（運転中編集のキュー化）**: [banto-hub-desktop-plan.md](banto-hub-desktop-plan.md) §9.3 TAG-P0-3。
- **SLMP 構造化エラー（H9）**: [h9-slmp-structured-error-spec.md](h9-slmp-structured-error-spec.md)。
- **タグ定義の single source of truth**: [tag-server-design.md](tag-server-design.md)。

### アーカイブ（役目終了・経緯として保存。現行仕様ではない）

各文書の冒頭に**アーカイブ・バナー**を付与済み。リンクは生きている（過去の rationale として参照可）。

| 文書                                                         | 状態                                                                                                               |
| ------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------ |
| [ux-plan.md](ux-plan.md)                                     | UX 改善計画（T9〜T13）。T13-2/3 は desktop-plan/T18 へ移管済み。T9〜T12 の rationale として参照のみ。              |
| [h10-3-read-scope-proposal.md](h10-3-read-scope-proposal.md) | H10③ read スコープの比較検討（案 B 採用、PR #75 で決着済み）。                                                     |
| [t5-handoff.md](t5-handoff.md)                               | T5 セッション引き継ぎメモ（内容は operations / desktop-plan に反映済み）。                                         |
| [banto-hub-t14-design.md](banto-hub-t14-design.md)           | T14（ランタイム状態管理・制御面分離）は実装完了。現行の設計判断は desktop-plan / tag-server-design へ吸収済み。    |
| [r1-plan.md](r1-plan.md)                                     | 記録計 R1 実施計画。**運用中**（R1-A 完了、R1-B は #383 段階2a で着手・レジストリ CRUD 実装済み、R1-C/D 未着手）。 |
| [real-machine-test-2026-09.md](real-machine-test-2026-09.md) | #130/#131/#123 の一回性実機検証記録。結果は tag-server-design.md §6.2 等に吸収済み。                               |
| [real-machine-mcp-2026-09.md](real-machine-mcp-2026-09.md)   | T19 S5 の MCP 実機検証（2026-09-04）の一回性記録。現行 IF・実機検証索引は banto-hub-mcp-reference.md。             |
| [r1a-readme-gaps.md](r1a-readme-gaps.md)                     | 上流 banto の README 手順の穴（外部フィードバック用チェックリスト。本リポの仕様ではない）。                        |

## 補足: なぜ状態ヘッダが厚くなるか

CLAUDE.md は「設計判断はオーナー決定として日付付きで docs に記録」「実装状況が変わる PR で `状態:` 行を更新」
（H8）を定めており、各文書の状態ヘッダに日付付きの決定・進捗が積層する。経緯は意図的に保存する方針のため、
現状だけを素早く引きたいときは本地図と各文書の**表**（§9 表など）を先に見るとよい。
