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
- R1-5（§7.3）: `session::run_all_against(driver, fixtures, proxy)`（C1〜C8・C12〜C15）。セッション型ドライバを、既に TCP サーバーとして立っているピア（シミュレータ）へ `FaultProxy` 経由でつなぎ、ホストの振る舞い（1 接続、致命で捨てて張り直す）を実時間で再現して確かめる。入力は `session::SessionFixtures`（`statics`・既知の値の読み取り `ReadCase { text, kind, expected: Value }`・機器が断るアドレス・型の合わない読み書き・書き込み `WriteCase { text, kind, value }`・壊れた応答の作り方 `ResponsePatch`・ワード内ビットの書き込み `BitWriteCase`・文字列の上限・タイムアウト）。設計 §7.2 の `run_all_against(driver, settings, fixtures, peer, faults)` から、`settings` を `fixtures.statics.settings`（`host` / `port` はハーネスがプロキシのものに置き換える）に、`peer` を `FaultProxy::start(peer)` に畳んだ（`frame::run_all` と同じ形）。
- R1-5: `proxy::Faults` に 1 回だけの `patch_response(k, offset, bytes)`（k 番目の応答の途中を書き換える。要求を写すヘッダ（トランザクション番号・シリアル）を残したまま壊せる）・`hold_response(k, d)`（k 番目の応答だけを遅らせる。C6）・`cut_response(k, n)`（先頭 n バイトだけ渡して切断。C4）を足し、`FaultProxy` に `delivered_count()`（渡し終えた応答の数）・`request_count()`（要求の数。接続ごとに「ピアの応答の後に来た最初のバイト」で 1 件と数えるので、1 つの要求が TCP で何回に分かれて届いても 1 件。`requests()` は TCP の読み取り 1 回ごとの記録で、要求の数には使えない）・`close_all()`（全接続を切り、中継が止まるまで待つ）を足した。いずれも追加のみ。C15 の「拒否の後に要求を送らない」は `request_count()` で数える（#R1-5 のオーナーレビュー P2: `requests().len()` で数えると、分かれて届いた 1 つの要求を 2 件と数えて正しいドライバを落としていた）。
