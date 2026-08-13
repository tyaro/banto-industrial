# H9: SLMP 構造化エラー — tyaro/slmp への実装仕様と banto 側の受け入れ

作成日: 2026-08-12
状態: **実装済み**（2026-08-12）。オーナーが `tyaro/slmp`（git 依存、tag `v0.2.0`）で
§2 の構造化エラー API を実装し、banto 側（§3）を同日中に対応した。banto-plc /
banto-plc-write の文言パース（`END_CODE_MARKER`/`parse_end_code`/`classify_io_error`）は
完全削除済み。broker の session/transport 共通化（§3 の5番目）は本書作成時点では対応に含めず
別スライス候補としていたが、2026-08-14 に `banto_plc::dial_slmp` への集約で完了した（§3-5参照）。
本書は (1) tyaro/slmp が公開した API と (2) それを受けて banto 側で行った変更（受け入れ条件）を記す。
関連: [improvement-plan.md](improvement-plan.md) §H9、[banto-hub-remaining-plan.md](banto-hub-remaining-plan.md) P3-c。

## 1. 現状（文言パース依存）

`crates/banto-plc/src/slmp/mod.rs` の `classify_io_error` は、wrapped `slmp` クレートが返す
`std::io::Error` を `PlcError` へ翻訳している。問題は、**非ゼロ SLMP 終了コード（デバイス側の
拒否＝非致命、per-request `Bad`）** と **フレーミング失敗（接続致命、要再接続）** が、slmp からは
**どちらも `ErrorKind::InvalidData`** として返り、区別する手段が**エラー文言のパースしかない**こと:

- `END_CODE_MARKER = "SLMP Returns Error:"`、形は `"SLMP Returns Error: {name} (0x{code:X})"`。
- `parse_end_code` がこの文言から `(code, name)` を取り出せれば `PlcError::SlmpEndCode`（非致命）、
  取り出せなければ `PlcError::Protocol`（致命）に倒す（**fail-closed**: 文言が変わったら再接続1回で済む側に倒す）。
- CI の tripwire テスト2本（`slmp/integration_tests.rs` の `slmp_end_code_is_bad_not_fatal` /
  `a_malformed_frame_is_fatal_even_though_it_shares_a_kind_with_an_end_code`）で結合を封じ込め済み。

この文言パースの**完全削除**が H9 の受け入れ条件。そのためには slmp 側が終了コードを構造化して露出する必要がある。

## 2. tyaro/slmp が公開すべき API（オーナー実装分）

banto が文言パース無しで「非ゼロ終了コード」と「フレーミング失敗」を区別できることが必須。最小要件:

- **読み取り/書き込みのエラー型を構造化する**（`std::io::Error` 一本化をやめる）。例:

  ```rust
  pub enum SlmpError {
      /// フレームは完全・長さ整合（データ長の宣言 vs 実到達を検証済み）だが、
      /// SLMP 終了コードが非ゼロ = デバイス側の拒否。バイト列は要求境界に整列した
      /// ままなので、呼び出し側は per-request 失敗として続行してよい。
      Device { end_code: u16 },
      /// フレーミング/長さ不整合など、応答の構造そのものが壊れている。
      /// バイト列が非同期化している可能性があり、呼び出し側は接続を切るべき。
      Framing(/* 既存の詳細を保持 */),
      /// 送信/受信デッドライン（現状 ErrorKind::TimedOut 相当）。
      Timeout,
      /// ストリーム未接続（現状 ErrorKind::NotConnected 相当）。
      NotConnected,
      /// それ以外の transport/IO（接続拒否・reset・broken pipe・EOF・DNS 等）。
      Io(std::io::Error),
  }
  ```

- **必須の区別**: `Device { end_code }`（well-formed frame + 非ゼロ終了コード）を、`Framing` /
  transport 系と**文言に頼らず**判別できること。これが banto の非致命/致命判定の核。
- **保持してほしい区別**: `Timeout` と `NotConnected`（banto は現状これらを個別扱いしている）。
- **`end_code` は `u16` の生値でよい**（シンボリック名は banto 側で自前テーブルにできる。slmp が名前も
  返すなら任意で受け取る）。frame の長さ整合検証を終了コード検査の**前**に行う現行の性質は維持すること
  （「終了コードに到達した＝完全なフレームだった」という不変条件が非致命判定の根拠）。
- 破壊的変更になるので **semver を上げて publish**（例 `0.1.24` or `0.2.0`）。

## 3. banto 側の受け入れ（実装済み、2026-08-12）

1. workspace `Cargo.toml` の `slmp` を `{ git = "https://github.com/tyaro/slmp", tag = "v0.2.0" }`
   へ更新（deny.toml `[sources] allow-git` に対応 URL を追加）。
2. `crates/banto-plc/src/slmp/mod.rs`: `classify_io_error` / `parse_end_code` / `END_CODE_MARKER` を削除し、
   `SlmpError::Device { end_code }` → `PlcError::SlmpEndCode { code, message }`（message は
   `slmp::end_code_name` 由来）、`Framing`/`Io` → 致命（`PlcError::Protocol`/`Connection`）、`Timeout` →
   `PlcError::ResponseTimeout`、`NotConnected` → `PlcError::NotConnected` の**構造化マッチ**
   （`classify_slmp_error`）へ置き換え済み。
3. `crates/banto-plc-write` 側の同種パース箇所も同様に置換済み（`classify_slmp_error` を
   `PlcWriteError` 版として実装)。
4. tripwire テスト2本を**構造化エラー版**へ置換済み（生の `slmp::SLMPClient` を直接叩いて
   `slmp::SlmpError::Device`/`Framing` variant を直接検証する形へ拡張）。
5. 「broker の session/transport 層の共通化」は本書作成時点ではスコープに含めず、別スライス候補として
   improvement-plan.md §H9 に記録した（`connect_attempt` の型変更 `io::Error`→`slmp::SlmpError` の
   コンパイル対応のみ実施）。その別スライスは 2026-08-14 に `banto-plc` の共有ヘルパー
   `dial_slmp` への集約で完了（`SlmpClient::connect`/`SlmpWriteClient::connect`/
   `banto-broker::connect_attempt` の3箇所が同一実装を共有。詳細は improvement-plan.md §H9）。

**受け入れ条件（§H9、達成）**: 文言パース（`END_CODE_MARKER`）の完全削除、tripwire テストの構造化
エラー版への置換。`grep -rn END_CODE_MARKER crates/` はヒット0。
