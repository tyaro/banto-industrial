<script lang="ts">
	/**
	 * リアルタイムトレンド（R1-D の D-3b、recorder-requirements.md §3.3 の「リアル
	 * タイム」）。共通の部品 `TrendChart`（`@banto-industrial/trend`、設計書
	 * docs/chronogazer/trend-design.md §10 の 3）で描く。
	 *
	 * 他のパネルと同じく、**グループの定義・ペンごとの表示・格子を props で受け取るだけ**で、
	 * 自分ではデータを取りに行かない（§3.7.4 の関所のプレビューが使い回す）。格子・帯・説明文の
	 * 判断は `trendLogic.ts`、バッファの持ち主は画面の `trendFeed.svelte.ts`。
	 *
	 * - **格子を部品の契約にする**: `trendBufferData`（表示範囲 = 取得範囲 = 格子の範囲）。
	 *   **追従だけ**で拡大・縮小・パンは持たない（`onRangeChange` を渡さない。過去へのパンは
	 *   ハイブリッドの PR）。
	 * - **値が無い刻みは線を切る**（`null` は欠測。0 の位置に描かない、#414 段階2）。
	 * - **縦軸はペンごと**（部品の既定。単位は凡例に、Q7）。bit のペンは縦軸 0〜1・小数 0 桁
	 *   （`toTrendPens`。ヒストリカルと共通）。
	 * - **系列の色はペンの色の枠（`colorSlot`）**。部品が `--banto-chart-<枠>` で塗る。
	 * - **凡例は部品のもの**。ペンを押すとしきい値の帯を出すペンが替わる（Q5: 帯は選んだ 1 ペン
	 *   だけ。`aria-pressed`、印は `legendBadge`）。同じタグを 2 回置いたときは、帯は先のペンで、
	 *   押した印もそのペンだけに付く（ヒストリカルと同じ）。
	 * - 部品の図は中が読まれないので、ペン・単位・窓・値の範囲・帯を画面外の文
	 *   （`trendDescription`）で伝える（D-2 の `meterDescription` と同じ作法）。
	 * - 時間窓は端末ごと（Q4）。選んだ窓は画面が localStorage に覚え、グループの定義には
	 *   書かない。
	 * - `width` は**描画域そのものの幅**（余白を除く。画面が刻みを決めるのに使う）。
	 */
	import { TrendChart, type TrendBands } from '@banto-industrial/trend';
	import type { DisplayGroup } from '#lib/banto/displayGroupsAdmin.js';
	import {
		thresholdBands,
		trendBufferData,
		trendDescription,
		trendWindowLabel,
		hasThresholds,
		type TrendBuffer,
		type TrendPenInfo
	} from './trendLogic';
	import { tagIdOfTrendPenId, toTrendPens } from './trendPens';

	interface Props {
		group: Pick<DisplayGroup, 'name'>;
		pens: readonly TrendPenInfo[];
		/** 格子（`null` = まだ無い・別のグループのもの）。 */
		buffer: TrendBuffer | null;
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
		buffer,
		windowSec,
		windowOptions,
		onWindowChange,
		bandTagId,
		onSelectBandPen,
		notices,
		historyLoading,
		width = $bindable(0)
	}: Props = $props();

	const trendPens = $derived(toTrendPens(pens));
	const rows = $derived(buffer?.rows ?? []);
	const view = $derived(
		trendBufferData(
			buffer ?? { rows: [], stepMs: 1000, windowMs: windowSec * 1000 },
			trendPens.map((pen) => pen.id)
		)
	);

	/** 帯のペン（同じタグが 2 回あれば先のペン）。 */
	const bandIndex = $derived(
		bandTagId === null ? -1 : pens.findIndex((p) => p.tagId === bandTagId)
	);
	const bandPen = $derived(bandIndex >= 0 ? pens[bandIndex] : null);
	const bandPenId = $derived(bandIndex >= 0 ? (trendPens[bandIndex]?.id ?? null) : null);
	const bands: TrendBands | null = $derived(
		bandPen && bandPenId !== null
			? {
					penId: bandPenId,
					items: thresholdBands(bandPen.thresholds).map((b) => ({
						from: b.from,
						to: b.to,
						colorVar: b.colorVar ?? 'var(--banto-warning)',
						label: b.label ?? ''
					}))
				}
			: null
	);

	function onPenSelect(penId: string): void {
		const tagId = tagIdOfTrendPenId(penId);
		if (tagId !== null) onSelectBandPen(tagId);
	}
	function penHasThresholds(penId: string): boolean {
		const tagId = tagIdOfTrendPenId(penId);
		return hasThresholds(pens.find((p) => p.tagId === tagId)?.thresholds);
	}

	const description = $derived(
		trendDescription({ windowSec, pens, rows, bandTagId: bandPen?.tagId ?? null })
	);
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

	<p class="sr-only">{description}</p>

	<TrendChart
		label={`${group.name} のトレンド`}
		range={view.range}
		pens={trendPens}
		data={view.data}
		height={320}
		emptyMessage="表示できる値はまだありません"
		{bands}
		bind:width
		{onPenSelect}
		selectedPenId={bandPenId}
		legendLabel="ペン（押すと、しきい値の帯をそのペンで表示します）"
	>
		{#snippet legendBadge(pen)}
			{#if pen.id === bandPenId}
				<span class="band-mark">{penHasThresholds(pen.id) ? '帯' : '帯（設定なし）'}</span>
			{/if}
		{/snippet}
	</TrendChart>
</section>

<style>
	.trend-panel {
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

	.band-mark {
		padding: 0 0.3rem;
		border-radius: var(--banto-radius);
		background: var(--banto-warning-tint);
		color: var(--banto-warning-tint-text);
		font-weight: 400;
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
