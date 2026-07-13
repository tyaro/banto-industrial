<script lang="ts">
	/**
	 * Shared body renderer for a single dashboard dock panel (spec §5.3 v2
	 * pop-out): extracted from the dashboard page's `dockPanel` snippet so the
	 * SAME markup can render inside the dashboard's `DockHost` (docked pane or
	 * floating pseudo-window) AND, unmodified, as the standalone content of a
	 * REAL Tauri `WebviewWindow` at `routes/panel/[id]` once a panel is popped
	 * out - that route has no access to the dashboard page's own locals, so
	 * this component is fully self-contained: it loads its own `items` data
	 * rather than receiving already-aggregated rows as props.
	 *
	 * NOTE (perf): this loads the full item list (up to 20,000 rows, same
	 * `initialParams` as the dashboard page) independently, once per
	 * component instance. That means a popped-out panel's native window loads
	 * its own copy on top of whatever the main window already holds - fine
	 * here since each Tauri window is its own document/JS heap with no shared
	 * cache between them (no cross-window `admin-core` cache exists yet); an
	 * app with a heavier data provider would want to add one before relying on
	 * this pattern for many simultaneous pop-out windows.
	 */
	import {
		BoxPlot,
		downloadSvg,
		Histogram,
		LineChart,
		ParetoChart,
		PieChart,
		rollingAppend
	} from '@banto/charts';
	import { createListResource } from '@banto/admin-core';
	import { onDestroy, onMount } from 'svelte';
	import type { Item } from '$lib/banto/sampleData';
	import {
		categoryCounts,
		nextTrendPoint,
		priceBuckets,
		priceByCategoryGroups,
		priceValues,
		seedTrendPoints,
		updatesByMonth,
		type TrendPoint
	} from '$lib/banto/dashboard';

	interface Props {
		id: string;
		/** Chart height in px; the caller measures its own container (dashboard: the dock pane's body; `/panel/[id]`: the route's viewport). */
		height?: number;
	}

	let { id, height = 280 }: Props = $props();

	const list = createListResource<Item>('items', {
		initialParams: { pagination: { offset: 0, limit: 20_000 } }
	});

	$effect(() => {
		void list.load();
		return () => list.dispose();
	});

	const monthCounts = $derived(updatesByMonth(list.rows));
	const buckets = $derived(priceBuckets(list.rows));

	// M13 SPC panel demo data (roadmap.md M13, `$lib/banto/dashboard.ts`):
	// price distribution (histogram + normal curve), category Pareto, price
	// spread per top category (box plot).
	const prices = $derived(priceValues(list.rows));
	const paretoItems = $derived(categoryCounts(list.rows));
	const boxGroups = $derived(priceByCategoryGroups(list.rows));

	// M13 trend panel demo (roadmap.md M13): 1 point/sec streaming via
	// `rollingAppend` (window capped so the chart never grows unbounded),
	// started/stopped with this component's own lifecycle - each dock
	// pane/floating window/popped-out Tauri window gets its own instance and
	// therefore its own interval (see `core/rolling.ts`, `LineChart`'s
	// streaming doc comment).
	const TREND_WINDOW = 120;
	const TREND_INTERVAL_MS = 1000;
	let trendData: TrendPoint[] = $state(seedTrendPoints(40));
	let trendTimer: ReturnType<typeof setInterval> | undefined;

	onMount(() => {
		if (id !== 'trend') return;
		trendTimer = setInterval(() => {
			const next = nextTrendPoint(trendData[trendData.length - 1]);
			trendData = rollingAppend(trendData, [next], TREND_WINDOW);
		}, TREND_INTERVAL_MS);
	});

	onDestroy(() => clearInterval(trendTimer));

	const trendBands = [{ from: 66, to: 70, label: '管理範囲', colorVar: 'var(--banto-success)' }];
	const trendMarkers = [{ at: 10, label: '点検', colorVar: 'var(--banto-warning)' }];

	// M13 SVG export demo (roadmap.md M13, `core/export.ts`): the chart
	// components don't expose their `<svg>` via a prop/ref, so the caller
	// grabs it with a plain DOM query on the wrapping element instead (the
	// pattern `core/export.ts`'s doc comment describes for "利用側").
	let histogramWrapper: HTMLElement | undefined = $state();
	function exportHistogram(): void {
		const svg = histogramWrapper?.querySelector('svg');
		if (svg) downloadSvg(svg, 'price-histogram.svg');
	}

	const countLabel = (n: number) => `${n.toLocaleString()}件`;
	const yen = (n: number) => `¥${n.toLocaleString()}`;
</script>

{#if list.loading && list.rows.length === 0}
	<p class="status">読み込み中…</p>
{:else if id === 'monthly'}
	<LineChart
		data={monthCounts}
		x={(row) => row.month}
		series={[{ id: 'count', label: '更新件数', y: (row) => row.count }]}
		area
		label="月別更新件数の面グラフ"
		{height}
		formatY={(n) => n.toLocaleString()}
	/>
{:else if id === 'priceBuckets'}
	<PieChart
		data={buckets}
		category={(row) => row.bucket}
		value={(row) => row.count}
		donut
		label="価格帯分布のドーナツグラフ"
		{height}
		formatValue={countLabel}
	/>
{:else if id === 'spc'}
	<div class="spc-panel">
		<section class="spc-chart" bind:this={histogramWrapper}>
			<div class="spc-chart-header">
				<h3>価格分布（正規分布カーブ）</h3>
				<button type="button" class="export-btn" onclick={exportHistogram}>SVGエクスポート</button>
			</div>
			<Histogram
				values={prices}
				label="価格分布のヒストグラム（正規分布カーブ重ね描き）"
				height={220}
				normalCurve
				formatValue={yen}
			/>
		</section>
		<section class="spc-chart">
			<h3>カテゴリ別商品数（パレート図）</h3>
			<ParetoChart
				items={paretoItems}
				label="カテゴリ別商品数のパレート図"
				height={220}
				formatValue={countLabel}
			/>
		</section>
		<section class="spc-chart">
			<h3>カテゴリ別価格分布（箱ひげ図）</h3>
			<BoxPlot
				groups={boxGroups}
				label="カテゴリ別価格分布の箱ひげ図（上位カテゴリ）"
				height={220}
				formatValue={yen}
			/>
		</section>
	</div>
{:else if id === 'trend'}
	<LineChart
		data={trendData}
		x={(row) => row.t}
		series={[
			{ id: 'temperature', label: '温度(℃)', y: (row) => row.temperature },
			{ id: 'pressure', label: '圧力(MPa)', y: (row) => row.pressure, axis: 'right' }
		]}
		label="温度・圧力トレンド（ズーム/パン、しきい値バンド、イベントマーカー、第2Y軸、1秒間隔ストリーミング更新）"
		{height}
		zoomable
		bands={trendBands}
		markers={trendMarkers}
		formatX={(v) => `#${v}`}
		formatY={(n) => `${n.toFixed(1)}℃`}
		formatYRight={(n) => `${n.toFixed(2)}MPa`}
	/>
{:else if id === 'memo'}
	<p class="memo">
		タイトルバー/タブをドラッグして分割・タブ化・再配置、仕切りでサイズ変更を試せます。レイアウトは自動保存されます。
	</p>
{:else}
	<p class="status">不明なパネルです</p>
{/if}

<style>
	.status {
		margin: 0;
		color: var(--banto-text-muted);
		font-size: 0.85rem;
	}

	.memo {
		margin: 0;
		color: var(--banto-text-muted);
		font-size: 0.85rem;
		line-height: 1.6;
	}

	.spc-panel {
		display: flex;
		flex-direction: column;
		gap: 1rem;
		height: 100%;
		overflow-y: auto;
	}

	.spc-chart h3 {
		margin: 0 0 0.4rem;
		font-size: 0.8rem;
		font-weight: 600;
		color: var(--banto-text-muted);
	}

	.spc-chart-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.5rem;
	}

	.spc-chart-header h3 {
		margin: 0;
	}

	.export-btn {
		border: 1px solid var(--banto-border);
		border-radius: 999px;
		background: var(--banto-surface);
		color: var(--banto-text);
		padding: 0.2rem 0.7rem;
		font-size: 0.75rem;
		cursor: pointer;
	}

	.export-btn:hover {
		border-color: var(--banto-primary);
		color: var(--banto-primary);
	}

	.export-btn:focus-visible {
		outline: none;
		box-shadow: var(--banto-focus-ring);
	}
</style>
