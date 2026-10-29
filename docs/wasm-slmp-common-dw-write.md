# SLMP WASM D/W 書込み境界

状態: **限定実装（2026-10-29）**。D/W 共通の u16 書込み（#877）と型付き書込み（#878）を実装した。2026-10-29 に D/W のワード内ビット書込み（guest #61 の F、RMW）を Host ゲートに足した（模擬のみ、実機未確認。下の「ワード内ビットの書込み」）。既定で無効で、本番鍵・保護配置・本番での有効化は未実施。受入れの対応表は末尾の節（2026-10-27）。Issue #875 は照合の結果をオーナーが確かめるまで開いたまま。

Hub は署名検証済みの write-v1 guest を明示選択した接続でのみ、SLMP の許可済み D/W 数値タグと標準 Q/R の M/Y/L/B Bool タグの書込みを受け付ける。通常の読取り専用 loader は write 宣言を拒否する。実際の配置先、信頼鍵、接続設定を整えるまで、本番での有効化は行わない。

## 接続設定と互換性

保存済み設定キー `d_u16_write_mode` と `d_u16_write_allowlist` を維持する。初期状態は `disabled` と空リスト。`enabled` と明示リストの両方が必要で、旧設定 `D3010-D3013` から W は許可されない。D と W に別の有効化スイッチは設けない。

```text
d_u16_write_mode=enabled
d_u16_write_allowlist=D3010-D3013,W1A-W1F,M10-M17,Y1A-Y1F
```

D/M/L 番地は10進、W/Y/B 番地は16進の大文字で記す。同一区間の両端は同じエリアとし、各エリア内の区間は昇順で重複させない。区間は最大16件、D/W は各区間・要求あたり最大960語、M/Y/L/B は最大7168点、設定文字列は最大256バイト。D3000/D3001を含む区間と、それらにまたがる要求は拒否する。空リスト、欠番、重複、逆順、エリア混在、不正な型・幅・値を含む要求では、一部だけを送信しない。`u16` タグの値は有限整数かつ `u16` 範囲内に限る。数値8型は型ごとの値域と語幅を検査し、公開値 API の f64 で正確に表せない 64 bit 整数は拒否する。

Hub は保存設定、タグの正規アドレスとエリア、幅、型、値、要求全体の連続性と許可範囲を送信前に独立検査する。guest は同一接続で取得した型式・機器別 SD 情報で全範囲を確認する。範囲不明は許可と見なさない。既存のタグ書込み認可、write control、broker 直列化、監査、署名と loader の検証を引き続き適用する。送信後のタイムアウト・切断・不正 ACK は結果不明として扱い、自動再送しない。送信前の全件検査は PLC 側の原子的更新を保証しない。

現行コードの通常書込み経路は StandardR の D/W 数値型、StandardQ の内部 W 数値型、同一接続の SD302 で内部範囲全体を証明できる Q25PRH の D 数値型、および標準 Q/R の M/Y/L/B Bool を対象とする。Host は数値8型の型・幅・値域を検査し、guest は数値型を語列へ変換する。Q25PRH の通常 D 書込みも明示 allowlist と独立した Host ゲートが必要で、`u16` のみに固定されていない。一方、隔離された `q25prh_d3010_3013_test_write` モードは D3010-D3013 の4点 `u16` に限定され、通常書込みと同時有効化できない。Q25PRH の数値8型すべてを本受入れ試験で検証したという意味ではない。既存の `d_u16_*` 設定キーを共通ゲートとして使うが、キー名は値型の上限を意味しない。Q25PRH の W・bit、標準 Q の D 書込み、timer/counter、および他の bit エリアはこの設定で許可されない（D/W の word-bit は下の節のビット区間だけで許可する）。読取りでは M/Y/L/B などの Bool を WIT Bool から Hub の Bit 値へ変換し、収集では true/false を 1/0・Good、機器拒否を値なし・Bad とする。別型の WIT 値は拒否し、数値8型読取りの f64 投影を維持する。

### ワード内ビットの書込み（F、2026-10-29、模擬のみ・実機未確認）

guest #61（`3e1c2d98`）は、要求の全項目が 1 つの D/W ワードの異なるビット（`D100.5`・`W1A.0`、値は Bool）のとき、0401 読取り → `(現在値 & !(set | clear)) | set` の 1401 書き戻し → 0401 確認読取りの 3 段で書く。許可は同じ `d_u16_write_allowlist` のビット区間（`D100.3-D100.5`・`W1A.0-W1B.F`、ワード番号は D が 10 進・W が 16 進の大文字、ビットは 16 進 1 桁の大文字）だけで、`ワード × 16 + ビット` で比べる。1 区間は 7168 ビット以下、同じデバイス内で昇順・重複なし（ワード区間とは別に数える）。区間数 16 と 256 文字の上限はワード区間と共通で、D3000/D3001 にかかるビット区間は拒否する。ワード区間はビットを許可せず、ビット区間はワードを許可しない。既存の保存値の意味は変わらない。Host（`operational_write.rs` の `WritePolicy`、`lib.rs` の `operational_word_bit_items`）は guest と同じ文法を独立に解析し、全項目が同じ接続・同じ D/W ワード・Bit 型・Bool 値・重複なしの正規アドレスで、各ビットがビット区間に入るときだけ guest へ渡す。ワードやビットデバイスと混ざった要求は全体を断る。RMW は原子的でなく、読取りと書き戻しの間に PLC や他局が同じワードの他のビットを変えると上書きする（guest の CONTRACT.md）。確認読取りの不一致は該当項目だけ、確認読取りの終了コードは全項目が `verification-failed` で、監査は `uncertain` になる。模擬試験は OW の `new_guest_word_bit_*`・`new_guest_hub_gate_refuses_word_bits_outside_bit_intervals_before_send`・`new_guest_rest_word_bit_writes_are_audited_by_outcome` と `crates/banto-driver-wasm/tests/slmp_word_bit_write.rs`。

CI は旧 read/write・診断 guest の固定コミットによる回帰を残し、最新の SLMP guest は guest main `3b7f0f61`（#44 の merge `c9c3d83d` に WIT の #47 を重ねた版）を独立ビルドして WIT/world と Hub ループバックを照合する（#887 で #34 の `c9cf3b8e` から #35 `b3d944f5` へ置き換え、続いて #43 の bit の outside 除外を含む #44 へ更新し、2026-10-28 #902 で #47 へ移した。#34 の V の拒否は #44 にも含まれる）。#47 より前の固定コミット（旧 read/write・診断・D 専用など）は `driver.wit` の `item-error` に `verification-failed` が無く、そのままではホストに読み込めないため、CI はそれらのコードと他の WIT はそのままに `driver.wit` だけを guest main のものに差し替えて作る。#34 は V edge relay を周期収集タグとして登録不可にする。1スキャンの edge を取り逃がし得るためであり、SLMP の低レベル V 読取り能力全体を否定するものではない。guest リポジトリには SLMP 専用 manifest がなく、Hub 受入れはこのリポジトリの checked-in テスト manifest を使う。旧 D 専用 guest の回帰も別に残す。模擬試験には実 PLC 通信、実際の本番有効化、信頼鍵の配置を含まない。

## 受入れの対応表（#875、2026-10-27）

Issue #875 の受入れ条件ごとの、実装と模擬試験の対応。統合試験は `#[ignore]` で、CI が固定 guest を渡して `--ignored` で実行する。実 PLC・本番鍵・本番の有効化は #875 の対象外。略記: POL = `crates/banto-driver-wasm/src/operational_write.rs`、LIB = `crates/banto-driver-wasm/src/lib.rs`、OW / W / QW / HUB = `apps/banto-hub/host/tests/` の `t12_slmp_wasm_operational_write`・`t12_slmp_wasm_write`・`t12_slmp_wasm_q_write`・`t12_slmp_wasm`。

| 条件                                           | 主な試験                                                                                                                                                                                                                                                                                                                  |
| ---------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 旧設定の維持・既定は無効・W を広げない         | POL `default_disabled_and_explicit_range_only`、LIB `operational_w_write_uses_the_same_policy_and_atomic_preflight`、OW `saved_hub_settings_and_broker_use_the_same_q_and_r_write_policy`・`explicit_w_range_uses_the_same_q_and_r_gate_without_broadening_d`                                                             |
| エリア付きの共通 allowlist（D 10 進・W 16 進） | POL `one_common_policy_requires_an_explicit_w_range`                                                                                                                                                                                                                                                                      |
| Host の独立した解析・重複・上限                | POL `malformed_or_broad_policy_fails_closed`・`overlap_is_rejected_even_when_another_area_sits_between`（他のエリアを挟んだ重複）、OW `malformed_allowlist_notation_is_rejected_and_sends_no_write_frame`（不正な表記で書込みフレームなし）                                                                               |
| 値と要求全体の事前検査・部分送信なし           | LIB `operational_write_preflight_is_atomic_and_bound_to_one_connection_range`、W `range_host_preflight_rejects_every_bad_item_without_a_write_frame`、OW `typed_words_are_one_complete_frame_and_bad_items_send_nothing`                                                                                                  |
| guest の SD による全範囲確認                   | QW `q25prh_sd302_missing_or_short_range_never_sends_write`、HUB `unknown_model_and_type_refusal_stop_before_sd_or_tag_read`。SD の番地の知識は guest だけが持つ                                                                                                                                                           |
| 認可・明示の write plugin・監査・保護番地      | W `hub_write_gates_and_audit_still_wrap_the_controlled_guest`、OW `rest_scoped_write_and_denied_spans_are_audited_without_extra_frames`、HUB `legacy_read_component_cannot_satisfy_a_write_manifest`、W の事前検査（D3000/D3001）                                                                                         |
| 結果不明・PLC エラーを成功にせず再送しない     | W `uncertain_four_word_ack_closes_session_without_a_second_write_frame`・`broker_timeout_after_write_closes_socket_without_resending_on_reconnect`、OW `disabled_short_range_and_bad_ack_never_replay_a_write`・`w_write_with_unknown_ack_or_plc_error_never_succeeds_or_replays`（W の ACK 不一致とエンドコード 0xC051） |
| D/W の正常書込み                               | OW `q_and_r_normal_write_are_one_frame_with_per_connection_gate`・`explicit_w_range_uses_the_same_q_and_r_gate_without_broadening_d`                                                                                                                                                                                      |

### 未検証・未対応

- SD の根拠不足による書込みの拒否を Hub 側で確かめたのは Q25PRH の D だけ。StandardR の D/W と StandardQ の内部 W は guest の試験で確かめる（banto-driver-wasm #37 の `standard_r_dw_and_q_internal_w_writes_send_nothing_without_complete_sd_span`: 点数の 1 語不足・SD の拒否と破損・型式不明・0101 の拒否で書込みフレームを作らない。ほかに `write_extension_borrows_the_same_discovered_resource`・`common_allowlist_w_write_uses_same_sd_evidence_and_word_ack`）。Hub が固定する guest（#44 `c9c3d83d`）は #37 を含む。
- 送信後に応答が来ない（ACK 不一致ではない純粋な無応答）W の書込みは、D の broker の timeout の試験だけ。
- 実 PLC での W の書込み、本番の有効化・信頼鍵の配置は未実施（#875 の対象外）。
- Q25PRH の W・bit、標準 Q の D 書込み（ワード内ビットも）、timer/counter は書込みの対象外。
- ワード内ビットの RMW の実機（iQ-R・Q）での確認は未実施（2026-10-29 時点で模擬のみ）。
