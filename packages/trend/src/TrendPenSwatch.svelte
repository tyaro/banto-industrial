<!--
  ペンの見本（凡例・表示グループのページ用）。線種・線幅・マーカーが分かる小さな SVG
  （幅 24px、線 + 中央のマーカー）。計算は `core/style.ts`。線なし・マーカーなしでも幅は保つ。
-->
<script lang="ts">
	import { penColorVar, type TrendPen } from './core/pens';
	import {
		dashArray,
		markerPath,
		resolveLineStyle,
		resolveLineWidth,
		resolveMarkerKind,
		resolveMarkerSize
	} from './core/style';

	let {
		pen
	}: {
		pen: Pick<TrendPen, 'colorSlot' | 'lineStyle' | 'lineWidth' | 'marker' | 'markerSize'>;
	} = $props();

	const W = 24;
	const H = 12;
	const color = $derived(penColorVar(pen.colorSlot));
	const lineOn = $derived(resolveLineStyle(pen.lineStyle) !== 'none');
	const width = $derived(resolveLineWidth(pen.lineWidth));
	const dash = $derived(dashArray(pen.lineStyle, pen.lineWidth));
	const markerD = $derived(
		markerPath(
			[{ x: W / 2, y: H / 2 }],
			resolveMarkerKind(pen.marker),
			resolveMarkerSize(pen.markerSize)
		)
	);
</script>

<svg class="bi-trend__swatch" width={W} height={H} viewBox="0 0 {W} {H}" aria-hidden="true">
	{#if lineOn}
		<line
			x1="1"
			x2={W - 1}
			y1={H / 2}
			y2={H / 2}
			stroke={color}
			stroke-width={width}
			stroke-linecap="round"
			stroke-dasharray={dash ?? undefined}
		/>
	{/if}
	{#if markerD}<path d={markerD} fill={color} />{/if}
</svg>

<style>
	.bi-trend__swatch {
		display: inline-block;
		flex: none;
		vertical-align: middle;
	}
</style>
