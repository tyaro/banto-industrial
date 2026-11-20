<script lang="ts">
	/**
	 * 監視画面のトレンド（R1-D の D-3b、recorder-requirements.md §3.3 の「リアル
	 * タイム」）。共通の部品 `TrendChart`（`@banto-industrial/trend`）で描く。設計書
	 * docs/chronogazer/trend-design.md §8・§10 の 4b で**ハイブリッド**にした（直近は
	 * リングバッファ、過去は履歴の区間）。
	 *
	 * 他のパネルと同じく、**グループの定義・ペンごとの表示・描画データを props で受け取るだけ**で、
	 * 自分ではデータを取りに行かない（§3.7.4 の関所のプレビューが使い回す）。管理線・説明文の
	 * 判断は `trendLogic.ts`、データの持ち主は画面の `trendFeed.svelte.ts`。
	 *
	 * - **描画データ**は `hybridData` の結果（`data`）。表示範囲（`view`）は画面が持ち、
	 *   拡大・縮小・パンの結果は `onRangeChange` で返す。追従中は右端が今。
	 * - **「最新へ」**: 追従をやめているときだけ出す。押すと時間窓の幅で追従に戻る。
	 * - **値が無い区間は線を切る**（`null` は欠測。0 の位置に描かない、#414 段階2）。
	 * - **縦軸はペンごと**（部品の既定。単位は凡例に、Q7）。bit のペンは縦軸 0〜1（小数桁は指定しない。False / True で出し、属性で帯にもできる）
	 *   （`toTrendPens`。ヒストリカルと共通）。
	 * - **系列の色はペンの色の枠（`colorSlot`）**。部品が `--banto-chart-<枠>` で塗る。
	 * - **凡例は部品のもの**（押せない一覧）。管理線はペンの属性で、部品が描く（`toTrendPens`）。
	 * - 部品の図は中が読まれないので、ペン・単位・窓・値の範囲を画面外の文
	 *   （`trendDescription`）で伝える（D-2 の `meterDescription` と同じ作法）。
	 * - 時間窓は端末ごと（Q4）。選んだ窓は画面が localStorage に覚え、グループの定義には
	 *   書かない。窓を選び直すと追従に戻る。
	 * - `width` は**描画域そのものの幅**（余白を除く。画面が履歴の区間の数を決めるのに使う）。
	 */
	import { TrendChart, type TimeRange, type TrendData } from '@banto-industrial/trend';
	import type { DisplayGroup } from '#lib/banto/displayGroupsAdmin.js';
	import { trendDescription, trendWindowLabel, type TrendPenInfo } from './trendLogic';
	import { toTrendPens } from './trendPens';

	interface Props {
		group: Pick<DisplayGroup, 'name'>;
		pens: readonly TrendPenInfo[];
		/** 描画データ（`null` = まだ無い・別のグループのもの）。 */
		data: TrendData | null;
		/** 表示範囲（`null` = まだ無い。窓の幅の仮の範囲で何も描かない）。 */
		view: TimeRange | null;
		/** 追従中か（やめていれば「最新へ」を出す）。 */
		following: boolean;
		windowSec: number;
		windowOptions: readonly number[];
		onWindowChange: (sec: number) => void;
		/** 拡大・縮小・パン。 */
		onRangeChange: (range: TimeRange) => void;
		/** 「最新へ」。 */
		onFollowLatest: () => void;
		/** パネルの上に添える注記（履歴を読めない・不明なタグ・シミュレーション）。 */
		notices: readonly string[];
		/** 履歴を読み込み中。 */
		historyLoading: boolean;
		/** 表示の時計（サーバーの時刻、ms。`null` = まだ無い）。パンの右端の丸めに使う。 */
		nowMs: number | null;
		/** 描画域の幅（px、画面が履歴の区間の数を決めるのに使う）。 */
		width?: number;
	}

	let {
		group,
		pens,
		data,
		view,
		following,
		windowSec,
		windowOptions,
		onWindowChange,
		onRangeChange,
		onFollowLatest,
		notices,
		historyLoading,
		nowMs,
		width = $bindable(0)
	}: Props = $props();

	const trendPens = $derived(toTrendPens(pens));
	const penIds = $derived(trendPens.map((pen) => pen.id));
	const range = $derived(view ?? { startMs: 0, endMs: windowSec * 1000 });

	const description = $derived(
		trendDescription({
			windowSec,
			view,
			following,
			pens,
			penIds,
			data
		})
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
		{#if !following}
			<button type="button" class="latest" onclick={onFollowLatest}>最新へ</button>
			<span class="paused" role="status">追従を止めています</span>
		{/if}
		{#if historyLoading}
			<span class="loading" role="status">履歴を読み込んでいます。</span>
		{/if}
	</div>

	{#each notices as notice (notice)}
		<p class="note warn" role="status">{notice}</p>
	{/each}

	<p class="sr-only">{description}</p>

	<TrendChart
		label={`${group.name} のトレンド`}
		{range}
		pens={trendPens}
		{data}
		height={320}
		emptyMessage="表示できる値はまだありません"
		bind:width
		nowMs={nowMs ?? undefined}
		{onRangeChange}
		legendLabel="ペン"
	/>
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

	.loading,
	.paused {
		color: var(--banto-text-muted);
	}

	.latest {
		font: inherit;
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
