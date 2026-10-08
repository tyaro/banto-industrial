<script lang="ts">
	/**
	 * デジタル表示（R1-D の D-1、recorder-requirements.md §3.2「現在値の数値大
	 * 表示（グリッド配置、単位・品質色分け）」）。
	 *
	 * **表示グループの定義と、ペンごとの表示（`PenView`）を props で受け取るだけ**で、
	 * 自分ではデータを取りに行かない。§3.7.4 の関所のプレビューが、監視画面と
	 * 同じ 4 種のパネルを使い回すため（プレビューは保存前の定義と、別の経路で
	 * 用意した値を渡す）。判断（値の文字列・品質・しきい値）は
	 * `monitorLogic.ts` の `penView` が済ませてあり、ここは描くだけ。
	 *
	 * - 値が無いときは「—」（0 と区別する）。`bad` / `stale` では最後の値を出さず、
	 *   最後に受け取った時刻を小さく添える（2026-10-08 オーナー決定 Q2）。
	 * - しきい値は**色と文字の両方**で出す（色だけで伝えない）。
	 * - `invalid`（設定不正）は直す場所へのリンクを添える（`tagsHref` が `null` なら
	 *   出さない - 閲覧公開のセッションはタグ設定を開けない）。
	 */
	import type { DisplayGroup } from '#lib/banto/displayGroupsAdmin.js';
	import type { PenView } from './monitorLogic';

	interface Props {
		group: Pick<DisplayGroup, 'name'>;
		pens: readonly PenView[];
		/** 時刻の表示（端末のロケール）。 */
		timeLabel: (epochMs: number) => string;
		/** 設定不正のペンに添えるタグ設定へのリンク（`null` = 出さない）。 */
		tagsHref: string | null;
	}

	let { group, pens, timeLabel, tagsHref }: Props = $props();
</script>

<ul class="digital-grid" aria-label={`${group.name} のデジタル表示`}>
	{#each pens as pen (pen.tagId)}
		<li
			class="digital-cell"
			data-state={pen.state}
			data-level={pen.level}
			style:--pen-color={`var(--banto-chart-${pen.colorSlot})`}
		>
			<div class="pen-name">{pen.name}</div>
			<div class="pen-value">
				<span class="value">{pen.display}</span>
				{#if pen.unit}<span class="unit">{pen.unit}</span>{/if}
			</div>
			<div class="pen-meta">
				<span class="state">{pen.stateLabel}</span>
				{#if pen.levelLabel}<span class="level">{pen.levelLabel}</span>{/if}
			</div>
			{#if pen.lastReceivedMs !== null}
				<div class="last-received">最後に受け取った値: {timeLabel(pen.lastReceivedMs)}</div>
			{/if}
			{#if pen.linkToTags && tagsHref}
				<!-- eslint-disable-next-line svelte/no-navigation-without-resolve -- tagsHref は呼び出し側が resolveAppPath() で作る -->
				<a class="fix-link" href={tagsHref}>タグ設定で直す</a>
			{/if}
		</li>
	{/each}
</ul>

<style>
	.digital-grid {
		list-style: none;
		margin: 0;
		padding: 0;
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(14rem, 1fr));
		gap: 0.75rem;
	}

	.digital-cell {
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
		padding: 0.75rem 1rem;
		border: 1px solid var(--banto-border);
		border-left: 4px solid var(--pen-color);
		border-radius: var(--banto-radius);
		background: var(--banto-surface);
		min-width: 0;
	}

	.pen-name {
		font-size: 0.85rem;
		color: var(--banto-text-muted);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.pen-value {
		display: flex;
		align-items: baseline;
		gap: 0.35rem;
		color: var(--banto-text);
	}

	.value {
		font-size: 2.25rem;
		font-weight: 600;
		font-variant-numeric: tabular-nums;
		line-height: 1.1;
	}

	.unit {
		font-size: 1rem;
		color: var(--banto-text-muted);
	}

	.pen-meta {
		display: flex;
		flex-wrap: wrap;
		gap: 0.35rem;
		font-size: 0.8rem;
	}

	.state,
	.level {
		padding: 0.05rem 0.4rem;
		border-radius: var(--banto-radius);
	}

	.state {
		background: var(--banto-success-tint);
		color: var(--banto-success-tint-text);
	}

	/* 品質の色分け。値が無い状態は値も淡くする（数値が出ていないことを目立たせる）。 */
	.digital-cell[data-state='bad'] .state,
	.digital-cell[data-state='invalid'] .state {
		background: var(--banto-danger-tint);
		color: var(--banto-danger-tint-text);
	}

	.digital-cell[data-state='stale'] .state,
	.digital-cell[data-state='uncollected'] .state {
		background: var(--banto-warning-tint);
		color: var(--banto-warning-tint-text);
	}

	.digital-cell:not([data-state='good']) .value {
		color: var(--banto-text-muted);
	}

	/* しきい値: 色と文字（.level）の両方で出す。 */
	.level {
		border: 1px solid var(--banto-border);
		color: var(--banto-text);
	}

	.digital-cell[data-level='H'] .level,
	.digital-cell[data-level='L'] .level {
		background: var(--banto-warning-tint);
		color: var(--banto-warning-tint-text);
	}

	.digital-cell[data-level='HH'] .level,
	.digital-cell[data-level='LL'] .level {
		background: var(--banto-danger-tint);
		color: var(--banto-danger-tint-text);
	}

	.digital-cell[data-level='H'] .value,
	.digital-cell[data-level='L'] .value {
		color: var(--banto-warning);
	}

	.digital-cell[data-level='HH'] .value,
	.digital-cell[data-level='LL'] .value {
		color: var(--banto-danger);
	}

	.last-received {
		font-size: 0.75rem;
		color: var(--banto-text-muted);
	}

	.fix-link {
		font-size: 0.8rem;
		color: var(--banto-primary);
	}
</style>
