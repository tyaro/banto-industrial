# H9: SLMP 構造化エラー — tyaro/slmp への実装仕様と banto 側の受け入れ

作成日: 2026-08-12
状態: **仕様確定・実装待ち（2段構え）**。2026-08-12 オーナー決定: slmp 本体（`tyaro/slmp`、
crates.io `slmp`）はオーナーが構造化エラーを実装・publish → その後こちらで banto 側を仕上げる。
本書は (1) tyaro/slmp が公開すべき API と (2) それを受けて banto 側で行う変更（受け入れ条件）を定める。
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

## 3. banto 側の受け入れ（実装待ち＝こちらで対応）

slmp の新バージョンが出たら、こちらで:

1. workspace `Cargo.toml` の `slmp` を新バージョンへ更新。
2. `crates/banto-plc/src/slmp/mod.rs`: `classify_io_error` / `parse_end_code` / `END_CODE_MARKER` を削除し、
   `SlmpError::Device { end_code }` → `PlcError::SlmpEndCode { code, message }`（message は banto 自前の
   終了コード→名前テーブル）、`Framing`/`Io` → 致命（`PlcError::Protocol`/`Connection`）、`Timeout` →
   `PlcError::ResponseTimeout`、`NotConnected` → `PlcError::NotConnected` に**構造化マッチ**で置き換える。
3. `crates/banto-plc-write` 側の同種パース箇所も同様に置換。
4. tripwire テスト2本を**構造化エラー版**へ置き換え（文言形式ではなく `SlmpError` variant を検証）。
5. 併せて H9 のもう一項目「broker の session/transport 層の共通化」を banto 側リファクタとして実施
   （§H9 記載。slmp API 差し替えと同時が効率的）。

**受け入れ条件（§H9）**: 文言パース（`END_CODE_MARKER`）の完全削除、tripwire テストの構造化エラー版への置換。
