<script lang="ts">
	/**
	 * リアルタイムトレンド（R1-D の D-3b、recorder-requirements.md §3.3 の「リアル
	 * タイム」）。banto の `LineChart`（v6.3.0 の `gaps: 'break'`・開いた帯）で描く。
	 *
	 * 他のパネルと同じく、**グループの定義・ペンごとの表示・格子の行を props で
	 * 受け取るだけ**で、自分ではデータを取りに行かない（§3.7.4 の関所のプレビューが
	 * 使い回す）。格子・帯・説明文の判断は `trendLogic.ts`、バッファの持ち主は画面の
	 * `trendFeed.svelte.ts`。
	 *
	 * - **値が無い刻みは線を切る**（`gaps: 'break'`。0 の位置に描かない、#414 段階2）。
	 * - **Y 軸は左の共通 1 本・自動スケール**、単位は凡例に（Q7）。
	 * - **系列の色はペンの色の枠（`colorSlot`）**。`LineChart` は系列の並び順で
	 *   `--banto-chart-1..8` を使うので、描画域の要素で `--banto-chart-<並び>` を
	 *   `--cg-slot-<枠>`（外側の要素で元の色を写しておいたもの）に差し替える。外側で
	 *   写してから内側で差し替えるのは、`--banto-chart-1: var(--banto-chart-3)` の
	 *   ような同じ要素での参照が入れ替え（1 と 3 の交換）で循環して無効になるため。
	 * - **凡例はこのパネルのもの**（`LineChart` の凡例は押せないので隠す）。ペンを押すと
	 *   しきい値の帯を出すペンが替わる（Q5: 帯は選んだ 1 ペンだけ。`aria-pressed`）。
	 * - 図は `role="img"` で中が読まれないので、ペン・単位・窓・値の範囲・帯を画面外の
	 *   文（`trendDescription`）で伝える（D-2 の `meterDescription` と同じ作法）。
	 * - 時間窓は端末ごと（Q4）。選んだ窓は画面が localStorage に覚え、グループの定義には
	 *   書かない。
	 */
	import { LineChart } from '@banto/charts';
	import type { DisplayGroup } from '#lib/banto/displayGroupsAdmin.js';
	import {
		penLegendLabel,
		thresholdBands,
		trendDescription,
		trendSeriesId,
		trendTooltipFormatter,
		trendTimeLabel,
		trendWindowLabel,
		trendIncludeY,
		trendYFormatter,
		hasThresholds,
		type TrendPenInfo,
		type TrendRow
	} from './trendLogic';

	interface Props {
		group: Pick<DisplayGroup, 'name'>;
		pens: readonly TrendPenInfo[];
		rows: readonly TrendRow[];
		windowSec: number;
		windowOptions: readonly number[];
		onWindowChange: (sec: number) => void;
		/** 帯を出すペンのタグ ID（`null` = 帯なし）。 */
		bandTagId: number | null;
		onSelectBandPen: (tagId: number) => void;
		/** パネルの上に添える注記（履歴を読めない・不明なタグ・シミュレーション）。 */
		notices: readonly string[];
		/** 直近の履歴を読み込み中。 */
		historyLoading: boolean;
		/** 描画域の幅（px、画面が刻みを決めるのに使う）。 */
		width?: number;
	}

	let {
		group,
		pens,
		rows,
		windowSec,
		windowOptions,
		onWindowChange,
		bandTagId,
		onSelectBandPen,
		notices,
		historyLoading,
		width = $bindable(0)
	}: Props = $props();

	const series = $derived(
		pens.map((pen, i) => ({
			id: trendSeriesId(i),
			label: penLegendLabel(pen),
			y: (row: TrendRow) => row.values[i]
		}))
	);

	/** `--banto-chart-<並び>` をペンの色の枠へ差し替える（doc 参照）。 */
	const colorStyle = $derived(
		pens.map((pen, i) => `--banto-chart-${i + 1}: var(--cg-slot-${pen.colorSlot});`).join(' ')
	);

	const bandPen = $derived(pens.find((pen) => pen.tagId === bandTagId) ?? null);
	const bands = $derived(bandPen ? thresholdBands(bandPen.thresholds) : []);

	// 縦軸・ツールチップの書式（全ペンが bit なら False / True。判断は trendYFormatter）。
	const formatY = $derived(trendYFormatter(pens));
	// ツールチップは縦軸と別の書式（ペンごと。bit は True / False、区間の中点は False / True）。
	const formatTooltip = $derived(trendTooltipFormatter(pens));
	// 全ペンが bit なら縦軸に 0 と 1 を必ず含める（中点 0.5 だけでも False / True が出る）。
	const includeY = $derived(trendIncludeY(pens));
	function formatX(v: unknown): string {
		return typeof v === 'number' ? trendTimeLabel(v) : String(v ?? '');
	}

	const description = $derived(
		trendDescription({ windowSec, pens, rows, bandTagId: bandPen?.tagId ?? null })
	);
	const data = $derived([...rows]);
</script>

<section class="trend-panel" aria-label={`${group.name} のトレンド表示`}>
	<div class="toolbar">
		<label class="window">
			時間窓
			<select
				value={String(windowSec)}
				onchange={(event) => onWindowChange(Number(event.currentTarget.value))}
			>
				{#each windowOptions as option (option)}
					<option value={String(option)}>{trendWindowLabel(option)}</option>
				{/each}
			</select>
		</label>
		{#if historyLoading}
			<span class="loading" role="status">直近の履歴を読み込んでいます。</span>
		{/if}
	</div>

	{#each notices as notice (notice)}
		<p class="note warn" role="status">{notice}</p>
	{/each}

	<ul class="pens" aria-label="ペン（押すと、しきい値の帯をそのペンで表示します）">
		{#each pens as pen, i (`${i}:${pen.tagId}`)}
			<li>
				<button
					type="button"
					class="pen"
					aria-pressed={pen.tagId === bandPen?.tagId}
					data-thresholds={hasThresholds(pen.thresholds) ? 'set' : 'none'}
					onclick={() => onSelectBandPen(pen.tagId)}
				>
					<span class="swatch" style:background={`var(--cg-slot-${pen.colorSlot})`}></span>
					<span class="pen-label">{penLegendLabel(pen)}</span>
					{#if pen.tagId === bandPen?.tagId}
						<span class="band-mark">{hasThresholds(pen.thresholds) ? '帯' : '帯（設定なし）'}</span>
					{/if}
				</button>
			</li>
		{/each}
	</ul>

	<p class="sr-only">{description}</p>

	<div class="chart-host" style={colorStyle} bind:clientWidth={width}>
		<LineChart
			{data}
			x="t"
			{series}
			label={`${group.name} のトレンド`}
			height={320}
			gaps="break"
			{bands}
			{formatY}
			{formatTooltip}
			{includeY}
			{formatX}
			messages={{ emptyState: () => '表示できる値はまだありません' }}
		/>
	</div>
</section>

<style>
	.trend-panel {
		/* 元の系列色を写しておく（`.chart-host` で並び順の枠をこれに差し替える）。 */
		--cg-slot-1: var(--banto-chart-1);
		--cg-slot-2: var(--banto-chart-2);
		--cg-slot-3: var(--banto-chart-3);
		--cg-slot-4: var(--banto-chart-4);
		--cg-slot-5: var(--banto-chart-5);
		--cg-slot-6: var(--banto-chart-6);
		--cg-slot-7: var(--banto-chart-7);
		--cg-slot-8: var(--banto-chart-8);
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
		padding: 0.75rem;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
		background: var(--banto-surface);
		min-width: 0;
	}

	.toolbar {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.75rem;
		font-size: 0.85rem;
	}

	.window {
		display: inline-flex;
		align-items: center;
		gap: 0.4rem;
		color: var(--banto-text-muted);
	}

	.window select {
		font: inherit;
	}

	.loading {
		color: var(--banto-text-muted);
	}

	.note {
		margin: 0;
		font-size: 0.85rem;
	}

	.note.warn {
		color: var(--banto-warning-tint-text);
		background: var(--banto-warning-tint);
		padding: 0.35rem 0.6rem;
		border-radius: var(--banto-radius);
	}

	.pens {
		display: flex;
		flex-wrap: wrap;
		gap: 0.35rem;
		margin: 0;
		padding: 0;
		list-style: none;
	}

	.pen {
		display: inline-flex;
		align-items: center;
		gap: 0.35rem;
		padding: 0.15rem 0.5rem;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
		background: transparent;
		color: var(--banto-text-muted);
		font: inherit;
		font-size: 0.8rem;
		cursor: pointer;
	}

	.pen[aria-pressed='true'] {
		border-color: var(--banto-text);
		color: var(--banto-text);
		font-weight: 600;
	}

	.swatch {
		width: 10px;
		height: 10px;
		border-radius: 2px;
		flex: 0 0 auto;
	}

	.band-mark {
		padding: 0 0.3rem;
		border-radius: var(--banto-radius);
		background: var(--banto-warning-tint);
		color: var(--banto-warning-tint-text);
		font-weight: 400;
	}

	.chart-host {
		min-width: 0;
	}

	/* 凡例はこのパネルの `.pens`（押せる）を使う。`LineChart` の凡例は重複するので隠す。 */
	.chart-host :global(.chart-legend) {
		display: none;
	}

	.sr-only {
		position: absolute;
		width: 1px;
		height: 1px;
		padding: 0;
		margin: -1px;
		overflow: hidden;
		clip: rect(0, 0, 0, 0);
		white-space: nowrap;
		border: 0;
	}
</style>
