# banto-hub ドライバプラグイン設計（WASM）

作成日: 2026-10-19
状態: **Draft・オーナー決定待ち（2026-10-19）**。実装は未着手。§2 は 2026-10-19 のオーナーとの議論で合意した方向、§10 は未決事項。
対象: PLC ドライバ（メーカー固有のプロトコルと登録 UI）を、banto-hub 本体を再ビルド・再リリースせずに追加・更新できるようにする。
関連: [tag-server-design.md](tag-server-design.md) §3.4（PLC 通信ドライバとの関係）・§3.4.1（新しい PLC ドライバの置き場所、#438）・§4（タグ空間のセマンティクス）・§6（書き込み）、
[banto-hub-t17-design.md](banto-hub-t17-design.md)（サービス・profile・UAC）、
[banto-hub-redundancy-design.md](banto-hub-redundancy-design.md)、
[banto-hub-external-db-design.md](banto-hub-external-db-design.md)（Source のプロセス内実装の前例）。

---

## 1. 背景と課題

### 1.1 現状

PLC ドライバは banto-hub のバイナリにコンパイル時に同梱される Rust クレートである（tag-server-design.md §3.4）。

- ワイヤ層: `banto-plc` の `PlcClient` trait（`connect` / `read_batch` / `disconnect`）。実装は Modbus TCP（`crates/banto-plc/src/modbus/`）と SLMP（`tyaro/slmp` をタグ参照）。
- 種別の選択: `banto-collect` の `Protocol` enum（`ModbusTcp` / `Slmp`）と `ProtocolConfig`。
- 再接続・バックオフ・品質判定: `banto-collect`。読み書きの単一セッション直列化: `banto-broker`。書き込み: `banto-plc-write`。
- 書き込み可能な領域: `banto-plc-address`（プロトコルの事実としての `is_writable`）。
- UI: `apps/banto-hub/src` にプロトコルごとの分岐（`ConnectionDrawer.svelte` の `form.protocol === 'slmp'` 等）と、Rust 側を**手で書き写した**アドレス表（`slmpDeviceTable.ts`。正本は `crates/banto-plc/src/slmp/address.rs`）を持つ。

### 1.2 課題

1. **ドライバの追加・更新のたびに Hub 全体の再ビルドと再リリースが要る**。PLC メーカーが違えばプロトコルも登録 UI も個別に作る必要があり、対応メーカーを増やすほど本体のリリースがドライバに引きずられる。
2. **UI のアドレス処理が Rust と TS の二重管理**。T20-④（bit サフィックスの基数を10進→16進へ是正、2026-09-04）では両方を直した。
3. §3.4.1（2026-10-18、#438）でワイヤ層は別リポジトリに出す方針になり CI の重さは軽減されるが、**配布単位は Hub のまま**で 1 は解消しない。

## 2. 方向（2026-10-19 オーナーとの議論）

検討した選択肢と結論:

| 方式                                    | 本体の再ビルド不要 | サービス運用（LocalSystem）での安全性                                                      | 障害の分離                       | 書き込みの安全契約                         | 判定                                             |
| --------------------------------------- | ------------------ | ------------------------------------------------------------------------------------------ | -------------------------------- | ------------------------------------------ | ------------------------------------------------ |
| 静的リンク（現状）                      | ×                  | ◎                                                                                          | ×（同一プロセス）                | ◎（broker・write path がそのまま）         | 主力ドライバは当面このまま                       |
| 別プロセス（kt_iot_hub 型: exe + gRPC） | ○                  | △（子プロセスが LocalSystem を継承、登録 UI は Session 0 で出せない、localhost gRPC 認証） | ◎                                | △（IPC 越しに作り直し）                    | x86/COM 等のベンダー部品に限る（本書対象外）     |
| DLL（動的リンク）                       | ○                  | ×（DLL planting の典型的な攻撃面）                                                         | ×                                | ○                                          | 不採用（Rust に安定 ABI なし、async が越えない） |
| **WASM（wasmtime Component Model）**    | ○                  | ◎（サンドボックス。ファイル・ネットワークに触れない）                                      | ○（trap で捕捉、Hub は落ちない） | ◎（broker・write path・関門は Hub に残る） | **採用方向**                                     |

合意した要点:

- **D1**: 本体と別に追加・更新したいプロトコルドライバは **WASM プラグイン**にする。
- **D2**: PLC メーカーごとに登録 UI が異なるため、**プラグインはメーカー固有の Web UI を同梱する**ことを標準とする（汎用フォームだけでは足りない）。
- **D3**: ただしアドレス関数・設定検証は **WASM 側に必須**とする。REST・MCP・タグ CSV 取り込み・設定パッケージ・未適用キューの適用は iframe の UI を通らないため、サーバー側で同じ検証が要る。
- **D4**: Hub はプラグインを**動的に発見・導入・更新**し、Hub の再起動を要しない。
- **D5**: 書き込みの安全判断（タグの `writable`・API キーの `write` スコープ・受付トグル・頻度制限・監査・シミュレーション拒否・broker の直列化）は **Hub に残す**。プラグインが宣言する「書き込み可能な領域」はプロトコルの事実であって安全判断ではない（`banto-plc-address` の `is_writable` と同じ位置づけ）。
- **D6**: プラグインはフレームを組み立てる側なので Hub はフレームの意味までは検証できない。これは静的リンクのクレートを信頼するのと同種の信頼であり、**出所の保証（署名・保護された置き場所・プラグイン側リポジトリのレビューと CI）**で担保する。

## 3. 全体構成

```
┌──────────────────────── banto-hub（サービス, LocalSystem） ────────────────────────┐
│ 収集（banto-collect）  品質判定  broker（直列化）  書き込みの関門・監査              │
│ ソケット / タイムアウト / 再接続        ← I/O はすべて Hub が持つ                     │
│                                                                                   │
│ Plugin Manager: 発見・署名検証・コンパイル・有効/無効・更新/ロールバック            │
│   ├─ wasmtime Engine ── driver.wasm（サンドボックス、WASI なし）                    │
│   │                      フレーム組み立て / 応答解析 / アドレス関数 / 設定検証       │
│   └─ 静的配信 /plugins/<id>/<ver>/ui/ ── iframe（sandbox）で表示                   │
│                                          postMessage ブリッジ（範囲を絞った API）   │
└───────────────────────────────────────────────────────────────────────────────────┘
```

| 層            | 担当                                                                                            |
| ------------- | ----------------------------------------------------------------------------------------------- |
| Hub（不変）   | 収集・品質・broker・書き込みの関門・監査・トランスポート I/O・プラグイン管理・iframe の sandbox |
| `driver.wasm` | フレームの組み立てと解析・アドレス関数・設定の検証（純粋な計算のみ）                            |
| `ui/`（同梱） | メーカー固有の接続設定・タグ登録画面                                                            |

## 4. パッケージとマニフェスト

### 4.1 パッケージ形式

1 ファイル `<id>-<version>.bantoplugin`（zip）:

```
manifest.json
driver.wasm
ui/            index.html ほか（任意だが D2 により標準）
signature      パッケージ内容のダイジェストに対する署名
```

### 4.2 manifest.json（v1）

| 項目              | 必須 | 内容                                                                          |
| ----------------- | ---- | ----------------------------------------------------------------------------- |
| `manifestVersion` | ○    | `1`                                                                           |
| `id`              | ○    | プロトコル ID。`[a-z0-9][a-z0-9-]*`。レジストリの `protocol` 列にそのまま入る |
| `version`         | ○    | SemVer                                                                        |
| `hubApi`          | ○    | 対応する WIT インターフェースのメジャー版（例: `"1"`）                        |
| `displayName`     | ○    | UI 表示名（例: `KEYENCE KV ホストリンク`）                                    |
| `vendor`          |      | 作成者                                                                        |
| `transports`      | ○    | 必要なトランスポート（`tcp` / `udp`。`serial` は §10 Q6）                     |
| `capabilities`    | ○    | `read` / `write` / `browse` 等。Hub はここにない操作を呼ばない                |
| `ui`              |      | 同梱 UI のエントリ（例: `ui/index.html`）                                     |

**組み込みの ID（`modbus-tcp` / `slmp` / `postgres` / `hub` / `virtual` 等）はプラグインで上書きできない**。同じ ID のプラグインは導入時に拒否する。

## 5. WASM インターフェース（WIT の叩き台）

設計原則:

- **I/O は Hub が持つ**。プラグインはソケットに触れず、「送るバイト列」と「受け取ったバイト列の解釈」だけを担う。WASI は与えない（ファイル・ネットワーク・時刻・乱数なし）。
- 要求-応答型のプロトコル（SLMP / Modbus / KV ホストリンク / FINS 等）を前提にする。接続時のハンドシェイク（FINS/TCP のノードアドレス交換、EtherNet/IP のセッション登録等）は**ステップ列**として表す。
- 1 回の呼び出しは同期・短時間。Hub は epoch interruption と fuel で上限を課し、超過・trap は**その接続の致命エラー**として扱う（broker の `Err = 接続致命` の契約に合わせる）。

```wit
package banto:driver@1.0.0;

interface types {
  record setting { key: string, value: string }
  variant value { bool(bool), int(s64), uint(u64), float(f64), text(string), bytes(list<u8>) }
  // 非致命（そのタグだけ Bad）と致命（接続を張り直す）を型で分ける（h9 の教訓）
  variant item-error { device(string), unsupported(string) }
  variant fatal-error { framing(string), protocol(string), invalid-config(string) }
}

interface session {
  use types.{setting, fatal-error};
  resource connection {
    constructor(settings: list<setting>);
    // 接続直後のハンドシェイク。空なら不要。
    handshake: func() -> result<list<step>, fatal-error>;
  }
  // Hub が実行する 1 往復。expect は応答の終端判定に使う。
  record step { send: list<u8>, expect: frame-rule }
  variant frame-rule { length-prefixed(prefix-spec), terminator(list<u8>), fixed(u32), plugin-decides }
  record prefix-spec { offset: u32, width: u8, big-endian: bool, adjust: s32 }
  // plugin-decides のとき Hub が逐次呼ぶ: 受信済みバッファが 1 応答として完結したか
  is-complete: func(conn: borrow<connection>, buf: list<u8>) -> result<option<u32>, fatal-error>;
}

interface read {
  use types.{value, item-error, fatal-error};
  use session.{connection, step};
  // 読み取り計画: アドレス群を往復単位にまとめる（banto-plc の planning 相当）
  plan: func(conn: borrow<connection>, addresses: list<string>) -> result<list<planned>, fatal-error>;
  record planned { step: step, covers: list<u32> }   // covers = addresses の添字
  decode: func(conn: borrow<connection>, p: planned, response: list<u8>)
    -> result<list<result<value, item-error>>, fatal-error>;
}

interface write {
  use types.{value, item-error, fatal-error};
  use session.{connection, step};
  record write-item { address: string, value: value }
  encode: func(conn: borrow<connection>, items: list<write-item>) -> result<list<step>, fatal-error>;
  check: func(conn: borrow<connection>, response: list<u8>) -> result<list<result<_, item-error>>, fatal-error>;
}

interface address {
  // D3: REST・MCP・CSV・設定パッケージ・UI が同じ関数を使う
  record parsed { canonical: string, area: string, writable-area: bool, bit: option<u8>, word-count: u32 }
  parse: func(text: string, data-type: string) -> result<parsed, string>;
  // 連続登録・オフセットコピー用: n 個先のアドレス
  offset: func(canonical: string, data-type: string, n: s64) -> result<string, string>;
  help: func(locale: string) -> string;
}

interface config {
  use types.{setting};
  record field-error { key: string, message: string }
  validate: func(settings: list<setting>) -> list<field-error>;
  // 秘密として扱う設定キー（パスワード等）。Hub は監査・エクスポートで伏せる
  secret-keys: func() -> list<string>;
}

world driver {
  export session; export read; export address; export config;
  export write;   // capabilities に write があるときだけ Hub が呼ぶ
}
```

補足:

- `value` と Hub のデータ型（スケーリング・ワード順・文字列）の対応は、既存の `banto-plc` のデコード規則と突き合わせて §9 の P0 で確定する。**スケーリングと閾値は Hub 側**（`banto-collect`）に残し、プラグインは生値を返す。
- `writable-area` はプロトコルの事実（D5）。Hub はタグ登録時の早期エラーに使うだけで、書き込み許可の判断には使わない。
- 品質判定（Stale・Bad）は Hub が行う（tag-server-design.md §4）。プラグインの `item-error` / `fatal-error` はその入力。

## 6. 発見・導入・更新

### 6.1 置き場所

- `%ProgramData%\BantoHub\plugins\<id>\<version>\`（展開済みのパッケージ）。
- ACL は管理者とサービスだけが書き込める保護された DACL。作成と検証は `banto-safe-root` の部品（辿らずに開く `pin_dir`、`acl::guard`）を使う。

### 6.2 導入経路

- **標準: Hub の管理 UI / admin REST からのアップロード**。管理者ロールのみ。Hub（サービス）が署名を検証し、自分で 6.1 へ展開する。現場の利用者がフォルダを触る必要がなく、ACL も崩れない。
- 導入・削除・有効化・ロールバックは監査ログに記録する。
- ロックダウン中の扱いは tag-server-design.md §5.6 の管理面と同じ（§10 Q7）。

### 6.3 発見

- 起動時に 6.1 を走査し、各バージョンについて マニフェスト検証 → 署名検証 → `hubApi` 照合 → WASM のコンパイル を行う。
- 導入・更新時はその場で登録し、Hub の再起動を要しない。
- 一覧 API `GET /api/v1/plugins` で、プラグインごとの状態（有効 / 無効とその理由: 署名不正・API 版不一致・コンパイル失敗・ID 衝突）を返す。

### 6.4 更新とロールバック

- **有効なバージョンはプラグイン ID ごとに 1 つ**（Hub 全体の設定）。接続ごとのバージョン固定は v1 では持たない（§10 Q4）。
- 更新時は、そのプラグインを使う接続のタスクだけを作り直す（`Collector::apply_config` の差分適用）。他の接続には触れない。
- 直前のバージョンは残し、管理 UI から 1 操作で戻せる。

### 6.5 プラグインが無いとき

- 接続・グループ・タグの設定は残す。その接続は「ドライバ未導入」状態で起動せず、タグの品質は Bad。
- 設定を消したり別プロトコルに読み替えたりしない。
- タグ登録（REST・MCP・CSV）は D3 の検証ができないため、そのプロトコル配下では拒否する。

## 7. 同梱 UI（iframe）

- Hub が `/plugins/<id>/<version>/ui/` で静的配信し、接続設定・タグ登録の画面で iframe として表示する。
- `sandbox="allow-scripts"`（`allow-same-origin` を付けない）と専用の CSP（外部への接続禁止）を付ける。プラグインの UI は Hub の Cookie・API トークン・DOM に触れない。
- Hub とは `postMessage` のブリッジだけでやりとりする。ブリッジが公開する操作（v1 案）:
  - 現在の接続設定の取得・保存の**提案**（保存は Hub 側のフォームで確定）
  - `address.parse` / `address.offset` / `config.validate` の呼び出し（D3 と同じ関数）
  - タグ登録案の提出 → **Hub 側のプレビューと確定**を経て既存の登録経路（検証・未適用キュー・監査）へ
- 秘密の設定値（`config.secret-keys`）は UI へ平文で返さない。
- デスクトップシェル（banto-hub-shell の WebView）でもブラウザでも同じに動く。別プロセスを起動しないので、サービス運用の Session 0 問題はない。

## 8. 信頼と署名

- 署名は Ed25519。検証鍵は Hub のビルドに埋め込み、鍵の更新に備えて複数を許す。
- v1 で受け入れるのは**自社の鍵で署名したプラグインのみ**。第三者製の受け入れは §10 Q2。
- プラグインのソースは別リポジトリ（§3.4.1 と同じ）に置き、通常のレビューと CI（ワイヤ層のテスト・モックでの分割受信・不正応答・無応答・切断）を通したものだけを署名する。
- WASM のリソース上限: メモリ上限・epoch interruption・fuel を Hub が設定する。超過は接続の致命エラー。

## 9. 段階計画（案）

| 段階 | 内容                                                                                                                                                     | 完了条件                                                               |
| ---- | -------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------- |
| P0   | スパイク: wasmtime 組み込みのサイズ・ビルド時間・性能を測る。Modbus TCP を WASM で書き直し、組み込み版と同じ実機・シミュレータで読み取りを比較。WIT 確定 | 計測値と WIT v1 を本書に記録しオーナー判断                             |
| P1   | Hub 側ランタイム: Plugin Manager（手動配置のみ）、`banto-collect` に WASM 接続種別、読み取りのみ                                                         | シミュレータで組み込み版と同じ値・品質                                 |
| P2   | 導入・発見: アップロード、署名検証、保護された置き場所、一覧 API、更新とロールバック、監査                                                               | 再起動なしで導入・更新・ロールバック                                   |
| P3   | UI: アドレス関数・設定検証を REST・MCP・CSV へ配線（D3）、iframe 配信とブリッジ                                                                          | プラグインの UI から接続とタグを登録でき、REST・MCP でも同じ検証が効く |
| P4   | 書き込み: `write` を broker 経由で配線。Hub の関門・監査はそのまま                                                                                       | 既存の書き込み系テストと同じ観点を WASM 接続で通す                     |
| P5   | 周辺: 設定パッケージへの必要プラグインの記録、冗長化の両系照合、ChronoGazer での読み込み                                                                 | 各文書へ反映                                                           |

## 10. 未決事項（オーナー判断）

- **Q1**: KEYENCE KV（#438）を最初のプラグインにするか、§3.4.1 のとおり組み込みで進めるか。
- **Q2**: 第三者（SI・メーカー）製プラグインを受け入れるか。受け入れるなら鍵の発行、読み取り専用への制限、UI ブリッジのさらなる制限が要る。
- **Q3**: 既存の SLMP / Modbus TCP をプラグインへ移すか（P0 の比較結果を見て判断）。
- **Q4**: 接続ごとのバージョン固定が必要か（v1 は Hub 全体で 1 バージョン）。
- **Q5**: タグの browse（PLC からのシンボル一覧取得）を v1 に入れるか。入れるなら `browse` インターフェースと、UI ブリッジからのオンライン操作の扱いを追加設計する。
- **Q6**: シリアル（RS-232C/485）トランスポートを対象にするか。
- **Q7**: ロックダウン中のプラグイン導入・更新を許すか（管理面の他操作と同じ扱いにするか）。
- **Q8**: x86 / COM / デスクトップ前提のベンダー部品（JoyWatcher・OPC DA 等）向けの別プロセス枠を別途設計するか（本書の対象外）。
