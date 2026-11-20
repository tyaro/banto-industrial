<!--
  ペンの見本（凡例・表示グループのページ用）。線種・線幅・マーカーが分かる小さな SVG
  （幅 32px、線 + 中央のマーカー）。計算は `core/style.ts`。線なし・マーカーなしでも幅は保つ。
-->
<script lang="ts">
	import { isBitBand, penColorVar, type TrendPen } from './core/pens';
	import {
		swatchDashArray,
		swatchWidth,
		markerPath,
		resolveLineStyle,
		resolveMarkerKind,
		resolveMarkerSize
	} from './core/style';

	let {
		pen
	}: {
		pen: Pick<
			TrendPen,
			'colorSlot' | 'lineStyle' | 'lineWidth' | 'marker' | 'markerSize' | 'bit' | 'bitStyle'
		>;
	} = $props();

	const W = 32;
	const H = 12;
	const color = $derived(penColorVar(pen.colorSlot));
	const lineOn = $derived(resolveLineStyle(pen.lineStyle) !== 'none');
	const width = $derived(swatchWidth(pen.lineWidth));
	const dash = $derived(swatchDashArray(pen.lineStyle, pen.lineWidth));
	const markerD = $derived(
		markerPath(
			[{ x: W / 2, y: H / 2 }],
			resolveMarkerKind(pen.marker),
			resolveMarkerSize(pen.markerSize)
		)
	);
</script>

<svg class="bi-trend__swatch" width={W} height={H} viewBox="0 0 {W} {H}" aria-hidden="true">
	{#if isBitBand(pen)}
		<rect x="1" y="2" width={W - 2} height={H - 4} fill={color} opacity="0.85" />
	{:else if lineOn}
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
	{#if markerD && !isBitBand(pen)}<path d={markerD} fill={color} />{/if}
</svg>

<style>
	.bi-trend__swatch {
		display: inline-block;
		flex: none;
		vertical-align: middle;
	}
</style>
