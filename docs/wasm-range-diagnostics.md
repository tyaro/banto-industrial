# WASM SLMP 範囲診断（#845）

`diagnostics-v1` は既存の read/write WIT world を変更しない任意の export です。
export がない旧 guest は診断非対応として表示し、既存の読取り・書込みは継続します。

Hub の `POST /api/plc-connections/{id}/diagnostics` は保存済み PLC タグから最大 256 件を選び、稼働中の同じ broker 接続で診断します。editor 認可と監査を通し、新しい PLC 接続は作りません。`GET` は現在有効な結果のみ返します。結果には型式、SD 由来の領域点数・単位・根拠、およびタグ別の `inside` / `outside` / `unknown` / `invalid` / `unsupported` と理由を含みます。

範囲判定は guest が担当します。Hub は `index + word_count <= count` のような一律計算を行いません。接続・設定・driver version・適用済みタグ一覧・診断試行が変われば、以前の結果を表示せず、遅着結果も棄却します。診断結果はメモリのみで保持し、DB やタグ設定には保存しません。
接続編集画面は表示中に現在性を再確認し、失効した診断を消します。

`outside` と確定したタグは同じ接続で以後の読取り要求から除外し、既存の現在値を欠測・Bad にします。再接続でこの抑止は失効します。`unknown` は範囲内とみなさず理由を表示し、従来どおり明示タグの読取りを許します。診断なしの旧 guest の挙動も変えません。

検証は模擬 transport と実 Wasmtime component で行います。実 PLC、通常起動への新 guest 登録、署名鍵配置、64 bit 整数を正確に扱う公開 API、標準 Q の D 書込みはこの変更の対象外です。

同じ PR の書込み適合では、既存の接続別 D/W 共通 allowlist を使い、数値 8 型と標準 Q/R の M/Y/L/B Bool を Hub の型・幅・値域検査へ追加します。旧 D/u16 は維持します。allowlist、独立した書込み認可と監査、署名付き loader、D3000/D3001 保護、送信後の結果不明時に自動再送しない規則は変えません。現行の公開値 API は f64 なので、表現不能な 64 bit 整数は丸めて書かず拒否します。標準 Q の D 書込みは引き続き非対応です。
