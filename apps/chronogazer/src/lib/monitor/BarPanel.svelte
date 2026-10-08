<script lang="ts">
	/**
	 * バー表示（R1-D の D-2、recorder-requirements.md §3.2「縦バーメータ + 目盛 +
	 * しきい値色」）。chronogazer 内の軽量コンポーネント（テンプレートに入れない、
	 * docs/r1-plan.md の R1-D）。
	 *
	 * `DigitalPanel.svelte` と同じく、**表示グループの定義と、ペンごとの表示
	 * （`MeterView`）を props で受け取るだけ**で、自分ではデータを取りに行かない
	 * （§3.7.4 の関所のプレビューが使い回す）。判断（レンジ・棒の高さ・レンジ外・
	 * 目盛・しきい値の印・色）は `meterLogic.ts` の `meterView` が済ませてあり、
	 * ここは描くだけ。
	 *
	 * - 値が無いとき（`bad` / `stale` / `invalid` / 未収集）は棒を描かず「—」と、
	 *   デジタルと同じ理由の文言（最後に受け取った時刻を含む）を出す（0 の位置に
	 *   描かない。#414 段階2・Q2）。
	 * - しきい値は**色と文字の両方**で出す。棒の色は HH/LL が danger、H/L が warning。
	 * - レンジの外は棒を端に丸め、「レンジ上限超え / 下限未満」を文字で添える。
	 * - レンジが決まらない（Q3）ときは棒の代わりに理由を出し、値の文字は出す。
	 * - 棒・目盛・印の図は `aria-hidden` にし、レンジとしきい値は画面外の文字
	 *   （`meterDescription`）で支援技術に伝える。同じ値のしきい値の名前は 1 つに
	 *   まとめる（`LL/L/H`。等号は正しい設定なので、重ねると読めない。#535）。
	 */
	import type { DisplayGroup } from '#lib/banto/displayGroupsAdmin.js';
	import { formatValue, lastReceivedText } from './monitorLogic';
	import { meterDescription, toneColorVar, type MeterView } from './meterLogic';

	interface Props {
		group: Pick<DisplayGroup, 'name'>;
		pens: readonly MeterView[];
		/** 時刻の表示（端末のロケール）。 */
		timeLabel: (epochMs: number) => string;
		/** 設定不正・レンジ未設定のペンに添えるタグ設定へのリンク（`null` = 出さない）。 */
		tagsHref: string | null;
	}

	let { group, pens, timeLabel, tagsHref }: Props = $props();

	function tickText(pen: MeterView, value: number): string {
		return pen.decimals === null ? String(value) : formatValue(value, pen.decimals);
	}

	const pct = (fraction: number) => `${(fraction * 100).toFixed(3)}%`;
</script>

<ul class="bar-grid" aria-label={`${group.name} のバー表示`}>
	{#each pens as pen (pen.tagId)}
		{@const lastReceived = lastReceivedText(pen, timeLabel)}
		<li
			class="bar-cell"
			data-state={pen.state}
			data-level={pen.level}
			data-tone={pen.tone}
			data-range={pen.range.kind}
			data-out={pen.bar?.out ?? 'none'}
			style:--pen-color={`var(--banto-chart-${pen.colorSlot})`}
			style:--tone-color={toneColorVar(pen.tone)}
		>
			<div class="pen-name">{pen.name}</div>
			<div class="pen-value">
				<span class="value">{pen.display}</span>
				{#if pen.unit}<span class="unit">{pen.unit}</span>{/if}
			</div>
			<p class="sr-only">{meterDescription(pen)}</p>

			{#if pen.range.kind === 'ok'}
				<div class="meter" aria-hidden="true">
					<div class="scale">
						{#each pen.ticks as tick (tick.value)}
							<span class="tick" style:bottom={pct(tick.position)}>{tickText(pen, tick.value)}</span
							>
						{/each}
					</div>
					<div class="track">
						{#if pen.bar}
							<div class="fill" style:height={pct(pen.bar.fill)}></div>
						{/if}
						{#each pen.markGroups as mark (mark.label)}
							<span
								class="mark"
								data-tone={mark.tone}
								style:bottom={pct(mark.position)}
								title={`${mark.label} ${tickText(pen, mark.value)}`}
							></span>
						{/each}
					</div>
					<div class="mark-labels">
						{#each pen.markGroups as mark (mark.label)}
							<span class="mark-label" data-tone={mark.tone} style:bottom={pct(mark.position)}
								>{mark.label}</span
							>
						{/each}
					</div>
				</div>
			{:else}
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
	.bar-grid {
		list-style: none;
		margin: 0;
		padding: 0;
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(11rem, 1fr));
		gap: 0.75rem;
	}

	.bar-cell {
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
		font-size: 0.9rem;
		color: var(--banto-text-muted);
	}

	/* 目盛（左）・棒（中）・しきい値の名前（右）。高さは 3 つで共有する。 */
	.meter {
		display: grid;
		grid-template-columns: auto 1.75rem auto;
		gap: 0.4rem;
		height: 12rem;
		margin: 0.5rem 0;
		justify-content: center;
	}

	.scale,
	.mark-labels {
		position: relative;
		min-width: 2rem;
	}

	.tick,
	.mark-label {
		position: absolute;
		transform: translateY(50%);
		font-size: 0.7rem;
		line-height: 1;
		font-variant-numeric: tabular-nums;
		white-space: nowrap;
	}

	.tick {
		right: 0;
		color: var(--banto-text-muted);
	}

	.mark-label {
		left: 0;
		font-weight: 600;
	}

	.track {
		position: relative;
		background: var(--banto-chart-grid);
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
		overflow: hidden;
	}

	.fill {
		position: absolute;
		left: 0;
		right: 0;
		bottom: 0;
		background: var(--tone-color);
	}

	.mark {
		position: absolute;
		left: -1px;
		right: -1px;
		height: 0;
		border-top: 2px dashed var(--banto-text-muted);
		transform: translateY(1px);
	}

	.mark[data-tone='warning'],
	.mark-label[data-tone='warning'] {
		border-color: var(--banto-warning);
		color: var(--banto-warning);
	}

	.mark[data-tone='danger'],
	.mark-label[data-tone='danger'] {
		border-color: var(--banto-danger);
		color: var(--banto-danger);
	}

	.range-message {
		margin: 0.5rem 0;
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

	/* 品質の色分け（DigitalPanel と同じ）。値が無い状態は値も淡くする。 */
	.bar-cell[data-state='bad'] .state,
	.bar-cell[data-state='invalid'] .state {
		background: var(--banto-danger-tint);
		color: var(--banto-danger-tint-text);
	}

	.bar-cell[data-state='stale'] .state,
	.bar-cell[data-state='uncollected'] .state {
		background: var(--banto-warning-tint);
		color: var(--banto-warning-tint-text);
	}

	.bar-cell:not([data-state='good']) .value {
		color: var(--banto-text-muted);
	}

	/* しきい値: 色と文字（.level）の両方で出す。 */
	.level,
	.out {
		border: 1px solid var(--banto-border);
		color: var(--banto-text);
	}

	.bar-cell[data-tone='warning'] .level {
		background: var(--banto-warning-tint);
		color: var(--banto-warning-tint-text);
	}

	.bar-cell[data-tone='danger'] .level {
		background: var(--banto-danger-tint);
		color: var(--banto-danger-tint-text);
	}

	.bar-cell[data-tone='warning'] .value {
		color: var(--banto-warning);
	}

	.bar-cell[data-tone='danger'] .value {
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
	/* 画面には出さず、支援技術にだけ読ませる（目盛・しきい値の説明、#535）。 */
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
