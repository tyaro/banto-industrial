# banto-hub ドライバプラグイン設計（WASM）

作成日: 2026-10-19
状態: **段階的な限定実装中（2026-10-25。2026-11-07: §10 の H2 の REST（7.）の切り離したタスクを、Hub 共有の数え（`crate::detached`）へ移した。2026-10-30: §5 の「シミュレーション」と §11 の Q9 を、R2b-5 で置き換わった形に直した（WASM の接続も値の層のシミュレータに乗り、実機へダイヤルしない。試験は #695 の `t12_wasm_all_simulation`）。同日: §11 の Q1 をオーナー決定で閉じた（KEYENCE KV は WASM のドライバで進める。当分は実機が無いので模擬だけで受け入れる）。2026-10-29（#914 の C1 の Hub 側）に WASM アダプタが文字列タグ（`string-words` / `value::bytes`）の読取りと Modbus・SLMP の運用書込みゲートを受け付けた。同日、ゲストの C1（banto-driver-wasm #63）と SLMP の E4（#62、UDP。#916）を固定 `f6c8781`（SLMP・Modbus とも。Modbus の manifest は 0.9.0）で component と t12 の模擬受入れまで済ませた（実機は未確認）。同日、SLMP の G1（#65、U\G バッファメモリの読取り。#917）も SLMP の固定を `1fac51a` に上げ、WASM アダプタの文字列タグが `U<h>\G` の領域を受けるようにして、component と t12 の模擬受入れまで済ませた（読取り専用。実機は未確認）。2026-11-06 に SLMP の G2a（#69、Q のプロセス CPU QnPH・QnUDPV・MELSEC-L・Q12PRH を 0101 の型名と型式コードの組で判別する）も SLMP の固定を `c193376` に上げ、component と t12 の模擬受入れまで済ませた（WIT・Hub の製品コードは不変。実機は未確認）。同日、ゲストの #71（ZR の番号を Q・R とも 10 進にする）に合わせて SLMP の固定を `abb1a1a` に上げ、component と t12 の ZR の試験を 10 進の前提に直した（WIT・Hub の製品コードは不変。実機は未確認）。同日、SLMP の G2b（#72、iQ-R のプロセス CPU・SIL2 プロセス CPU・安全 CPU・MX-R を 0101 の型名と型式コードの組で判別する）に合わせて SLMP の固定を `b705fd2` に上げ、component と t12 の模擬受入れまで済ませた（WIT・Hub の製品コードは不変。実機は未確認）。同日、SLMP の G2c（#73、FX5 を 0101 の型式コードの表と `FX5` の型名で判別し、3E・自局だけで読書きする。読取り専用のファイルレジスタ `R<n>` を足す）に合わせて SLMP の固定を `aa3a1c1` に上げ、component と t12 の模擬受入れまで済ませた（WIT・Hub の製品コードは不変。実機は未確認）。同日、SLMP の G3（#74、GX Works3 のグローバルラベル `@名前` を 041C で読み、041C の型の確認と 141B で書く）に合わせて SLMP の固定を `fc0bd10` に上げ、component と t12 の模擬受入れまで済ませた。今回は Hub の製品コードも変えた: WASM アダプタの文字列タグが area `@` を受け、運用書込みゲートと保存済みポリシーにゲストと同じ文法の `label_write_allowlist` を足した（`d_u16_write_mode` は共用で、`enabled` には D/W かラベルのどちらかの許可リストが要る。WIT は不変。実機は未確認）。2026-11-07 に SLMP の H（#76、PLC の時計合わせ）の前半（H1）として、SLMP の固定を `ea4d84c` に上げ、任意の WIT `clock-v1` を driver-api・engine・broker・WASM アダプタまで通し、component の模擬受入れまで済ませた（時計合わせを出すのは運用書込みの loader で読み込んだゲストだけ。Hub の接続ごとの設定・時刻表・REST・監査は H2 で行う。実機は未確認）。同日、H2 として Hub の接続ごとの時計合わせの設定（`hub_clock_sync`）・時刻表のスケジューラ・REST `GET`／`PUT`／`POST /api/plc-connections/{id}/clock-sync`・監査・MCP の `get_clock_sync`（見るだけ）を足し、t12 の模擬受入れまで済ませた（UI は無し。実機は未確認）。同日、H3（PLC の時計の読取り、ゲスト #78）として SLMP の固定を `5281578` に上げ、任意の WIT `clock-read-v1` を driver-api・engine・broker・WASM アダプタまで通し（loader を問わず出す。PLC に何も書かない）、Hub に REST `GET /api/plc-connections/{id}/clock`（5 秒の使い回し）・MCP の `read_plc_clock`・時計合わせの前後の読取りの監査を足し、component と t12 の模擬受入れまで済ませた（UI は無し。実機は未確認）。2026-10-29（#915）に Modbus WASM の Hub の運用書込みゲートで連続コイルの FC15 と `register_write=fc6` の FC6 を受け付けた、模擬のみ。2026-10-28（#899）に FC22 の無応答の失敗文言へ「FC22 非対応の可能性」のヒントを足した（動きは変えない）。2026-10-28 に Modbus WASM の結合試験の固定版を FC22 の書込みへ更新、模擬のみ。2026-10-28（#902）に WIT の `item-error` へ `verification-failed` を足し、ホストの WIT とゲストの固定を guest main `3b7f0f6` に揃えた（今のゲストはこの値を返さない）。2026-10-29（#916）に Q6 を「UDP を対象にする」と決め、manifest の `transports` に `udp` を認めた（§4.2・§11。Hub 側のみ実装、UDP のゲストは未実装））**。2026-10-19 の実装保留判断の後、受け皿と P0/P1 の一部（U16/U32 の明示登録、broker 収集・即時読取り）を実装した。非 Modbus アドレスの実 component 試験を追加した。通常起動の明示選択と署名再検証は限定実装中。実運用の信頼鍵・保護配置の設定、導入・更新は未完了。本書全体の承認を意味せず、§2 の D1〜D6 は当初の方向・§11 は未決事項として扱う。着手する場合も一括移行はせず、組み込みドライバとの併用期間を経て判断する。§2 の D1〜D6 は 2026-10-19 のオーナーとの議論で出た方向で、**本書の承認をもって決定**とする（承認後に日付付きの決定記録へ移す）。§11 は未決事項、§12 は破壊的変更を許す場合の構成案（2026-10-19 検討・未決定。ただし外部 DB の取り込み口 `push` の形は external-db-design §9.5 でオーナー決定）（Q11 は 2026-10-19 オーナー決定で解決済み）。2026-10-19 に上位モデルの監査を受け、指摘（現状記述の誤り・broker 構造・WIT の読み取り単位・同梱 UI と展開経路の安全・レジストリ変更）を反映済み。同日の Codex レビュー（ハンドシェイクを応答依存で進める・有効化前の preflight・fingerprint に内容のダイジェスト・同梱 UI の配信の認証）と、オーナーのレビュー（プラグイン固有のアドレスを broker まで運ぶ経路 §3.2、ハンドシェイクの拒否応答）も反映済み。同日、§6.2 の WIT を R1 の詳細設計 [banto-driver-api-design.md](banto-driver-api-design.md) §9.2 に合わせて改訂し、§12.6 に「R1 の時点の CI の姿」を補足した。
対象: PLC ドライバ（メーカー固有のプロトコルと登録 UI）を、banto-hub 本体を再ビルド・再リリースせずに追加・更新できるようにする。
関連: [tag-server-design.md](tag-server-design.md) §3.4（PLC 通信ドライバとの関係）・§3.4.1（新しい PLC ドライバの置き場所、#438）・§4（タグ空間のセマンティクス）・§6（書き込み）、
[banto-hub-t17-design.md](banto-hub-t17-design.md)（サービス・profile・UAC）、
[banto-hub-redundancy-design.md](banto-hub-redundancy-design.md)、
[h9-slmp-structured-error-spec.md](h9-slmp-structured-error-spec.md)（エラーの構造化）、
[banto-hub-external-db-design.md](banto-hub-external-db-design.md)（Source のプロセス内実装の前例）。

---

## 1. 背景と課題

### 1.1 現状

PLC ドライバは banto-hub のバイナリにコンパイル時に同梱される Rust クレートである（tag-server-design.md §3.4）。

- ワイヤ層: `banto-plc` の `PlcClient` trait（`connect` / `read_batch` / `disconnect`）。読み取り単位は `ReadRequest { address, data_type }` と `StringReadRequest { address, words, encoding }`（`crates/banto-plc/src/types.rs`）。実装は Modbus TCP（`crates/banto-plc/src/modbus/`）と SLMP（`tyaro/slmp` をタグ参照）。
- 種別の選択: `banto-collect` の公開 enum `Protocol`（`ModbusTcp` / `Slmp`）と `ClientProtocol`。banto-hub はこれらを網羅的に `match` しているため、種別を enum に足すと Hub 側にも影響が出る（`crates/banto-collect/src/config.rs`）。内部の `ProtocolConfig` は `pub(crate)` で、`hub` 接続用の `External` も持つ。
- **Hub の読み取りは broker のセッションに相乗りする**。1 接続 = 1 ソケットで、`hub_client_factory` は SLMP / Modbus とも `BrokerReadClient` に流す（`apps/banto-hub/core/src/broker_glue.rs`）。これは 1 対 1 接続しか許さない機器で収集が全滅した #337 の対策（tag-server-design.md §6）。broker のセッションは `CollectorManager` の外で生き続ける。
- broker のドライバ境界は `BrokerSession`（`read_batch` / `write_batch` / `disconnect`、`Err = 接続致命`。`crates/banto-broker/src/session.rs`）。
- 再接続・バックオフ・品質判定: `banto-collect`。書き込み: `banto-plc-write`。書き込みの関門: `apps/banto-hub/core/src/write_path.rs`。
- 書き込み可能な領域: `banto-plc-address`（プロトコルの事実としての `is_writable`）。
- シミュレーション: `BrokerSimRegistry::resolve`（`broker_glue.rs`）は `protocol == "modbus-tcp"` 以外をすべて SLMP シミュレータに倒す。シミュレーション中の書き込みは拒否せずシミュレータへ反映する（#363、2026-09-15 オーナー決定）。
- レジストリ: `plc_connections.protocol` は SQL の `CHECK (protocol IN ('modbus-tcp','slmp','virtual','postgres','hub'))`（migration 0019）と `ALLOWED_PROTOCOLS`（`crates/banto-tags/src/plc_connection.rs`）で固定。接続設定は固定列で、`postgres` の `password` は平文で保存している。
- UI: `apps/banto-hub/src` にプロトコルごとの分岐（`ConnectionDrawer.svelte` の `form.protocol === 'slmp'` 等）と、Rust 側を**手で書き写した**アドレス表（`slmpDeviceTable.ts`。正本は `crates/banto-plc/src/slmp/address.rs`）を持つ。設定パッケージの検証も `protocol` を固定値で見る（`configPackage.ts`）。

### 1.2 課題

1. **ドライバの追加・更新のたびに Hub 全体の再ビルドと再リリースが要る**。PLC メーカーが違えばプロトコルも登録 UI も個別に作る必要があり、対応メーカーを増やすほど本体のリリースがドライバに引きずられる。
2. **UI のアドレス処理が Rust と TS の二重管理**。T20-④（bit サフィックスの基数を10進→16進へ是正、2026-09-04）では両方を直した。
3. §3.4.1（2026-10-18、#438）でワイヤ層は別リポジトリに出す方針になり CI の重さは軽減されるが、**配布単位は Hub のまま**で 1 は解消しない。

## 2. 方向（2026-10-19 オーナーとの議論。本書の承認で決定）

検討した選択肢:

| 方式                                    | 本体の再ビルド不要 | サービス運用（LocalSystem）での安全性                                                                                                                                           | 障害の分離                       | 書き込みの安全契約                         | 判定                                             |
| --------------------------------------- | ------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------- | ------------------------------------------ | ------------------------------------------------ |
| 静的リンク（現状）                      | ×                  | ◎                                                                                                                                                                               | ×（同一プロセス）                | ◎（broker・write path がそのまま）         | 主力ドライバは当面このまま                       |
| 別プロセス（kt_iot_hub 型: exe + gRPC） | ○                  | △（子プロセスが LocalSystem を継承する。サービス化した場合は登録 UI を Session 0 から出せない。localhost gRPC は認証なし - kt_iot_hub 自身のレビューも指摘、Named Pipe 案あり） | ◎                                | △（IPC 越しに作り直し）                    | x86/COM 等のベンダー部品に限る（本書対象外、Q8） |
| DLL（動的リンク）                       | ○                  | ×（DLL planting の典型的な攻撃面）                                                                                                                                              | ×                                | ○                                          | 不採用（Rust に安定 ABI なし、async が越えない） |
| **WASM（wasmtime Component Model）**    | ○                  | ◎（サンドボックス。ファイル・ネットワークに触れない。ただし置き場所の書き込み経路は DLL と同じ厳しさで守る - §7）                                                               | ○（trap で捕捉、Hub は落ちない） | ◎（broker・write path・関門は Hub に残る） | **採用方向**                                     |

kt_iot_hub はデスクトップアプリ（利用者の権限で動く）なので別プロセス方式が素直に成り立つ。banto-hub はサービス（LocalSystem）で動くため、同じ形をそのまま持ち込めない。

方向:

- **D1**: 本体と別に追加・更新したいプロトコルドライバは **WASM プラグイン**にする。
- **D2**: PLC メーカーごとに登録 UI が異なるため、**プラグインはメーカー固有の Web UI を同梱する**ことを標準とする（汎用フォームだけでは足りない）。
- **D3**: ただしアドレス関数・設定検証は **WASM 側に必須**とする。REST・MCP・タグ CSV 取り込み・設定パッケージ・未適用キューの適用は iframe の UI を通らないため、サーバー側で同じ検証が要る。
- **D4**: Hub はプラグインを**動的に発見・導入・更新**し、Hub の再起動を要しない。
- **D5**: 書き込みの安全判断（タグの `writable`・API キーの `write` スコープ・受付トグル・頻度制限・監査・broker の直列化。`write_path.rs` のゲート群）は **Hub に残す**。プラグインが宣言する「書き込み可能な領域」はプロトコルの事実であって安全判断ではない（`banto-plc-address` の `is_writable` と同じ位置づけ）。
- **D6**: プラグインはフレームを組み立てる側なので Hub はフレームの意味までは検証できない。これは静的リンクのクレートを信頼するのと同種の信頼であり、**出所の保証（署名・保護された置き場所・プラグイン側リポジトリのレビューと CI）**で担保する。

### 2.1 進め方（2026-10-19 オーナー決定）

- **2026-10-19 当時の方針**: 本書は計画として止め、実装は保留する（他の作業を優先するため）。着手時期は未定で、着手するまでは tag-server-design.md §3.4 / §3.4.1（組み込み・ワイヤ層は別リポジトリ）が正。2026-10-25 の限定着手については下記を参照。
- **着手する場合も、いきなりすべてを移さない**。組み込みドライバ（SLMP / Modbus TCP）は残したまま、プラグインの経路を**併用**で導入し、実運用で使えると判断してから移行する。移行の判断（Q3）は併用期間の結果を見て行う。
- **2026-10-19 追加（オーナー決定）**: ドライバは 100 種類規模（シリアルを含む）を見込み、OSS として他者がドライバを作りやすくするため、**WASM の受け皿（WIT と同じ形の trait・`DriverRegistry`・不透明なアドレス・設定の JSON・接続ごとの実装の選択・iframe の枠・適合テスト）だけを先に作り、WASM 本体は後回し**にする。計画は [banto-hub-driver-refactor-plan.md](banto-hub-driver-refactor-plan.md)（§12 の R1〜R2 を具体化）。
- **2026-10-25 限定着手（オーナー承認）**: 上記の保留後、明示登録された U16/U32 の読み取りをローカル試験と CI で確認した。本 PR は P1 のアドレス境界を独自文法の実 component で確かめる。通常起動登録・署名付き配置・native 置換・WIT 拡張・実機通信はこの承認に含めない。§7 の導入方式と §11 の未決事項を一括承認済みとは扱わない。

## 3. 全体構成

```
┌──────────────────────── banto-hub（サービス, LocalSystem） ────────────────────────┐
│ 収集（banto-collect）→ BrokerReadClient → banto-broker（1 接続 = 1 セッション）       │
│                                            └ WasmSession（BrokerSession 実装）       │
│                                               ソケット / タイムアウト / 再接続は Hub  │
│ 品質判定・スケーリング・書き込みの関門・監査（write_path.rs）                        │
│                                                                                   │
│ Plugin Manager: 発見・署名検証・展開・コンパイル・有効/無効・更新/ロールバック      │
│   ├─ wasmtime Engine ── driver.wasm（サンドボックス、WASI なし）                    │
│   │                      フレーム組み立て / 応答解析 / アドレス関数 / 設定検証       │
│   └─ 静的配信 /plugins/<id>/<ver>/ui/ ── iframe（sandbox + 応答 CSP sandbox）        │
│                                          postMessage ブリッジ（範囲を絞った API）   │
└───────────────────────────────────────────────────────────────────────────────────┘
```

| 層            | 担当                                                                                                  |
| ------------- | ----------------------------------------------------------------------------------------------------- |
| Hub（不変）   | 収集・品質・スケーリング・broker・書き込みの関門・監査・トランスポート I/O・プラグイン管理・UI の隔離 |
| `driver.wasm` | フレームの組み立てと解析・アドレス関数・設定の検証（純粋な計算のみ）                                  |
| `ui/`（同梱） | メーカー固有の接続設定・タグ登録画面                                                                  |

### 3.1 broker への組み込み

- Hub では WASM ドライバを **`banto-broker` の `BrokerSession` 実装（`WasmSession`）** として P1 から入れる。読み取りも書き込みも同じセッションに相乗りし、#337 の「1 接続 = 1 ソケット」を崩さない。
- `banto-collect` 直結の経路（`ClientProtocol` / `default_client_factory`）は ChronoGazer 用で、P5 で扱う。
- プラグインの更新時に作り直す単位は「**broker セッション + そのプラグインを使う接続の収集タスク**」。他の接続には触れない。

### 3.2 プラグイン固有のアドレスを broker まで運ぶ経路

`WasmSession` を足すだけでは、プラグイン独自のアドレス文法が broker に届かない。今の要求の経路は組み込みの 2 プロトコルに閉じている:

- `banto_plc::Address` は `ModbusRef` と `Slmp` の 2 つだけの enum（`Copy`）で、`BatchReadRequest` / `BatchWriteRequest` はこれを運ぶ。
- 文字列のアドレスを `Address` に変換する箇所は、`protocol == "modbus-tcp"` 以外を SLMP として解析する（`write_path.rs` の 2 か所、`read_path.rs`、`banto-collect` の `config.rs` の `build_request`、`rest.rs` の即時読み取り）。プラグインの ID の接続がここに来ると、SLMP の文法で解析されて断られるか、別のアドレスとして解釈される。

そのため P1 で次を行う:

- broker の要求が運ぶアドレスを、組み込みの `Address` と「プラグイン ID + プラグインが `address.parse` で正規化した文字列（`canonical`）」のどちらかを持つ型に広げる。`Address` の `Copy` を崩さないよう、`Address` 自体には足さず、要求の層で包む。
- 上の変換箇所をすべて `protocol` の明示的な `match` にし、**組み込みでもプラグインでもない ID は断る**（既定で SLMP に倒さない）。
- 読み取り（収集・即時読み取り）と書き込み（単発・レシピ）の両方の経路を同じ型で通す。`WasmSession` はプラグインのアドレスだけを受け、`tag-spec.address` に `canonical` を渡す。

## 4. パッケージとマニフェスト

### 4.1 パッケージ形式

1 ファイル `<id>-<version>.bantoplugin`（zip）:

```
manifest.json   ┐
driver.wasm     ├ 署名の対象（エントリ名と内容の正規化ダイジェスト、§7.1）
ui/...          ┘
signature       上記に対する Ed25519 署名（自身は対象外）
```

### 4.2 manifest.json（v1）

| 項目              | 必須 | 内容                                                                        |
| ----------------- | ---- | --------------------------------------------------------------------------- |
| `manifestVersion` | ○    | `1`                                                                         |
| `id`              | ○    | プロトコル ID。`[a-z0-9][a-z0-9-]*`。レジストリの `protocol` 列に入る（§5） |
| `version`         | ○    | SemVer                                                                      |
| `hubApi`          | ○    | 対応する WIT インターフェースのメジャー版（例: `"1"`）                      |
| `displayName`     | ○    | UI 表示名（例: `KEYENCE KV ホストリンク`）                                  |
| `vendor`          |      | 作成者                                                                      |
| `transports`      | ○    | `tcp` / `udp` の空でない重複のない部分集合（#916。シリアルは対象外、Q6）    |
| `capabilities`    | ○    | `read` / `write`。world の選択（§6.4）と一致させる                          |
| `ui`              |      | 同梱 UI のエントリ（例: `ui/index.html`）                                   |

`transports` は Hub がそのまま descriptor に写し、broker は接続の設定 `transport`（既定 `tcp`）がそこに含まれるときだけ繋ぐ。`udp` を足しても `hubApi`・`manifestVersion` と WIT は変えない（ゲストはトランスポートを知らない）。`tcp` だけの既存のゲストはそのまま動く。UDP の契約（1 データグラム = 1 応答・再送しない・1 往復以上のハンドシェイクが必須）は [io-design §7.6](banto-hub-driver-io-design.md)。

**組み込みの ID（`modbus-tcp` / `slmp` / `postgres` / `hub` / `virtual` 等）はプラグインで上書きできない**。同じ ID のプラグインは導入時に拒否する。

## 5. レジストリと既存機能の変更

- **`protocol` 列**: SQL の `CHECK` と `ALLOWED_PROTOCOLS` を緩め、組み込みの ID に加えて「導入済みプラグインの ID」を受け付ける。SQL 側は `CHECK` を外し（migration）、検証は `banto-tags` の Rust 側に一本化する。
- **プラグイン固有の設定**: `plc_connections.settings`（JSON）列を足す。共通の列（`host` / `port` / `word_order` / タイムアウト等）は従来どおり固定列。`word_order` はプラグインへ設定として渡す（§6.2）。
- **秘密値**: `config.secret-keys` が返すキーの値は、平文の列には置かず `banto-secret-store`（DPAPI の machine スコープ）に保存する。REST・MCP・UI・設定パッケージ・監査ログでは伏せる（既存の `passwordSet` と同じ見せ方）。既存の `postgres` の平文保存の扱いは本書の範囲外。
- **検証の注入**: `banto-tags` は `banto-plc` に依存しない方針（`banto-plc-address` の lib.rs 参照）を守り、`PlcConnectionService` / タグの検証にプラグイン検証のコールバックを渡す形にする。
- **設定パッケージ**: `protocol` の許容値を導入済みプラグインに広げ、必要なプラグインの `(id, version)` を記録する。取り込み先に足りないプラグインがあれば、**取り込み前に一覧で断る**（取り込みは 1 トランザクションの全件検証なので、部分取り込みはしない）。
- **未適用キュー**: 適用時に、プラグインの有無と版を再検証する（キューに積んだ時点と版が変わっていれば検証し直す）。
- **冗長化**: 設定世代の fingerprint に「必要プラグインの `(id, version, 内容のダイジェスト, 署名鍵の ID)` の集合」を含め、両系で揃っていることを確かめる。`(id, version)` だけでは、同じ版番号で中身の違うパッケージを見分けられない（版の不変性は §7.1 でも守るが、fingerprint でも確かめる）。設定パッケージに記録する必要プラグインも同じ組にする。
- **シミュレーション**: ~~v1 ではプラグイン接続の `simulation = true` を**項目エラーで断る**（`virtual` と同じ扱い）。`BrokerSimRegistry::resolve` の既定が SLMP シミュレータに倒れるため、そのままではプラグインのフレームを SLMP シミュレータへ投げてしまう。全シミュレーション運転時は、プラグイン接続は起動しない。プラグイン側にシミュレータ（応答を組み立てる側）を持たせるかは Q9。~~ → **R2b-5（2026-10-24、[banto-hub-driver-sim-design.md](banto-hub-driver-sim-design.md)）で置き換わった**: シミュレーションの対象はレジストリの Frame / Session のドライバ全部になり、WASM のドライバ（`FrameDriver`）も値の層のシミュレータ（`SimDevice`）に乗る。`BrokerSimRegistry::resolve` のプロトコル別の分岐は無くなった。保存値 `simulation = true` の接続も、全シミュレーション運転で上書きされた接続も、実機へダイヤルしない。これを WASM の接続で固定する試験は `apps/banto-hub/host/tests/t12_wasm_all_simulation/`（#695、2026-10-30）。収集が止まっている間は、起動のときも登録のあと（`commit_catalog_and_apply_live`）も catalog を入れ替えるだけで、どの接続へもダイヤルしない（試験は登録の直後から止めたあとまで 0 件を確かめる）。
- **接続テスト**: 保存前の疎通確認（`POST /api/plc-connections/test` 相当）は、`handshake-next` を完了まで進められるかで判定する。

## 6. WASM インターフェース（WIT の叩き台）

### 6.1 原則

- **I/O は Hub が持つ**。プラグインはソケットに触れず、「送るバイト列」と「受け取ったバイト列の解釈」だけを担う。WASI は与えない（ファイル・ネットワーク・時刻・乱数なし）。
- **対象外: OPC UA クライアントのドライバ**（2026-10-19 オーナー確認）。セキュアチャネル（証明書・署名・暗号化）・セッション・サブスクリプションの状態を持ち、「I/O は Hub、プラグインはフレームの組み立てと解析だけ」の形に合わない。プラグイン化を進める場合も、OPC UA クライアントは組み込み（ワイヤ層は別リポジトリ、tag-server-design.md §3.4.1）のまま残す。
- 要求-応答型のプロトコル（SLMP / Modbus / KV ホストリンク / FINS 等）を前提にする。接続時のハンドシェイク（FINS/TCP のノードアドレス交換、EtherNet/IP のセッション登録等）は、**応答を受け取りながら 1 往復ずつ進める**（`handshake-next`）。後続の通信が応答の値に依存するため、全ステップを先に作ることはできない。
- **エラーの既定は致命**（`PlcError::is_connection_fatal` と同じ）。プラグインが `item-error` と分類したものだけが非致命（そのタグだけ Bad）で、それ以外の失敗・trap・タイムアウト・上限超過はすべて接続致命として broker の `Err = 接続致命` の契約に写す。エラーは文言のパースで分類しない（h9 の教訓）。
- タイムアウト・未接続は Hub 側で起きるので WIT には出てこない。応答待ちのタイムアウトは従来どおり接続致命。
- **生値は Hub へ**: スケーリングは Hub 側（`banto-collect`）に残す。しきい値は Hub が持たない（#533、2026-10-08 オーナー決定。使う側の設定）。文字列はプラグインが `bytes` を返し、エンコーディング（SJIS / UTF-8）のデコードは Hub が従来どおり行う。

### 6.2 WIT

```wit
package banto:driver@1.0.0;

interface types {
  record setting { key: string, value: string }
  variant value { %bool(bool), int(s64), uint(u64), float(f64), bytes(list<u8>) }
  // 非致命（そのタグだけ Bad）。これ以外はすべて接続致命（§6.1）。
  // rejected = プラグイン自身が送る前に断った（同じワードの同じビットへ true / false が同時）。
  // verification-failed = 書込みを送ったあとの RMW の確認読みの不一致（機器には効いているかもしれない。2026-10-28、#902）。
  // どちらも機器の拒否（device）でも設定の不整合（unsupported）でもない非致命
  variant item-error { device(device-error), unsupported(string), rejected(string), verification-failed(string) }
  record device-error { code: u32, message: string }
  variant fatal-error { framing(string), protocol(string), invalid-config(string) }
  // 読み書きの単位。現行の ReadRequest / StringReadRequest に合わせる
  record tag-spec {
    address: string,
    // Hub のデータ型名。綴りは banto-plc の DataType の Display と同じ
    // （bit / i16 / u16 / i32 / u32 / f32 / i64 / u64 / f64）と "string"
    data-type: string,
    string-words: option<u32>,  // 文字列のときの語数
  }
}

interface session {
  use types.{setting, fatal-error};
  // 終端の決め方。none = 応答を待たない送信
  variant frame-rule {
    none,
    fixed(u32),
    length-prefixed(prefix-spec),
    terminator(list<u8>),
    plugin-decides,
  }
  // 長さ欄の読み方。SLMP の ASCII フレームは長さ欄が ASCII の 16 進
  enum prefix-encoding { binary, ascii-hex, ascii-dec }
  record prefix-spec { offset: u32, width: u8, big-endian: bool, adjust: s32, encoding: prefix-encoding }
  // Hub が実行する 1 往復。受信が max-len を超えたら接続致命
  record step { send: list<u8>, expect: frame-rule, max-len: u32 }
  // is-complete の答え。discard(n) = 先頭の n バイトはごみ（RS-485 の自己エコー・前の応答の残り）、捨てて続ける
  variant frame-check { incomplete, complete(u32), discard(u32) }

  resource connection {
    // settings には共通列（word_order 等）とプラグイン固有の設定の両方が入る
    create: static func(settings: list<setting>) -> result<connection, fatal-error>;
    // 接続直後のハンドシェイクを 1 往復ずつ進める。Hub は最初に none で呼び、
    // 以降は直前の step の応答を渡す。プラグインは応答から得た値（FINS のノード
    // アドレス、EtherNet/IP のセッションハンドル等）を connection に保持し、
    // 次の step を返す。none を返したら完了。応答が拒否（ノード不一致・
    // セッション登録失敗等）なら fatal-error を返し、接続成功とはみなさない。
    // fixed / length-prefixed の step でも応答は必ずここへ渡す。接続テストにも使う
    handshake-next: func(prev-response: option<list<u8>>) -> result<option<step>, fatal-error>;
    // 切断の直前に送る step（EtherNet/IP の UnRegisterSession 等）。応答は待たない。無ければ none
    farewell: func() -> option<step>;
    // plugin-decides のとき Hub が受信のたびに呼ぶ。step は今待っている応答の step
    is-complete: func(step: step, buf: list<u8>) -> result<frame-check, fatal-error>;
  }
}

interface read {
  use types.{tag-spec, value, item-error, fatal-error};
  use session.{connection, step};
  record planned { step: step, covers: list<u32> }   // covers = specs の添字
  // 読み取り計画（banto-plc の planning 相当）。ワード順は connection の設定で決まる
  plan: func(conn: borrow<connection>, specs: list<tag-spec>) -> result<list<planned>, fatal-error>;
  // specs は plan に渡したものと同じ。planned は step と covers しか持たないので、
  // 同じアドレス・同じフレームを u32 と f32 で読み分けるには specs[covers[i]].data-type が要る
  decode: func(conn: borrow<connection>, p: planned, specs: list<tag-spec>, response: list<u8>)
    -> result<list<result<value, item-error>>, fatal-error>;
}

interface write {
  use types.{tag-spec, value, item-error, fatal-error};
  use session.{connection, step};
  // value は Hub が bool / float / bytes の 3 つだけで渡す（int / uint は使わない）。
  // 整数型への丸めと範囲の検査はプラグイン側（banto-plc-write の encode と同じ規則）
  record write-item { spec: tag-spec, value: value }
  record planned-write { step: step, covers: list<u32> }   // covers = この step が関わる items の添字（ログ・監査用）
  variant write-progress { send(planned-write), done(list<result<_, item-error>>) }
  // 書き込みは状態機械（api-design §4.2.1、2026-10-19 レビュー指摘で encode / check から変更）。
  // 1 回の論理書き込みにつき、最初は prev-response = none、以降は直前の step の応答。items はその間ずっと同じ。
  // ワード内ビットの RMW（今の値を読む → 書き戻す → 確認読み）を、読んだ値に応じた書き戻しのフレームで表すため。
  // done までホストは同じ接続に他の要求を挟まない。step 数には上限があり、超えたら致命。
  write-next: func(conn: borrow<connection>, items: list<write-item>, prev-response: option<list<u8>>)
    -> result<write-progress, fatal-error>;
}

interface address {
  // D3: REST・MCP・CSV・設定パッケージ・UI が同じ関数を使う
  record parsed {
    canonical: string,
    area: string,
    writable-area: bool,      // プロトコルの事実（D5）。書き込み許可の判断には使わない
    bit: option<u8>,          // 記法はプラグインの文法に従う（SLMP なら .0〜.F の16進、T20-④）
    word-count: u32,
    index: option<u64>,       // area の中の位置（ワードの番号。SLMP の16進も数値に直す。ワード内ビットはそのワードの番号）。重なりの判定用（R2b-6、ui-design §3.6）
  }
  // string-words は data-type が "string" のときの語数（word-count を決めるために要る）
  parse: func(text: string, data-type: string, string-words: option<u32>) -> result<parsed, string>;
  // 連続登録・オフセットコピー用: n 個先のアドレス
  offset: func(canonical: string, data-type: string, string-words: option<u32>, n: s64) -> result<string, string>;
  // このドライバが読める・書けるタグの種類（"bit"・"u16"…"f64"・"string"）。UI の選択肢と型の検査に使う（R2b-6）
  supported-kinds: func() -> list<string>;
  // canonical を n ワード物理的に動かした正規形。ワード内ビットはビットを保ち、ビットデバイスは n 点（既定は非対応のエラー。R2b-6）
  offset-words: func(canonical: string, n: s64) -> result<string, string>;
  help: func() -> string;
}

interface config {
  use types.{setting};
  record field-error { key: string, message: string }
  validate: func(settings: list<setting>) -> list<field-error>;
  // 秘密として扱う設定キー（パスワード等）。§5 の保存先へ
  secret-keys: func() -> list<string>;

  // 接続の設定の欄（Hub の汎用フォームの元。R2b-6、ui-design §3）。条件つきの表示は v1 に無い
  record choice-option { value: string, label: string }
  record text-spec { max-len: option<u32>, placeholder: option<string> }
  record integer-spec { min: option<s64>, max: option<s64> }
  variant field-kind { text(text-spec), host, integer(integer-spec), choice(list<choice-option>), %bool, secret }
  variant setting-value { text(string), integer(s64), boolean(bool) }
  enum field-group { basic, advanced }
  record setting-field {
    key: string,
    label: string,
    help: option<string>,
    kind: field-kind,
    required: bool,
    default: option<setting-value>,
    group: field-group,
  }
  schema: func() -> list<setting-field>;
}

// WIT の export は必須なので、書き込みの有無で world を分ける
world driver    { export session; export read; export address; export config; }
world driver-rw { include driver; export write; }
```

補足:

- 文言（`help`・`field-error.message`・`device-error.message`）は v1 では日本語固定（Hub の UI と同じ）。多言語化は扱わない。
- データ型名・ワード順・bit 記法と既存の `banto-plc` のデコード規則の突き合わせは P0 で行い、組み込み版と同じ値になることを完了条件にする。
- 2026-10-19: [banto-driver-api-design.md](banto-driver-api-design.md) §9.2 の対応表に合わせて直した（`item-error.rejected`・`prefix-encoding`・`frame-check`・`farewell`・`decode` / `check` に `specs` / `items`・`address.parse` / `offset` に `string-words`・`data-type` の綴り）。Rust の trait と WIT は同じ PR で直す。
- RS-485 のマルチドロップの局番は `parsed` に持たせず、接続の `settings`（`unit_id` / `station`）で渡す。1 つのポートを複数の局で共有するときの直列化は Hub が持つ（api-design §8。局番のモデルを 1 つにするため）。
- `borrow<connection>` は可変性を区別しない。接続は 1 つのタスクが専有するので、`plan` / `encode` が接続の中の採番（Modbus TCP のトランザクション ID 等）を進めてよい（Rust 側は `&mut self`、api-design §4.2）。

### 6.3 インスタンスと並行性

wasmtime の `Store` は並行に使えないため、用途ごとに分ける:

- **接続用**: 接続ごとに 1 つの `Store` とインスタンスを持ち、その broker セッションのタスクが専有する。trap したらインスタンスを破棄し、次の再接続で作り直す（線形メモリの状態を持ち越さない）。
- **検証用**（`address` / `config`）: REST・MCP・CSV・UI ブリッジから並行に呼ばれるので、状態を持たないインスタンスのプールで処理する。trap は検証エラーとして返し、接続には影響させない。

### 6.4 リソース上限

- 実行時間は **epoch interruption** で区切る（fuel は使わない。計測は P0）。超過は接続用なら接続致命、検証用なら検証エラー。
- 線形メモリに上限を設ける。受信バッファは `step.max-len` で上限を持つ（`plugin-decides` で完結しないまま伸び続けることを防ぐ）。

### 6.5 接続状態に依存する機器情報と範囲判定（将来境界、未実装）

SLMP の `0101/0000` による型式情報と SD による実設定点数の取得、および機種・経路に応じたタグ範囲判定は、同じ物理セッションを所有する guest の責務とする。現行 `connection.handshake-next` は Hub の直列 I/O を通して取得要求と応答解析を進められるが、取得結果を Hub に構造化して返す export はない。`address.parse` は接続非依存の文法・静的構造検証であり、取得後の範囲判定の代わりにはならない。`address.probe` は接続試験用のタグ 1 件で、機器情報の搬送口ではない。

将来の追加契約では、guest が接続状態から型式の生値・判別結果、経路、SD の取得値、適用規則と根拠、およびタグごとの `in_range` / `out_of_range` / `unknown` と理由を返す。Hub は値の意味を再解釈せず、型とサイズの検証、接続・設定・ドライバ版と取得試行の世代管理、結果の保持・表示・品質を担う。`index + word_count <= count` を Hub に一律実装しない。bit/word の占有単位、SD の飽和・適用条件、穴や拡張領域の対応は guest が判定する。`unknown` は無制限の許可や成功を意味しない。

取得は broker の同一セッション内で直列化し、Hub は再接続・設定やドライバの変更で旧結果を失効させ、遅れて届いた旧世代の結果を棄却する。収集タスクの停止だけで物理セッションが終わったとみなさない。値 API やログへ情報を詰め込む方法は採らない。追加 export と公開 API、Unknown 時の運用方針は別の互換性・安全設計を経て決める。この節は責務境界の案であり、現行 WIT や実装の機能ではない。

Hub へ公開する契約の検討案（未実装・WIT 互換方式は未決）: 接続確立後の guest の取得結果を、型式コードの生値と表示用機種名、SD の設定点数と単位・取得元、適用した規則の識別子・根拠、各項目の取得成否と `unknown` の理由を持つ構造化スナップショットとして返す。Hub は接続・設定・driver 版の世代と取得試行番号を付けて保持し、旧世代を遅着棄却して表示する。未取得や一部取得を 0 点・範囲内・取得成功へ変換しない。既存 guest に必須 export を追加すると互換性を壊すため、任意機能としての発見方法、WIT world の分割または版上げ、保存・公開 API は別の設計確認後に定める。`unknown` 中に通常タグ読取りを許すかどうかも、0101 の先行試行を含めて別判断とする。

## 7. 発見・導入・更新

### 7.1 導入（アップロードと展開）

LocalSystem のサービスが利用者由来のファイルを展開する経路なので、DLL を却下したのと同じ厳しさで守る。

1. 導入は Hub の管理 UI / 管理 REST（`/api/plugins`、管理者ロールのみ）からのアップロードだけ。現場の利用者がフォルダを触る経路は作らない。
2. アップロードは展開せずにサービス専有の staging に受け取る。サイズに上限を設ける。
3. **署名を検証するまで一切展開しない**。署名の対象は「全エントリの名前と内容」の正規化ダイジェスト（`signature` 自身を除く）。
4. エントリ名は許可リスト（英数字・`-`・`_`・`.`・`/` の相対パスのみ。`..`・絶対パス・ドライブ指定・`:`・予約名を断る）。展開後の合計サイズとエントリ数にも上限を設ける。
5. 展開は `banto-safe-root` の `root`（`open_root` / `SafeRoot` / `ManagedDir`）でハンドル基準に行い、完成した staging を `<id>\<version>\` へ原子的に移す。**版は不変**: 同じ `(id, version)` が既にあれば、内容のダイジェストが同じなら何もせず、違えば導入を断る（同じ版番号で中身を差し替えない。直すときは版を上げる）。
6. 導入・削除・有効化・ロールバックは監査ログに記録する。
7. ロックダウン中の扱いは Q7。

### 7.2 置き場所

- `%ProgramData%\BantoHub\plugins\<id>\<version>\`（展開済み）。
- 管理者とサービスだけが書き込める保護された DACL。作成と確かめは `banto-safe-root` の部品（`acl::pin_dir`、`guard`、`root`）を使う。

### 7.3 発見

- 起動時に 7.2 を走査し、各バージョンについて マニフェスト検証 → **署名の再検証** → `hubApi` 照合 → 失効の確認（§9）→ WASM のコンパイル を行う。
- 導入・更新時はその場で登録し、Hub の再起動を要しない。
- 一覧 API `GET /api/plugins`（管理面）で、プラグインごとの状態（有効 / 無効とその理由: 署名不正・API 版不一致・コンパイル失敗・ID 衝突・失効）を返す。

#### 限定実装: 署名済みディレクトリの起動時明示登録（2026-10-25）

P1 の先行範囲として、`banto-hub` ホストの起動時に `BANTO_WASM_PLUGIN=<id>@<version>` が明示された場合だけ `%ProgramData%\BantoHub\plugins\<id>\<version>\` を読む。未指定なら何も走査しない。指定ディレクトリは管理者とサービスだけが書き込める状態で事前配置する想定で、この実装は配置・ACL 変更・アップロードを行わない。信頼できる自社の公開検証鍵はまだ未設定であり、実運用での登録はその出所と配布方法の確認まで拒否する。隔離テストだけが一時鍵を注入する。

この限定形式ではディレクトリ内を `manifest.json`・`component.wasm`・`signature.ed25519` の 3 ファイルに固定する。署名ファイルは生の Ed25519 署名 64 バイト。署名対象は `SHA-256("banto-hub-wasm-directory-v1\0" || entry...)` の 32 バイトで、`entry` は `component.wasm`、`manifest.json` の順に、それぞれファイル名の UTF-8 バイト長（u16 big-endian）・ファイル名・内容長（u64 big-endian）・内容の生バイトを連結する。署名ファイル自体を含めない。上限は manifest 64 KiB、component 64 MiB。検証済みの同一バイト列を WASM ローダーへ渡し、署名、`hubApi`、選択した id/version、既存 ID との衝突のいずれかが不正なら登録せず理由を記録し、組み込みドライバを残す。再起動時も検証し直す。P2 のパッケージ形式・失効・更新方式をここで確定するものではない。

案（通常 D/u16 書込みの限定追加）: 読取り専用 guest の登録は従来どおりとし、書込み宣言を持つ guest は `BANTO_WASM_WRITE_PLUGIN` が `BANTO_WASM_PLUGIN` と同じ `id@version` を明示した場合だけ、上記の署名検証後に登録する。接続設定の `d_u16_write_mode` は既定 `disabled`、`d_u16_write_allowlist` は既定で空とし、通常書込みには両方の明示設定を要する。許可リストは昇順・非重複の `D3010-D3013,D4000-D4009` 形式、最大 16 区間・各区間 960 語まで。Hub は全項目と全 span を送信前に検査し、D3000/D3001 に重なる書込みを拒否する。guest は同一接続の機種・SD 範囲を別に確認する。既存の認可・書込み制御・監査・broker は維持する。信頼鍵と保護配置が未設定の間は実運用での登録を開始しない。

### 7.4 更新とロールバック

- **有効なバージョンはプラグイン ID ごとに 1 つ**（Hub 全体の設定）。接続ごとのバージョン固定は v1 では持たない（Q4）。
- **有効化の前に保存済みの設定を新しい版で検証する（preflight）**。新しい版の `config.validate` をそのプラグインを使う全接続に、`address.parse` を配下の全タグにかけ、今の版の結果（`canonical` / `area` / `writable-area` / `word-count`）と比べる。1 件でも失敗または解釈の違いがあれば**有効化せず今の版を保ち**、該当する接続・タグを一覧で返す。設定の書き換えなしに、書き込み先のアドレスの解釈が黙って変わることを防ぐ。ロールバックにも同じ preflight をかける。
- preflight を通ったら、§3.1 の単位（broker セッション + 収集タスク）だけを作り直す。
- 直前のバージョンは残し、管理 UI から 1 操作で戻せる。ただし失効した版（§9）には戻せない。

### 7.5 プラグインが無いとき

- 接続・グループ・タグの設定は残す。その接続は「ドライバ未導入」状態で起動せず、タグの品質は Bad。
- 設定を消したり別プロトコルに読み替えたりしない。
- タグ登録（REST・MCP・CSV）は D3 の検証ができないため、そのプロトコル配下では断る。設定パッケージは §5 のとおり取り込み前に断る。

## 8. 同梱 UI（iframe）

- Hub が `/plugins/<id>/<version>/ui/` で静的に配信し、接続設定・タグ登録の画面で iframe として表示する。
- **配信には認証をかけない**。管理 UI のセッションは Cookie ではなく、フロントが `Authorization: Bearer` ヘッダに付けるトークンなので、iframe の画面遷移では送れない。配信するのは署名済みの静的ファイルだけで、秘密は含まない。データへのアクセスはすべてブリッジ経由で、親（管理 UI）が自分のトークンで行う。その代わり、UI のファイルは認証なしで読める（ID と版が推測できれば、その版が導入されているかを確かめられる）。
- **2026-10-19 オーナー決定（Q11）: 認証なしの配信を許容する**。理由: Hub の管理 UI 本体の静的ファイル（`assets.rs` → `banto_server` の `static_router`）も、ログイン前に読み込むため同じく認証なしで配信しており、新しい種類の露出ではない。また、推測で分かるのは導入済みのプロトコルと版の程度で、同じ LAN からは PLC のポートを直接調べるほうが多くが分かる。許容の前提として次を守る:
  1. **一覧 API（`GET /api/plugins`）は認証付きのまま**にする。認証なしで読めるのは UI の静的ファイルだけ。
  2. **UI のファイルに秘密や現場固有の情報を入れない**。プラグイン側リポジトリのレビュー（§9、署名の前）で確かめる項目にする。設定値・タグ・値はすべてブリッジ経由で親が取得する。
  3. **欠陥のある版は失効（§9）で止める**。版が外から分かっても、失効した版は Hub が読み込まないので狙う意味がない。
- **トップレベルで直接開かれても隔離が効くように、応答ヘッダで隔離する**。埋め込む側の `sandbox` 属性だけでは、URL を直接開かれたときに Hub のオリジンで動いてしまい、管理 UI がブラウザに持つトークンと管理 REST に届く。プラグイン UI の応答には次を付ける:
  - `Content-Security-Policy: sandbox allow-scripts; default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'none'; form-action 'none'; frame-ancestors 'self'`
- 埋め込む側にも `sandbox="allow-scripts"`（`allow-same-origin` は付けない）を付ける。Hub 本体の CSP（`hub_security_headers`）に `frame-src 'self'` を明示する。
- Hub とは `postMessage` のブリッジだけでやりとりする。sandbox 化した文書の `event.origin` は `"null"` になるため、相手は **`event.source === iframe.contentWindow`** で確かめ、応答はその `event.source` にだけ返す。
- ブリッジが公開する操作（v1 案）:
  - 現在の接続設定の取得（秘密値は伏せる）と、保存の**提案**（保存は Hub 側のフォームで確定）
  - `address.parse` / `address.offset` / `config.validate` の呼び出し（D3 と同じ関数）
  - タグ登録案の提出 → **Hub 側のプレビューと確定**を経て既存の登録経路（検証・未適用キュー・監査）へ
- デスクトップシェル（banto-hub-shell の WebView）でもブラウザでも同じに動く。別プロセスを起動しないので、サービス運用の Session 0 問題はない。

## 9. 信頼・署名・失効

- 署名は Ed25519。検証鍵は Hub のビルドに埋め込み、鍵の更新に備えて複数を許す。
- v1 で受け入れるのは**自社の鍵で署名したプラグインのみ**。第三者製の受け入れは Q2。
- プラグインのソースは別リポジトリ（§3.4.1 と同じ）に置き、通常のレビューと CI（ワイヤ層のテスト・モックでの分割受信・不正応答・無応答・切断）を通したものだけを署名する。
- **失効**: 漏洩した鍵や欠陥のある版に備え、「失効した鍵」と「プラグインごとの最低許容版」の一覧を持つ。ロールバック（§7.4）は署名済みの脆弱な旧版へ戻す経路にもなりうるため、失効した版には戻せないようにする。一覧の配り方（Hub の更新に同梱するか、別に署名付きで配るか）は Q10。

## 10. 段階計画（案）

§2.1 の実装保留は当初判断で、現在は冒頭の「状態」に記した限定範囲を実装中。P0〜P5 の間は組み込みドライバをそのまま残し、プラグインは追加の経路として併用する。組み込みドライバの移行は P5 の後、併用期間の結果を見て Q3 で判断する（本表には含めない）。

| 段階 | 内容                                                                                                                                                                                                                                        | 完了条件                                                                                                                                                                                               |
| ---- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| P0   | スパイク: wasmtime 組み込みのサイズ・ビルド時間（Windows CI を含む）・性能を測り、`cargo deny`（依存ツリー・ライセンス）を確認する。Modbus TCP を WASM で書き直し、組み込み版と同じ実機・シミュレータで読み取りを比較。WIT 確定             | 計測値と WIT v1 を本書に記録し、組み込み版と同じ値（ワード順・型を含む）                                                                                                                               |
| P1   | Hub 側ランタイム: Plugin Manager（手動配置のみ）、`WasmSession`（`BrokerSession` 実装）で読み取り、プラグインのアドレスを運ぶ要求型と変換箇所の明示的な分岐（§3.2）、レジストリ変更（§5 の `protocol`・`settings`・シミュレーションの拒否） | 組み込み版と同じ値・品質、broker の 1 接続 = 1 ソケットを保つ。**組み込みにないアドレス文法を持つ最小のテスト用プラグイン**が収集・即時読み取りを通る（Modbus の比較だけでは §3.2 の境界が隠れるため） |
| P2   | 導入・発見: アップロード、署名の検証、安全な展開（§7.1）、保護された置き場所、一覧 API、更新とロールバック、失効、監査                                                                                                                      | 再起動なしで導入・更新・ロールバック、不正なパッケージを展開前に断る                                                                                                                                   |
| P3   | UI: アドレス関数・設定検証を REST・MCP・CSV へ配線（D3）、秘密値の保存、iframe の配信とブリッジ（§8）                                                                                                                                       | プラグインの UI から接続とタグを登録でき、REST・MCP でも同じ検証が効く                                                                                                                                 |
| P4   | 書き込み: `driver-rw` の `write` を `WasmSession::write_batch` に配線。Hub の関門・監査はそのまま                                                                                                                                           | 既存の書き込み系テストと同じ観点を WASM 接続で通す。テスト用プラグインのアドレスで単発・レシピの書き込みが通る                                                                                         |
| P5   | 周辺: 設定パッケージ・未適用キュー・冗長化の fingerprint（§5）、ChronoGazer（`banto-collect` 直結の経路）での読み込み                                                                                                                       | 各文書へ反映                                                                                                                                                                                           |

2026-10-25 時点で P4 のうち `write-v1` の隔離テスト用登録と、SLMP R の `D3010/u16` 単点について直接経路・Hub REST API + broker 経路の実機試験を行った（[試験記録](../apps/banto-hub/host/tests/t12_slmp_wasm_hub_live/README.md)）。通常の `WasmDriver::load` は書込みマニフェストを拒否したまま。任意のプラグインやレシピ書込み、通常起動での登録・本番鍵配布は、この限定試験では確認していないため、P4 の完了とは扱わない。

2026-10-28: Modbus WASM の結合試験（CI の `modbus_join` と `t12_connection_test` の `modbus_wasm_join_test::`）の固定版を tyaro/banto-driver-wasm #44（`c9c3d83d06416c37a97934ab84f935bc38e55d0d`、manifest の `version` 0.4.0 → 0.5.0、`settingsVersion` は 1 のまま）へ上げた。それまでの固定版 `02a92645` は `4xxxx.N` / `3xxxx.N` のレジスタのビットの書込みを送る前に断っていた（#884 の試験はこの拒否を確かめていた）。#44 では `4xxxx.N` を 1 点ずつ FC22 Mask Write Register の 1 要求で書く（AND マスク `!(1 << N)`、OR マスクは set で `1 << N`・clear で `0`。読んでから書く手順は無い）。Hub 側の運用書込みゲート（`crates/banto-driver-wasm/src/modbus_write.rs`、保存済みの `modbus_operational_write_mode` / `_allowlist`）も、単独の保持レジスタのビット 1 点だけを、その語が許可範囲に入るときに限って guest へ渡すように広げた（入力レジスタのビット・数値との混在・2 点以上・Bool 以外は従来どおり guest へ渡さない）。模擬サーバで確かめたのは次のとおり: 3 つのゲート（Hub の保存済み方針、書込み受付と API キーの書込み権限、guest の `write_mode=enabled` と保持レジスタの `write_allowlist`）を通ったときだけ FC22 の set・clear が正確なマスクで届き監査が `ok`、どちらかのゲートが断れば機器へ何も届かない、機器の例外応答（FC22 非対応の例外コード 1）はその項目だけの失敗（REST は 502 `write_failed`、監査は `failed`）で接続と他の項目は続く、ACK のマスクが要求と違えば結果は未確定として 502 `write_failed`・監査 `failed` になり、その接続は切られ、同じ要求は送り直さない。実機の確認（2026-10-28、アダプタの層。Hub の REST・preflight は通していない）: iQ-R（R08ENCPU）の Modbus/TCP は保持レジスタ 400001 → D0 の既定の対応で、FC22 を受け付けた。安全範囲の D の 2 語（元の値が 0 の語と、ビットが立っている語）で bit0 を立てて落とし、SLMP と FC3 の両方で変わったのはそのビットだけだと確かめ、元の値に戻した。Modbus のシミュレータ（ModbusSlave）では、入力レジスタのビット（3xxxx.N）の Bool の読取りが生の値と一致した。3xxxx.N への書込みは送る前に断られ、入力領域を許可範囲に入れる設定はゲストと Hub の両方で断られた。このシミュレータは FC22 にも未定義の FC にも応答しなかった（例外を返さない）。そのため FC22 はバッチ全体の `Timeout` になり、セッションは破棄された。**FC22 の例外応答を項目ごとの `Device` にする経路は、実機では未確認**（模擬試験だけ）。2026-10-28（#899、オーナー決定: 動きは変えない）: 保持レジスタのビット（FC22）の書込みが機器の無応答（`Timeout`）で失敗したときは、Hub が失敗の文言（応答と監査の `detail`）に「機器が FC22 に対応していない可能性があります」というヒントを添える。例外応答（`Device`）や他の失敗、ワード・コイルの書込みには付けない。監査は `uncertain`、HTTP は 502 `write_failed` のまま。本番での書込みの有効化は別途オーナーの承認が要る。

2026-10-28（#902）: WIT の `item-error` に `verification-failed(string)` を足した（tyaro/banto-driver-wasm #47、guest main `3b7f0f6120af689fdd7a807cff7837657242c6b2`）。ホストの `driver.wit` はこれとバイト単位で同じ。WASM のアダプタはこれを `ItemError::VerificationFailed` に写し、Hub の書込み監査は `uncertain` にする（今のゲストはこの値を返さない）。Wasmtime の component の型検査は variant の値が完全に一致することを求めるので、#47 より前の WIT で作ったゲストはこのホストに読み込めない（破壊的変更）。CI の結合試験は、#44 を固定していた SLMP の受入れと Modbus の結合を guest main `3b7f0f6` へ移した（コードは #44 と同じ）。それより前の回帰用の固定 10 か所（Modbus FC3・SLMP 読取り・SLMP 単点書込み・範囲書込み・Q25PRH・D 専用・D/W 共通・診断・数値の段 2・FC4/W）は振舞いを固定したまま、`driver.wit` だけを guest main のものに差し替えて作る（差し替える前の blob が `313a79ed` であることを確かめる。差は `item-error` の 1 行だけ）。

2026-10-29（#915、模擬のみ・実機未確認）: Hub の運用書込みゲート（`crates/banto-driver-wasm/src/modbus_write.rs` の `preflight`）が、2 点以上のコイルの書込みを受け付けるようにした（guest #53 の FC15 に合わせる。固定版は `f06a90f` のまま）。条件は、すべて `Bit` 型・`Bool` 値・同じ接続のコイルで、番地が昇順に隙間なく続き、範囲全体が保存済みの `modbus_operational_write_allowlist` の 1 つの区間に入ること。上限は min(Modbus の 1968, guest の 123 項目) = 123 点。区間をまたぐ・隙間・重複・降順・保持レジスタとの混在・124 点以上は、guest へ渡さずに断る（`Unsupported`）。1 点のコイル（FC5）、保持レジスタ（FC16）、保持レジスタのビット（FC22）の扱いは変えていない。FC6 は guest の接続設定 `register_write=fc6`（既定 `fc16`）で選ぶ。Hub はこの設定を guest のスキーマのまま通し、ゲートでは区別しない（1 語の保持レジスタは従来どおり Hub の許可範囲で判定する）。2 語以上が要る書込みは guest が送る前に断る（`Rejected`）。監査は変えていない。REST の一括書込み（`POST /api/v1/values/batch`）はコイル 1 点ごとにタグ・要求値・結果の監査行を残すので、FC15 の点数と範囲はその行の集まりで分かる。模擬サーバで確かめたのは次のとおり: 8 点のコイルの一括書込みが 1 つの FC15（LSB から詰めたバイト）で届いて各点の監査が `ok`、Hub の区間をまたぐ一括書込みは機器へ何も届かず各点の監査が `failed`、FC15・FC6 の例外応答は項目ごとの `Device`（REST は項目ごとの失敗、監査は `failed`）で接続は続く、`register_write=fc6` の u16 の書込みが FC16 ではなく FC6 で届く、u32 は送る前に断られる。

2026-10-29（#914 の C1 の Hub 側のみ。ゲストの C1 は未実装のため、模擬の結合も実機も未確認で、アダプタの単体試験だけ）: `crates/banto-driver-wasm` が文字列タグを通すようにした。規約は banto-driver-wasm の `docs/PLAN.md` の C1（2026-10-28 作者決定）: `value::bytes` は文字順の符号化済みバイト列で長さは常に `2 × string-words`、1 ワード内の詰め方はゲストの仕事（SLMP は下位バイト先、Modbus は上位バイト先）。

- 登録: `supported-kinds` の `string` を 1 回まで受け付ける。アドレスは `word-count` が語数と一致し、SLMP は D/W、Modbus は保持・入力レジスタのときだけ通す。Modbus は 1 要求 123 ワードなので、124 語以上は登録時（`parse_address`）に断る（登録の上限 128 との差）。
- 読取り: 文字列タグの `bytes` は長さがちょうど `2 × 語数` のときだけそのまま engine の `decode_value`（native SLMP と同じ。最初の NUL で切り、不正な符号は項目エラー）へ渡す。長さ違い・文字列タグへの `bytes` 以外・数値タグへの `bytes` は、その項目のエラー（`Unsupported`）にしてバッチ全体は止めない。数値の型・範囲違いは従来どおり致命的エラー。
- 書込み: 要求が文字列タグ 1 つだけのとき受け付ける（他の項目との混在は全体を断る）。SLMP は D/W で範囲 `[先頭, 先頭 + 語数 − 1]` 全体が `d_u16_write_allowlist` の 1 つの区間に入ること（D3000・D3001 の保護はそのまま）、Modbus は保持レジスタで 123 語以下、範囲全体が `modbus_operational_write_allowlist` の 1 つの区間に入ること。値は native SLMP・Hub のゲートと同じ `banto_plc_write::encode_string_value` で符号化し（表せない文字・長すぎる値は断る。切り詰めない）、0 埋めした全 span を `data-type` `"string"`・`string-words` 付きの `value::bytes` で guest に渡す。監査は Hub の書込み経路（文字列は `value_requested_text`）のまま変えていない。

2026-10-29（同日、模擬のみ・実機未確認）: ゲストの C1（banto-driver-wasm #63）を、CI の固定 `f6c8781`（SLMP・Modbus とも。Modbus の manifest は 0.9.0）で受け入れた。ゲストの返す正規表記は同じ先頭の数値タグと同じ（SLMP は `D200`・`W1A`、Modbus は 6 桁の `400101`・`300001`）で、Hub のゲートが要求する形と一致した。

- component（`crates/banto-driver-wasm/tests/`）: SLMP の D/W の文字列は 1 回の 0401 で全体を読み、応答のデータをそのまま（ワードの下位バイト先）ちょうど `2 × 語数` バイトで返し、NUL で切らない。128 語は通り、129 語・0 語・ワード内ビット・ビットデバイスは登録で断る。書込みは 1 回の 1401 で 0 埋めした全体を送り、許可リストの外へはみ出す範囲・他の項目との混在・長すぎる値は Hub のゲートが送る前に断る（`slmp_udp_strings.rs`）。Modbus の保持・入力レジスタの文字列は 1 回の FC3 / FC4 で、レジスタの上位バイト先のまま返す。123 語は通り、124 語・コイル・接点・レジスタのビットは登録で断る。保持レジスタの書込みは 1 回の FC16（123 語も 1 回）で、`register_write=fc6` では 1 語だけが FC6 になり 2 語以上はゲストが送る前に断る（`Rejected`）。Hub の許可リスト外・入力レジスタ・混在・長すぎる値は Hub のゲートが断る（`modbus_join.rs`）。
- Hub（t12、`apps/banto-hub/host/tests/`）: SLMP は保存済みの D・W の文字列タグを API キーで REST から書くと、1 回の 1401 が下位バイト先・0 埋めで届いて監査が `ok`、read-now が同じ文字列を返す。許可リストの外へはみ出す範囲と長すぎる値は何も送らず監査が `failed`（`t12_slmp_wasm_operational_write` の `new_guest_saved_d_and_w_strings_are_written_audited_and_read_back_over_rest`）。Modbus は保存済みの保持レジスタの文字列が 1 回の FC16（上位バイト先・0 埋め）で届いて監査が `ok`、read-now が読み戻し、Hub の区間からはみ出す範囲と長すぎる値は何も送らず監査が `failed`（`t12_modbus_wasm_join` の `holding_string_tag_rest_write_is_one_fc16_audited_and_read_back`。模擬機器は 400101 以降だけ FC16 の値を覚えて FC3 で返す）。周期収集は設計どおり文字列を扱わない。

2026-10-29（同日、模擬のみ・実機未確認）: ゲストの G1（banto-driver-wasm #65、U\G バッファメモリの読取り。#917）を、SLMP の固定 `1fac51a`（Modbus の固定は `f6c8781` のまま）で受け入れた。Hub の製品コードの変更は `checked_string_words` が SLMP の文字列タグの `area` に、ゲストの正規表記の `U<h>\G`（`U` と先頭の 0 なしの大文字 16 進 1〜3 桁と `\G`。`U0` は可）を足したことだけ（`is_unit_buffer_area`）。数値・ワード内ビットの `U<h>\G<n>` は、これまでの「ゲストの文法と正規表記の往復で照合する」経路のまま通った。ゲストの返す `area` はユニットごとの `U<h>\G`（`U3\G`・`U3E0\G`）、`index` は `n`、`writableArea` は偽で、`wordCount` は型の語数。Hub の重なり判定（登録画面の `addressSpan.ts`）は `area` が違えば `index` が同じでも重ならないとするので、`U3\G10` と `U5\G10` は別のものとして登録できる。

- component（`crates/banto-driver-wasm/tests/slmp_unit_buffer.rs`、6 件）: R04CPU は 0082（番号 4 バイト、CPU バッファメモリ `U3E0` だけ拡張指定 FAH、ほかは F8H）、Q02UCPU は 0080（番号 3 バイト）のフレームを、ループバックの TCP の模擬機器でバイト単位に照合した（4E。要求データ長は Q が 19、R が 21）。数値・ワード内ビット・文字列（1〜128 語、下位バイト先）を読み、1 タグ 1 要求で隣の `U3\G11` や D のタグとまとめない。Q の `U3E0`〜`U3E3`・Q で末尾が 24 ビットを越える範囲・Q25PRH の `U\G` は、送る前に項目ごとの `Unsupported` で、同じバッチの他のタグを読み、接続も切らない。PLC の終了コードは項目の `Device` エラー。`U\G` への書込み（数値・ビット・文字列、許可リストの D と混ぜたものも）は、何も送らずに断る。登録では正規表記（先頭の 0・小文字の 16 進は正規形に直る）と、文法の外（`U100`〜`U3DF`・`U3E4` 以上・`HG`・語数 0 と 129 の文字列）の断りを確かめた。
- Hub（t12、`apps/banto-hub/host/tests/t12_slmp_wasm_operational_write/`、2 件。R と Q）: 保存済みの `U\G` の数値・ワード内ビット・文字列のタグが REST（`POST /api/tags`）で登録でき、`writable: true` は読み取り専用の領域として 422 で断られる。`POST /api/tags/parse-addresses` が `area`・`index`・`wordCount` を返し、`U3\G10` と `U5\G10` は `area` が違う。周期収集で Good（文字列は周期収集の対象外で、read-now だけ）、read-now が値を返し、受け取ったフレームは 1 タグ 1 要求のバイト列に一致する。Q では `U3E0\G1` と `UFF\G16777215`（4 バイト）が Good にならず送られない。API キーでの `U\G` への書込みは何も送られない。
- Hub 側のブロッカーは見つからなかった: 登録（`parse_tag_address`）・読取り（`read_path`）・収集・REST は、ゲストの `area` に依存した分岐を文字列の `checked_string_words` 以外に持たず、書込みの関門（`WriteArea::from_name` が D/W/M/Y/L/B だけ）は `U\G` を元から断る。

2026-11-06（模擬のみ・実機未確認）: ゲストの G2a（banto-driver-wasm #69、Q のプロセス CPU・QnUDPV・MELSEC-L・Q12PRH の判別）を、SLMP の固定 `c193376`（Modbus の固定は `f6c8781` のまま）で受け入れた。WIT と Hub の製品コードは変更なし。0101 の型名と型式コードの組が表と一致しない組（`L02CPU` と 0544H、`Q12PRHCPU` と 004CH など）は従来どおり Unknown のまま。旧固定 `1fac51a` ではこれらの組がすべて Unknown だった。

- component（`crates/banto-driver-wasm/tests/slmp_g2a_models.rs`、5 件）: QnPH（Q02PH〜Q25PH）は標準の非 U の Q で、handshake は 0101 と SD302-303 だけ（SD310-311 を読まない）、D と ZR は平文の 0401、`UFF\G` は 0080、`U3E0\G` は項目ごとの `Unsupported` で送らず、SD303 を越える W は読まない。QnUDPV は QnUDV と要求列がバイト単位で等しく（0101、SD302-303、SD310-311、読取り）、SD310-311 の合計の内側の拡張 W を読む。MELSEC-L（10 組）は標準 Q の Q06HCPU と要求列が等しく、ZR を読み、SD310-311 は読まない。D の書込みは標準 Q と同じく SD の証明がないので送らない。Q12PRH は Q25PRH と同じ読取り（SD302-303 だけ、D は読め、ZR と `U\G` は送らず項目ごとの `Unsupported`）と、許可リストの D 書込みで同一の 1401。組の不一致は Unknown で、0101 の後は何も送らない。
- Hub（t12、3 件）: 保存済みのタグが周期収集と read-now を通る。MELSEC-L（L26CPU-PBT）は D・ZR・`UFF\G` が Good、`U3E0\G` は Bad で送られず、SD は SD302-303 だけ。QnUDPV は QnUDV と同じ要求（SD310-311 を含む）で、拡張 W も Good。Q12PRH は Q25PRH と同じ要求で、D だけ Good、ZR と `U\G` は Bad で送られない。
- ゲストの挙動で想定と違った点（受入れの記録として残す）: (1) 型式が Unknown の接続では、読取りの batch 全体が項目ごとの `Unsupported` ではなく protocol エラー（範囲が決まっていない）になる。項目ごとの `Unsupported` になるのは、経路を持たない既知の機種（Q25PRH・Q12PRH の ZR と `U\G`）だけ。(2) component の handshake は、どの機種でも SD282-285（QnUDV の高速 D/W）を読まない（ドライバのライブラリだけが計画する）。QnUDPV を QnUDV と同じと見なす証明は、要求列の一致で行った。(3) Q12PRH の D3010 から D3013 の試験書込みの設定 `q25prh_d3010_3013_test_write` は管理された試験用の loader の設定で、運用書込みの loader は書込みを有効にした接続との併用を断る。運用の loader では許可リストの D 書込みで Q25PRH と同一であることを確かめ、試験書込みの設定は Hub では確かめていない。Hub の REST 経由の Q12PRH の書込みも、この受入れでは確かめていない。

2026-11-06（模擬のみ・実機未確認）: ゲストの #71（2026-11-06 作者決定。ZR の番号を GX Works の表記に合わせて Q・R とも 10 進にする）を、SLMP の固定 `abb1a1a`（Modbus の固定は `f6c8781` のまま）で受け入れた。WIT と Hub の製品コードは変更なし。Hub 組み込みの SLMP ドライバ（`banto-plc`）は元から ZR を 10 進で読むので、両者の表記がそろった。

- component（`slmp_g2a_models.rs`）と Hub（t12）: `ZR10` は 10 番（0x0A）の平文 0401 で送られる。旧固定 `c193376` では 0x10 番を送るので、ZR を含む試験（component 3 件・t12 3 件）が落ちることを確かめた。
- Hub（t12）: `ZR1A` のような英字を含む番号は、アドレスの解釈で断られ、何も送られない。
- 既存の保存済みタグへの影響: ゲストの ZR のタグは、同じ表記のまま別の番号を指すようになる（例: `ZR10` は 16 番から 10 番へ。`ZR1A` のような表記は解釈できなくなる）。この PR は保存済みのアドレスを移し替えない。実運用で ZR のタグを持つ接続は、ゲストを上げる前に表記を見直す必要がある。

2026-11-06（模擬のみ・実機未確認）: ゲストの G2b（banto-driver-wasm #72、iQ-R のプロセス CPU・SIL2 プロセス CPU・安全 CPU・MX-R の判別）を、SLMP の固定 `b705fd2`（Modbus の固定は `f6c8781` のまま）で受け入れた。WIT と Hub の製品コードは変更なし（`write-v1.wit`・`diagnostics-v1.wit` の blob はゲストと一致）。0101 の型名と型式コードの組は、プロセス CPU（`R08PCPU` 4841H〜`R120PCPU` 4844H）・SIL2 プロセス CPU（`R08PSFCPU` 4851H〜`R120PSFCPU` 4854H）・安全 CPU（`R08SFCPU` 4891H〜`R120SFCPU` 4894H）・MX-R（`MXR300-16` 48E9H・`MXR300-32` 48EAH・`MXR300-64` 48EBH・`MXR500-128` 48EEH・`MXR500-256` 48EFH）が標準の R と同じ扱いになる。旧固定 `abb1a1a` ではこれらの組がすべて Unknown だった。

- component（`crates/banto-driver-wasm/tests/slmp_g2b_models.rs`、4 件）: プロセス CPU（4 組）は S を標準の R と同じく読み、要求列が R08CPU とバイト単位で等しい（0101、SD280-283、D の 0002、S のビット単位 0003）。SIL2 プロセス CPU・安全 CPU・MX-R（9 組）は S のタグが項目ごとの `Unsupported` で何も送らず、同じバッチの D は R08CPU の D と同じ要求で読める。S のタグだけのバッチは handshake のほかは何も送らない。組の不一致（`R08PCPU` と 4851H、`R08SFCPU` と 4801H、`MXR300-16` と 48EAH など）、表にない組（`MXR500-64`、`R04PCPU`）、`MX-R`/037EH・`RCPU`/0360H は Unknown のままで、0101 の後は何も送らず、読取りの batch 全体が protocol エラーになる。旧固定 `abb1a1a` では組の不一致の 1 件を除く 3 件が落ちることを確かめた（不一致の 1 件は旧固定でも通る）。
- Hub（t12、3 件）: 保存済みのタグが周期収集と read-now を通る。安全 CPU（`R08SFCPU`）・SIL2（`R16PSFCPU`）・MX-R（`MXR300-32`）は D が Good で、S は Bad のまま送られず、要求は R08CPU と同じ（SD280-283 と D の 1 回の読取り）。プロセス CPU（`R32PCPU`）は D と S がともに Good で、S は 98 のビット読取り、要求は R08CPU と同じ。`R08SFCPU` と 4801H の不一致の組は Unknown で、タグは Good にならず、SD も機器の読取りも送られない。旧固定では前の 2 件が落ちる。t12 の合計は 48 件から 51 件になった。
- ゲストの挙動で想定と違った点: (1) 型式が Unknown の接続では、読取りの batch 全体が項目ごとの `Unsupported` ではなく protocol エラーになる（G2a と同じ）。(2) S の 1 ビット読取りに模擬機器が 0x11（下位ニブルも 1）で応えると、ゲストは「invalid SLMP response」にした。ゲストはビット読取りの応答で、奇数点の末尾の下位ニブル（詰め物）が 0 でないものを枠の誤りとして断る（ゲストの `drivers/slmp/src/lib.rs` の `decode_read`。G2b 以前からの挙動）ので、模擬機器は 0x10 で応える。(3) 書込みの経路は確かめていない（安全 CPU・MX-R など、新しく標準の R として扱われる機種への D 書込みの許可は、この受入れの範囲外）。

2026-11-06（模擬のみ・実機未確認）: ゲストの G2c（banto-driver-wasm #73、FX5 / MELSEC iQ-F の判別）を、SLMP の固定 `aa3a1c1`（Modbus の固定は `f6c8781` のまま）で受け入れた。WIT と Hub の製品コードは変更なし（`write-v1.wit`・`diagnostics-v1.wit` の blob はゲストと一致）。ゲストは、0101 の型式コードが FX5 の表（FX5U 18・FX5UC 9・FX5UJ 9・FX5S 12 の 48 個）にあり、かつ型名が `FX5` で始まるときだけ FX5 とする（片方だけなら Unknown。型名は系列の型式コードとは組にしない）。FX5 は 3E・自局だけで扱い、配線は Q と同じ（0401/1401 のサブコマンド 0000/0001、デバイス番号 3 バイト）。0101 は型式が分かる前なので監視タイマ 0010H、FX5 と判別した後の要求（SD・読取り・書込み）は 0000H で送る。SD は SD280-283（D・W）を 1 回、その後 SD304-305（R）を別の要求で読み、M/L/B の書込みを許可したときは SD262-275 も読む。

- component（`crates/banto-driver-wasm/tests/slmp_g2c_models.rs`、7 件）: 表の 48 個すべての型式コード（型名は系列の接頭辞に合わせる）と、実際の型名に近い 8 組が FX5 と判別され、要求列がバイト単位で期待どおり（0101 の 0010H、SD280-283 と SD304-305 の 0000H、D は A8H、`R<n>` は AFH で 10 進）。`R0100` は R100 で、32 ビットは 2 ワード。FX5 の X・Y・ZR・RD・LTN・LCN・`U\G` は項目ごとの `Unsupported` で送られず、同じバッチの D と R は読める。標準 Q（3E）と R08CPU（4E）では `R<n>` が項目ごとの `Unsupported` で送られず、同じバッチの D は従来どおり読める。表のコードで型名が `FX5` で始まらない組と、`FX5` の型名で表にないコードの組は Unknown で、0101 の後は何も送らず、読取りの batch 全体が protocol エラー、書込みは項目ごとに断られる。FX5 の組を 4E（既定の `frame`）、または他局の 3E（`network_no`・`station_no`）や `target_cpu=cpu1` で受けても Unknown と同じで、0101 の後は何も送らない。書込みは D が 1401/0000・監視タイマ 0000H の 1 フレーム、M が 1401/0001 の 1 フレーム（M を許可すると handshake に SD262-275 が加わる）で、`R<n>` は送る前に断られる。旧固定 `b705fd2` では 7 件すべてが落ちる（`R` が文法にないため、`R10` のタグを含む 6 件はその時点で落ちる。Unknown の 2 件はその挙動自体は新しくなく、`R` のタグを同じ batch に入れたために落ちる）。
- Hub（t12、3 件）: FX5U を `frame=3e` で受けた保存済みのタグは、D と `R10`（Hub の WASM アダプタは area `R` の数値タグを受け付ける）が周期収集と read-now で Good になり、X・Y・ZR・`U\G` は Bad のまま送られない。handshake は 0101（0010H）、SD280-283、SD304-305 で、以後の要求はすべて 3E・自局・0000H、1401 は送られない。FX5UC の組を既定の 4E、または他局の 3E で受けた接続は、タグが Good にならず read-now が失敗し、SD も機器の読取りも送られない（0101 だけ）。FX5S の 3E の接続では、REST の書込みで D3010 が 1401/0000・監視タイマ 0000H の 1 フレームで書け、`R<n>` のタグは Hub が writable での登録を断り（「R はプロトコル上の読み取り専用領域」）、書込みの要求も断られて PLC に何も送られない。旧固定では 3 件とも落ちる（`R` のタグが作れない／FX5 が Unknown）。t12 の合計は 51 件から 54 件になった。
- ゲストの挙動で想定と違った点: (1) 型名を系列の型式コードと組にしないため、`FX5S` の型名に `FX5U` のコードの組も FX5 と判別される（ゲストの設計どおり。実機の型名の文字列は未確認）。(2) Unknown の接続で読取りの batch が protocol エラーになると、その接続は破棄され、同じセッションでの次の書込みは「接続は破棄済み」になる（書込みを先に試した）。(3) `R<n>` の書込みは、component の試験ではゲストに届く前に Hub のゲートが `Unsupported`（許可リスト外）として断り、t12 では writable の登録の時点で Hub が断る。ゲスト自身の拒否はゲスト側の試験の範囲。(4) 空白だけの型名の 0101 は FX5 以前から「invalid SLMP response」で接続が成り立たない。(5) 模擬機器は 4E の 0101 にも応えるが、実機の FX5 は 4E に応答しない。実機での確認が要るのは、0010H の 0101 への応答、FX5 の型名の文字列、SD262-275 の読取り（FX5 で使わない SD272-273 を含む）、X・Y と `U\G`。

2026-11-06（模擬のみ・実機未確認）: ゲストの G3（banto-driver-wasm #74、GX Works3 のグローバルラベル）を、SLMP の固定 `fc0bd10`（Modbus の固定は `f6c8781` のまま）で受け入れた。WIT は変更なし（`write-v1.wit`・`diagnostics-v1.wit` の blob はゲストと一致）。ラベルのタグは `@` に続けてラベル名を書いたとおり（大小文字を保ち、非 ASCII も可）で、`address.parse` は area `@`・`index` なし・`writable-area` 真を返す。ゲストは 0101 で標準 R と確定した接続だけで、読取りは 041C（サブコマンド 0000、略号なし）1 回にまとめ、書込みは 041C で全ラベルのデータ型 ID と長さを確かめてから 141B 1 回で書く。Hub の製品コードの変更は次のとおり。

- `crates/banto-driver-wasm`: 文字列タグの `checked_string_words` が SLMP の area `@` を受ける。`operational_write.rs` の `WritePolicy` に `label_write_allowlist` を足した。文法はゲストの `label.rs`（`LabelAllowlist::parse`・`parse_entry`・`parse_name`・`allows`）を写したもの: `;` 区切り、各項目は正規表記のラベルか最後の添字の区間（`@Arr[0-9]`・`@M[1,0-15]`、始点 ≤ 終点、先頭の 0 なし）、空白・制御文字・`*`・空の項目・同じ項目の重複は設定エラー、32 項目・2048 文字（文字数で数える）まで、大小文字を区別する。`d_u16_write_mode` は共用で、`enabled` には D/W かラベルのどちらかの許可リストが空でないことを要する（従来は D/W が必須。旧い保存値の意味は変わらない）。Q の試験書込み（`q25prh_d3010_3013_test_write`）とは、ラベルの許可リストだけで有効にした場合も同時に有効にできない。ゲストのスキーマが `label_write_allowlist` を宣言するときは、秘密でない・既定が空・`max_len` が 2048 以下の文字列の欄であることを読込み時に要する（宣言しない G3 以前のゲストはそのまま読める）。書込みの要求に area `@` の項目があれば、全項目がこの接続のラベル（`index`・`bit` なし、型に合うワード数）で、許可リストに入り、重複しないときだけゲストへ渡す。デバイスのタグとの混在・許可リストの外・同じラベルの 2 回指定は要求全体を送る前に断る。値は bit が Bool、u16/i16/u32/i32 が整数で型の範囲内、f32/f64 が有限（ラベルに 64 ビット整数の型はない）、文字列は D/W の文字列と同じ `encoded_string` で 0 埋めした `2 × 語数` バイト。
- 非 ASCII のラベル名: タグ登録（REST の `/api/tags`、保存値はゲストの正規表記）、`parse-addresses`、周期収集・read-now、REST の書込みと監査（非 ASCII のタグ名の `externalNameSnapshot` を含む）を t12 で通した。アドレスに ASCII の制限はない（`crates/banto-tags/src/tag.rs` の `normalize_tag_address` はドライバの解析に任せる）。設定パッケージはアドレスを文字列のまま運ぶ（`apps/banto-hub/src/lib/banto/configPackage.ts`）。UI の欄にも文字の種類の制限はない。
- 重なりの判定: `parse-addresses` はラベルに `index: null` を返し、UI の `spansOverlap`（`apps/banto-hub/src/lib/banto/addressSpan.ts`）は `index` が無いものを `canonical` の一致だけで比べる（§3.6 の `None` の扱いのとおり）。構造体とそのメンバのような同じ記憶域の別名は判定しない。
- component（`crates/banto-driver-wasm/tests/slmp_g3_labels.rs`、6 件）: アダプタを通るラベルの文法（`@Arr[03]` → `@Arr[3]`、area `@`、`index` なし、型のワード数、`offset` は最後の添字だけ、`@X.3`・i64/u64 の拒否）。R08CPU（4E）で同じ batch のラベル 7 個（`@タンク1.水位` を含む）が 041C 1 回にバッチの順で入り、バイト列が期待どおり（名前は UTF-16LE）で、D100 は別の 0401。型の違うラベル（ID 4 を u16 で読む）はその項目だけ `Rejected`、PLC が断った 041C はそのフレームの全項目の `device`。D/W の許可リストを空にしてラベルの許可リストだけで有効にした書込みは、041C の確認 1 回と 141B 1 回だけ（bit は 1 ワード、数値はラベルの下位ワード先、文字列は符号化したバイト）。許可リストの外・大小文字違い・区間外・デバイスとの混在・重複・無効のモード・D/W の許可リストだけの接続は Hub のゲートが断り、PLC には何も送られない。型の違いは 041C だけで 141B は送られない。標準 Q（Q06HCPU、3E）では読取りが項目ごとの `Unsupported`、書込みはゲスト自身が `Unsupported` で、041C も 141B も送られない。設定の検査は不正な許可リスト・33 項目・2049 文字をゲストと Hub が同じキーで断り、32 項目・2048 文字は通る。旧固定 `aa3a1c1` では 6 件すべてが落ちる（`@` のアドレスが解析できない／`label_write_allowlist` を知らない）。
- Hub（t12、3 件）: R08CPU の保存済みのラベルのタグ（REST で登録。`@タンク1.水位`・f32・bit・文字列・型の違う `@Word`）と D100 は、周期収集で文字列以外のラベルを 041C 1 回にまとめて読み、型の合うものと D100 が Good、`@Word` は Bad で read-now も失敗する。文字列のラベルは read-now で読める。1401 も 141B も送られない。ラベルの許可リストだけを設定した接続では、REST の書込みで非 ASCII のラベルが 041C 1 回と 141B 1 回で書け（監査は `ok`、タグ名 `水位`）、許可リストの外は何も送らず、型の違うラベルは 041C だけで 141B は送られない（どちらも監査は `failed`）。標準 Q（3E）ではラベルのタグが Good にならず、read-now も REST の書込みも失敗し、041C も 141B も送られない。旧固定では 3 件とも落ちる（`label_write_allowlist` が設定に無い欄、`@` のアドレスが解析できない）。t12 の合計は 54 件から 57 件になった。旧固定 `aa3a1c1` でも、今回の Hub の変更で既存の 54 件は通る。
- 想定と違った点・補足: (1) 文字列のラベルは、D/W の文字列と同じく周期収集の対象外で read-now だけで読む（Hub の既存の挙動）。(2) 値が `2 × 語数` バイトを埋めて 0x00 を含まない文字列の書込みは、Hub は通すがゲストが 041C を送る前に `rejected` にする（何も送られない）。(3) ゲストの PLAN.md は「正規表記の重複は従来どおり Hub の登録で弾く」としているが、Hub の保存の層はアドレスの重複を断らない（§3.6 と同じ。UI の構造体登録・オフセットコピーの衝突判定が同じグループの同じ `canonical` を重なりとして示すだけ）。(4) UI の連続登録の開始番号の既定は `index` から取るので、ラベルでは空になる（0 として扱う。動作の誤りではない）。(5) 実機での確認が要るのは、ラベル名の大小文字の区別、ラベルがないときと外部アクセスが無効のときの終了コード（模擬機器は 40C0H で要求全体を断る）、GX Works3 の文字列のデータ長、ラベル操作に対応する機種。

2026-11-07（模擬のみ・実機未確認）: ゲストの H（banto-driver-wasm #76、PLC の時計合わせ）の Hub 側の前半（H1、tyaro/banto-industrial#1053 の H の部分）を、SLMP の固定 `ea4d84c`（Modbus の固定は `f6c8781` のまま）で受け入れた。WIT は `clock-v1.wit`（interface `clock-v1` と world `driver-with-write-diagnostics-and-clock-v1`・`driver-with-clock-v1`）をゲストからバイト単位で写し、既存の 3 つの WIT は変えていない（4 つとも blob はゲストと一致）。Hub の製品コードの変更は次のとおり。後半（H2）は接続ごとの `clockSync` の設定と時刻表、REST `POST /api/plc-connections/{id}/clock-sync` と直近の結果の `GET`、監査、MCP（見るだけ）、t12 の模擬受入れ。

- `crates/banto-driver-api`: `CivilTime`（年・月・日・時・分・秒。曜日と暦の検査はドライバが持つ）と `ClockProgress`（`Send(Step)`／`Done(Result<(), ItemError>)`）を足した。`FrameConnection` に既定つきの `supports_clock`（既定は偽）と `set_clock_next`、`Session` に既定つきの `set_clock`（既定は `Ok(None)` = このドライバに時計合わせの契約がない）を足した。既定があるので既存の実装は変わらない。版は上げていない（diagnostics を足したときと同じ）。
- `crates/banto-driver-engine`: `MAX_CLOCK_STEPS = 8`。`FrameSession::set_clock` は、ドライバの `capabilities.write` が偽か `supports_clock` が偽なら何も送らずに `Ok(None)`。手順は診断と同じく同じ Core の上で 1 回ずつ排他に行い、9 回目の `Send` は契約違反（C16、step の上限）として致命にし、途中の致命・期限切れ・外側のキャンセルでは接続を閉じる（時計が変わったかは分からない）。`Done(Err)` は接続を保つ。
- `crates/banto-broker`: `Job::SetClock` と `BrokerHandle::set_clock`（`ReadOnlyHandle` には付けない。PLC に書くため）。上限は `clock_job_limit` = `8 × 2T + 1 s`。超えたら他のジョブと同じく致命で、接続を捨てて再接続する。broker は時計合わせを再送しない。
- `crates/banto-driver-wasm`: world `driver-with-clock-v1` を bind し、`banto:driver/clock-v1@1.0.0` の export があるか（`get_export_index`）で判別する。時計合わせを出すのは、署名付きで書込みを明示的に選んだ運用書込みの loader（`ControlledWriteTarget::Operational`）で読み込んだゲストだけで、読取り専用の loader と制御された試験用 fixture（ループバック・実機の試験書込み）では出さない（実機での時計合わせは別途承認を要する）。ゲスト自身のゲートは設定 `clock_set`（既定は無効）。
- component（`crates/banto-driver-wasm/tests/slmp_h_clock.rs`、6 件）: R08CPU で `clock_set=enabled` のとき、4E と 3E のどちらも 0401（SM210–SM211 のビット読み）、1401（SD210–SD216 のワード書き。2026-11-07 09:05:30 は 2026・11・7・9・5・30・曜日 6）、1401（SM210 ON のビット書き）、0401（確認読み）の 4 要求がバイト単位で期待どおり（ゲストの `slmp-runtime/tests/clock.rs` と同じバイト列）で `Ok`、同じ接続で D の読取りも続けてできる。SM210 が最初から ON なら読取り 1 回だけで `Rejected`、確認読みで SM211 が ON なら `Device`、SM210 が 3 回とも ON なら `VerificationFailed`（確認読みは 3 回だけ）、各段の終了コードは `Device` でそれ以降は送らない。どれも接続は保つ。`clock_set` が無効・未設定のときと標準 Q（Q06HCPU、3E）では何も送らない（`Rejected`／`Unsupported`）。短い応答は致命で接続を閉じる。同じゲストを読取り専用の loader で読み込むと時計合わせは出ず（`Ok(None)`）、何も送らない。旧固定 `fc0bd10` では 6 件すべてが落ちる（`clock_set` を知らない設定として断る）。
- 旧固定の回帰（`crates/banto-driver-wasm/tests/diagnostics_v1.rs`）: CI のその段で使う旧ゲスト `532e0ba`・`f207660`（どちらも `clock-v1` を持たない）がそのまま読み込め、`set_clock` は `Ok(None)` で、ハンドシェイクの後に何も送らない。
- 単体: driver-api の既定 2 件、engine 5 件（成功、`Done(Err)` で接続を保つ、9 回目の `Send` で致命・接続を閉じる、ドライバの致命、契約なしで何も送らない）、broker 2 件（結果をそのまま返して接続を保つ、終わらない時計合わせは上限で致命になり、再接続後も再送しない）。

2026-11-07（模擬のみ・実機未確認）: H の Hub 側の後半（H2、tyaro/banto-industrial#1053 の H の残り）として、接続ごとの時計合わせの設定と時刻表、定期と手動の実行、監査、直近の結果、REST・MCP を足した（SLMP の固定は `ea4d84c` のまま。WIT・ゲストは不変）。実装は `apps/banto-hub/core/src/clock_sync.rs`（モジュール doc に設計意図）と `rest/clock_sync.rs`。決めたこと（判断事項）:

1. 保存: Hub の DB の新しい表 `hub_clock_sync`（migration `0110_hub_clock_sync.sql`。`connection_id` が主キー、`enabled`、`schedule_kind`（`hourly`／`daily`）、`hour`（hourly は NULL）、`minute`。FK は張らない）。行が無い = 無効で、GET は既定の「無効・毎時 5 分」を返す。`plc_connections`（banto-tags）は作り直さず列も足さない。
2. 設定の変更は保留キュー（pending_changes）を通さず PUT で即時に保存する（接続の設定ではなく Hub の運用の時刻表。二重のゲートのもう片方、ゲストの `clock_set` は接続の設定にあり、従来どおり保留キューを通る）。接続を消しても行は残る（孤児。削除の適用経路に Hub 側の接続ごとの状態を掃除する場所は無かった。接続の id は再利用されず、適用済みのカタログに無い接続は実行しないので害はない）。設定パッケージ（export/import）には含めない。
3. 時刻表は「毎時 m 分」（`{"kind":"hourly","minute":m}`）か「毎日 hh:mm」（`{"kind":"daily","hour":h,"minute":m}`）だけで、cron は持たない。時刻は Hub の PC の現地時刻（`chrono::Local`。Cargo.lock に新しいパッケージは増えない）。
4. スケジューラ: Hub の runtime の 1 本の tokio タスク。1 秒ごとに現地時刻を読み、分が前に評価した分と違うときだけ評価する（分の境目まで sleep しないので時計の飛びで寝過ごさない）。有効な行のうち今の分が時刻表に合い、適用済みのカタログにある有効な接続だけを出す。過ぎた回は後から出さない（Hub の停止中・PC のスリープ・時計の飛び越し）。接続ごとに定期で出した分の集合をメモリに持ち、集合にある分（夏時間の巻き戻しや時計の手直しで、いくつか前の naive の分がもう一度来ても）では 2 回目を出さない。集合は評価のたびに今の分の前後 26 時間より離れた分を捨てる（夏時間の巻き戻しと日ごとの時刻表を覆いつつ、PC の時計が大きくずれていて直されたときに時刻表が永久に止まらないようにするため）。各回の実行は `JoinSet` の別のタスクで、ループは待たない（1 回が長引いても次の分の評価と別の接続の回は遅れない）。評価を待つ間に止める合図が来たら、その評価の回は出さない。評価を待つ間に分が変わっていたら（スリープからの復帰など）、その評価の回は捨てる（追いつかない。捨てた分の印は残すので同じ分で出し直さない）。止めるのは合図で、abort しない（終了の冒頭で合図、broker を止めた後に残る回も全部待つ）。
5. 実行（定期と手動で共通の 1 関数）: 書込み可能なフル `BrokerHandle` を非ダイヤルの peek で取り、無い・切断中なら送らずに `unavailable`。接続ごとの実行中の印で重ねない（2 つ目は `busy`）。`CivilTime` は broker がジョブを始めるときに現地時刻から作る（`BrokerHandle::set_clock_now` に時刻を作る関数を渡し、ドライバの `set_clock` の直前に呼ばれる。キューの待ちを時刻の誤差にしない。秒は切り捨て、曜日は渡さない）。監査と直近の結果の `time` は broker が返した、実際に渡した時刻。時間切れは broker の `clock_job_limit` だけで、Hub は外側の時間切れを足さない。自動では再送しない。結果の写像は `ok`・`unsupported`・`rejected`・`device`（`code` に終了コード）・`verificationFailed`・`unavailable`（送る前の broker の失敗を含む）・`unknown`（`ConnectionFailed`・`TaskGone`、結果不明）・`notOffered`（`Ok(None)`、このドライバ・loader は時計合わせを出さない）・`busy`。毎回（`busy`・`unavailable` も）監査し、直近の結果（メモリ、接続ごとに 1 件）を更新する。
6. 監査: `audit_log` に `resource = "plc_connection_clock_sync"`・`entity_id = 接続 id`。実行は `action = "clock_sync"`・`detail = {trigger, time, result, code, message}`（`time` は渡した現地時刻 `YYYY-MM-DDTHH:MM:SS`、送らなかったときは `null`）、設定の変更は `action = "update"`・`detail = {enabled, schedule}`。`result` 列は成功だけ `ok`、ほかは `failed`。手動は `origin = "rest"` と操作者、定期は actor なしで `origin = "schedule"`（`audit_log.origin` に CHECK は無いので migration は要らない。既存の値は `rest`・`mcp`・`grpc`・`elev`）。
7. REST（3 本とも editor、管理系なので `X-Banto-Client` ヘッダも要る）: `GET /api/plc-connections/{id}/clock-sync` は `{settings: {enabled, schedule}, last: 直近の結果 | null}`（`last` は `{at（UTC の RFC3339）, trigger, time, result, code, message}`）。`PUT` の本文は `{enabled, schedule}`（未知のキー・型の違い・範囲外は 400）。`GET`・`PUT` は保存済みの接続に無ければ 404（適用済みのカタログは収集中にしか無いので、収集を止めたまま設定できるよう保存済みで見る）。`POST` は今すぐ 1 回実行して結果を返し、適用済みのカタログに無い接続は 404。手動は Hub の `enabled` を問わず出せる（PLC に書くのは editor の明示の操作で、ゲストの `clock_set` と運用書込みの loader のゲートは残る）。実行は `tokio::spawn` で切り離し、要求の接続が切れても監査と直近の結果を書き切る。PUT の保存と監査も同じく切り離す。切り離したタスクは Hub 共有の数え（`crate::detached`、`CollectorManager::detached` の `spawn`。API キーの変更・OPC UA 設定の PUT と同じもの）で数え、Hub の終了は冒頭で受付を閉じて以後の POST・PUT を 503 にし、HTTP サーバーを止めた後・pool を閉じる前に残りを待つ（PLC に送った後の監査を pool の close と競らせない）。OpenAPI（`/api/v1/openapi.json`）に 3 本を載せ、説明に危険を書いた。
8. MCP: 読むだけの `get_clock_sync`（引数は接続 id、admin スコープ、REST の GET と同じ形、監査しない）。実行と設定の変更のツールは足さない。
9. UI はこの段では足さない（REST と MCP だけ）。
10. PLC の時計の読取りは H3 として別に行う（2026-11-07 オーナー決定。表示と前後の値の監査に使い、ずれが小さいときに合わせない、はしない）。

危険（OpenAPI の説明にも書いた）: PLC 自身の SNTP・PLC のプログラム・GOT の時刻同期などが同じ時計を合わせていると互いに上書きする。毎回 PLC のイベント履歴に「時計設定」（イベントコード 24000）が残るので、間隔を短くしすぎない。

- 単体（`banto-hub-core`）: 時刻表の一致（hourly・daily の 0／59／23 の境界）、二重に出さない（同じ分・巻き戻し）、追いつかない（分の飛び越し）、PUT の本文の検証（範囲外・未知のキー・形の違い）、結果の写像の全分岐（純関数の表）、表の CHECK、送らない分岐（セッションが無い・実行中）の監査、スケジューラが合図で止まること、REST の既定値・保存・監査・400・404・403。
- 模擬受入れ（t12、`apps/banto-hub/host/tests/t12_slmp_wasm_clock`、6 件、運用書込みの loader と模擬の SLMP 機器）: (1) 定期: `enabled` が偽の間は合う分でも出さず、真にした後は評価済みの分を追いかけず、次に合う分で 1 回だけ 4 要求（SD210–SD216 に渡した現地時刻 2026-11-07 09:05:30）を送り、同じ分の再評価・次の分・巻き戻した同じ分では送らない。成功が監査（`schedule`・`ok`・actor なし）と GET に出る。(2) 手動の POST で成功（監査は `rest`・`admin`）。(3) `clock_set` が未設定・無効なら `rejected`、読取り専用の loader では `notOffered`、どちらも時計合わせの要求を送らない。(4) 標準 Q（3E）は `unsupported`、2 段目の終了コード C059 は `device`（`code` 0xC059）、SM210 が 3 回 ON は `verificationFailed`、SM210 が最初から ON は `rejected`。どれも接続は保ち、監査に残る。(5) 応答しない PLC は `unknown` で、broker は張り直し、時計合わせは再送しない。(6) viewer は GET・PUT・POST とも 403、収集していない接続の POST は 404。CI の rust-wasm に段を足した（6 件）。
- 未確認: 実機（ゲストの「H の残件」1–3 のまま）。設定の説明の UI。Windows での `chrono::Local` の現地時刻（手元は Linux だけで確かめた）。

2026-11-07（模擬のみ・実機未確認）: H3（PLC の時計の読取り、ゲスト banto-driver-wasm #78、tyaro/banto-industrial#1053 の H の続き）を、SLMP の固定 `5281578`（Modbus の固定は `f6c8781` のまま）で受け入れた。使い道は 2026-11-07 のオーナー決定のとおり (a) 表示（REST の GET・MCP で PLC の時刻・PC の時刻・差）と (b) 時計合わせの前後の値を監査に残すことで、ずれが小さいときに合わせない、はしない。

第 1 段（ドライバ層、H1 と同じ範囲）:

- WIT: ゲストの `clock-read-v1.wit`（interface `clock-read-v1` と world `driver-with-write-diagnostics-clock-and-clock-read-v1`・`driver-with-clock-read-v1`）をバイト単位で写した。既存の 4 つの WIT は変えていない（5 つとも blob はゲストと一致）。WIT の `civil-time` は `clock-read-v1` の中の別の record（小さい world が `clock-v1` を import できないため）だが欄は同じで、WASM アダプタが driver-api の `CivilTime` に写す。
- `crates/banto-driver-api`: `ClockReadProgress`（`Send(Step)`／`Done(Result<CivilTime, ItemError>)`）、`FrameConnection` の既定つきの `supports_clock_read`（既定は偽）と `read_clock_next`、`Session` の既定つきの `read_clock`（既定は `Ok(None)` = 契約なし）。
- `crates/banto-driver-engine`: `MAX_CLOCK_READ_STEPS = 4`（ゲストは 2 段）。時計合わせと違い PLC に何も書かないので、書けない（読取り専用の）接続でも出す。上限超過（C16）・途中の致命・外側のキャンセルは接続を閉じる。`Done(Err)` は接続を保つ。
- `crates/banto-broker`: `Job::ReadClock` と `BrokerHandle::read_clock`。書かないので `ReadOnlyHandle` にも `read_clock` を付けた（同じジョブを出すだけ）。上限は `clock_read_job_limit` = `4 × 2T + 1 s`。超えたら致命で再接続し、broker は再送しない。同じ接続の他のジョブとは既存のキューで逐次。
- `crates/banto-driver-wasm`: world `driver-with-clock-read-v1` を bind し、`banto:driver/clock-read-v1@1.0.0` の export の有無（`get_export_index`）で判別する。loader を問わず出す（読取り専用の loader でも。ゲストの設計の案 1）。ゲストに設定は無い（`clock_set` は見ない）。
- component（`crates/banto-driver-wasm/tests/slmp_h3_clock_read.rs`、8 件）: R04CPU（4E・3E）で 0401（SM210–SM213 の 4 点のビット読み）と 0401（SD210–SD216 のワード読み、BIN）の 2 要求がバイト単位で期待どおり（ゲストの `slmp-runtime/tests/clock_read.rs` と同じ）で `Ok`、同じ接続で D の読取りも続けてできる。Q02UCPU は Q のデバイス指定で SD210–SD213（BCD。QCPU のマニュアルの例 1993-07-31 10:35:48 も）。SM213 が OFF・SM210 が ON は状態の読取り 1 回で `Rejected`。BCD でない桁・暦にない日付は `VerificationFailed`。各段の終了コードは `Device` でそれ以降は送らない。Q25PRH・FX5 は何も送らずに `Unsupported`。短い応答は致命で接続を閉じる。読取り専用の loader は読取りを出し、時計合わせは出さない。旧固定 `ea4d84c` では読取りが出ない（`Ok(None)`、`diagnostics_v1.rs` の回帰）。
- 単体: engine 5 件（成功で接続を保つ、`Done(Err)` をそのまま返して接続を保つ、上限を超える `Send` で致命・接続を閉じる、ドライバの致命・期限切れ・キャンセルで接続を閉じる、契約なしで何も送らないが書けない接続でも読める）、broker 2 件（`ReadOnlyHandle` からも結果をそのまま返し他のジョブと逐次、終わらない読取りは上限で致命になり再接続後も再送しない）。

第 2 段（Hub、H2 と同じ範囲）。実装は `apps/banto-hub/core/src/clock_read.rs`（モジュール doc に設計意図。結果の分類・差・使い回し）と `clock_sync.rs`（`ClockSync::read_plc_clock`・前後の読取り）、`rest/clock_sync.rs`。決めたこと（判断事項）:

1. 結果は `ok`・`unsupported`・`rejected`・`device`（`code` に終了コード）・`verificationFailed`・`unavailable`（送っていない: セッションが無い・切断中・broker が送る前に断った）・`unknown`（致命・時間切れ。読取りは何も書かないので時計には影響せず、接続は再接続される）・`notOffered`（`Ok(None)`）。写像は H2 の `classify` と同じ形の純関数 `classify_read`。`verificationFailed` の文は driver-api の「書込み後の確認読み」が読取りに合わないので自前の文にした。
2. 記録 `ClockReading` は `{at（UTC の RFC3339 秒）, result, code, message, plcTime, pcTime, offsetSeconds}`。`plcTime`（`YYYY-MM-DDTHH:MM:SS`）と `offsetSeconds`（`plcTime − pcTime` の秒、PLC が進んでいれば正）は `ok` のときだけ。`pcTime` は broker の応答を受けた直後（送らなかったときは送らないと決めたとき）の PC の現地時刻を秒に切り捨てたもので、常に載る。どちらも秒に切り捨てなので差には ±1 s の誤差がある。PC の時刻は時計合わせと同じ差し替えられる現地時刻（`ClockSync` の `now`）。
3. REST `GET /api/plc-connections/{id}/clock`: editor（PLC と通信するため。診断の GET・H2 の 3 本と同じ。拒否は `plc_connections` の `denied` として監査）、保存済みの接続に無ければ 404。応答は `ClockReading` に `cached` を足したもの。Hub の `clockSync.enabled` は問わない。ハンドルは非ダイヤルの読取り専用の peek（`HubSessions::handle_for`）で、無い・`Connected` でなければ送らずに `unavailable`。読取りは監査しない（表示だけ）。PLC に何も書かず残すものも無いので、切り離しタスクは作らず handler の中で待つ。OpenAPI に載せ、説明に「PLC に何も書かない。PLC のプログラムで SM213 を ON にしておく必要がある（OFF なら rejected）。iQ-R 系と標準 Q・L だけ（他は unsupported）。直近の結果を 5 秒使い回す」を書いた。
4. 使い回し: 接続ごとに直近の結果を読み終えたときから 5 秒（`CLOCK_READ_REUSE`）使い回す（`cached: true`）。接続ごとの `tokio::sync::Mutex` で同じ接続の同時の要求を 1 回の読取りにまとめる。結果が `ok` でなくても使い回す（PLC に連打しないため）が、`unavailable` は使い回さない。時計合わせ（H2）を 1 回実行したら（`busy`・`unavailable` の回も含めて）その接続の使い回しを捨てる。捨てるときは世代を進め、捨てた時点で走っていた読取りの結果は保存しない（時計合わせの前の値を後から使い回さない）。
5. MCP: `read_plc_clock`（admin スコープ、引数は接続 id。`get_clock_sync` と同じ扱い）。REST の GET と同じサービス・使い回しを通り、同じ形を返す。監査しない。
6. 時計合わせの前後の読取り: `ClockSyncRecord`（直近の結果・POST の応答・監査の `detail`）に `before`・`after`（`ClockReading` か `null`）を足した（`detail` は JSON なので migration は要らない）。手順は、`busy`・ハンドル無し・切断中の判定（この 3 つは前後とも `null`）→ 送る直前に同じ書込みハンドルで新しく読む（使い回さない）→ `set_clock_now`（時刻はジョブ開始時に作るのは変えない）→ 直後に新しく読む。読取りの結果が何であっても時計合わせは止めず、`result` と監査の `result` 列（`ok`／`failed`）も時計合わせの結果だけで決める。時計合わせが `unknown` などで接続が落ちていても直後の読取りは試みる（切断中なら送らずに `unavailable` が残る）。POST の handler には時間の上限が無いので足すものは無い（1 回の実行の上限は `clock_job_limit + 2 × clock_read_job_limit`）。

- 単体（`banto-hub-core`）: `classify_read` の全分岐（純関数の表）、差（正・負・年をまたぐ・暦にない PLC の時刻）、記録の形、使い回し（5 秒以内は 1 回・過ぎたら読む・`unavailable` は使い回さない・捨てる・読取りの途中で捨てたら保存しない・同時の要求をまとめる）、セッションが無いときの GET の `unavailable` と送らなかった回の前後が `null`。REST（`rest/tests/clock_read.rs`）: editor と admin は 200 で応答の形、viewer は 403（拒否は監査）、保存済みに無い接続は 404、読取りは監査されない。MCP（`host/tests/mcp.rs`）: 形・`isError`（read スコープ・保存済みに無い接続）。
- 模擬受入れ（t12、`apps/banto-hub/host/tests/t12_slmp_wasm_clock` に 5 件を足して 11 件。模擬機器は時計の読取りの要求を時計合わせの要求とは別に記録し、書かれた SD210–SD216 を以後の読取りで返す）: (7) R04CPU（BIN）と Q02UCPU（BCD）で GET が PLC の時刻 09:00:00・PC の時刻 09:05:30・差 −330 を返し、2 要求がバイト単位で期待どおり。Hub の `enabled` は既定の偽、ゲストの `clock_set` も無し。5 秒以内の 2 回目は送らない（`cached: true`）。監査に残らない。(8) SM213 が OFF なら `rejected`（状態の読取り 1 回）で、それも使い回す。(9) 読取り専用の loader の接続でも読める。(10) 手動の時計合わせの応答と監査の `detail` に `before`（09:00:00、−330）と `after`（09:05:30、0）が載り、その後の GET は使い回さずに読む。(11) 直前の読取りが `rejected` でも時計合わせは `ok` まで進む。H2 の 6 件はそのまま通る（応答しない PLC の回では、直前の読取りが `ok` で直後の読取りも記録に残る）。CI の rust-wasm の H2 の段の件数を 6 から 11 にした。
- 未確認: 実機（ゲストの「H3 の残件」: SM213 なしで SD210 以降が更新されるか、SM213 が ON の間の更新の頻度）。UI。

## 11. 未決事項（オーナー判断）

- ~~**Q1**: KEYENCE KV（#438）を最初のプラグインにするか、§3.4.1 のとおり組み込みで進めるか~~ → **2026-10-30 オーナー決定: WASM のドライバで進める**（組み込みの `banto-driver-*` には足さない。#845 の「PLC 固有の仕様は tyaro/banto-driver-wasm にまとめる」と同じ向き）。対象は #438・#694（通信・読取り・書込み）・#695（シミュレーション）。
  - **当分は実機が無い**。受入れは SLMP・Modbus のゲストと同じく、component の試験（ゲスト側）と Hub の模擬受入れ（t12）だけで行い、記録には「模擬のみ・実機未確認」と書く。実機で確かめるまで、対応する機種・ユニットを名乗らない（#438 の「未検証機種を含む全 KV シリーズ対応は宣言しない」のとおり）。
  - [tyaro/kv-link](https://github.com/tyaro/kv-link) の資産（KV-XLE02 のマニュアルとの照合・アドレス文法・型・構造化エラー・モック）をゲストでどう使うかは、ゲスト側の計画（banto-driver-wasm の `docs/PLAN.md`）で決める。
- **Q2**: 第三者（SI・メーカー）製プラグインを受け入れるか。受け入れるなら鍵の発行、読み取り専用（`driver` world のみ）への制限、UI ブリッジのさらなる制限が要る。
- **Q3**: 既存の SLMP / Modbus TCP をプラグインへ移すか。2026-10-19 オーナー決定で「一括では移さず、併用期間を経て使えると判断してから移行」と方針は決まった。移すかどうか・時期は併用の結果を見て判断する。
- **Q4**: 接続ごとのバージョン固定が必要か（v1 は Hub 全体で 1 バージョン）。
- **Q5**: タグの browse（PLC からのシンボル一覧取得）を v1 に入れるか。入れるなら `browse` インターフェースと、UI ブリッジからのオンライン操作の扱いを追加設計する。
- ~~**Q6**: UDP・シリアル（RS-232C/485）のトランスポートを対象にするか~~ → **2026-10-29 オーナー決定（#916 の 2026-10-28 の作者決定）: UDP を対象にする**。manifest の `transports` に `udp` を認め（§4.2）、契約は io-design §7.6。UDP でも SLMP の 3E を許す（シリアル番号が無く、重複したデータグラムで古い値を取り込む危険は利用者の責任とし、ゲストの設定の説明に明記する）。4E の書込みは最初から許し、ゲートは TCP と同じ（ゲストの allowlist と Hub の独立ゲートの両方）。自動再送はせず、応答が失われた書込みは結果不明（`Uncertain`）として監査に残す。UDP ではハンドシェイクが 1 往復以上必須（0 往復の接続は Hub が断る）。**シリアルは引き続き対象外**（#803）。
- **Q7**: ロックダウン中のプラグイン導入・更新を許すか（管理面の他操作と同じ扱いにするか）。
- **Q8**: x86 / COM / デスクトップ前提のベンダー部品（JoyWatcher・OPC DA 等）向けの別プロセス枠を別途設計するか（本書の対象外）。
- ~~**Q9**: プラグイン接続のシミュレーションを提供するか（v1 は断る）。提供するなら WIT にシミュレータ（応答を組み立てる側）のインターフェースを足す~~ → **R2b-5（2026-10-24）で解消**: プラグイン接続も値の層のシミュレータで動き、ゲストにシミュレータを持たせる必要は無くなった（§5 の「シミュレーション」）。
- **Q10**: 失効一覧の配り方（Hub の更新に同梱するか、署名付きで別に配るか）。
- ~~**Q11**: 同梱 UI のファイルを認証なしで配信してよいか~~ → **2026-10-19 オーナー決定: 許容する**（§8。一覧 API は認証付きのまま・UI のファイルに秘密を入れない・欠陥版は失効で止める、を前提とする）。

## 12. 破壊的変更を許す場合の構成案（2026-10-19 検討・未決定）

§3〜§10 は今の Hub の構造に WASM を「追加」する案。本節は、banto-hub 側の破壊的変更を許す場合に、より整理された形へ組み替える案を記録する。§2.1 のとおり計画は保留中で、本節も採否は未決定。

### 12.1 今の構造の歪み

1. **プロトコルの知識が散っている**: `banto_plc::Address`（`ModbusRef` / `Slmp` だけ）、`banto-collect` の `Protocol` / `ClientProtocol` / `ProtocolConfig`、broker の `DRIVERS`、`banto-tags` の `ALLOWED_PROTOCOLS`、SQL の `CHECK`、`protocol` の文字列で分岐する変換箇所（§3.2）、`BrokerSimRegistry::resolve` のシミュレータの選択、UI の分岐、設定パッケージの固定値。
2. **読み書きの経路が 2 系統**: `banto-collect` の直結（`PlcClient`、ChronoGazer）と broker（`BrokerSession`、Hub）。
3. **I/O が各ドライバの中にある**: SLMP は `tyaro/slmp` の非同期クライアント、Modbus TCP は `banto-plc` の自前クライアント。分割受信・遅延応答・再接続などの事故対策が実装ごとに重複する。
4. **接続設定がプロトコル別の固定列**（`postgres` の `password` が平文など）。
5. **UI に Rust 側の手書きの写し**がある（`slmpDeviceTable.ts`）。

### 12.2 案: すべてをドライバとして扱う

```
                   ┌──────────── DriverRegistry（唯一の正）────────────┐
 tags の検証 ─────▶│ id → Driver（組み込み: Modbus / SLMP / ...）       │
 REST・MCP・CSV ──▶│        （WASM: KV / ...）                          │
 UI（スキーマ・UI）▶│ フレーム型: config / address / plan / decode /    │
 設定パッケージ ──▶│             encode / handshake（I/O を持たない）    │
                   │ セッション型: I/O を自分で持つ（OPC UA クライアント │
                   │             ・外部 DB・hub 接続。組み込みのみ）     │
                   └─────────────────────────┬─────────────────────────┘
                                             ▼
                 Session エンジン（1 つ。Hub と ChronoGazer で共用）
                 TCP・終端判定・タイムアウト・再接続・1 接続 = 1 ソケット
                 致命 / 非致命の分類・読み書きの直列化
```

1. **ドライバの trait を §6 の WIT と同じ形にする**。組み込みの Modbus / SLMP はこの trait をネイティブに実装し、WASM ホストはこの trait のもう 1 つの実装になる。Hub から見て組み込みと WASM の区別がなくなる。
2. **ドライバは 2 種類**:
   - **フレーム型**（I/O を持たない）: 要求-応答のプロトコル。I/O は Session エンジンが持つ。組み込みと WASM のどちらでも実装できる。
   - **セッション型**（I/O を自分で持つ）: OPC UA クライアント（§6.1 で WASM の対象外）・外部 DB（`db_source`）・`hub` 接続のように、セキュアチャネル・セッション・購読の状態を持つもの。組み込みのみ。レジストリ・アドレス・設定・UI の扱いはフレーム型と同じ。
     外から値を送り込まれる `push`（外部 DB 連携の取り込み口。ChronoGazer の `hub` 接続と同じ種類）もセッション型に入る（2026-10-19 オーナー決定、[banto-hub-external-db-design.md](banto-hub-external-db-design.md) §9.5）。
3. **Session エンジンを 1 つにする**。`PlcClient` と `BrokerSession` を 1 つの `Session` にまとめ、Hub と ChronoGazer が同じエンジンを使う。#337（1 接続 = 1 ソケット）・H9（構造化エラー）・遅延応答の扱いが 1 か所になり、新しいフレーム型ドライバは自動的にこれを受け継ぐ。
4. **アドレスを不透明にする**。`Address` enum をやめ、`DriverAddress { driver, canonical, area, writable_area, word_count, bit }` にする。中身はドライバの `parse` だけが作る。DB には正規化済みの文字列を保存し、`protocol` の文字列による分岐をなくす。
5. **レジストリを唯一の正にする**。SQL の `CHECK`・`ALLOWED_PROTOCOLS`・`DRIVERS`・UI の分岐・設定パッケージの固定値を、すべて `DriverRegistry` の参照に置き換える。接続は共通列（名前・有効・ドライバ ID）と `settings`（JSON。ドライバのスキーマで検証、秘密値は `banto-secret-store`）にする。
6. **シミュレーションを値の層に移す**（**実装済み、2026-10-24、R2b-5。[banto-hub-driver-sim-design.md](banto-hub-driver-sim-design.md)**）。プロトコル別のシミュレータの代わりに、「正規化アドレス → 値」を持つ汎用の `SimSession` を 1 つ置く。WASM を含むすべてのドライバがそのままシミュレーションでき、#363（書き込みはシミュレータへ反映）も成り立つ。代わりにシミュレーションではワイヤの処理を通らなくなるので、ワイヤのテストはドライバ側の CI で行う（§3.4.1 の決まり 3 と同じ）。
7. **UI はドライバから組み立てる**（R2b-6 で実装済み、2026-10-24。iframe の枠は W 段）。汎用フォーム（スキーマ）とドライバ同梱の UI（§8）で画面を組み、アドレスの補助は API 経由でドライバに聞く。プロトコル別の Svelte の分岐と TS の写しをなくす。

### 12.3 併用の方針（§2.1）との関係

同じプロトコル ID に「組み込み版」と「WASM 版」の 2 つのフレーム型の実装を登録し、**接続ごとにどちらを使うか選べる**ようにする。

- 1 台の PLC に組み込み版と WASM 版の接続を並べ、値を比べられる。
- 問題があれば、接続の設定 1 つで組み込み版に戻せる。

§2.1 の「併用を経て、使えると判断してから移行」を仕組みとして支える。

### 12.4 破壊的変更の範囲と代償

壊れるもの:

- DB: `plc_connections` の固定列から `settings` の JSON への移行、`CHECK` の撤去、既存タグのアドレスの正規化。
- REST / MCP / 設定パッケージ: 接続の項目の形が変わる（設定パッケージは形式の版を上げる）。
- crate の公開 API: `banto-collect` の `Protocol` などを使う ChronoGazer も同時に直す。

代償と危険:

- 作業量が大きい（broker・collect・plc・plc-write・tags・Hub の REST / MCP・UI・ChronoGazer）。
- Session エンジンを 1 つにするとき、今の Modbus / SLMP のクライアントが持つ「事故で学んだ振る舞い」（分割受信、遅れて届いた応答を次の要求に流用しない等）を落とす危険がある。**今の振る舞いを固定するテストを新しいエンジンに対しても全部通すことを、移行の条件にする**。
- 既存タグのアドレスの正規化では、新しい `parse` が今と完全に同じ解釈をする必要がある。~~§7.4 の preflight と同じ仕組みで、移行時に全件を照合する。~~ → **2026-10-22 のオーナー決定（使用者無しの前提）で行わない**（[banto-hub-driver-address-design.md](banto-hub-driver-address-design.md) §0.1）。R2b-2 は保存時にドライバで解析して正規形で保存するだけで、既存の行は書き換えない。

### 12.5 進め方（案）

| 段階 | 内容                                                                                                                                                                         | 破壊的変更 |
| ---- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------- |
| R1   | I/O を持たない trait と `DriverRegistry` を入れ、Modbus / SLMP をネイティブ実装へ載せ替える。WASM はまだ入れない。振る舞いを変えないリファクタで、既存テストが判定基準になる | なし       |
| R2   | Session エンジンの一本化、アドレスの不透明化、DB と REST / MCP / 設定パッケージの移行、ChronoGazer の追従                                                                    | あり       |
| R3   | WASM ホストを「もう 1 つの Driver 実装」として追加し、接続ごとに組み込み / WASM を選べるようにする（§10 の P1〜P4 に相当）                                                   | なし       |
| R4   | 併用で比べ、使えると判断したプロトコルから移行する（Q3）                                                                                                                     | なし       |

R1 は WASM と無関係に 12.1 の 1（散らばったプロトコルの分岐）を減らすので、プラグイン計画を保留したままでも単独で価値がある。

### 12.6 R1 の CI 上の利点: ドライバ API で依存を逆転する（2026-10-19 検討）

> **R2b-3 の後の現状（2026-10-23）**: Modbus TCP・SLMP は `banto-driver-api` の `FrameDriver` になり、通信は公開 crate `banto-driver-engine` が担う（`banto-broker` が API とエンジンに依存し、`banto-plc` / `banto-plc-write` の I/O は消えた。計画・デコード・フレーム・シミュレータの部品は残る。io-design Q7）。CI の `driver-api` ジョブと判定（`ci-changes.mjs` の `run_driver_api`）は `banto-driver-api`・`banto-driver-engine`・`banto-driver-conformance` の 3 つを対象にしている（MSRV・features・semver。semver の基準は crate ごとの `<crate 名>-v*` のタグで、最初の公開まで飛ばす）。`banto-plc` の実装本体の変更が core・ChronoGazer に届く点は、部品が残るので完全には外れていない。下の「今の依存」は 2026-10-19 の時点の記録。

R1 は WASM と無関係に、**ドライバだけの変更で CI を短くする**利点がある。

**今の依存（2026-10-19 に `cargo metadata` で確認）**:

```
banto-plc（SLMP / Modbus の実装と共通の型が同居）
  ├─ banto-collect ─┬─ chronogazer-core ── chronogazer
  │                 └─ banto-hub-core
  ├─ banto-broker ──── banto-hub-core
  └─ banto-plc-write ─ banto-broker
banto-hub-core は banto-plc を直接も使う（Address・DataType・TagValue・ReadRequest など）
```

型と実装が同じ crate にあるため、ドライバの中身だけを変えても `banto-hub-core` と `chronogazer-core` が作り直される。2026-10-19 に CI の判定（`.github/scripts/ci-changes.mjs`）へ変更ファイルの例を渡して確かめた結果:

| 変更したもの                                                  | Rust の対象                                                  | E2E                |
| ------------------------------------------------------------- | ------------------------------------------------------------ | ------------------ |
| `crates/banto-plc`                                            | 10 パッケージ（`banto-hub-core`・`chronogazer-core` を含む） | Hub と ChronoGazer |
| `crates/banto-plc-write`                                      | 5 パッケージ（`banto-hub-core` を含む）                      | Hub                |
| `crates/banto-broker`                                         | 4 パッケージ（`banto-hub-core` を含む）                      | Hub                |
| 別リポジトリのドライバの版上げ（`Cargo.toml` / `Cargo.lock`） | `--workspace`                                                | Hub と ChronoGazer |

同日の cargo timings（main への push のフル CI 3 回）では、CI の長さを決める Windows の `rust` ジョブ（17〜25 分）の大半が nextest で、ビルドの CPU 時間の約 45% が `banto-hub-core`（統合テストのバイナリ約 20 本）だった。つまり今は、ドライバだけの変更でも実質的にほぼ全体の CI が走る。

**依存を逆転した形**:

```
banto-driver-api（型と trait だけ。ロジックなし）
  ├─ banto-collect / banto-broker ── banto-hub-core / chronogazer-core   ← API にだけ依存
  └─ 各ドライバ（SLMP / Modbus / KV ...）                                ← API にだけ依存

banto-drivers（組み立て役: 組み込みドライバの一覧）
  └─ アプリのバイナリ（banto-hub・chronogazer）だけがこれに依存
```

（2026-10-19 補足、api-design Q2: **これは R2b の後の姿**。R1 の時点では、core が `banto-drivers` を dev-dependency で持つので薄いアダプター crate の変更でも core が対象に残り、`banto-plc` の実装本体の変更は今までどおり core と ChronoGazer に届く。core が外れるのは R2b-0、実装本体の分は R2b-2 / R2b-3 の後。下の条件 4 の「API の crate が提供する」は別 crate `banto-driver-conformance`、条件 5 の `ReadRequest` 系は R2b-2 で `TagSpec` に替わる。詳細は [banto-driver-api-design.md](banto-driver-api-design.md) §2.5・§13）

ドライバを変えたときの CI の対象は、そのドライバ・`banto-drivers`・アプリのバイナリ crate だけになり、`banto-hub-core` と `chronogazer-core` のテスト（偽のドライバで動く）は対象から外れる。Windows の `rust` ジョブは数分程度に縮むと見込む（推定。ドライバと組み立て役のテストの量による）。E2E は組み立て後のバイナリを使うので走らせ続ける（そのための CI の判定の変更は条件 6）。別ジョブで並列なので全体の長さは決めない。

**成り立たせる条件**:

1. **API の crate を小さく、動かさない**。API を変えると全部に波及するので、置くのは型と trait だけにする。
2. **アドレスを不透明にする（12.2 の 4）**。今の `Address` enum（`ModbusRef` / `Slmp`）を API に移すと、プロトコルを足すたびに API が変わり、毎回全体が走る。API には「ドライバ ID + 正規化した文字列」（と `area` 等の解釈結果）だけを置き、解釈はドライバに任せる。
3. **接続部を差し込み式にする**。`banto-collect` には既に `ClientFactory` の差し込み口がある。broker の固定の登録表 `DRIVERS` を、組み立て役から渡せる形に変える。
4. **適合テストを API の crate が提供する**。どのドライバも満たすべき振る舞い（エラーの致命 / 非致命の分類、結果の件数と順序、分割受信・遅延応答など）のテスト一式を API の crate に置き、各ドライバの CI で走らせる。アプリ側の統合テストが偽のドライバになる分、実ドライバの確認はこの適合テストと E2E で担保する。
5. **共通の型を API へ移す**。`DataType`・`TagValue`・`ReadRequest` 系・エラーの分類を `banto-plc` から API の crate へ移す。
6. **CI の判定を組み立て役に合わせて直す**。今の判定（`.github/scripts/ci-changes.mjs` の `JOB_PACKAGES`）は、Hub / ChronoGazer の E2E を `banto-hub-core` / `chronogazer-core` への逆依存の到達で選ぶ。依存を逆転してドライバが core から外れると、ドライバだけの変更では**両方の E2E も対象から外れてしまい**、実ドライバの確認を E2E に任せる前提が崩れる。E2E の対象判定を、実際にドライバを組み込む組み立て役（`banto-drivers`）とアプリのバイナリの package に合わせて更新し、「ドライバだけの変更 → core のテストは対象外・両方の E2E は対象」を確かめる判定の回帰テスト（`ci-changes.test.mjs`）を足す。
