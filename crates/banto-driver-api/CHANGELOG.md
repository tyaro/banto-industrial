# 変更履歴 (banto-driver-api)

banto-hub / ChronoGazer のドライバの境界（型と trait）の crate の変更履歴。版は [SemVer](https://semver.org/lang/ja/) に従い、**0.x の間は minor を破壊の単位にする**（patch は互換のみ。alpha / beta は使わない）。破壊のある版には「何が壊れ、どう直すか」を必ず書く（docs/banto-driver-api-design.md §9.1）。

- **タグの名前**: 公開した版ごとに `banto-driver-api-v<版>`（例 `banto-driver-api-v0.1.0`）を付ける。ワークスペースのリリースのタグ（`v0.2.0-alpha.32` 等）とは別。CI の `cargo-semver-checks` は、この形の最も新しいタグを基準（baseline）にする。タグが 1 つも無い間は飛ばす。
- **MSRV**: `Cargo.toml` の `rust-version`。CI がその版で `cargo check` する。上げるときは minor を上げ、ここに書く。

## 0.3.0 — 未公開

R2b-6a（docs/banto-hub-driver-ui-design.md §3。2026-10-24）。UI をドライバのスキーマから組み立てるための追加。`0.2.0` も未公開なので、公開する最初の版は `0.3.0` になりうる。

### 追加（既定つき。既存のドライバは何も直さずに動く）

- 新しいモジュール `schema`: `SettingsSchema` / `SettingField`（`new` + `required` / `default` / `help` / `advanced`）/ `FieldKind`（`Text` / `Host` / `Integer` / `Choice` / `Bool` / `Secret`）/ `ChoiceOption` / `SettingValue` / `FieldGroup`。どれも `#[non_exhaustive]`、`serde` feature で UI にそのまま渡せる JSON になる。
- `KindName`（`Numeric(DataType)` / `String`。JSON では `"bit"`・`"u16"`…`"f64"`・`"string"`）。
- `DriverBase::settings_schema`（既定: 空）、`DriverBase::supported_kinds`（既定: 64bit を除く数値と bit。string なし）、`DriverBase::offset_words`（既定: 「ワード単位のオフセットに対応しません」のエラー）。
- `ItemError::VerificationFailed(String)`（#902）: 書込みを送ったあとの、ドライバ自身の確認読みの不一致（RMW の確認読み）。機器には効いているかもしれない。これまで `Rejected` に混ざっていたものを分けた。`Rejected` は送る前の拒否（同じビットに true と false など）だけになる。`ItemError` は `#[non_exhaustive]` なので、`_` を書いた `match` は直さなくてよい。WIT の `item-error` にはまだ無い（WASM ゲストの `rejected` は `Rejected` になる。WIT への追加は別の PR）。
- `DriverAddress::index: Option<u64>` と `with_index`（アドレスの area の中の位置。`new` は `None`）。`DriverAddress` は `#[non_exhaustive]` なので、`new` / `with_*` で作っているコードは直さなくてよい。

### 壊れること

- なし（0.x の minor は設計 §3.6 の方針で上げる）。`DriverAddress` を `Hash` / `Eq` で比べるコードは、`index` も比べられることに注意。
- `ItemError::Rejected` の意味を送る前の拒否だけに狭めた（#902）。送ったあとの確認読みの不一致を `Rejected` で返していたドライバは `VerificationFailed` に変える（conformance の C14 で落ちる）。

## 0.2.0 — 未公開

R2b-3（docs/banto-hub-driver-io-design.md §10。2026-10-23、banto-hub v0.2.0-alpha.37）。**`0.1.0` はどこにも公開していない**（`banto-driver-api-v0.1.0` のタグも無い）ので、公開する最初の版はこの `0.2.0` になる。次の `0.1.0` 節は R1 の形の記録として残す（この版から見ると、`HostBound` を含む形）。R1 の形に合わせて書いたドライバがあれば、下の「壊れること」で直す。

### 壊れること（0.1.0 の R1 の形から）

- **`DriverKind::HostBound` と `DriverEntry::host_bound` を消した**（R2b-1a で `DriverKind::HostProvided` / `DriverEntry::host_provided` に名前と意味を替えた。R2b-3a-2 で組み込みの Modbus / SLMP が使っていた `HostBound` を消した）。
  - 直し方: 識別・設定の検証・アドレスの関数だけを持ち I/O はホストが持つプロトコルは `DriverEntry::host_provided(driver)`。I/O もドライバに任せるなら、フレーム型は `DriverEntry::frame(driver)`、セッション型は `DriverEntry::session(driver)`。`DriverKind` は `#[non_exhaustive]` なので、`match` に `_` を書いていれば他は変わらない。
  - 背景: R1 の `HostBound` は、`banto-broker` の側表（`DriverTable`）と、`banto_plc::Address` への戻し（橋）を前提にした一時の種類だった。broker はレジストリだけを見て接続をダイヤルし、要求の型は `TagSpec` / `WriteItem` になった（`DriverTable`・`DialFn`・`DriverDial` は `banto-broker` から消えた）。
- Modbus TCP・SLMP の組み込みドライバは `FrameDriver` として登録される（R2b-3c・3d）。API の型・trait は変わらない。API を使う側（ホスト）が「`HostBound` の id」を数えていたなら、`DriverKind::Frame` / `Session` の id を数える。

### 追加（破壊ではない）

- `DriverBase::probe`（既定は `None`）: 接続テストが読む 1 点（アドレスの文字列と `TagKind`）。ホストの接続テストは、保存済みの接続の broker のセッションが使えればそのセッションで `probe` の 1 点を読み、無ければエンジンの 1 回きりの接続（ハンドシェイク + `probe` の 1 点）で行う（R2b-3a-2）。
- `DriverDescriptor::settings_version`（`new` は 1。`with_settings_version`。serde は欠けていれば 1）（R2b-1a）。
- `DriverKind::HostProvided` と `DriverEntry::host_provided`（R2b-1a）。
- 共通のキー `keys::BAUD` / `DATA_BITS` / `PARITY` / `STOP_BITS`（シリアルの線の設定）と、`FrameConnection::inter_frame_gap`（既定は 0。エンジンが守る前のフレームからの最小の無音。Modbus RTU の t3.5 用。エンジンの上限 500 ms を超える値は契約違反として送信の前に断られる）（R2b-3e-1）。`keys::SERIAL_PORT` / `STATION` は予約でなく実際に使うキーになった。
- 契約の明確化（型は変わらない）: フレーム型の `decode_read` は要求の `TagKind` に合う `RawValue` を返す（ビットは `Bool`、他の数値は `Int` / `Uint` / `Float`、文字列は `Bytes`。合わなければ項目ごとの `ItemError::Unsupported`）。書き込みの 1 つの step の `covers` に同じ添字を重ねない（違反は致命の `FatalError::Protocol`）。`FatalError::InvalidConfig` は設定の誤りだけで、タグのアドレスの誤りは `ItemError::Unsupported`（R2b-2a。接続は落とさない）。
- 要求 - 応答の手順（`FrameDriver` の契約）を実装する本番の実行系は、新しい公開 crate **`banto-driver-engine`**（0.1.0）にある。ホストは API だけでなくこの crate も使う。

### まだ入っていないこと

- シリアル接続（Modbus RTU、時間切れ後の線の同期）は #803 で後回しにした（2026-10-23）。API 側の形（`Transport::Serial` / `SerialOverTcp`、線の設定キー、`inter_frame_gap`）は入っている。

## 0.1.0 — 未公開（公開せず 0.2.0 に置き換えた）

R1-1・R1-2（docs/banto-driver-api-design.md §10）。最初の版。R1-1 で型を、R1-2 で trait と `DriverRegistry` を入れた。MSRV は Rust 1.85。必須の依存はなし（`serde` は feature で任意）。

### `banto-plc` から移した型（`#[non_exhaustive]` なし）

`banto-plc` は同じ名前・同じパスで再輸出するので、`banto_plc::*` の呼び出し側は変わらない。

- `DataType`（`parse`・`Display` と一緒に）。`register_span` は `pub` になった（`banto-plc` では `pub(crate)`）。
- `TagValue`
- `Value`（`banto_plc::PlcValue` の新しい名前。`banto-plc` は `pub use banto_driver_api::Value as PlcValue`）。`From<TagValue>` と `as_tag_value` も一緒に移した。
- `StringEncoding`
- `WordOrder`（列挙と `Default` だけ。ワード順の組み立ては `banto-plc` に残る）

これらは `#[non_exhaustive]` ではないので、**variant を足すことは破壊（minor を上げる）**。

### 新しい型（`#[non_exhaustive]`、コンストラクタつき）

- 読み書きの単位: `RawValue`・`TagKind`（`numeric` / `string` / `word_count`）・`TagSpec`・`WriteItem`
- アドレス: `DriverAddress`（`new` / `with_bit`）と `DriverId`（`[a-z0-9][a-z0-9-]*`。`new` が文法を確かめる。`Borrow<str>`）・`InvalidDriverId`
- エラーの分類: `ItemError`（`Device` / `Unsupported` / `Rejected`）・`DeviceError`・`FatalError`（`Framing` / `Protocol` / `InvalidConfig` / `Io` / `Timeout`）。`Display` は手書き
- 接続設定: `Settings`（順序つき・重複キーなし。`from_pairs` / `get` / `insert` / `iter`）・`Setting`・`DuplicateKey`・`FieldError`・共通のキー `keys::*`

これらに variant・field を足すことは破壊ではない。crate の外では struct リテラルで作れないので、コンストラクタを使い、`match` には `_` / `..` を書く。

### feature `serde`

すべての公開する値の型に `Serialize` / `Deserialize` を付ける。列挙の綴りは snake_case（`DataType` は `"i16"` 等で `parse` / `Display` と同じ、`WordOrder` は `"high_low"` / `"low_high"`、`StringEncoding` は `"utf8"` / `"shift_jis"`）。`DriverId` は文字列、`Settings` は `{ key, value }` の配列で、読み込みでも文法と重複キーを確かめる。

### trait と `DriverRegistry`（R1-2）

- `driver`: `DriverBase`（`descriptor` / `validate_config` / `secret_keys`（既定は空）/ `parse_address` / `offset_address` / `address_help`）・`DriverDescriptor`（`new` は `transports = [Tcp]`・`api_version = API_VERSION`。`with_vendor` / `with_transports`）・`Capabilities`（`new` / `READ_ONLY` / `READ_WRITE`）・`Transport`（`Tcp` / `Serial` / `SerialOverTcp`）・`AddressError`（`new` / `message`）・`API_VERSION = 1`。`DriverId` は R1-1 のまま `address` にあり、ルートの再輸出も変わらない。
- `frame`: フレーム型の `FrameDriver`（`create_connection`）と `FrameConnection`（`handshake_next` / `farewell`（既定は無し）/ `is_complete` / `plan_read` / `decode_read` / `write_next`（既定は `FatalError::Protocol` で断る））。`Step`・`FrameRule`（`None` / `Fixed` / `LengthPrefixed` / `Terminator` / `DriverDecides`）・`PrefixSpec`（`new` は `Binary`。`with_encoding`）・`PrefixEncoding`・`Planned`・`FrameCheck`（`Incomplete` / `Complete { len }` / `Discard`）・`WriteProgress`（`Send` / `Done`）。書き込みは `write_next` の状態機械（§4.2.1）。
- `session`: セッション型の `SessionDriver`（`connect`）と `Session`（`read_batch` / `write_batch`（既定は `FatalError::Protocol` で断る）/ `disconnect`）・`ConnectContext`・`BoxFuture`（`banto_plc::BoxFuture` と同じ定義。std だけ）。
- `registry`: `DriverRegistry`（`new` / `register` / `get` / `contains` / `ids` / `len` / `is_empty`。同じ id の登録は `RegistryError::DuplicateId`）・`DriverEntry`（`frame` / `session` / `host_bound` / `base` / `descriptor` / `kind`）・`DriverKind`（`Frame` / `Session` / `HostBound`）。**`DriverKind::HostBound` は R1 限定で、R2b-3 で消す（そのときは minor を上げる破壊）**。
- R2b-1a（docs/banto-hub-connection-settings-design.md §3.4・§4.1）の追加（`0.1.0` は未公開なのでこの節に書く。公開後なら patch / minor の互換な追加）: `DriverKind::HostProvided`（`#[non_exhaustive]` の列挙への variant）と `DriverEntry::host_provided`、`DriverDescriptor::settings_version: u32`（`new` は 1。`with_settings_version`。serde は欠けていれば 1）。`HostProvided` は「識別・設定の検証・秘密のキー・アドレスの関数は `DriverBase`、I/O はホストが持つ」プロトコル（banto-hub の `push`・`postgres`・`virtual`）の恒久の種類で、R2b-3 で消える `HostBound` とは寿命が違う。いずれも追加だけで、既存の呼び出しは変わらない。
- R2b-3e-1（docs/banto-hub-driver-io-design.md §7.1・§7.3.1）の追加（`0.1.0` は未公開なのでこの節に書く。公開後なら互換な追加）: 共通のキー `keys::BAUD` / `DATA_BITS` / `PARITY` / `STOP_BITS`（シリアルの線の設定）、`FrameConnection::inter_frame_gap`（既定は 0。前のフレームの最後のバイトから次の送信までにエンジンが空ける最小の無音。Modbus RTU の t3.5 用。エンジンの上限 500 ms を超える値は契約違反として、送信の前に断られる）。`keys::SERIAL_PORT` / `STATION` は予約でなく実際に使うキーになった。
- 新しい型はすべて `#[non_exhaustive]`。struct はコンストラクタで作る。trait に既定実装つきのメソッドを足すのは破壊ではなく、既定実装の無いメソッドを足すのは破壊。
- feature `serde`: 値の型（`DriverDescriptor`・`Capabilities`・`Transport`・`FrameRule`・`PrefixSpec`・`PrefixEncoding`・`Step`・`Planned`・`FrameCheck`・`WriteProgress`）にも `Serialize` / `Deserialize` を付けた（列挙は snake_case）。trait・`DriverRegistry`・`ConnectContext`・エラーの型には付けない。
- MSRV 1.85 には trait の upcasting（1.86）が無いので、`DriverEntry::base` は内部で `DriverBase` を転送する包みを持つ（公開の形には出ない。MSRV を 1.86 以上にしたら外せる）。
