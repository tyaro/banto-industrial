# WASM ドライバ #917 の段階の実機確認

#917 の段階（E1・E2・A2・D1〜D3・G1・F・C1・E4、Modbus の A1・B1・B2・C1）を、テスト機 R `192.168.11.200`・Q `192.168.11.10` と手元の ModbusSlave `127.0.0.1:502`（unit 1）で確かめる。ケースはすべて `#[ignore]` で、さらに `BANTO_WASM_LIVE_917=1` が無ければソケットを開く前に失敗する。通常起動の WASM 登録・本番鍵・設定は変えない。

- 試験ファイル: 直接経路 `tests/slmp_wasm_live_917.rs`・`tests/modbus_wasm_live_917.rs`、Hub REST 経路 `tests/t12_wasm_live_917/mod.rs`（`t12_connection_test` の `wasm_live_917_test::`）。
- SLMP guest: `tyaro/banto-driver-wasm` の `1fac51ad380a9c3b5416660dea6bdd4926b2879f`（CI の `$newSlmpSha`）。manifest は repo の `t12_slmp_wasm_operational_write/manifest.json`。
  - component SHA-256: `04e3706f2ad54dc174cd44f9fa058250655ce3d72b75ec0bc486e54521131e11`
- Modbus guest: 同 repo の `f6c8781bab6f85a6c866e4d552039b547c4390ac`（CI の `$modbusJoinSha`）。manifest はその commit の `component/manifest.json`。
  - component SHA-256: `af6cb1685991b5296f3423ac129e6c1482c371dc0a18e1b9135b1482e9eb888a`
  - manifest SHA-256: `7399a90c0a145d803c73ed6ded9b28c695d9462f8989cf608f30dca8df2f6c43`
- 試験は component（Modbus は manifest も）のパスを環境変数で受け取り、読み込む前に SHA-256 を照合する。

## component の作り方（CI と同じ）

```powershell
git clone https://github.com/tyaro/banto-driver-wasm guest
# SLMP
git -C guest checkout --force --detach 1fac51ad380a9c3b5416660dea6bdd4926b2879f
cargo +1.98.1 build --manifest-path guest/slmp-component/Cargo.toml --target wasm32-unknown-unknown --locked
wasm-tools component new guest/slmp-component/target/wasm32-unknown-unknown/debug/banto_slmp_read_component.wasm -o slmp-917-component.wasm
wasm-tools validate slmp-917-component.wasm
wasm-tools component targets crates/banto-driver-wasm/wit slmp-917-component.wasm --world driver-with-write-and-diagnostics-v1
# Modbus（この pin の wit/driver.wit は Host と同じなので差し替え不要）
git -C guest checkout --force --detach f6c8781bab6f85a6c866e4d552039b547c4390ac
cargo +1.98.1 build --manifest-path guest/component/Cargo.toml --target wasm32-unknown-unknown --locked
wasm-tools component new guest/component/target/wasm32-unknown-unknown/debug/banto_modbus_tcp_component.wasm -o modbus-917-component.wasm
wasm-tools validate modbus-917-component.wasm
wasm-tools component targets crates/banto-driver-wasm/wit modbus-917-component.wasm --world driver-with-write-v1
Copy-Item guest/component/manifest.json modbus-917-manifest.json
```

（`CARGO_TARGET_DIR` を guest 用に別に置くと、repo の target と混ざらない。）

## 宛先と安全の作り

- TCP: 直接経路は R・Q とも 3109、Hub REST 経路は R・Q とも 3108。常駐の BantoHub サービスが使う R 5200・Q 3100 は、設定・実 transport・保存済み接続のどの検査でも断る。Modbus は `BANTO_MODBUS_LIVE_917_TARGET` で選んだ 1 つの宛先 unit 1 だけ（`slave` = 既定の `127.0.0.1:502`、`r` = `192.168.11.200:502`。それ以外の値は panic）。設定・実 transport・Hub の保存済み接続の検査は選んだ宛先だけを通し、`slave` で R を、`r` で 127.0.0.1 を通すことはない。`audit` の行に `target=` が付く。
- UDP: R・Q とも自動オープン UDP ポート（三菱の既定 5000）。`BANTO_WASM_LIVE_917_UDP_PORT` で上書きできる（5200・3100 は不可）。段階 0 は 4E・3E の型名読取り（0101）を 1 データグラムずつ送り、再送しない。段階 7（E4）は `BANTO_WASM_LIVE_917_E4=1` を付けたときだけ走る（段階 0 で応答があったときに付ける。フレームは `BANTO_WASM_LIVE_917_E4_FRAME`、既定 `4e`）。
- 直接経路の `WireGate` は送信前に全フレームを検査する。経路は自局（00 FF FF03 00）だけ、0101 と 0401（U\G の拡張読取りを含む）は通す。1401 はワード書込みだけ、R は D3020〜D3039、Q は D1020〜D1039・W1020〜W103F の中だけ、しかもケースがその直前に `authorize(n)` した回数だけ通す。ほかのコマンドは断って接続を切る。Modbus も同じで、読取り FC1〜4 は通し、書込み FC5・15 はコイル 00001〜00008、FC6・16・22 は保持レジスタ 40010〜40029 の中で、許可した回数だけ通す。
- ゲストの allowlist: R `D3020-D3039,D3020.0-D3020.F`、Q `D1020-D1039,W1020-W103F,W1020.0-W1020.F`。Modbus は target ごとに、`slave` が `000001-000008` / `400010-400029`、`r` が `009193-009200`（コイル 09193〜09200 = M1000〜M1007、オフセット 9192〜9199）/ `403021-403040`（保持レジスタ 43021〜43040 = D3020〜D3039、オフセット 3020〜3039）。`r` の B1 は `43021.3`、B2 の FC6 は `43021`、C1 の文字列は `43031`〜（4 語）。
- 書込みの回数: F（ビット 1 回 + 元ワード 1 回）、C1（文字列 1 回 + 元の 4 ワードを 1 フレームで 1 回）、E4（2 回）、Modbus B2（FC15・FC6 とその復元で 4 回）、Modbus C1（2 回）。復元は、書いた本人が書く前に 2 回読んで一致した値だけを書く。読み戻しが合わない・timeout・`Uncertain` のときは再送も自動復元もせず止め、その時点の値を `audit` 行に残す。
- 読取りだけの段階（0〜4）は書込みを 1 つも許可しない。書込みが送信前に断られることの確認は、ゲストか Hub のゲートが断り、かつ `WireGate` に 1401 が届かないことの両方で見る。
- Hub REST 経路: 一時 DB に接続とタグを登録し、保存前後と broker 起動前に宛先を確かめる。一時キーはそのケースのタグの read（書込みケースは read/write）scope だけ、期限 10 分、終了時に revoke する。読取りケースは write control を有効にせず、read-only キーの書込みが 403 で断られることも見る。外部 listener は立てない。キーの平文は出力しない。`TestApp` の書込みレート制限は既定（60 秒に全体 30・タグ 10）で、どのケースもその内側（最大は Modbus B2 の 18）。
- 段階 0 と Modbus の UDP 確認は、ゲストを使わず生のデータグラムを 1 つ送って応答の有無を記録するだけ。

## ゲストの扱いどおりに「記録」する項目

- Q25PRH: ゲストは U\G（`U0\G`・`U3E0\G`）と ZR に経路を持たない（`unit_buffer_read_allowed`・D 範囲の規則）。Q の G1 と ZR10 は `Unsupported` が返り、1 フレームも送られないことを記録する（断られることを assert はしない）。
- Q の `D1020.5` のビット書込みは、Q25PRH の D に書込みの範囲の証明が無いのでゲストが送信前に断る（直接経路は `Rejected`、Hub は 200 以外）。
- R の `U3E0\G0`（CPU バッファ）はゲストの対象。応答の値はそのまま記録。
- 文字列の元の値は `Value::Str`（NUL で切る）ではなく u16 ×4（Modbus は u32 ×2）で読み、復元もその数値で書く。

## 実行

```powershell
$env:CARGO_TARGET_DIR='D:/develop/banto-industrial/target'; $env:CARGO_INCREMENTAL='0'; $env:CARGO_PROFILE_DEV_DEBUG='0'
$env:BANTO_WASM_LIVE_917='1'
$env:BANTO_SLMP_LIVE_917_COMPONENT_WASM='<path>/slmp-917-component.wasm'
$env:BANTO_MODBUS_LIVE_917_COMPONENT_WASM='<path>/modbus-917-component.wasm'
$env:BANTO_MODBUS_LIVE_917_MANIFEST='<path>/modbus-917-manifest.json'
# Modbus の宛先: 既定は手元の ModbusSlave。R の CPU に向けるときだけ $env:BANTO_MODBUS_LIVE_917_TARGET='r'（slave / r 以外は panic）
# 任意: $env:BANTO_WASM_LIVE_917_UDP_PORT='5000'; $env:BANTO_WASM_LIVE_917_E4='1'; $env:BANTO_WASM_LIVE_917_HUB_FRAME='3e'

# 直接経路（1 ポート 1 接続なので 1 本ずつ、--test-threads=1）
cargo +1.98.1 test -p banto-hub-host --test slmp_wasm_live_917 --locked r_s0 -- --ignored --nocapture --test-threads=1
#   r_s0_udp_discovery / q_s0_udp_discovery
#   r_s1_e1_e2_frame_3e_and_own_route / q_s1_…
#   r_s2_a2_w_word_bit_read / q_s2_…
#   r_s3_d1_d2_d3_unknown_range_reads / q_s3_…
#   r_s4_g1_unit_buffer_reads / q_s4_…
#   r_s5_f_word_bit_write_readback_restore / q_s5_…
#   r_s6_c1_string_write_readback_restore / q_s6_…
#   r_s7_e4_udp_word_write_readback_restore / q_s7_…（BANTO_WASM_LIVE_917_E4=1 のときだけ送る）
cargo +1.98.1 test -p banto-hub-host --test modbus_wasm_live_917 --locked modbus_a1 -- --ignored --nocapture --test-threads=1
#   modbus_a1_coil_only_probe_connects_with_one_fc1_read
#   modbus_b1_register_bit_write_without_fc22_is_refused_before_send
#   modbus_b2_fc15_coils_and_fc6_register_write_readback_restore
#   modbus_c1_holding_string_write_readback_restore
#   modbus_udp_probe_records_whether_the_slave_answers

# Hub REST 経路
cargo +1.98.1 test -p banto-hub-host --test t12_connection_test --locked wasm_live_917_test::hub_r_read -- --ignored --nocapture --test-threads=1
#   hub_r_read_stages_e1_e2_a2_d1_d3_g1_c1 / hub_q_read_…
#   hub_r_write_stages_f_c1_readback_restore / hub_q_write_…
#   hub_modbus_a1_coil_only_connection_test
#   hub_modbus_b1_register_bit_write_refused_without_fc22
#   hub_modbus_b2_fc15_coils_and_fc6_register_write_readback_restore
#   hub_modbus_c1_holding_string_write_readback_restore
```

opt-in 無しの確認: `BANTO_WASM_LIVE_917` を消して同じコマンドを走らせると、どのケースも `require_opt_in` の assert で止まり、ソケットを開かない。opt-in 無しで走る検査（`WireGate`・宛先の検査・opt-in の検査）は `cargo test -p banto-hub-host --test slmp_wasm_live_917` などで通る。

## 結果（2026-10-29 UTC 09:36〜09:55）

ゲストと Hub は上の固定版。R・Q へ書いた値は、すべて元の値に戻したことを読み戻しで確かめた（試験の前の値は、どの番地も 0）。

### SLMP（R08ENCPU・Q25PRH）

| 段階              | R（直接経路）                                                                                            | Q（直接経路）                                                                                           | Hub REST 経路                                                                                                                              |
| ----------------- | -------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------ |
| s0 UDP の発見     | 5000 に 4E・3E とも応答（型名 R08ENCPU）                                                                 | 5000 に 4E・3E とも応答（型名 Q25PRHCPU）                                                               | —                                                                                                                                          |
| s1 E1・E2         | 3E、自局の経由指定（4E・3E）で D3020〜D3023 を読めた                                                     | 同じく D1020〜D1023 を読めた                                                                            | R・Q とも `read-now` で読めた。read だけの鍵の書込みは 403、終了時に revoke                                                                |
| s2 A2             | `W1A`・`W1A.0`・`W1A.F`・`W1A.5` を読め、ワードの値と一致                                                | 同じ                                                                                                    | R・Q とも読めた                                                                                                                            |
| s3 D1〜D3         | Z3・RD7・ZR10 を読めた。S10 は PLC が終了コード 0x4031（S の点数が無い設定）。4 つの書込みは送信前に拒否 | S10・Z3・ZR10 はゲストが送信前に `Unsupported`（計画に含まれない）。書込みも送信前に拒否                | R の S10 は 502（0x4031）。Q は 3 つとも 502（計画に含まれない）                                                                           |
| s4 G1             | `U0\G0`（u16・u32・ビット・文字列）と `U3E0\G0` を読めた（値はどれも 0）。書込みは送信前に拒否           | `U0\G0`・`U3E0\G0` ともゲストが送信前に `Unsupported`。書込みも送信前に拒否                             | R は読めた。Q は 502（計画に含まれない）                                                                                                   |
| s5 F              | `D3020.5` を 1 にして 0→32、ほかのビットは不変。元のワード 0 に戻した（1401 は 2 回）                    | `D1020.5` は送信前に拒否。`W1020.5` もゲストが送信前に `Rejected`（標準 Q/R でない）。W1020 は 0 のまま | R は成功（監査 ok 7 行、書込み無効の間は 503 と suppressed_disabled 1 行）。Q は F で拒否されて試験が止まり、Q の C1 は Hub 経路では未確認 |
| s6 C1             | D3030〜 に "AB"（1 フレーム）、読み戻し "AB"・`4142 0000 0000 0000`、元の 4 語を 1 フレームで戻した      | D1030〜 で同じく成功                                                                                    | R は成功（4 語を batch で戻した）                                                                                                          |
| s7 E4（UDP 5000） | 4E・3E とも、D3020 に 1 を書いて読み戻し、0 に戻した                                                     | 4E・3E とも、D1020 で同じく成功                                                                         | —                                                                                                                                          |

### Modbus（ModbusSlave 127.0.0.1:502、unit 1）

| ケース    | 直接経路                                                                                                                                                                                          | Hub REST 経路                     |
| --------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------- |
| a1        | コイルだけの設定で、接続処理は FC1 を 1 回読むだけで成功                                                                                                                                          | `/api/plc-connections/test` が ok |
| b1        | `register_bit_write=disabled` で `40010.3` への書込みが送信前に `Rejected`（フレーム 0）                                                                                                          | 502（ドライバが断った）           |
| b2        | **未確認**。FC15（`start=0 count=8 bits=ff`）に正常応答が返るが、1 点ずつ読むと 00001 だけ ON。生の FC15 でも同じなので ModbusSlave 側の挙動。FC6 は送らずに停止、コイルは生の FC15 で 0 に戻した | 未実行（同じ理由）                |
| c1        | 40020〜 に "AB"（FC16 1 回）、読み戻し "AB"、元の値を FC16 1 回で戻した                                                                                                                           | 成功（監査 ok 3 行）              |
| udp probe | ModbusSlave は UDP を受けていない（10054）                                                                                                                                                        | —                                 |

### 分かったこと

- Q25PRH では、U\G・Z・ZR・S の読取りとワード内ビットの書込みが、ゲストの契約で対象外（どれも送信前に断る）。Q の U0（イーサネットのユニット）や ZR を読むには、ゲスト側の契約の変更が要る。
- R の `U0\G0`・`U3E0\G0` は値が 0 で、読めたことは確かめたが、番地の正しさは値では確かめていない。
- UDP の自動オープンポート（5000）は R・Q とも 4E・3E の両方で読み書きできた。
