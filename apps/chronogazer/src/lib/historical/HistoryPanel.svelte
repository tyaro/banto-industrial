<script lang="ts">
	/**
	 * ヒストリカルのトレンド（#1207 PR2）。`TrendPanel`（監視画面）と同じく、データを取りに
	 * 行かず props で受けて描くだけ。違いは、時間窓の選択が無い（期間は画面の上の指定）
	 * ことと、行が「区間の最小 → 最大」の 2 行ずつであること（`historyLogic.ts`）。
	 *
	 * 色・凡例・しきい値の帯（押したペン 1 本だけ）・縦軸と値の書式は `TrendPanel` と同じ
	 * 部品・同じ作法（`--banto-chart-<並び>` を `--cg-slot-<枠>` へ差し替える）。
	 */
	import { LineChart } from '@banto/charts';
	import {
		hasThresholds,
		penLegendLabel,
		thresholdBands,
		trendIncludeY,
		trendSeriesId,
		trendTooltipFormatter,
		trendYFormatter,
		type TrendPenInfo,
		type TrendRow
	} from '#lib/monitor/trendLogic.js';
	import { historyTimeLabel } from './historyLogic.js';

	interface Props {
		groupName: string;
		pens: readonly TrendPenInfo[];
		rows: readonly TrendRow[];
		/** 期間の幅（ms、時刻の書式を決める）。 */
		spanMs: number;
		/** 帯を出すペンのタグ ID（`null` = 帯なし）。 */
		bandTagId: number | null;
		onSelectBandPen: (tagId: number) => void;
		/** 画面外の説明文。 */
		description: string;
		/** 描画域の幅（px、画面が `bins` を決めるのに使う）。 */
		width?: number;
		/** 図の代わりに出す空の表示の文言。 */
		emptyMessage: string;
	}

	let {
		groupName,
		pens,
		rows,
		spanMs,
		bandTagId,
		onSelectBandPen,
		description,
		width = $bindable(0),
		emptyMessage
	}: Props = $props();

	const series = $derived(
		pens.map((pen, i) => ({
			id: trendSeriesId(i),
			label: penLegendLabel(pen),
			y: (row: TrendRow) => row.values[i]
		}))
	);

	const colorStyle = $derived(
		pens.map((pen, i) => `--banto-chart-${i + 1}: var(--cg-slot-${pen.colorSlot});`).join(' ')
	);

	const bandPen = $derived(pens.find((pen) => pen.tagId === bandTagId) ?? null);
	const bands = $derived(bandPen ? thresholdBands(bandPen.thresholds) : []);
	const formatY = $derived(trendYFormatter(pens));
	const formatTooltip = $derived(trendTooltipFormatter(pens));
	const includeY = $derived(trendIncludeY(pens));
	const data = $derived([...rows]);

	function formatX(v: unknown): string {
		return typeof v === 'number' ? historyTimeLabel(v, spanMs) : String(v ?? '');
	}
</script>

<section class="history-panel" aria-label={`${groupName} のヒストリカル表示`}>
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
			label={`${groupName} のヒストリカル`}
			height={360}
			gaps="break"
			{bands}
			{formatY}
			{formatTooltip}
			{includeY}
			{formatX}
			messages={{ emptyState: () => emptyMessage }}
		/>
	</div>
</section>

<style>
	.history-panel {
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
