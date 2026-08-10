# 改善計画(H系): AI コードレビュー指摘の対応

作成日: 2026-08-08
状態: **進行中(Phase 1 = PR #58、Phase 2 = PR #59/#73、Phase 3 = H10 ①②③ を PR #74/#75 でマージ済み)**。
H1〜H4・H6・H8・H10 完了、H5 は vitest 導入まで完了(E2E 拡充は Phase 4)。
H7 は ⑤ フレーク安定化 4 件(A.1/A.3/A.4/A.5)+ ②③④ 堅牢性テスト(crash 再オープン・DST・
read-while-write)+ A.2 決定化 + ④ TsQuery ギャップ修正を 2026-08-09 に対応。残りは H7 の ① 実機 soak・
H9(SLMP 構造化エラー)・H5 の E2E 拡充(いずれも Phase 4 相当/環境依存)。
最終検証日(コード照合): 2026-08-09

## 0. 背景と位置づけ

2026-08-07〜08 に、リポジトリ全体の多角 AI コードレビューを 2 件実施した
(ChatGPT による評価と、Claude 上位モデルによる検証レビュー)。後者は前者の
指摘を実コードで file:line まで裏取りし、いくつかの訂正と新規発見を加えた。
本ドキュメントはその検証済み指摘を実行計画に落としたもので、オーナー指示
「改善計画をドキュメント化してから進める」(2026-08-08)に基づく。

採番は既存の I/R/W/T 系と衝突しない **H 系(Hardening)** とする。

### レビューで確認された主な強み(維持すべきもの)

- 読み書き分離・broker の単一セッション所有・write-capable handle の隔離と
  いった「危険な処理ほど構造で縛る」設計が実装・テストまで一貫している
- 書き込みプランナーのギャップ許容ゼロ、bit-in-word の
  read→modify→write→確認読み、矛盾ビット書き込みの双方拒否、ルールの
  保存時循環検出(DFS)など、現場事故を想定した防御が要所にある
- 監査は log-before-write かつ仮状態 `failed` 挿入(クラッシュ時に安全側で
  残る)、監査 DB 書き込み失敗時は物理書き込み自体を中止(fail-closed)
- 衛生値: 本番コード unsafe ゼロ、TODO/FIXME 実質ゼロ、テスト 1,000 本超
  (実ソケットのプロトコルシミュレータ+異常注入+決定論クロック)

## 1. 運用ルール

- CLAUDE.md の役割分担に従う: **実装 = sonnet サブエージェント、
  タスク分解・レビュー・コミット判断 = 上位モデル**
- 1 項目 = 1 コミット以上。各項目の「受け入れ条件」を満たしてからコミット
- 進捗は本ドキュメントの各項目「状態」欄で管理する:
  `未着手 / 実装中 / レビュー中 / 完了 / オーナー判断待ち`
- オーナー決定が必要な項目は、決定内容を**日付付きで本ドキュメントに追記**
  してから着手する

## 2. 改善項目一覧(優先順)

| ID  | 内容                                                     | 優先度 | 規模 | 状態                |
| --- | -------------------------------------------------------- | ------ | ---- | ------------------- |
| H1  | banto-expr の式長・ネスト深さ上限(DoS 根治)              | 最高   | 小   | 完了(#58/#59)       |
| H2  | 手動書き込み(タグモニタ)の安全意味論                     | 最高   | 中   | 完了(#58)           |
| H3  | banto-hub gRPC の bind 設定化(既定 127.0.0.1)            | 高     | 中   | 完了(#58)           |
| H4  | 収集タイムスタンプ逆行対策 + append 失敗の可視化         | 高     | 中   | 完了(#58)           |
| H5  | フロントテスト基盤(vitest)+ hub/relay-wright E2E         | 高     | 大   | vitest 完了・E2E 残 |
| H6  | サプライチェーン/再現性(deny・audit・toolchain 固定ほか) | 高     | 中   | 完了(#59)           |
| H7  | ソーク実行・障害系テスト(crash 再オープン・DST・並行)    | 中     | 大   | ⑤+②③④+A.2完了・残 ① |
| H8  | ドキュメント整合(状態ヘッダ・README・状態欄義務化)       | 中     | 小   | 完了(#58/#59)       |
| H9  | SLMP 構造化エラー(fork/upstream)+ transport 共通化       | 中     | 大   | 未着手              |
| H10 | 認可の細粒度化(キー有効期限・read スコープ・arm 失効)    | 中     | 中   | 完了(#74/#75)       |

## 3. 各項目の詳細

### H1: banto-expr の式長・ネスト深さ上限 — 状態: 完了(2026-08-08、本 PR。残件あり)

- **事実**: `crates/banto-expr/src/parser.rs` の再帰下降パーサに深さ制限が
  なく、式文字列の長さ上限も banto-tags 検証・hub(`computed.rs`)・
  banto-expr のどの層にも存在しない。演算タグを登録できる認証済み
  クライアント(Editor 権限)が深いネスト式(括弧・単項演算子連鎖・関数
  ネスト)を送ると、tokio ワーカースレッド(既定スタック 2MiB)で
  スタックオーバーフロー = hub プロセス全体の abort が起こり得る
- **方針**: 上限は消費者側でなく **banto-expr の `compile()` 自身**に置き、
  全消費者を無条件に保護する。`MAX_SOURCE_CHARS = 1024` /
  `MAX_NESTING_DEPTH = 64`(実用式は数十文字・数段であり十分に寛大)。
  超過は `CompileError` の専用 variant(登録時 400 として現れる)
- **受け入れ条件**: 深い括弧 / `-` 連鎖 / `!` 連鎖 / 関数ネストの各経路で
  クラッシュせず Err になるテスト、境界値テスト、既存テスト green、
  clippy -D warnings 通過
- **実施記録(2026-08-08)**: `MAX_SOURCE_CHARS = 1024` /
  `MAX_NESTING_DEPTH = 64` を banto-expr に実装。深さガードは
  `Parser::descend` 1ヘルパに増減を閉じ込め、式の再入口(`parse_or`)と
  単項演算子の自己再帰の2箇所で3経路(括弧・単項連鎖・関数引数)を被覆。
  テスト 130 本 green・clippy clean。`CompileError` の外部消費者は
  `Display` 利用のみで variant 追加は非破壊であることを確認済み
- **残件(H1 実装時に発見、Phase 2 で対応)**: `banto-expr/src/dag.rs` の
  `validate_dag` 内 `visit` が真の再帰実装のため、演算タグを数千個一列に
  チェーン登録(T11 の一括登録 API で可能)すると登録時リビルド
  (`computed.rs` の `validate_dag` 呼び出し)で同種のスタック
  オーバーフローが理論上可能。対応は visit の反復化(明示スタック)か
  依存チェーン深さの上限追加。式1本内のネストを抑える H1 のガードでは
  防げない別経路
- **実施記録(残件、#59/#73)**: `banto-expr/src/dag.rs` の `visit` を
  明示スタックの反復 DFS へ書き換え、一括登録での同種スタック
  オーバーフロー経路を解消(Phase 2)

### H2: 手動書き込み(タグモニタ)の安全意味論 — 状態: 完了(2026-08-08、本 PR)

- **事実**: relay-wright のタグモニタ手動書き込み
  (`POST /api/monitor/write` / Tauri `monitor_tag_write`、Editor 権限)は
  arm ゲート・レート制限・dry-run を**すべてバイパス**する
  (`core/src/engine/monitor.rs`)。disarm 中でも物理書き込みが着弾する
  ことを固定するテストが存在する
  (`monitor_integration.rs` `manual_write_lands_while_disarmed_and_is_audited`)。
  防護は監査ログ(`manual_write`)とロールのみ。意図的設計として文書化は
  されているが、「disarm = 書き込み不能」という運用者の直感と食い違う
- **選択肢**:
  - A) 手動書き込みも arm ゲート配下に入れる(最も安全だが、トラブル
    シュート時に「まず arm」の一手間が増える)
  - B) 設定で明示的に有効化した場合のみ手動書き込みを許可(既定は無効。
    有効化は Admin、監査に残す)+ UI に「安全ゲート対象外」の警告表示
  - C) 現状維持 + UI と README/運用ドキュメントに「disarm は手動書き込みを
    止めない」ことを明記
- **推奨**: B(C の明記も同時に実施)。理由: デバッグ用途の価値は認めつつ、
  「気づかず常時有効」状態を無くせる。A は緊急時の即応性を損なう
- **決定(2026-08-08 オーナー決定)**: **B 案を採用**。手動書き込みは
  設定で明示的に有効化した場合のみ許可(既定は無効、変更は Admin のみ・
  監査記録あり)。UI には「安全ゲート対象外」の警告を表示し、C の
  「disarm は手動書き込みを止めない」旨の明記(UI・README・運用
  ドキュメント)も同時に実施する
- **受け入れ条件**(決定後に確定): 決定内容の本ドキュメントへの日付付き
  追記、実装、挙動を固定するテストの更新(現行テストは仕様変更に合わせて
  書き換え)、relay-wright README の安全上の注意の更新
- **実施記録(2026-08-08)**: 設定キー `monitor.manual_write_enabled`
  (既定 false)を追加。ゲートは `EngineControl::monitor_write` 入口の
  単一チョークポイント(監査・ワイヤ到達より前、設定読み取り失敗時も
  書き込み中止 = fail-closed)。拒否は `write_audit_log` の CHECK を
  変えず、REST/Tauri 両配線層が共有定数の完全一致判定
  (`is_manual_write_disabled`)で検出して一般 `audit_log` に
  `denied`/`resource:"monitor"` を記録(origin を持つのが配線層のため。
  二重記録・漏れが構造的に起きない形)。トグルは
  `GET/PUT /api/monitor/config`(GET viewer+ / PUT admin、
  settings_change 監査)+ Tauri コマンド。UI はモニタ画面の書き込み
  ゲート+有効時の常時警告バナー、設定画面に Admin 限定トグル+警告文。
  README・manual に「既定無効。有効化すると disarm と無関係に書き込ま
  れる」を明記。既存テスト5本を新仕様へ更新し、既定拒否・有効化→
  disarm 中でも着弾・再無効化→即拒否・拒否の監査・Admin 限定トグルの
  新規テストを追加。relay-wright-core 279 テスト green・clippy clean
  (src-tauri のコンパイルはコンテナ制約により CI(Windows)で検証)
- **観察(将来検討)**: 無効時の REST 応答は既存の設定競合ガード流儀に
  合わせ 500(`BantoError::Other`)。`BantoError` が外部 banto クレート
  由来で variant を足せないための制約で、403/409 系の専用エラー表現は
  banto 側の改修機会に検討

### H3: banto-hub gRPC の bind 設定化 — 状態: 完了(2026-08-08、本 PR)

- **事実**: `apps/banto-hub/core/src/grpc.rs` の `GrpcServer::apply` が
  `"0.0.0.0:{port}"` をリテラルで bind しており、変更する設定キーが存在
  しない(`GrpcSettings` は enabled/port のみ)。gRPC は API キー認証必須
  だが TLS が無いため、有効化すると **API キーが平文で全インターフェース
  に流れる**。REST/WS の既定(127.0.0.1)と非対称
- **方針**(2026-08-08 決定): 設定キー `grpc.bind` を追加し既定を
  `127.0.0.1` に変更。公開は管理者の明示 opt-in。bind は `IpAddr` として
  検証し、不正値でもプロセスを落とさない(gRPC 起動をスキップ+ログ)。
  `SocketAddr::new` を使い IPv6 も壊れない形にする
- **互換性**: 既に gRPC を LAN 公開で運用している環境は、アップグレード後
  に `grpc.bind` の再設定が必要(意図した安全側の破壊的変更として運用
  ドキュメントに記載)
- **受け入れ条件**: settings round-trip・REST PUT/GET・不正値 400・不正
  保存値で落ちないことのテスト、既存 gRPC テスト green、運用ドキュメント
  更新
- **実施記録(2026-08-08)**: 設定キー `grpc.bind`(既定
  `DEFAULT_GRPC_BIND = "127.0.0.1"`)を追加し、`GrpcServer::apply` は
  `IpAddr` パース + `SocketAddr::new`(IPv6 対応)で bind、不正保存値では
  gRPC のみ起動スキップ(プロセスは落ちない)。`PUT /api/grpc-settings`
  の `bind` は省略時に現在値維持(mqtt.password と同じ規約)、不正値は
  既存流儀どおり 422(validation)で拒否。pre-H3 DB(bind キー無し)の
  フォールバックを含むテスト 6 本を追加、`cargo test -p banto-hub-core`
  232 本 green。設定 UI に入力欄+平文リスクの注記、運用ドキュメント
  §1/§2/§6/§8 を更新(アップグレード時の再設定注意を明記)
- **関連(同梱しない)**: relay-wright の開発用バイナリ
  `relay-wright-serve` の既定 bind も `0.0.0.0`
  (`core/src/bin/relay-wright-serve.rs`)。開発用途と明記されているため
  H3 では触らず、扱いは H8 の README 整備時に「開発用・公開注意」の明記で
  対応する

### H4: 収集タイムスタンプ逆行対策 + append 失敗の可視化 — 状態: 完了(2026-08-08、本 PR)

- **事実**: 収集ティックはモノトニック時計駆動だが、保存タイムスタンプ
  `ptime_ms` は壁時計(`SystemClock::now_ms`)。時計が逆行すると使用済み
  `ptime` と衝突し、`INTEGER PRIMARY KEY` 制約で INSERT が失敗する。
  append エラーは 24/365 継続のため意図的に黙殺されており
  (`banto-collect/src/task.rs` の `let _ = writer.append(...)`)、
  **逆行区間のデータが警告ゼロで消える**。Windows の時刻同期がステップ
  補正する環境で現実に起こり得る
- **方針(先行可・オーナー判断不要)**: append 失敗の計数と可視化。
  失敗を `collect_events` へイベントとして記録し、連続失敗はステータス
  API に露出する。「黙殺」を「記録された欠測」に変える
- **決定(2026-08-08 オーナー決定)**: 時計逆行で `ptime` が衝突した
  場合は**新データで既存行を上書き**する(単調化クランプは行わず、壁時計
  を常に正とする)。理由(オーナー): 「時刻合わせを行うのは今から正しい
  時間で実行するという意味なので、過去データより時刻合わせ後のデータを
  尊重する」。帰結として、逆行区間の旧データ(誤った時計で記録された分)
  は補正後の時刻が再到達した時点で順次置き換わり、再到達しなかった範囲は
  残る(仕様として記録する)。実装は tstore の INSERT を upsert
  (`ON CONFLICT(ptime) DO UPDATE`)に変更し、collect 側で時計逆行の
  検出イベントを残す
- **関連**: tstore の `synchronous` / `page_size` PRAGMA が未指定
  (sqlx 既定依存)。電源断時の耐久性ポリシーとして**明示**する(現行
  挙動と同値の値を書くだけなら挙動変更なし。値を変える場合はオーナー判断)
- **受け入れ条件**: 失敗計数のテスト(重複 ptime を注入して欠測イベントが
  残る)、既存テスト green
- **実施記録(2026-08-08)**: tstore の samples INSERT を
  `ON CONFLICT(ptime) DO UPDATE`(全値カラム置換)へ変更(`OR REPLACE`
  は delete+insert で rowid=ptime のクラスタ順序を乱すため不採用。タグ
  0 本グループは `DO NOTHING`)。同一フラッシュバッチ内の重複も
  last-wins をテストで固定。collect には接続単位の
  `ClockRegressionTracker` とグループ単位の `AppendHealth`(いずれも
  純粋なエッジ検出器)を追加し、`clock_regression_entered/cleared`・
  `append_failure_entered/cleared` の4イベント種を collect_events へ
  エピソード遷移時のみ発行(kind 列は CHECK 制約なしのためスキーマ
  無変更)。append 失敗は毎回 eprintln + エッジでイベント化(24/365
  継続設計は不変)。新規テスト 16 本、tstore 76 / collect 66 /
  hub-core 全スイート green
- **残件(観察)**: 連続失敗回数のリアルタイム外部公開(status API)は
  未実装(イベント detail と ログのみ)。必要になれば追加

### H5: フロントテスト基盤 + E2E 拡充 — 状態: vitest 導入完了(2026-08-08)・E2E 拡充は Phase 4

- **事実**: フロントエンドのユニットテストは 0 本。CI の Test ステップは
  `--if-present` で全パッケージがスキップされる no-op。Playwright E2E は
  chronogazer のみ(banto-hub は CI 内コメントで既知ギャップ、
  relay-wright は言及もなし)
- **方針**: vitest を導入し、まず純関数(例: banto-hub の `tagCsv.ts` —
  「手元スクリプトで手動検証」とコメントされている)から着手。banto-hub
  の Playwright 設定を追加し、ログイン→タグ作成→現在値表示の smoke を
  CI に載せる。relay-wright は Tauri 依存のため WebDriver 検討が必要で
  後回しでよい
- **受け入れ条件**: CI の Test ステップが実テストを実行して落ちうる状態に
  なること、banto-hub E2E ジョブの追加
- **実施記録(2026-08-08)**: 旧サンドボックスをブロックしていた
  lockfile 問題(プロキシが `@banto/*` git 依存 URL を CI 非互換の形式へ
  書き換える)は、GitHub 直アクセス可能な Windows 開発機で
  `pnpm --filter banto-hub add -D vitest` を実行して解消。lockfile 差分は
  vitest 追加分のみで既存 git 依存 URL は無変更であることを確認済み。
  `apps/banto-hub` に vitest ^4.1.10 + `"test": "vitest run"` を導入し、
  `vitest.config.ts`(node 環境・純関数のみ対象の最小構成)と
  `tagCsv.test.ts`(136 テスト、`tagCsv.ts` の全公開 API を網羅。
  未終端引用符・フィールド途中引用符など現実装挙動の固定、
  serialize/parse ラウンドトリップ、接続スコープのグループ名解決、
  tagKind 別の address/expression/retain 強制ルールを含む)を実装。
  旧セッションの実装物(97 テスト)はサンドボックス消滅により回収
  不能だったため本セッションで再実装した。CI の Test ステップ
  (`pnpm --recursive --if-present test`)は banto-hub に test
  スクリプトが生えたことで実テストを実行して落ちうる状態になった
  (受け入れ条件の前段を充足)。vitest 136 green・svelte-check
  0 エラー・eslint / prettier 通過
- **残り(Phase 4)**: banto-hub / relay-wright の E2E 拡充

### H6: サプライチェーン/再現性 — 状態: 完了(2026-08-08、#59)

- **事実**: `rust-toolchain.toml` なし(CI は floating stable)、
  cargo-deny / cargo-audit / SBOM / dependabot なし
- **方針**: ① `rust-toolchain.toml` でバージョン固定(更新は意図した
  コミットで)② `deny.toml` 追加(ライセンス+advisory)と CI ジョブ
  ③ dependabot(cargo / npm / actions)④ CI の Test ステップ実効化は
  H5 と同時
- **受け入れ条件**: CI に deny/audit ジョブが載り green、toolchain 固定後
  も全ジョブ green
- **実施記録(2026-08-08、#59)**: `rust-toolchain.toml` でツールチェーンを
  固定・`deny.toml`(ライセンス+advisory)+
  `.github/workflows/supply-chain.yml`(cargo-deny ジョブ)・
  `.github/dependabot.yml`(cargo/npm/actions)を追加、CI green

### H7: ソーク実行・障害系テスト — 状態: ⑤+②③④+A.2 完了・④ ギャップ修正済み(2026-08-09)。残るは ①(実機 soak、環境依存)のみ

- **事実**: ソークハーネスは**既にある**(banto-hub `tests/soak.rs`
  = 72h 用・Windows メモリプローブ付き、banto-collect に `#[ignore]` の
  60 秒シード)。未実施なのは「実行と結果記録」。また crash 後の
  再オープン(WAL 回復)・DST 遷移・並行 read-while-write はテスト未カバー
- **方針**: ① 実機相当環境(Windows)で soak を実行し結果を docs に記録
  ② 途中 kill → 再オープンで直近フラッシュ済みデータが読めるテスト
  ③ `ManualClock::set_utc_offset_ms` を使った DST 遷移テスト
  ④ writer append と tsquery 読みを実際に競走させるテスト
  ⑤ 既知フレークの安定化: 依存整理作業(2026-08-08〜09)で繰り返し観測した
  タイミング/スループット依存フレークを、当リポジトリ既存の実証済みパターン
  (`2a96f20` = 検証意図を保ったまま最終 assert 直前に `wait_until` の
  bound-wait を挟む / `3835779` = 収集周期 `period_ms` を広げる)で安定化する
- **実施記録(2026-08-09、本 PR)**: ⑤ のうち原因が明確な 4 件をテスト専用変更で
  安定化(prod コード不変・`banto-hub-core` / `banto-collect` の clippy・fmt clean)。
  - **A.3** gRPC `stream_values_sends_initial_snapshot_then_on_change`: 読み取り時の
    quality 導出(`period_ms × STALE_PERIOD_FACTOR 2.5` = 250ms)が 250ms の eval tick と
    同オーダーのため、値不変のまま `Good→Stale` の偽 on_change が本命の 10→99 変化より
    先に届きうる。**99 が届くまでストリームを drain**(偽 stale を読み飛ばし、全体
    デッドライン付き)して吸収
  - **A.5** `integration.rs`
    `an_invalid_config_keeps_the_old_collector_and_surfaces_last_config_error`:
    同じ 250ms 余裕。最終 `q=="good"` の直前に `wait_until(8s)` の bound-wait を挿入
    (`2a96f20` と同型、hard assert は不変)
  - **A.1** `stream.rs` `plc_disconnect_is_relayed_as_an_event`: subscribe-after-publish
    競合(`connect_ws` 返却は client が 101 を見ただけで、server の `handle_socket` が
    `subscribe_events()` に到達済みとは限らない。`broadcast` はリプレイ無しなので
    `sim.stop()` が先行するとイベントを取りこぼす)。`sim.stop()` の**前に**値購読+初期
    スナップショット往復で購読の生存を確定
  - **A.4** soak スループット下限を生存性フロアに緩和(`tests/soak.rs` mini_soak =
    理論値 1/3 → 1/15、`banto-collect/tests/integration.rs` mini_soak = `>=10` → `>=2`、
    上限は維持)。`MissedTickBehavior::Skip` で欠落 tick は取り戻さないため過負荷ランナーで
    スループットが ~1/10 まで下がりうる。当テストの意図は生存性でありスループット精度は
    `#[ignore]` の long soak が担保
- **実施記録(2026-08-09、②③④ = 別 PR)**: コンテナで実施可能な新規堅牢性テスト 3 件を
  追加(テスト専用・prod/`Cargo.toml` 不変、`banto-tstore` / `banto-tsquery` の clippy・fmt
  clean。各テストは条件待ち+寛大なタイムアウトで**それ自体が flaky にならない**設計)。
  - **② crash 再オープン**(`banto-tstore/src/writer.rs`
    `crash_drop_without_close_keeps_flushed_rows_and_loses_only_buffered`): `TsWriter` は
    `Drop` 無し → `close()` せず drop = クラッシュ模擬。固定 `ManualClock` + 閾値未満バッチで
    「flush 済みのみ WAL で生存・未 flush バッファは消失」を検証
  - **③ DST 遷移**(同
    `a_runtime_utc_offset_change_alone_rotates_across_the_local_date_it_crosses`): `now_ms`
    固定のまま `set_utc_offset_ms` のみ変更(+9h→+10h)でローカル日付境界をまたぎ、
    `rotate_if_needed` が実行中のオフセット変更を反映して新日付ファイルへローテーション
    することを検証(D1/D2 は `LocalDate::from_epoch_ms` で自己検証)
  - **④ read-while-write**(`banto-tsquery/tests/concurrency.rs`
    `concurrent_reads_during_writes_never_corrupt_or_error`): WAL の 1-writer/N-reader で、
    背景 writer の append+flush 中に前景で `read_range`/`read_decimated`/`catalog` を反復。
    破損・エラー・count 逆行なし、writer 完了後に全 N 行を確認(5+40 回連続 green)
  - **④ で判明したギャップ → 本 follow-up PR(A.2 と同時)で修正済み**: `TsQuery` の
    「最初のファイル生成レース中は absent/empty を返す(エラーにしない)」契約に実ギャップが
    あった(生成途中の 0 テーブル DB を全 read が hard `IncompatibleFile` にしていた)。
    修正内容は下記「A.2 + ④ ギャップ」の実施記録を参照
- **実施記録(2026-08-09、A.2 + ④ ギャップ = follow-up PR)**:
  - **A.2** `stream.rs` `a_slow_subscriber_gets_disconnected_once_the_outbound_queue_fills`
    のフレーク根治(**prod 不変**)。原因は「アウトバウンドキュー溢れは決定的」という前提が
    `multi_thread` 下で崩れること(別 spawn の `writer_task` が並行 drain し `try_send` が
    `Full` を観測し損ねる)。修正は当該テストを **`#[tokio::test]`(current_thread)化**する
    だけ — 単一ワーカーでは同期・await 無しの `evaluate()` ファンアウト中に writer_task が
    走れず、320 本が容量 256 のキューを 257 本目で確定的にオーバーフローし backpressure
    close が必ず発火(**40 回連続 green** で決定性確認)。`multi_thread` に戻さない旨コメント明記
  - **④ TsQuery 生成途中ファイル対応**(prod 修正): `banto-tstore` に
    `TstoreError::Uninitialized` を新設し、`read_file_meta` が `sqlite_master` で `tstore_meta`
    不在を判定して返す(`tstore_meta` を持つが書式不整合は従来どおり `IncompatibleFile`)。
    `banto-tsquery` の全 read 経路(`raw.rs` / `plan.rs`=decimate・aggregate / `catalog.rs`)は
    `Uninitialized` のファイルを **skip(空扱い)**。concurrency テストは初回行前の許容を撤去し
    「read は一切エラーしない」に強化。banto-tstore 79 / banto-tsquery 41 green
  - **意味論変更(オーナー可視)**: `tstore_meta` テーブルが**無い**ファイル(生成途中・
    無関係な sqlite・稀に meta だけ失ったファイル)は「クエリ全体をエラー」から「その file を
    静かに skip」へ。異物 1 個で read API 全体が壊れなくなる利点と、`tstore_meta` だけ失った
    病的ケースが黙って skip される trade-off(後者は日次ファイル+スキーマ凍結上ほぼ起きない)
- **備考**: ① 実機相当環境(Windows)での 72h soak 実行はコンテナ/CI では完結しない
  (実行環境と 72h の実時間が必要)ため当セッション対象外

### H8: ドキュメント整合 — 状態: 完了(#58/#59)

- **事実**: `docs/tag-server-design.md` はヘッダが「設計先行(実装未着手)」
  のまま、同一ファイル §9 の表が T6〜T12 を「実装済み」と記載する自己矛盾。
  `docs/plan.md` の状態行も T8 止まり。root README にはリポジトリ最大の
  アプリである banto-hub の記載が**一切ない**(構成ツリーにも本文にも)。
  per-app README は relay-wright(安全注意)のみ
- **方針**: ① 状態ヘッダ 2 件の実態合わせ(本 PR で実施)② README への
  banto-hub セクション追加 ③ 全 docs に `状態:` と `最終検証日:` の
  ヘッダを義務化(CLAUDE.md にルール追記)④ banto-hub / chronogazer の
  最小 README
- **受け入れ条件**: ヘッダと実装状況表の矛盾ゼロ、README から全アプリに
  到達可能
- **実施記録(#58/#59)**: 状態ヘッダ2件(tag-server-design.md・plan.md)の
  実態合わせ、README に banto-hub セクション追加、CLAUDE.md に `状態:`/
  `最終検証日:` 義務化ルールを追記、banto-hub / chronogazer の最小
  README を追加

### H9: SLMP 構造化エラー + transport 共通化 — 状態: 未着手

- **事実**: 外部 `slmp` クレートがエンドコードをエラー文言でしか露出しない
  ため、banto-plc / banto-plc-write の 2 箇所で文言パースしている。ただし
  `ErrorKind::InvalidData` との二重フィルタ + パース不能時 fatal 側の
  fail-closed + CI で実行される tripwire テスト 2 本で封じ込め済み。
  故障モードは可用性劣化であり誤データ方向には倒れない。また同クレートは
  受信バッファ 2048 バイト固定(分割応答の再組み立てなし)という制約もある
- **方針**: fork(`tyaro/slmp`)または upstream PR で構造化エラー
  (`SlmpError::Device { end_code }` 相当)を得る。その作業に broker の
  接続処理重複解消(session/transport 層の共通化)を同梱する。**慌てて
  やる必要はない**が、`slmp` のバージョンを上げる前には必ずここを通す
- **受け入れ条件**: 文言パース(`END_CODE_MARKER`)の完全削除、tripwire
  テストの構造化エラー版への置き換え

### H10: 認可の細粒度化 — 状態: 完了(①② = PR #74、③ = PR #75、2026-08-08)

- **事実**: ① API キーに有効期限がない(revoke のみ)② read スコープは
  全タグ一括(タグ単位は write のみ)③ relay-wright の arm に時限失効が
  ない(rate-limit トリップ以外で自動 disarm しない)
- **論点(オーナー判断)**: ①期限の既定(無期限を許すか、既定 1 年等に
  するか)②read スコープをタグ単位にする場合の catalog API の扱い(見える
  タグ一覧も絞るか)③arm の失効時間の既定値(例: 8 時間 = 1 シフト)
- **経過(2026-08-08)**: オーナーより「API キーの有効期限は知識が
  ないため後日議論」。前提知識の Q&A(キーは証明書ではなく Bearer
  文字列であること、期限を付けた場合は期限毎に「新キー発行+クライアント
  設定の書き換え」の手動作業が発生すること、現場機器では期限切れが可用性
  事故になりやすく「既定無期限+キー毎の任意期限+UI 警告」が現実的で
  あること)は会話で回答済み。決定は持ち越し
- **決定(2026-08-08 オーナー決定、「H10 は推奨」)**: 推奨案を採用する。
  ① API キー有効期限: 既定は無期限を維持し、キー毎に**任意**で期限を
  設定可能にする。UI に「期限接近」「長期未使用」の警告を表示。主たる
  統制は失効(revoke)と last_used 監視のまま ② arm 時限失効
  (relay-wright): 導入する。既定 8 時間(1 シフト)・設定変更可能
  ③ read スコープのタグ単位化: catalog API の扱い(スコープ外タグを
  一覧から隠すか、一覧には出すが値を読めなくするか)の比較案を実装前に
  オーナーへ提示してから着手する
- **受け入れ条件**(①②は確定): 期限切れキーの 401 と UI 警告のテスト、
  無期限キーの従来動作不変、arm が既定 8h で自動 disarm し監査に残る
  テスト、既存テスト green
- **実施記録(2026-08-08、PR #74)**: ①② を実装(③ は対象外)。
  - **① banto-hub**: API キーに任意の `expires_at`(epoch ミリ秒、`NULL`=
    無期限)を追加。`ApiKeyLookup::Expired` を追加し `lookup` を
    ハッシュ一致 → revoked → tripped → expired の順に判定(secret 不一致は
    常に `NotFound` の情報漏洩防止規律を維持)、REST 401 / gRPC
    unauthenticated で拒否し監査。生成時は「未来限定」検証(過去/現在以下は422)。UI は一覧に有効期限列と「期限接近/期限切れ/長期未使用」バッジ、
    作成フォームに任意の期限入力(判定は依存ゼロの純関数 `apiKeyWarnings`
    を vitest でテスト、閾値 14日/90日)。`cargo test -p banto-hub-core`
    245・vitest 150 green
  - **② relay-wright**: arm に時限失効を追加。`ArmingState` が窓
    (`auto_disarm`)と arm 時刻(`armed_at`、注入 `Instant`)を保持し、
    engine ループの毎 tick(アイドルでも回る)で `is_expired` を判定 →
    失効時は in-memory disarm + `persist_armed` + 監査(既存 `Disarm` を
    actor=None + 区別 detail で再利用、`action` CHECK 変更なし)。設定
    `arm.auto_disarm_secs`(既定 28800=8h、0=無効)を engine 起動/reload で
    反映、`EngineStatus` に残り秒/設定秒、REST `GET/PUT /api/engine/config`
    (viewer+/admin)+ UI。`cargo test -p relay-wright-core` 293 green
    (src-tauri のコンパイルと svelte-check はコンテナ制約により CI
    (windows-latest / Frontend)で検証)
  - **③ read スコープのタグ単位化**(PR #75、2026-08-08 マージ済み): 案 B を
    オーナー決定(catalog は全タグ+PLC アドレスを開示、per-tag read スコープは
    値の読み取り=単一/バルク/WS・gRPC ストリームのみ絞る。理由「アドレスが
    見えると割り付けミスに気づきやすい」)。文法 `read:{conn}.{group}.{tag}` /
    `read:{conn}.{group}.*`(read 限定ワイルドカード、write は非対称)、素の
    `read`・セッション認証は不変。詳細は docs/h10-3-read-scope-proposal.md。
    banto-hub-core 273 green

## 4. フェーズ分け

- **Phase 1(PR #58、2026-08-08 マージ済み)**: H1、H3、H8 の状態ヘッダ、
  H2、H4(H2/H4 はオーナー決定が Phase 1 期間中に出たため前倒しで同梱)
- **Phase 2(PR #59/#73、2026-08-08 マージ済み)**: H6、H5 の vitest 導入
  - CI test 実効化、H1 残件(dag.rs 反復化)、H8 残り
- **Phase 3(PR #74/#75、2026-08-08 マージ済み)**: H10 ①任意期限+警告・
  ②arm 時限失効(#74)、③read スコープのタグ単位化(案 B、#75)
- **Phase 4(環境・実時間依存)**: H5 の E2E 拡充、H7 の soak 実行、H9

## 5. スコープ外(明示)

- **実機 PLC での検証と 72h soak の実運転**はコード修正項目ではなく
  **リリースゲート**として別管理(docs/plan.md §4b の W5 残項目と同じ扱い)
- 外部 `banto` リポジトリ側(banto-server のセッション認証・banto-storage
  の SQLite 接続設定)の監査は本リポジトリでは完結しない。必要になった
  時点で banto 側のマイルストーンとして起票する

## 6. バックログ(未採番・観察事項)

採番するほどの緊急性はないが、レビューで観察された事項:

- broker のジョブキューが完全 FIFO で write の優先レーンがない(遅い PLC
  ではポーリング read の背後で write レイテンシが伸びる)
- 全 PLC 接続が単一 `TsWriter` の mutex に結合(遅いディスクで全接続の
  ティックが波及遅延 → Skip 欠測)。接続数を増やす前に実測を
- CI の E2E は単一 spec / 単一ワーカー。画面が増えたら分割
- `AGENTS.md` / `SECURITY.md` / `CONTRIBUTING.md` の整備(OSS として
  外部コントリビュータを迎える段階になったら)
- **T17-1 cross-session mutex 診断**（2026-08-10 Windows 実機観察、
  [banto-hub-t17-design.md](banto-hub-t17-design.md) §8「Windows 実機検証」）:
  LocalSystem（Session 0）が profile mutex を保持している間、ユーザーセッション
  から Console 起動すると**起動拒否自体は成功**するが、
  `ProfileLockError::AlreadyHeld`（owner 診断付き）ではなく
  `ProfileLockError::Io`（os error 5 / Access Denied）になることがある。
  T16-2 fallback UI 向けに `CreateMutexW` の `ERROR_ALREADY_EXISTS` と
  `ACCESS_DENIED` を分岐し、可能なら `profile.lock` から owner を読んで
  `AlreadyHeld` に正規化する改善を検討（T17-2 以前の小改修候補）
  → **2026-08-10 対応済み**（`profile_lock.rs` の `OpenMutexW` /
  `profile.lock` フォールバック。実機で `AlreadyHeld` + owner 確認）
