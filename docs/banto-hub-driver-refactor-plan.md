# banto-hub ドライバ基盤の改造計画

作成日: 2026-10-19
状態: **計画（2026-10-19 オーナー決定の方針）・P0（P0-1・P0-2・P0-3）完了（2026-10-20）、R1 は R1-3b（組み立て役 `banto-drivers` と `HubDrivers` の注入）まで、R2a 以降は未着手**。段の順番・受け皿の範囲・シリアル対応は決定済み。最初の対象は手元にある MELSEC と Modbus（§7 Q1）。Q3（crates.io に公開する）・Q4（シリアルは COM ポート直結とシリアル - Ethernet 変換器）も決定済み。Q2（ドライバの置き場所: R1〜R2b は本リポジトリ、API の公開後は公式ドライバを別リポジトリへ）も決定済み。§7 の Q1〜Q4 の大方針は決まっており、最初のシリアルドライバ（Modbus RTU が候補）や COM ポートの扱いの詳細は R2b 以降で詰める。 2026-10-19: §8 に P0 の作業指示、付録 A に P0-2 の棚卸しの結果を追加（P0-2 は完了）。SLMP の分割受信は `tyaro/slmp` v0.2.1 で解消（§6・§8.3）。2026-10-20: P0-3 完了（振る舞いを固定するテスト 6 件とシミュレータの新モードを足し、§8.3 に対応表を載せた）。同日、R1 の詳細設計 [banto-driver-api-design.md](banto-driver-api-design.md) に合わせて §4 の R1-1・R1-3〜R1-7 の文言を直し、R2b-0（旧 R1-7b）を足した。
対象: banto-hub（と ChronoGazer）のドライバまわりの構造を、**ドライバを 100 種類規模で増やせる形**に組み替える。WASM プラグインは**受け皿だけ先に作り、WASM 本体は後回し**にする。
関連: [banto-driver-api-design.md](banto-driver-api-design.md)（R1 の詳細設計: 型・trait・レジストリ・PR の分け方）、[banto-hub-driver-plugin-design.md](banto-hub-driver-plugin-design.md)（WASM プラグインの設計。§12 が本計画の元になった構成案）、[banto-hub-external-db-design.md](banto-hub-external-db-design.md) §9.5（外部 DB の取り込み口 `push`）、[tag-server-design.md](tag-server-design.md) §3.4 / §3.4.1（今のドライバの置き場所）。

---

## 1. 前提

- **対応ドライバは 100 種類規模を見込む**（他社の OPC サーバの対応機種を見た 2026-10-19 のオーナーの見立て）。シリアル（RS-232 / RS-485）の機器も対象に含む。
- **ドライバは一度できれば仕様はあまり変わらない**。コストの中心は「ドライバを足す手間」と「品質をそろえる手間」で、個々のドライバの改修ではない。
- **OSS として、他者がドライバを作りやすい環境にしたい**。WASM プラグインはその手段の 1 つ（言語を問わず、Hub を再ビルドせずに配れる）。
- 今の構造ではドライバを 1 つ足すたびに、SQL の `CHECK`・`ALLOWED_PROTOCOLS`・broker の `DRIVERS`・`banto_plc::Address` の enum・UI の分岐・設定パッケージの固定値などを同時に直す必要がある（plugin-design §12.1）。100 種類では回らない。

## 2. 決定事項（2026-10-19 オーナー決定）

1. **WASM は受け皿だけ先に作り、WASM 本体（wasmtime のホスト・パッケージ・署名・導入）は後回しにする**。受け皿の範囲は §3。
2. **順番は P0 → R1 → R2a → R2b**。その後、主要メーカーのドライバを組み込みで足しながら、WASM 本体（W 段）の着手時期を決める。
3. **I/O を担う部分の一本化とアドレスの不透明化は R2b でやる**（100 種類のドライバに事故対策を散らさないため）。
4. **シリアル通信を対象に含む**。I/O の一本化（R2b）は最初からシリアルを見込んだ形にする。
5. 外部 DB は案 A（取り込み口 `push` + DB ブリッジ）。形は external-db-design §9.5。

## 3. 「受け皿」の範囲

WASM のドライバを後から「もう 1 つのドライバ実装」として差し込めるように、Hub 側の形を先にそろえる。

**作るもの（R1・R2 の中で作る）**:

- ドライバの trait を plugin-design §6 の WIT と同じ形にする（I/O を持たないフレーム型: `config` / `address` / `plan` / `decode` / `encode` / `handshake`）。読み取り単位はデータ型・文字列長・エンコーディングを持つ（plugin-design の監査 B2）。
- `DriverRegistry` を唯一の正にする（SQL の `CHECK`・`ALLOWED_PROTOCOLS`・`DRIVERS`・UI・設定パッケージの固定値をなくす）。
- 不透明なアドレス（`DriverAddress`）と、接続設定の JSON（ドライバのスキーマで検証、秘密値は `banto-secret-store`）。
- 1 つのプロトコル ID に複数の実装を登録し、**接続ごとに実装を選べる**仕組み（plugin-design §12.3。組み込み版と WASM 版の併用に使う）。
- UI の汎用フォーム（ドライバのスキーマから組み立てる）と、ドライバ同梱 UI を載せる iframe の枠（中身は W 段）。
- ドライバの適合テスト一式（どのドライバも満たすべき振る舞い）。

**作らないもの（W 段）**: wasmtime の組み込み、WIT の確定と公開、プラグインのパッケージ形式・署名・導入・更新・失効、同梱 UI の配信。

## 4. 段階と PR の分け方

各段は振る舞いを変えない PR を積み、段の完了条件を満たしてから次へ進む。PR の単位は目安で、着手時に詰める。

### P0 準備（振る舞いを変えない）

作業指示は §8、P0-2 の棚卸しの結果は付録 A。

| PR   | 内容                                                                                                                                                    | 完了条件                                                 |
| ---- | ------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------- |
| P0-1 | `apps/banto-hub/core/src/rest.rs`（18,904 行）をリソースごとのモジュールに分割する（移すだけ）                                                          | 既存テストが全部通る。公開 API・ルートの一覧が変わらない |
| P0-2 | プロトコル名の文字列（`"postgres"` / `"modbus-tcp"` / `"slmp"` 等）で分岐する箇所の棚卸し（2026-10-19 時点で 42 ファイル）。本書に一覧を足す            | R1・R2b の作業範囲が確定する                             |
| P0-3 | 今の Modbus / SLMP クライアントの振る舞いを固定するテストを足す（分割受信・遅れて届いた応答・再接続・致命 / 非致命の分類・#337 の 1 接続 = 1 ソケット） | R2b の I/O 一本化の判定基準として使える                  |

### R1 ドライバ API とレジストリ（互換を壊さない）

| PR   | 内容                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |
| ---- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| R1-1 | `banto-driver-api` を作り、共通の型（`DataType`・`TagValue`・`PlcValue`・`StringEncoding`・`WordOrder`）を `banto-plc` から移し、エラーの分類・`DriverAddress` 等の新しい型を足す（型と trait だけ、ロジックなし）。**読み取り要求（`ReadRequest` 系）は R1 では移さない**（`Address` を含むため。R2b-2 で `TagSpec` に替わる。api-design §3.1・§13 の 7）                                                                                                           |
| R1-2 | フレーム型ドライバの trait（§3 の形）とセッション型の扱いを定義する                                                                                                                                                                                                                                                                                                                                                                                                  |
| R1-3 | `DriverRegistry` と組み立て役 `banto-drivers` を作り、broker の `DRIVERS` を外から差し込める形にする（api-design §10 で R1-3a = broker の表 `DriverTable` と `HubSessions`、R1-3b = 組み立て役と注入の配線、に分ける）                                                                                                                                                                                                                                               |
| R1-4 | Modbus / SLMP の**静的な部分**（識別・設定の検証・アドレス関数）を trait（`DriverBase`）の実装に載せ替える。I/O は今のまま broker の表（`DriverTable`）に注入する形（api-design §4.4 の `HostBound`）。フレーム型への載せ替えは R2b-3（2026-10-19 api-design で文言を修正）                                                                                                                                                                                          |
| R1-5 | 適合テスト一式を **別 crate `banto-driver-conformance`** に置き（API crate の依存ゼロを保つため。api-design §2.2、Q1）、Modbus / SLMP で走らせる（R1 で通すのは api-design §7.3 の範囲）                                                                                                                                                                                                                                                                             |
| R1-6 | CI の判定（`.github/scripts/ci-changes.mjs`）を組み立て役に合わせる（plugin-design §12.6 の条件 6）。R1-7a の後に行う（判定の対象に `banto-hub-host` が要るため。api-design §10）                                                                                                                                                                                                                                                                                    |
| R1-7 | banto-hub のバイナリ（`banto-hub`・`banto-hub-elev`）を core と別の Cargo package `banto-hub-host` に分け、`banto-drivers` への normal 依存をそちらだけに置く（api-design §2.4 の R1-7a）。**core が CI の対象から外れるのはこの段では無理**: CI の逆依存は dev-dependency も辺にし、core の統合テストと `src` 内の単体テストが実ドライバを使うため、テストの移動と書き換え（R2b-0）が要る。ChronoGazer のバイナリの分離も R2b（2026-10-19 api-design の監査で修正） |

完了条件: 既存テストが全部通り、Modbus / SLMP が適合テスト（api-design §7.3 の範囲）を通る。詳細は [banto-driver-api-design.md](banto-driver-api-design.md)。

### R2a 取り込み口（互換を壊さない）

external-db-design §9.5 の形で作る。

| PR    | 内容                                                                                                   |
| ----- | ------------------------------------------------------------------------------------------------------ |
| R2a-1 | Hub の収集で外部ソース（`ExternalSource`）を受けられるようにする                                       |
| R2a-2 | `push` ドライバ（接続の種類を足す）、`ingest:{接続}` の権限、WebSocket / REST の送信口、接続単位の監査 |
| R2a-3 | `banto-hub-sink` を DB ブリッジに広げ、Source を担わせる。Hub 内の PostgreSQL Source と併用する        |

完了条件: 同じ DB の値を Hub 内の Source とブリッジ版の両方で取り込み、値と品質が一致する。

### R2b 構造の整理（互換を壊す）

| PR    | 内容                                                                                                                                                                                                                                                                              |
| ----- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| R2b-0 | （旧 R1-7b）banto-hub の core の統合テストのうち実ドライバを要るものを `banto-hub-host` へ移し、core の `src` 内の単体テストを偽ドライバで書き直し、core の dev-dependency から `banto-drivers` を外す。これで core がドライバだけの変更の CI の対象から外れる（api-design §2.4） |
| R2b-1 | 接続設定を固定列から JSON（ドライバのスキーマで検証）へ移し、秘密値を `banto-secret-store` へ。SQL の `CHECK` を撤去してレジストリを正にする                                                                                                                                      |
| R2b-2 | アドレスの不透明化（`DriverAddress`）。既存タグのアドレスを移行時に全件照合する（plugin-design §7.4 の preflight と同じ仕組み）                                                                                                                                                   |
| R2b-3 | I/O を担う部分の一本化（Hub と ChronoGazer で共用）。**トランスポートを抽象化し、TCP・シリアル（COM ポート直結）・変換器向けの TCP（生データ）を扱える形にする（最初の実装は TCP と COM 直結。§7 Q4）**。P0-3 のテストを全部通す                                                  |
| R2b-4 | 接続ごとに実装を選べる仕組み（§3）                                                                                                                                                                                                                                                |
| R2b-5 | シミュレーションを値の層へ移す（plugin-design §12.2 の 6）                                                                                                                                                                                                                        |
| R2b-6 | UI をドライバのスキーマから組み立てる汎用フォームに替え、iframe の枠を用意する                                                                                                                                                                                                    |
| R2b-7 | REST / MCP / 設定パッケージの形の変更（設定パッケージは版を上げる）と ChronoGazer の追従                                                                                                                                                                                          |
| R2b-8 | 併用の結果を見て、`postgres` 接続・タグ種別 `db`・グループの `query_sql`・`db_source` を `push` へ移して撤去する                                                                                                                                                                  |

完了条件: P0-3 の振る舞い固定テストと適合テストを新しい I/O の上で全部通す。既存の設定は移行して同じ値・品質で動く。

### その後: ドライバの追加と W 段

- 主要メーカーのドライバを組み込みで足す（最初は手元にある MELSEC と Modbus。§7 の Q1）。シリアルのドライバを少なくとも 1 つ入れ、R2b-3 のトランスポートの抽象が実用になることを確かめる。
- **W 段（WASM 本体）**: plugin-design §10 の P0〜P5。着手時期は、ドライバ数・Hub のビルドとリリースの重さ・外部からの貢献の状況を見て決める。

## 5. OSS としての配慮

他者がドライバを作る前提なので、ドライバとの境目は公開の契約として扱う。

- `banto-driver-api` は小さく、動かさない（plugin-design §12.6 の条件 1）。変えるときは版を上げ、変更履歴を残す。
- 適合テスト一式は Hub なしでも走らせられる形にする（ドライバ単体のリポジトリの CI で使える）。
- W 段の前でも、他者は trait を実装した組み込みドライバを PR で寄せられる。W 段の後は WASM で、言語を問わず Hub の再ビルドなしに配れる。
- ドライバ作者向けの手引き（作り方・適合テスト・アドレスと設定のスキーマの書き方）は R1 の完了時に書く。
- ライセンスはリポジトリと同じ MIT。他者のドライバのライセンスの扱いは W 段で決める。

## 6. 危険と対策

| 危険                                                                                                                               | 対策                                                                                                                                                                                                             |
| ---------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| I/O の一本化で、今のクライアントが事故で学んだ振る舞い（分割受信・遅延応答など）を落とす                                           | P0-3 の振る舞い固定テストを、新しい I/O で全部通すことを R2b の完了条件にする                                                                                                                                    |
| ~~SLMP（外部の `tyaro/slmp` v0.2.0）は応答を 1 回の `read()` で受けるため、分割して届くと Framing の致命エラーで切断・再接続する~~ | **2026-10-19 解消**: オーナー判断で `tyaro/slmp` 側で直した（[tyaro/slmp#1](https://github.com/tyaro/slmp/pull/1)、v0.2.1: 13 バイトのヘッダを読んでから宣言長ちょうどを読む）。本リポジトリは v0.2.1 を参照する |
| アドレスの移行で解釈が変わる                                                                                                       | 移行時に全件照合し、1 件でも合わなければ移行しない                                                                                                                                                               |
| `banto-driver-api` が頻繁に変わり、全ドライバと CI に波及する                                                                      | 型と trait だけを置き、R1 の時点で WIT の形と照らして固める                                                                                                                                                      |
| 作業が大きく、他の作業と競合する                                                                                                   | 段ごとに完了条件で区切り、振る舞いを変えない PR を積む。R2b 以外は互換を壊さない                                                                                                                                 |

## 7. 未決事項

- ~~**Q1**: 最初に対応するメーカーの優先順位~~ → **2026-10-19 オーナー決定: 最初は手元に実機がある MELSEC（SLMP）と Modbus**。どちらも既存の組み込みドライバなので、R1〜R2b はこの 2 つで確かめる。シリアルの最初の 1 つは Modbus RTU を候補とする（同じ Modbus の実機で TCP と比べられるため。未決定）。
- ~~**Q2**: ドライバの置き場所~~ → **2026-10-19 オーナー決定: 段階で分ける**。R1〜R2b の間は本リポジトリ（`banto-driver-api` の形がまだ動くため。別リポジトリだと API を変えるたびに両方を直して版を合わせる手間が出る）。API が固まって crates.io に公開した後は、公式ドライバを別リポジトリ（例: `banto-drivers`、メーカーごとの crate のワークスペース）へ移す。外部の作者のドライバは作者のリポジトリに置く。`tyaro/slmp` のようなワイヤ層は今どおり別リポジトリ（tag-server-design §3.4.1）。
- ~~**Q3**: `banto-driver-api` と適合テスト一式を crates.io に公開するか~~ → **2026-10-19 オーナー決定: 公開する**（他者が Hub のリポジトリなしでドライバを作れるようにする。§5 の「版を上げる・変更履歴を残す」が公開の契約になる）。
- ~~**Q4**: シリアルの接続方式~~ → **2026-10-19 オーナー決定: COM ポート直結と、シリアル - Ethernet 変換器の両方を対象にする**。変換器の仮想 COM 方式は COM ポートに見えるので直結の対応で動く。TCP で直接つなぐ方式（生のシリアルのフレームを TCP で流す）は、R2b-3 のトランスポートに「TCP（生データ）」の枠を設計で用意し、実装は必要になってから足す（最初に実装するのは COM 直結）。設計で押さえる点: フレームの終わりを無音の間隔でなく中身（長さ・CRC）で判定する、変換器を通る分タイムアウトを長めにとれる、RS-485 のマルチドロップ（1 つのポート・変換器に局番違いの機器が複数）を 1 接続で扱う、シリアル設定（速度等）は変換器側で持つ。Windows サービスからの COM ポートの扱い（排他・権限・ポート名の指定）も R2b-3 で詰める。

## 8. P0 の作業指示（2026-10-19）

実装担当がこの節だけで迷わず進められるように書く。3 つの PR は互いに独立で、どの順でも並行でもよい。どれも**振る舞いを変えない**。CLAUDE.md・[implementation-checklist.md](implementation-checklist.md) を先に読むこと。

### 8.1 P0-1: `rest.rs` の分割

**目的**: `apps/banto-hub/core/src/rest.rs`（18,904 行、うちテスト 8,565 行）を、中身を変えずにリソースごとのファイルへ移す。R2b で接続・タグ・グループの API を作り替えるときの差分とレビューを小さくする。

**やらないこと**: 関数・型の名前や中身の変更、エラーメッセージや HTTP ステータスの変更、ルートの追加・削除、ミドルウェアの順番の変更、リファクタ（共通化・整理）。見つけた改善点は PR の説明に「後でやること」として書くだけにする。

**分割の形**（前例: `apps/chronogazer/core/src/rest.rs` の `mod display_groups;` と `rest/display_groups.rs`）:

- `rest.rs` を `rest/mod.rs` にはしない（`git log --follow` を保つため `rest.rs` はそのまま親として残す）。子は `apps/banto-hub/core/src/rest/<名前>.rs` に置き、親に `mod <名前>;` を足す。
- 子から親の共通部品は `use super::{...};` で使う。子の関数・型の可視性は、他の子や `mcp.rs` などが使うものだけ `pub(crate)`、親からだけ使うものは `pub(super)`。**今 `pub` のものは `pub` のまま**にし、親で `pub use <子>::<名前>;` して `crate::rest::<名前>` のパスを保つ。
- 1 PR で全部を移さない。下の順に、1 PR = 1〜3 グループで進める（1 PR の移動量の目安は 3,000 行まで）。

| 順  | 子モジュール                                                                 | 移すもの（2026-10-19 の行範囲。着手時に再確認）                                                                                                                                                 |
| --- | ---------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 1   | `settings.rs`                                                                | MQTT（1427-1641）・store（1642-1918）・gRPC（1919-2098）・OPC UA（2099-2360、`opcua_settings_tests` を含む）                                                                                    |
| 2   | `api_keys.rs`・`write_control.rs`・`collection_control.rs`・`write_audit.rs` | 605-830・831-1141・1142-1426・2361-2403                                                                                                                                                         |
| 3   | `plc_connections.rs`・`crud.rs`                                              | I1 CRUD 共通層と接続（2404-3514）・接続テスト（3515-4104）。共通層はグループ・タグ・保留中の変更・`mcp.rs` も使うので `crud.rs` に分けた（2026-10-20）。グループ・タグのペイロードは順 4 で移す |
| 4   | `collection_groups.rs`・`tags.rs`                                            | 4105-4423・タグ（4424-5239。`TxTestHook` は `tags.rs` へ）と一括操作（6024-6648）・`tag_registry_router`（6649-6763）                                                                           |
| 5   | `pending_changes.rs`                                                         | 5240-6023                                                                                                                                                                                       |
| 6   | `sink.rs`                                                                    | 6764-7235                                                                                                                                                                                       |
| 7   | `tag_space.rs`・`admin_status.rs`・`write.rs`                                | `/api/v1` 読み取り（7236-8256）・管理の状態と値（8257-8949）・書き込み（8950-9322）・`/api/v1` 認証と `tag_space_router`（9401-9693）                                                           |
| 8   | `openapi.rs`・`commissioning.rs`・`expression.rs`                            | 9323-9400・9687-9693・9694-9811。表から漏れていた #342 の式チェック・関数表も `expression.rs` へ移した（2026-10-20）                                                                            |

- 親（`rest.rs`）に残すもの: モジュール文書（1-85）、`use` とre-export、共通の認証部品（158-604）、ルーターの組み立て（`api_router_with_controller_mode` ほか 9812-10339）。
- **テスト（10340-18904 の `mod tests`）は最後に別 PR で**、バナーコメント（API keys 11371、store 16046、sink 17579 など）を境に `rest/tests/<名前>.rs` へ分ける。共通の補助関数（`test_env`・`router_with_token`・`session_*` など）は `rest/tests/common.rs` に `pub(super)` で置く。前例: `apps/chronogazer/core/src/rest/local_grant_tests.rs`。

**守るもの（外から使われているパス）**: `crate::rest::` 経由で `runtime.rs:181`・`opcua.rs:547`・`stream.rs:164`・`mcp.rs:93-101`（26 個）と `mcp.rs` 内の `registry_change_should_queue` ほかが使っている。統合テストは `banto_hub_core::rest::{api_router, api_router_with_controller, user_session_lookup}` を使う。TS 側が形を写している `pub` のペイロード型（`PlcConnectionPayload` ほか）も名前とパスを保つ。

**確かめ方**（各 PR で全部）:

1. `cargo fmt --all --check`、`cargo clippy -p banto-hub-core --all-targets -- -D warnings`、`cargo test -p banto-hub-core`（`cargo nextest run -p banto-hub-core` でもよい）。
2. **ルート一覧が変わらないこと**: 今はルート全体を書き出す仕組みがない（OpenAPI のスナップショット `openapi_snapshot_matches` は `/api/v1` の 8 本だけ）。最初の PR で、`api_router_with_controller` が組み立てたルーターの「メソッド + パス」の一覧をスナップショットと比べるテストを足すか、それが難しければ `.route(` の行（2026-10-19 時点で 61 箇所）を移動前後で機械的に比べ、その結果を PR の説明に貼る。
3. `openapi_snapshot_matches` が更新なしで通ること（`UPDATE_OPENAPI_SNAPSHOT` を使わない）。
4. `git diff --stat` で、移したファイル以外に変更がないこと。`git diff -M` で移動として認識される量を確認する。
5. `cargo check --workspace --all-targets`（`mcp.rs` などの呼び出し側を見落とさないため。checklist §2-3、#677）。

**完了条件**: 上の確かめ方がすべて通り、`rest.rs` が親の役割（共通部品と組み立て）だけになる。

**結果（2026-10-20 完了）**: `rest.rs` は 18,904 行から 1,249 行（モジュール文書・`use` と re-export・共通の認証部品・組み立て）になった。子モジュールは `rest/` に 17 個。テストは `rest/tests.rs`（`mod` 宣言だけ）の下の `rest/tests/` に 15 個（共通の補助関数は `common.rs`）。どの段もルート一覧と OpenAPI のスナップショットは再生成なしで一致し、テストの件数（727）と名前の集合は変わっていない（#721 から始まる一連の PR）。

### 8.2 P0-2: プロトコル名の分岐の棚卸し

2026-10-19 に調べた結果を付録 A に載せた（これで P0-2 は完了とする）。R1・R2b の各 PR は、着手時にこの表を `grep` で再確認し、自分が消す・置き換える箇所を PR の説明に書く。

### 8.3 P0-3: Modbus / SLMP の振る舞いを固定するテスト

**目的**: R2b-3 で I/O を一本化するとき、今のクライアントが事故で学んだ振る舞いを落とさないための判定基準を作る。

**今あるテスト**（足さなくてよい）: 応答タイムアウトで接続を壊す、途中切断、再接続とバックオフ、致命 / 非致命の分類、#337 の 1 接続 = 1 ソケット（Hub 経由）、ワード順（#334）、文字列のエンコーディング、ビットアドレス。場所は付録 A の調査結果のとおり（例: `crates/banto-plc/src/modbus/integration_tests.rs:173`、`crates/banto-plc/src/slmp/integration_tests.rs:554`、`apps/banto-hub/core/tests/integration.rs:1155`）。

**足すもの**:

| #   | 振る舞い                                    | 対象            | やり方                                                                                                                                                                                                                   |
| --- | ------------------------------------------- | --------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| 1   | 応答が分割して届いても 1 つの応答として読む | Modbus          | `crates/banto-plc/src/modbus/simulator.rs` に「応答を N バイトずつ間を空けて送る」モードを足し、読み取りが正しい値を返すことを確かめる                                                                                   |
| 2   | 遅れて届いた古い応答を次の要求に使わない    | Modbus          | シミュレータに「タイムアウト後に前の応答を送る」と「トランザクション ID の違う応答を送る」モードを足す。前者は次の要求が正しい値か致命エラーになること、後者は致命エラー（`error.rs:88`）になることを確かめる            |
| 3   | 閉じたポートへの接続                        | Modbus          | SLMP の `slmp/integration_tests.rs:624` と同じ形で足す                                                                                                                                                                   |
| 4   | 型の混ざった一括読み取り                    | Modbus          | SLMP の `read_batch_mixed_*`（`slmp/integration_tests.rs:835-1106`）に相当するものを足す（文字列は今どおり Bad になることも固定する）                                                                                    |
| 5   | 1 接続 = 1 ソケット（収集単体）             | `banto-collect` | Hub 経由のテストはあるが収集単体にはないので、`crates/banto-collect/tests/integration.rs` に足す                                                                                                                         |
| 6   | 応答が分割して届いても 1 つの応答として読む | SLMP            | 2026-10-19 オーナー決定で追加（slmp v0.2.1 で通るようになった）。`crates/banto-plc/src/slmp/simulator.rs` に Modbus の 1 と同じ「応答を N バイトずつ間を空けて送る」モードを足し、読み取りが正しい値を返すことを確かめる |

**SLMP の分割受信について（重要。2026-10-19 にコードで確認）**: 外部の `tyaro/slmp`（v0.2.0、`src/lib.rs:261`）は応答を 1 回の `read()` で受け取り、`validate_response` が宣言長と受信長を照合する（`:289-292`）。TCP で応答が分割して届くと、値を取り違えることはないが `LengthMismatch`（Framing）になり、致命エラーとして接続を切って再接続する。つまり**誤った値は出ないが、分割が起きる環境では不要な切断と Bad が起きうる**（実機で起きているかは未確認）。古い応答の流用については、応答タイムアウトで接続を壊す（既存テストあり）ので、遅れた応答が次の要求に使われる経路は今は無い（応答の照合は固定の `serial_id` だけなので、接続を壊さない形に変えると危なくなる）。P0-3 では次のようにする:

- （当初）SLMP について「分割して届いても正しく読む」テストは、v0.2.0 では失敗する見込みなので**足さない**としていた。v0.2.1 で通るようになったので、上の表の 6 として足す（2026-10-19 オーナー決定）（失敗するテストを入れて CI を赤くしない、無効化したテストも入れない）。
- 代わりに、この件をオーナーに報告した。→ **2026-10-19 オーナー決定: `tyaro/slmp` 側で直す**。v0.2.1（[tyaro/slmp#1](https://github.com/tyaro/slmp/pull/1)）で、13 バイトのヘッダを `read_exact` で読み、宣言された長さちょうどを読む形に直し、分割受信・ヘッダ途中での分割・途中切断・宣言長の異常・途中停止のテストを足した。本リポジトリは v0.2.1 を参照する（テスト用シミュレータの「壊れたフレーム」は長さを偽る形から応答コードを偽る形に変えた。0.2.1 は宣言長まで待つので、長さを偽るとタイムアウトになるため）。

**確かめ方**: `cargo test -p banto-plc -p banto-collect`、`cargo clippy -p banto-plc -p banto-collect --all-targets -- -D warnings`。シミュレータに足すモードは既存のテストの振る舞いを変えない（既定は今のまま）。

**完了条件**: 上の 6 つのテストが通り、付録 A のどの振る舞いがどのテストで固定されているかの対応表を本書に足す。

**完了（2026-10-20）**: 上の 6 つのテストを足した。シミュレータの新しいモードはすべて既定で OFF（`Simulator::set_response_chunking` は Modbus・SLMP の両方、`hold_next_response`・`set_wrong_transaction_id` は Modbus）で、既存のテストの振る舞いは変わらない。本番コードの変更はなく、テストが本物の不具合を見つけることもなかった。行番号は目安で、R2b の各 PR は着手時に再確認する。

**対応表（振る舞い → 固定しているテスト）**:

足した 6 件（`crates/banto-plc/src/` は略して `plc/`）:

| #   | 振る舞い                                                                             | テスト（ファイル:行）                                                                                                                                                                                                                                                    |
| --- | ------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| 1   | Modbus: 分割して届いた応答を 1 つの応答として読む                                    | `a_response_split_across_segments_is_read_as_one_response`（`plc/modbus/integration_tests.rs:323`。1・3・7・8 バイトずつ、各 2 回）                                                                                                                                      |
| 2a  | Modbus: 遅れて届いた古い応答を次の要求の値に使わない                                 | `a_late_response_after_a_timeout_is_never_returned_as_a_later_value`（同 `:376`、クライアント。次は致命の `NotConnected`、再接続後は新しい値）／`a_late_response_on_a_reused_stream_is_a_fatal_protocol_error`（同 `:408`、同じ stream を使い続けると致命の `Protocol`） |
| 2b  | Modbus: トランザクション ID の違う応答は致命エラー                                   | `a_response_with_the_wrong_transaction_id_is_fatal`（同 `:458`）                                                                                                                                                                                                         |
| 3   | Modbus: 閉じたポートへの接続は失敗し、ハングせず `NotConnected` のまま               | `connect_to_a_closed_port_fails_without_hanging`（同 `:485`）                                                                                                                                                                                                            |
| 4   | Modbus: 型の混ざった一括読み取り。文字列は今どおり `Bad`（`UnsupportedCombination`） | `read_batch_mixed_reads_numerics_and_keeps_strings_bad`（同 `:520`。`plan_batch_requests` + `execute_modbus_reads` を実ソケットで）                                                                                                                                      |
| 5   | 収集単体で 1 接続 = 1 ソケット                                                       | `collection_over_several_periods_uses_exactly_one_socket`（`crates/banto-collect/tests/integration.rs:1595`。値が Good で読めたうえで、数周期後も `connection_count() == 1`）                                                                                            |
| 6   | SLMP: 分割して届いた応答を 1 つの応答として読む（`slmp` v0.2.1）                     | `a_response_split_across_segments_is_read_as_one_response`（`plc/slmp/integration_tests.rs:1163`。1・5・13 バイトずつ、各 2 回）                                                                                                                                         |

既にあった振る舞い（付録 A の調査結果の範囲）:

| 振る舞い                                | テスト（ファイル:行）                                                                                                                                                                                                           |
| --------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 応答タイムアウトで接続を壊す            | Modbus `plc/modbus/integration_tests.rs:173`、SLMP `plc/slmp/integration_tests.rs:554`                                                                                                                                          |
| 途中切断で接続を壊す                    | Modbus `plc/modbus/integration_tests.rs:194`、SLMP `plc/slmp/integration_tests.rs:583`                                                                                                                                          |
| 再接続（クライアント単体）              | Modbus `plc/modbus/integration_tests.rs:230`、SLMP `plc/slmp/integration_tests.rs:651`・`:678`                                                                                                                                  |
| 再接続と値の復帰（収集）                | `crates/banto-collect/tests/integration.rs:646`（Modbus、切断中は NULL 行）・`:734`（Modbus、復帰）・`:867`・`:956`（SLMP 同様）                                                                                                |
| バックオフ                              | `crates/banto-collect/src/task.rs:1273`・`:1278`・`:1287`・`:1299`、`crates/banto-broker/src/lib.rs:1437`・`:1442`・`:1452`                                                                                                     |
| 致命 / 非致命の分類                     | `plc/error.rs:166`・`:199`、SLMP `plc/slmp/mod.rs:847`・`:891`、SLMP `plc/slmp/integration_tests.rs:384`（終了コードは非致命）・`:456`（壊れたフレームは致命）、Modbus `plc/modbus/integration_tests.rs:97`（例外応答は非致命） |
| #337 の 1 接続 = 1 ソケット（Hub 経由） | `apps/banto-hub/core/tests/integration.rs:1155`                                                                                                                                                                                 |
| ワード順（#334）                        | `plc/decode.rs:215`〜`:301`、SLMP `plc/slmp/integration_tests.rs:139`、収集の経路 `crates/banto-collect/tests/integration.rs:386`、`crates/banto-collect/src/task.rs:2013`〜`:2031`                                             |
| 文字列のエンコーディング                | `plc/decode.rs:529`〜`:589`、SLMP `plc/slmp/integration_tests.rs:1015`（UTF-8）・`:1057`（同じバイト列を Shift-JIS で読むと別の文字列）                                                                                         |
| ビットアドレス                          | Modbus `plc/modbus/integration_tests.rs:42`（コイル・ディスクリート入力）、SLMP `plc/slmp/integration_tests.rs:87`・`:113`（ワード内ビット）、`plc/decode.rs:389`〜`:418`、収集 `crates/banto-collect/tests/integration.rs:533` |

## 付録 A. プロトコル名の分岐の棚卸し（2026-10-19、P0-2）

調査時点の main（`006c4e7` 前後）。行番号は目安で、着手時に再確認する。分類: (a) 許可リスト・検証、(b) SQL・マイグレーション、(c) クライアントの組み立て、(d) アドレスの解釈、(e) UI の分岐、(f) 設定パッケージ、(g) シミュレーション、(i) その他。テストだけの箇所は除く。

**Rust**:

| ファイル                                                            | 箇所数 | 分類       | 主な場所                                                                                                        |
| ------------------------------------------------------------------- | ------ | ---------- | --------------------------------------------------------------------------------------------------------------- |
| `crates/banto-tags/src/plc_connection.rs`                           | 約 30  | a, i       | `ALLOWED_PROTOCOLS` 236、定数 298-317、`HUB_PROTOCOL` の分岐 453/481/536/652、許可リストの照合 801              |
| `crates/banto-tags/src/tag.rs`・`collection_group.rs`               | 約 11  | a          | `HUB_PROTOCOL` 896/1088/1133/1217、collection_group 29/251                                                      |
| `crates/banto-tags/migrations/*.sql`                                | 約 20  | b          | `CHECK (protocol IN ...)` 0004:74・0007:41・0014:49・0019:61、hub の部分一意索引 0019:100、0020 のトリガー      |
| `crates/banto-broker/src/lib.rs`                                    | 6      | a, c       | 定数 238/243、`DRIVERS` 750-752、`is_supported_protocol` 798、検索 841                                          |
| `crates/banto-collect/src/config.rs`                                | 約 30  | a, c, d, g | 文字列 → `Protocol` 1295/1586、`Protocol` → `ProtocolConfig` 1252、`Address::parse` 1470、`External` 396ほか    |
| `crates/banto-collect/src/{task,collector,simulation}.rs`           | 約 20  | c, g       | クライアントの組み立て task 348-398・collector 821-900、シミュレータの起動 simulation 198                       |
| `crates/banto-plc/src/address.rs` ほか planning                     | 約 13  | d          | `Address::ModbusRef` / `Address::Slmp` 217-293                                                                  |
| `apps/banto-hub/core/src/rest.rs`                                   | 8      | a, c, g    | `reject_hub_connection_protocol` 2585、接続テストの振り分け 4085-4087、書き込み可否 4519、シミュレーション 7341 |
| `apps/banto-hub/core/src/{hub,broker_glue,read_path,write_path}.rs` | 約 16  | a, c, d, g | `broker_glue.rs` 716-739（`Protocol::`）・841-864（`ClientProtocol::`）                                         |
| `apps/banto-hub/core/src/mcp.rs`                                    | 3      | a          | JSON スキーマの enum 596/663/722（中身がそろっていない: 2 つは `postgres` を含み 1 つは `virtual` を含む）      |
| `apps/banto-hub/core/src/{runtime,db_source,sink}`                  | 約 6   | a, i       | `VIRTUAL_PROTOCOL`、`POSTGRES_PROTOCOL`                                                                         |
| `apps/chronogazer/core`・`apps/chronogazer/src-tauri`               | 約 12  | a, i       | `HUB_PROTOCOL` の分岐（hub_tags）、`ALLOWED = ["modbus-tcp","slmp","hub"]`（src-tauri lib.rs:2279）             |

合計（目安）: (a) 約 45、(b) 約 20、(c) 約 20、(d) 約 25、(g) 約 10、(i) 約 20。

**TypeScript / Svelte**（banto-hub UI が中心）: `tagRegistryAdmin.ts`（型と判定関数）、`plcConnectionForm.ts`（選択肢・既定ポート・既定ワード順・postgres の検証）、`tagAddressHelp.ts`（プロトコル別のアドレス説明）、`configPackage.ts`・`configPackageAdmin.ts`（設定パッケージ）、`ConnectionDrawer.svelte`（約 10 分岐）、`tags/+page.svelte`。ChronoGazer の UI にも同種の分岐がある。合計（目安）: (e) 約 70、(a) 約 10、(f) 約 8。

**気づいた点**: `mcp.rs` の 3 つの JSON スキーマで許すプロトコルの列挙がそろっていない（596・663 は `postgres` を含み、722 は `virtual` を含む）。R1 でレジストリから生成するまでの間、意図どおりか確認する。
