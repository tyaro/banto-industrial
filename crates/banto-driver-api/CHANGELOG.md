# 変更履歴 (banto-driver-api)

banto-hub / ChronoGazer のドライバの境界（型と trait）の crate の変更履歴。版は [SemVer](https://semver.org/lang/ja/) に従い、**0.x の間は minor を破壊の単位にする**（patch は互換のみ。alpha / beta は使わない）。破壊のある版には「何が壊れ、どう直すか」を必ず書く（docs/banto-driver-api-design.md §9.1）。

- **タグの名前**: 公開した版ごとに `banto-driver-api-v<版>`（例 `banto-driver-api-v0.1.0`）を付ける。ワークスペースのリリースのタグ（`v0.2.0-alpha.32` 等）とは別。CI の `cargo-semver-checks` は、この形の最も新しいタグを基準（baseline）にする。タグが 1 つも無い間は飛ばす。
- **MSRV**: `Cargo.toml` の `rust-version`。CI がその版で `cargo check` する。上げるときは minor を上げ、ここに書く。

## 0.1.0 — 未公開

R1-1（docs/banto-driver-api-design.md §10）。最初の版。型だけ（trait と `DriverRegistry` は R1-2 で足す）。MSRV は Rust 1.85。必須の依存はなし（`serde` は feature で任意）。

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
