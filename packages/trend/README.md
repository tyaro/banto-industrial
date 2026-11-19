# @banto-industrial/trend

記録計のトレンドの共通部品（時刻の軸・ペンごとの縦軸・MIN/MAX の包絡・カーソル）。ChronoGazer と temp-scada が使う。設計は [docs/chronogazer/trend-design.md](../../docs/chronogazer/trend-design.md)。

- 表示だけを持ち、データの取り方は持たない。利用側が `TrendFetch` を渡す。
- 共通の契約は `Bin { tMs, min, max }` だけ（両方 `null` の区間は欠測）。
- `TrendChart` は制御された部品。表示範囲は利用側が持ち、操作の結果は `onRangeChange` で返る。

## 使い方

```svelte
<script lang="ts">
	import {
		TrendChart,
		binCount,
		binMs,
		createRangeLoader,
		presetRange,
		type Bin,
		type TimeRange,
		type TrendPen
	} from '@banto-industrial/trend';

	const pens: TrendPen[] = [
		{ id: 'tag-1', label: '炉温', colorSlot: 1, unit: '℃', decimals: 1 },
		{ id: 'tag-2', label: '圧力', colorSlot: 2, unit: 'kPa', axis: 'right' }
	];
	let range = $state<TimeRange>(presetRange(60 * 60 * 1000, Date.now()));
	let width = $state(0);
	let binsByPen = $state<Record<string, Bin[]>>({});
	// 区間の幅は、受け取ったデータと組で持つ（読み直しを待つ間・失敗したときも、
	// 手元のデータはそれを読んだときの幅で描く。今の範囲と幅から計算し直さない）。
	let loadedBinMs = $state(0);
	let loading = $state(false);

	const loader = createRangeLoader({
		// 利用側の取り方（ChronoGazer は GET /api/collect/history、SCADA は History API）。
		fetch: ({ range, penIds, bins, signal }) => fetchHistory(range, penIds, bins, signal),
		onResult: (result, req) => {
			binsByPen = result;
			loadedBinMs = binMs(req.range, req.bins);
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
	{binsByPen}
	binMs={loadedBinMs}
	{loading}
	bind:width
	onRangeChange={(r) => (range = r)}
/>
```

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
