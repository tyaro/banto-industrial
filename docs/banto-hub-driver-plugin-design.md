# banto-hub ドライバプラグイン設計（WASM）

作成日: 2026-10-19
状態: **段階的な限定実装中（2026-10-25）**。2026-10-19 の実装保留判断の後、受け皿と P0/P1 の一部（U16/U32 の明示登録、broker 収集・即時読取り）を実装した。非 Modbus アドレスの実 component 試験を追加した。通常起動の明示選択と署名再検証は限定実装中。実運用の信頼鍵・保護配置の設定、導入・更新は未完了。本書全体の承認を意味せず、§2 の D1〜D6 は当初の方向・§11 は未決事項として扱う。着手する場合も一括移行はせず、組み込みドライバとの併用期間を経て判断する。§2 の D1〜D6 は 2026-10-19 のオーナーとの議論で出た方向で、**本書の承認をもって決定**とする（承認後に日付付きの決定記録へ移す）。§11 は未決事項、§12 は破壊的変更を許す場合の構成案（2026-10-19 検討・未決定。ただし外部 DB の取り込み口 `push` の形は external-db-design §9.5 でオーナー決定）（Q11 は 2026-10-19 オーナー決定で解決済み）。2026-10-19 に上位モデルの監査を受け、指摘（現状記述の誤り・broker 構造・WIT の読み取り単位・同梱 UI と展開経路の安全・レジストリ変更）を反映済み。同日の Codex レビュー（ハンドシェイクを応答依存で進める・有効化前の preflight・fingerprint に内容のダイジェスト・同梱 UI の配信の認証）と、オーナーのレビュー（プラグイン固有のアドレスを broker まで運ぶ経路 §3.2、ハンドシェイクの拒否応答）も反映済み。同日、§6.2 の WIT を R1 の詳細設計 [banto-driver-api-design.md](banto-driver-api-design.md) §9.2 に合わせて改訂し、§12.6 に「R1 の時点の CI の姿」を補足した。
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
| `transports`      | ○    | v1 は `tcp` のみ（UDP・シリアルは Q6）                                      |
| `capabilities`    | ○    | `read` / `write`。world の選択（§6.4）と一致させる                          |
| `ui`              |      | 同梱 UI のエントリ（例: `ui/index.html`）                                   |

**組み込みの ID（`modbus-tcp` / `slmp` / `postgres` / `hub` / `virtual` 等）はプラグインで上書きできない**。同じ ID のプラグインは導入時に拒否する。

## 5. レジストリと既存機能の変更

- **`protocol` 列**: SQL の `CHECK` と `ALLOWED_PROTOCOLS` を緩め、組み込みの ID に加えて「導入済みプラグインの ID」を受け付ける。SQL 側は `CHECK` を外し（migration）、検証は `banto-tags` の Rust 側に一本化する。
- **プラグイン固有の設定**: `plc_connections.settings`（JSON）列を足す。共通の列（`host` / `port` / `word_order` / タイムアウト等）は従来どおり固定列。`word_order` はプラグインへ設定として渡す（§6.2）。
- **秘密値**: `config.secret-keys` が返すキーの値は、平文の列には置かず `banto-secret-store`（DPAPI の machine スコープ）に保存する。REST・MCP・UI・設定パッケージ・監査ログでは伏せる（既存の `passwordSet` と同じ見せ方）。既存の `postgres` の平文保存の扱いは本書の範囲外。
- **検証の注入**: `banto-tags` は `banto-plc` に依存しない方針（`banto-plc-address` の lib.rs 参照）を守り、`PlcConnectionService` / タグの検証にプラグイン検証のコールバックを渡す形にする。
- **設定パッケージ**: `protocol` の許容値を導入済みプラグインに広げ、必要なプラグインの `(id, version)` を記録する。取り込み先に足りないプラグインがあれば、**取り込み前に一覧で断る**（取り込みは 1 トランザクションの全件検証なので、部分取り込みはしない）。
- **未適用キュー**: 適用時に、プラグインの有無と版を再検証する（キューに積んだ時点と版が変わっていれば検証し直す）。
- **冗長化**: 設定世代の fingerprint に「必要プラグインの `(id, version, 内容のダイジェスト, 署名鍵の ID)` の集合」を含め、両系で揃っていることを確かめる。`(id, version)` だけでは、同じ版番号で中身の違うパッケージを見分けられない（版の不変性は §7.1 でも守るが、fingerprint でも確かめる）。設定パッケージに記録する必要プラグインも同じ組にする。
- **シミュレーション**: v1 ではプラグイン接続の `simulation = true` を**項目エラーで断る**（`virtual` と同じ扱い）。`BrokerSimRegistry::resolve` の既定が SLMP シミュレータに倒れるため、そのままではプラグインのフレームを SLMP シミュレータへ投げてしまう。全シミュレーション運転時は、プラグイン接続は起動しない。プラグイン側にシミュレータ（応答を組み立てる側）を持たせるかは Q9。
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
  // rejected = プラグイン自身が断った・確認で不一致を見つけた（同じワードの同じビットへ true / false が同時、
  // RMW の確認読みの不一致）。機器の拒否（device）でも設定の不整合（unsupported）でもない非致命
  variant item-error { device(device-error), unsupported(string), rejected(string) }
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

## 11. 未決事項（オーナー判断）

- **Q1**: KEYENCE KV（#438）を最初のプラグインにするか、§3.4.1 のとおり組み込みで進めるか。
- **Q2**: 第三者（SI・メーカー）製プラグインを受け入れるか。受け入れるなら鍵の発行、読み取り専用（`driver` world のみ）への制限、UI ブリッジのさらなる制限が要る。
- **Q3**: 既存の SLMP / Modbus TCP をプラグインへ移すか。2026-10-19 オーナー決定で「一括では移さず、併用期間を経て使えると判断してから移行」と方針は決まった。移すかどうか・時期は併用の結果を見て判断する。
- **Q4**: 接続ごとのバージョン固定が必要か（v1 は Hub 全体で 1 バージョン）。
- **Q5**: タグの browse（PLC からのシンボル一覧取得）を v1 に入れるか。入れるなら `browse` インターフェースと、UI ブリッジからのオンライン操作の扱いを追加設計する。
- **Q6**: UDP・シリアル（RS-232C/485）のトランスポートを対象にするか（v1 は TCP のみ。UDP ではデータグラム境界が終端になる）。
- **Q7**: ロックダウン中のプラグイン導入・更新を許すか（管理面の他操作と同じ扱いにするか）。
- **Q8**: x86 / COM / デスクトップ前提のベンダー部品（JoyWatcher・OPC DA 等）向けの別プロセス枠を別途設計するか（本書の対象外）。
- **Q9**: プラグイン接続のシミュレーションを提供するか（v1 は断る）。提供するなら WIT にシミュレータ（応答を組み立てる側）のインターフェースを足す。
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
