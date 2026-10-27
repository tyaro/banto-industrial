# WASM SLMP 範囲診断（#845）

状態: **限定実装・模擬試験済み（2026-10-27）**。診断 API・世代管理（#878）、旧結果と未保存設定の遮断（#886）、SLMP guest #35 の Hub 受入れ（#887）を実装した。受入れ項目の対応表は末尾の節（2026-10-27 作成）。A1・A5 の扱いはオーナー判断待ち。全設定・全世代の組合せの網羅、実 PLC・本番での確認は未実施。

`diagnostics-v1` は既存の read/write WIT world を変更しない任意の export です。
export がない旧 guest は診断非対応として表示し、既存の読取り・書込みは継続します。

Hub の `POST /api/plc-connections/{id}/diagnostics` は保存済み PLC タグを診断します。明示選択は最大 256 件、全件指定（空の `tagIds`）は内部で最大 256 件ずつ分割・集約します。editor 認可と監査を通し、稼働中の同じ broker 接続を使用します。再診断では同じ物理接続上で新しい guest connection resource を作り、0101 と適用 SD を再取得します。別の PLC ソケットは開きません。chunk 間で型式・点数根拠や接続世代が変われば結果を破棄します。複数の SD 読取りは原子的な PLC スナップショットではありません。`GET` は現在有効な結果のみ返します。結果には型式、SD 由来の領域点数・単位・根拠、およびタグ別の `inside` / `outside` / `unknown` / `invalid` / `unsupported` と理由を含みます。

範囲判定は guest が担当します。Hub は `index + word_count <= count` のような一律計算を行いません。接続・設定・driver version・適用済みタグ一覧・診断試行が変われば、以前の結果を表示せず、遅着結果も棄却します。診断結果はメモリのみで保持し、DB やタグ設定には保存しません。
接続編集画面は表示中に現在性を再確認し、失効した診断を消します。

採用済みの診断で `outside` と確定したタグは同じ接続で以後の読取り要求から除外し、既存の現在値を欠測・Bad にします。timeout・接続変更・世代不一致などで破棄された診断は読取り抑止へ反映しません。再接続でこの抑止は失効します。`unknown` は範囲内とみなさず理由を表示し、従来どおり明示タグの読取りを許します。診断なしの旧 guest の挙動も変えません。

検証は模擬 transport と実 Wasmtime component で行います。実 PLC、通常起動への新 guest 登録、署名鍵配置、64 bit 整数を正確に扱う公開 API、標準 Q の D 書込みはこの変更の対象外です。

同じ PR の書込み適合では、既存の接続別 D/W 共通 allowlist を使い、数値 8 型と標準 Q/R の M/Y/L/B Bool を Hub の型・幅・値域検査へ追加します。旧 D/u16 は維持します。allowlist、独立した書込み認可と監査、署名付き loader、D3000/D3001 保護、送信後の結果不明時に自動再送しない規則は変えません。現行の公開値 API は f64 なので、表現不能な 64 bit 整数は丸めて書かず拒否します。標準 Q の D 書込みは引き続き非対応です。

## SLMP guest #34 の追加受入れ

SLMP guest #34 (`c9cf3b8e`) は V edge relay を周期収集で確実に捕捉できないため登録・offset・read plan で拒否し、診断では理由付き `invalid` にします。混在要求の正常タグは元の covers 順と値対応を保ちます。Hub の接続設定保存・タグ登録・broker 読取り・REST 診断を固定 guest のループバック試験で確認します。診断 `unknown` は理由を表示しつつ明示読取りを続け、`Bad` へ落とす対象は現在の有効な診断の `outside` に限ります。全タグ範囲の実機検証や世代管理など #845 の残件は、この模擬受入れだけで完了としません。

## SLMP guest #35 の Hub 受入れ

CI は SLMP guest #35 merge `b3d944f5` を固定して別途ビルドし、既存の Host テスト用 manifest と `driver-with-write-and-diagnostics-v1` world で Hub のループバック受入れを行います。標準 Q の X は SD290、標準 R の SB/F は SD268–271 の点数を報告します。各領域の N−1/N とゼロ、Q X=32768 の exact、R の low/high word、SD 取得拒否時の `unknown`、`outside` の読取り除外を確認します。旧 guest の固定回帰、`outside` の `Bad` 品質、接続停止後の GET `unavailable` も確認します。#886 の診断 UI 修正には手を加えません。実 PLC と本番配置は対象外です。

## 受入れの対応表（#845、2026-10-27）

Issue #845 の受入れ条件ごとの、実装と模擬試験の対応（main `19c90110` 時点）。「模擬試験あり」は模擬 transport と固定 guest による確認で、実 PLC・本番での確認は含まない。略記: OW = `apps/banto-hub/host/tests/t12_slmp_wasm_operational_write/mod.rs`、UI = `e2e/tests-banto-hub/banto-hub-tags-range-diagnostic.spec.ts`。

| 条件                                              | 状態                       | 主な実装と試験                                                                                                                                                                                                                 |
| ------------------------------------------------- | -------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| A1 保存時の静的検査と実機検証の分離               | 部分的（オーナー判断待ち） | 診断は保存済みタグに対する別の操作（`apps/banto-hub/core/src/rest/diagnostics.rs`）で、保存を止めない。「実機未確認」という明示のタグ状態・表示はない。`unknown` と未診断で足りるかは未決。                                    |
| A2 接続成功と全タグ成功の区別、タグ別の理由       | 模擬試験あり               | `TagVerdict` が理由付き。UI「unknown の理由と outside を分け、接続成功を全タグ成功として扱わない」、OW `saved_tags_are_diagnosed_through_the_authenticated_rest_and_broker_path`。                                             |
| A3 読取りのみ、outside は送信しない、境界またぎ   | 部分的                     | outside の読取り除外は `banto-driver-engine` の `read`。OW `new_guest_q_x_sd290_exact_bound_and_refusal_reach_hub` ほか。U32・文字列の境界またぎを Hub 側で確かめる試験はない（guest の責務）。                                |
| A4 outside・未確認で過去の正常値を使わない        | 模擬試験あり               | `banto-collect` の `mark_outside_bad` と `outside_diagnostic_invalidates_value_without_faking_a_read`。`unknown` は契約どおり Bad にせず明示読取りを続ける。                                                                   |
| A5 書込み前の全範囲検査、範囲外なら全体をエラー   | 部分的（オーナー判断待ち） | Host の allowlist・型・幅・値域の全件検査と guest の SD による全範囲確認で拒否する（OW `typed_words_are_one_complete_frame_and_bad_items_send_nothing`）。書込みは診断の outside 集合を使わない。                              |
| A6 接続先・時点の対応付け、再取得、取得失敗の扱い | 模擬試験あり               | revision・driver version・epoch で現在性を検査（`apps/banto-hub/core/src/hub.rs` の `current_range_diagnostic`）。`banto-broker` の `diagnostic_epoch_rejects_reconnect_and_replaced_task`、UI の 10 件、SD 拒否は `unknown`。 |

検証観点のうち、通常範囲・範囲外の混在・点数取得失敗・同じ接続での点数の縮小（OW の `set_count(2000)` の再診断）は模擬試験がある。型式不明だけの試験、「未確認から PLC の設定完了後の再テスト」の流れ、再接続をまたいだ縮小で Bad になる試験、D の末尾の複数語タグの Hub 側の試験はない。全設定・全世代の組合せ、実 PLC・本番は別枠で未実施。
