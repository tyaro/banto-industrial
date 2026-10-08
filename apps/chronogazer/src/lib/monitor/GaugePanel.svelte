<script lang="ts">
	/**
	 * 計器表示（R1-D の D-2、recorder-requirements.md §3.2「円形ゲージ」= 既存
	 * Gauge 拡張）。ペンごとに banto の `Gauge`（`@banto/charts`、v6.3.0 で値なしと
	 * 下側しきい値が入った）を 1 つ置く。
	 *
	 * `DigitalPanel.svelte` と同じく、**表示グループの定義と、ペンごとの表示
	 * （`MeterView`）を props で受け取るだけ**で、自分ではデータを取りに行かない
	 * （§3.7.4 の関所のプレビューが使い回す）。判断（レンジ・しきい値の写し方・
	 * レンジ外）は `meterLogic.ts` の `meterView` が済ませてある。
	 *
	 * - 値が無いとき（`bad` / `stale` / `invalid` / 未収集）は `Gauge` に `null` を
	 *   渡し、弧を描かずに「—」を出させる（0 の位置に描かない。#414 段階2）。
	 *   理由の文言（最後に受け取った時刻を含む）はデジタルと同じものを下に出す。
	 * - しきい値は H/HH/L/LL を `warning` / `danger` / `warningLow` / `dangerLow` に
	 *   写す（`gaugeThresholds`）。弧の色に加えて文字（`levelLabel`）でも出す。
	 * - レンジが決まらない（Q3）ときは計器を描かず、値の文字と理由を出す。
	 * - `Gauge` は `role="img"` で中の文字が支援技術に読まれないので、`label` に
	 *   名前・値・単位を入れる。
	 */
	import { Gauge } from '@banto/charts';
	import type { DisplayGroup } from '#lib/banto/displayGroupsAdmin.js';
	import { formatValue, lastReceivedText } from './monitorLogic';
	import type { MeterView } from './meterLogic';

	interface Props {
		group: Pick<DisplayGroup, 'name'>;
		pens: readonly MeterView[];
		/** 時刻の表示（端末のロケール）。 */
		timeLabel: (epochMs: number) => string;
		/** 設定不正・レンジ未設定のペンに添えるタグ設定へのリンク（`null` = 出さない）。 */
		tagsHref: string | null;
	}

	let { group, pens, timeLabel, tagsHref }: Props = $props();

	function formatter(pen: MeterView): (n: number) => string {
		const decimals = pen.decimals;
		return decimals === null ? (n) => String(n) : (n) => formatValue(n, decimals);
	}

	function gaugeLabel(pen: MeterView): string {
		return `${pen.name} ${pen.display}${pen.unit ? ` ${pen.unit}` : ''}`;
	}
</script>

<ul class="gauge-grid" aria-label={`${group.name} の計器表示`}>
	{#each pens as pen (pen.tagId)}
		{@const lastReceived = lastReceivedText(pen, timeLabel)}
		<li
			class="gauge-cell"
			data-state={pen.state}
			data-level={pen.level}
			data-tone={pen.tone}
			data-range={pen.range.kind}
			data-out={pen.bar?.out ?? 'none'}
			style:--pen-color={`var(--banto-chart-${pen.colorSlot})`}
		>
			<div class="pen-name">
				{pen.name}{#if pen.unit}<span class="unit">（{pen.unit}）</span>{/if}
			</div>

			{#if pen.range.kind === 'ok'}
				<Gauge
					value={pen.value}
					min={pen.range.min}
					max={pen.range.max}
					label={gaugeLabel(pen)}
					formatValue={formatter(pen)}
					thresholds={pen.gaugeThresholds}
				/>
			{:else}
				<div class="pen-value">
					<span class="value">{pen.display}</span>
					{#if pen.unit}<span class="unit">{pen.unit}</span>{/if}
				</div>
				<div class="range-message">
					{pen.rangeMessage}
					{#if pen.range.kind === 'unset' && tagsHref}
						<!-- eslint-disable-next-line svelte/no-navigation-without-resolve -- tagsHref は呼び出し側が resolveAppPath() で作る -->
						<a class="fix-link" href={tagsHref}>タグ設定を開く</a>
					{/if}
				</div>
			{/if}

			<div class="pen-meta">
				<span class="state">{pen.stateLabel}</span>
				{#if pen.levelLabel}<span class="level">{pen.levelLabel}</span>{/if}
				{#if pen.outLabel}<span class="out">{pen.outLabel}</span>{/if}
			</div>
			{#if lastReceived !== null}
				<div class="last-received">{lastReceived}</div>
			{/if}
			{#if pen.linkToTags && tagsHref}
				<!-- eslint-disable-next-line svelte/no-navigation-without-resolve -- tagsHref は呼び出し側が resolveAppPath() で作る -->
				<a class="fix-link" href={tagsHref}>タグ設定で直す</a>
			{/if}
		</li>
	{/each}
</ul>

<style>
	.gauge-grid {
		list-style: none;
		margin: 0;
		padding: 0;
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(14rem, 1fr));
		gap: 0.75rem;
	}

	.gauge-cell {
		display: flex;
		flex-direction: column;
		gap: 0.35rem;
		padding: 0.75rem 1rem;
		border: 1px solid var(--banto-border);
		border-top: 4px solid var(--pen-color);
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
		font-size: 1.6rem;
		font-weight: 600;
		font-variant-numeric: tabular-nums;
		line-height: 1.1;
	}

	.unit {
		font-size: 0.85rem;
		color: var(--banto-text-muted);
	}

	.range-message {
		padding: 0.5rem;
		font-size: 0.8rem;
		color: var(--banto-text-muted);
		border: 1px dashed var(--banto-border);
		border-radius: var(--banto-radius);
	}

	.pen-meta {
		display: flex;
		flex-wrap: wrap;
		gap: 0.35rem;
		font-size: 0.8rem;
	}

	.state,
	.level,
	.out {
		padding: 0.05rem 0.4rem;
		border-radius: var(--banto-radius);
	}

	.state {
		background: var(--banto-success-tint);
		color: var(--banto-success-tint-text);
	}

	/* 品質の色分け（DigitalPanel と同じ）。 */
	.gauge-cell[data-state='bad'] .state,
	.gauge-cell[data-state='invalid'] .state {
		background: var(--banto-danger-tint);
		color: var(--banto-danger-tint-text);
	}

	.gauge-cell[data-state='stale'] .state,
	.gauge-cell[data-state='uncollected'] .state {
		background: var(--banto-warning-tint);
		color: var(--banto-warning-tint-text);
	}

	.gauge-cell:not([data-state='good']) .value {
		color: var(--banto-text-muted);
	}

	/* しきい値: 弧の色（banto）と文字（.level）の両方で出す。 */
	.level,
	.out {
		border: 1px solid var(--banto-border);
		color: var(--banto-text);
	}

	.gauge-cell[data-tone='warning'] .level {
		background: var(--banto-warning-tint);
		color: var(--banto-warning-tint-text);
	}

	.gauge-cell[data-tone='danger'] .level {
		background: var(--banto-danger-tint);
		color: var(--banto-danger-tint-text);
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
