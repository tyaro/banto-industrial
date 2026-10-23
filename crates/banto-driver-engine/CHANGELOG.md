# 変更履歴 (banto-driver-engine)

`banto-driver-api` のフレーム型ドライバ（`FrameDriver`）を、期限つきの `Session` として動かす通信エンジンの変更履歴。版は [SemVer](https://semver.org/lang/ja/) に従い、**0.x の間は minor を破壊の単位にする**（patch は互換のみ。alpha / beta は使わない）。破壊のある版には「何が壊れ、どう直すか」を必ず書く（docs/banto-driver-api-design.md §9.1）。

- **タグの名前**: 公開した版ごとに `banto-driver-engine-v<版>`（例 `banto-driver-engine-v0.1.0`）を付ける。ワークスペースのリリースのタグとは別。
- **公開の順**: `banto-driver-api` → `banto-driver-engine` → `banto-driver-conformance`。公開前の確認は 3 つ同時の `cargo publish --dry-run -p banto-driver-api -p banto-driver-engine -p banto-driver-conformance`（API が crates.io に無い間は単独では失敗する）。
- **MSRV**: `Cargo.toml` の `rust-version`（API と同じ）。

## 未リリース

R2b-5a（docs/banto-hub-driver-sim-design.md §3）。配線はしない（アプリの振る舞いは変わらない）。

- `sim`: 値の層のシミュレータ `SimDevice`（`DriverAddress::canonical` を鍵にした共有の表。`get` / `set` / `write_count` / `held_count`）と、その接続 `SimSession`（`Session`）。読み取りは held（書かれた値）があればそれ、無ければ時刻（100 ms）と初めて見た順から決定的に生成する（ビットは反転・整数と浮動小数はランプ・文字列は `SIM-{index}-{tick}` を語数 × 2 バイトに切る）。書き込みは書けない領域・種類違い・型の範囲外（四捨五入の後）・文字列の長すぎを `ItemError::Rejected` にし、通れば held にする。`SessionDriver` は実装せず、`SimDevice::connect` で作る。

## 0.1.0 — 未公開

R2b-3b-core（docs/banto-hub-driver-io-design.md §5）・R2b-3e-1（同 §7）。最初の版。MSRV は Rust 1.85。依存は `banto-driver-api`・`tokio`（`rt` / `time` / `net` / `io-util`）・`tokio-serial`（`default-features = false`。Linux でも libudev は要らない）・`tracing` だけ。banto-hub の `banto-broker` が `Frame` の接続をこのエンジンで動かし、適合テスト `banto-driver-conformance` も同じエンジンを記憶上の通信路で動かす（手順の実装は 1 つ）。

- `FrameSession`（`banto_driver_api::Session` を実装）: `FrameDriver` の要求 - 応答の手順（api-design §4.2・§4.2.1）を実装する。読み取りの計画（`plan_read`）に沿った送受信・分割して届く応答の組み立て（`FrameRule` の各規則・`Discard`）・`decode_read` と `covers` による結果の並べ替え・書き込みの状態機械（`write_next`。1 回の論理書き込みの間は他の要求を割り込ませない）・ハンドシェイク（32 step まで）・別れ（`farewell`）。
- 時間の上限: すべての `await` に期限がある（step ごとの応答の期限・接続の外側の期限・`CLOSE_TIMEOUT` = 100 ms）。時間切れ・契約違反（`max_len` 超過・`covers` の範囲外・`Discard(0)` など）・致命の分類のときは、受信バッファを捨てて接続を閉じる（遅れて届いた応答を次の要求に流用しない）。外側の future のキャンセルでも壊れた接続を再利用しない。
- `transport`: `Transport` trait（送る・受ける・受信の残りを捨てる・閉じる・送り終えの見積もり `tx_time`）、`TcpTransport`（`StreamTransport<TcpStream>`）、`SerialTransport`（`tokio-serial`）、`TransportSpec::from_settings`（設定の `transport` キーから TCP・シリアル・`serial_over_tcp` を選ぶ）、線の設定（`SerialLine`・`DataBits`・`Parity`・`StopBits`）、ポート名の許可リスト（`SerialPortName`）。
- フレーム間の無音: `FrameConnection::inter_frame_gap`（上限 `MAX_INTER_FRAME_GAP` = 500 ms。超える値は契約違反で送信の前に断る）。前のフレームの最後のバイト（送り終え・受け終えの遅い方）から空ける。
- `decode`: `decode_string`・`decode_value`（`RawValue` → `Value`。文字列の decode をエンジンで揃える）。
- `EngineEvents` / `EngineEvent`: 適合テストとホストの診断のための観測口（契約違反・ドライバの致命・step の開始・書き込みの step）。通信の結果は変えない。送信バイトに秘密値が入りうるので、観測側はバイト列をログに残してはならない。

### まだ入っていないこと

- **シリアル接続は broker から使えない**（2026-10-23 オーナー判断で #803 へ後回し）。このエンジンにシリアルのトランスポートは入っているが、時間切れ後の線の同期（設計 §7.3）は未実装で、`banto-broker` はシリアルの接続を断ったまま。同期が入るまで、シリアルで時間切れが起きたときの安全は保証しない。
