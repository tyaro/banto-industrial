# @banto-industrial/trend

記録計のトレンドの共通部品（時刻の軸・ペンごとの縦軸・MIN/MAX の包絡・カーソル）。ChronoGazer と temp-scada が使う。設計は [docs/chronogazer/trend-design.md](../../docs/chronogazer/trend-design.md)。

- 表示だけを持ち、データの取り方は持たない。利用側が `TrendFetch` を渡す。
- 共通の区間の契約は `Bin { tMs, min, max }` だけ（両方 `null` の区間は欠測）。
- データは「取得範囲 + ペンごとの `{ bins, binMs }`」（`TrendData`）で渡す。幅は API の実際の値をデータと組で持つ。
- `TrendChart` は制御された部品。表示範囲は利用側が持ち、操作の結果は `onRangeChange` で返る。

## 使い方

```svelte
<script lang="ts">
	import {
		TrendChart,
		binCount,
		createRangeLoader,
		presetRange,
		type TimeRange,
		type TrendData,
		type TrendPen
	} from '@banto-industrial/trend';

	const pens: TrendPen[] = [
		{ id: 'tag-1', label: '炉温', colorSlot: 1, unit: '℃', decimals: 1 },
		{ id: 'tag-2', label: '圧力', colorSlot: 2, unit: 'kPa', axis: 'right' }
	];
	let range = $state<TimeRange>(presetRange(60 * 60 * 1000, Date.now()));
	let width = $state(0);
	// 取得した範囲と各ペンの { bins, binMs } は組のまま持つ（読み直しを待つ間・失敗したときも、
	// 手元のデータは取得範囲と各系列の幅でだけ描かれる）。
	let data = $state<TrendData | null>(null);
	let loading = $state(false);

	const loader = createRangeLoader({
		// 利用側の取り方（ChronoGazer は GET /api/collect/history、SCADA は History API）。
		// 各ペンの { bins, binMs } を返す。binMs は API が返した実際の値を使う
		// （要求の区間の数から推定しない。収集周期まで広がり、ペンごとに違いうる）。
		fetch: ({ range, penIds, bins, signal }) => fetchHistory(range, penIds, bins, signal),
		onResult: (d) => {
			data = d;
			loading = false;
		},
		onError: () => (loading = false)
	});

	$effect(() => {
		if (width <= 0) return;
		loading = true;
		loader.request({ range, penIds: pens.map((p) => p.id), bins: binCount(width) });
	});
	$effect(() => () => loader.dispose());
</script>

<TrendChart
	label="炉の温度と圧力"
	{range}
	{pens}
	{data}
	{loading}
	bind:width
	onRangeChange={(r) => (range = r)}
/>
```

## 帯と凡例の選択（任意の props）

| props           | 型                                                  | 動き                                                                                                        |
| --------------- | --------------------------------------------------- | ----------------------------------------------------------------------------------------------------------- |
| `bands`         | `{ penId, items: { from, to, colorVar, label }[] }` | `penId` のペンの縦軸で、横の帯を線の下に描く。`from`・`to` が `null` の側は縦軸の端まで（しきい値の帯など） |
| `onPenSelect`   | `(penId) => void`                                   | 渡すと凡例の各項目がボタンになる（押した結果は利用側が決める）                                              |
| `selectedPenId` | `string \| null`                                    | 凡例で押した印（`aria-pressed`）を付けるペン                                                                |
| `legendLabel`   | `string`                                            | 凡例の一覧の名前（aria-label）                                                                              |
| `legendBadge`   | `Snippet<[TrendPen]>`                               | 凡例の項目の末尾に足す利用側の印                                                                            |

帯の型は汎用の形で、利用側の型（ChronoGazer のしきい値など）は利用側で変換して渡す。管理線（設計書 §10 の 7）に置き換えるときに見直す。

## ペンの線種・線幅・マーカー

`TrendPen` の任意の `lineStyle`（`solid`・`dashed`・`dashDot`・`dashDotDot`・`none`）・`lineWidth`（1〜4 px、既定 1.5）・`marker`（`none`・`circle`・`square`・`triangle`・`diamond`）・`markerSize`（2〜8 px、既定 4）。`null`・未指定は既定、範囲外は丸める。計算は `dashArray`・`markerPoints`・`markerPath`（`core/style.ts`、描画の層から独立）。凡例の見本は `TrendPenSwatch`（`pen` を渡すと線 + 中央のマーカーの小さな SVG になり、表示グループのページでも使える）。

## 管理線と、越えた区間の色替え

`TrendPen.limits`（`{ value, side: 'upper' | 'lower', colorVar, label }[]`、null・空は引かない）。そのペンの縦軸で、描画域の幅いっぱいの細い破線を引き、右端の内側に `label` を添える。`upper` は値より上、`lower` は値より下を「越えた」区間とし、その区間だけペンの線とマーカーを `colorVar` で重ね描きする（並びの後ろが前の上に重なるので、H → HH の順に渡せば HH の色が勝つ）。縦軸の外の線は引かないが、越えた区間には効く。線なし（`lineStyle: 'none'`）のペンは線を重ねず、マーカーだけ。要約（aria）に「管理線 HH 90・H 80」と値を足す。計算は `limitLayout`（`core/limits.ts`、描画の層から独立）。

## bit のペンの帯と True / False

`TrendPen.bit` が真のペンは、値 0 / 1 を `False` / `True`（0.5 以上が True）と出す。カーソルの値は `False`・`True`・区間の中で変わったら `False〜True`、凡例の範囲と要約は `False / True`、縦軸の持ち主が bit のペンなら 0・1 の目盛りが `False`・`True` になる。`formatBitValue`（`core/pens.ts`）。

`bitStyle: 'band'` なら描画域の下の帯（タイムチャート）で描く。`bitBandHeight`（px、既定 16、8〜32）。ペンの並びの順に下から積み、帯の間は 2px。帯の合計が描画域の半分を超えたら太さを同じ割合で縮める。アナログのペンの縦の長さは帯の分だけ縮み、帯のペンは線・マーカー・管理線・縦軸の目盛りの持ち主から外れる。True は濃い塗り、区間の中で変わった区間は薄い塗り（opacity 0.4）、False・欠測は塗らない。計算は `bitBandLayout`・`bitSpans`（`core/bitBands.ts`、描画の層から独立）。未指定・`'step'` は今までどおりの線（段差）。

## 操作

| 操作                     | 動き                               |
| ------------------------ | ---------------------------------- |
| Ctrl（Cmd）+ホイール     | ポインタの時刻を基準に拡大・縮小   |
| Shift+ホイール・ドラッグ | 横のパン（今より先へは行かない）   |
| ホイールだけ             | ページのスクロール（図は止めない） |
| ← →                      | 幅の 10% のパン                    |
| + / -                    | 中央を基準に拡大・縮小             |
| Esc                      | カーソルを消す                     |

## 確かめ方

```sh
pnpm --filter @banto-industrial/trend test   # vitest（TZ=Asia/Tokyo に固定）
pnpm --filter @banto-industrial/trend check  # svelte-check
```
