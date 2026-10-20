# 変更履歴 (banto-driver-conformance)

`banto-driver-api` のドライバの適合テスト一式の変更履歴。版は [SemVer](https://semver.org/lang/ja/) に従い、**0.x の間は minor を破壊の単位にする**（patch は互換のみ。alpha / beta は使わない）。破壊のある版には「何が壊れ、どう直すか」を必ず書く（docs/banto-driver-api-design.md §9.1）。

- **タグの名前**: 公開した版ごとに `banto-driver-conformance-v<版>`（例 `banto-driver-conformance-v0.1.0`）を付ける。`banto-driver-api-v*` やワークスペースのリリースのタグとは別。CI の `cargo-semver-checks` は、この形の最も新しいタグを基準（baseline）にする。タグが 1 つも無い間は飛ばす。
- **API との関係**: `banto-driver-api` に `{ path, version }` で依存する。API の minor が上がったら、この crate の版と依存の `version` も合わせて上げる。公開前の確認は 2 つ同時の `cargo publish --dry-run -p banto-driver-api -p banto-driver-conformance`（この crate 単独では API が crates.io に無い間は失敗する）。
- **適合テストの追加**: 新しい確認（C 番号）を足して、今まで通っていたドライバが落ちるようになるのは、この crate の minor を上げる変更として扱い、ここに書く。
- **MSRV**: `Cargo.toml` の `rust-version`（API と同じ）。CI がその版で `cargo check` する。

## 0.1.0 — 未公開

R1-2（docs/banto-driver-api-design.md §7・§10）。骨格。MSRV は Rust 1.85。依存は `banto-driver-api` と tokio（`rt` / `time` / `net` / `io-util` / `macros`）だけ。

- `static_checks`（C1・C2。tokio 不要）と `StaticFixtures`（最小の設定・正しい / 不正なアドレス・オフセット）。
- `frame::run_all`（C1〜C7・C9〜C12）: フレーム型ドライバを、ハーネスが Hub のエンジンの手順（§4.2・§4.2.1）を仮想の時刻で再現して確かめる。分割・遅延・壊れた応答・伸び続ける応答・先頭のごみ・途中の切断はハーネスが起こす。入力は `FrameFixtures`（読み取りの期待値・機器が断るアドレス・型の合わないアドレス・離れた書き込み・ごみのバイト列）と `frame::Responder`（`on_connect` / `reply` / `reply_broken`）。
- `frame::WriteCase` は読み戻しの期待値 `expected: RawValue` を持つ（`WriteCase::new(text, kind, value, expected)`・`FrameFixtures::write` も 4 引数）。C3・C9・C10 の値の照合は `TagKind` に沿った 1 つの規則（`Bit` は一致、`F32` は `f32` に丸めて一致、`F64` はビット一致、整数は同じ variant で一致、文字列は `words × 2` 以下で最初の NUL までのバイト列が一致。バイト列はタグの encoding のまま比べ、UTF-8 に符号化し直さない）。#739 のオーナーレビュー P2: 以前は C10 が Shift_JIS の文字列を正しく書くドライバを落とし、`F64` を `f32` の精度で書くドライバを通していた。
- `fake::echo::EchoFrameDriver` と `EchoResponder`: 参照実装のフレーム型ドライバ（ハンドシェイク・トランザクション番号・同じ語の範囲を 1 フレームで読み型ごとに decode・ワード内ビットの RMW）。設定 `framing` で `Fixed` / `LengthPrefixed` / `DriverDecides` を選べる。`echo::fixtures` がその期待値。
- `fake::session::FakeSessionDriver`: メモリ上の表を返すセッション型ドライバ。`fail_next` で次の読み書きを致命にできる。
- `proxy::FaultProxy`: 127.0.0.1 の TCP 中継。応答の遅延・分割・k 番目の応答の先頭の差し替え・全接続の切断・送られたバイト列の記録・accept の計数。
- セッション型のハーネス（`session::run_all_against`、C1〜C8・C12〜C15）と Modbus / SLMP の fixtures は R1-5。
