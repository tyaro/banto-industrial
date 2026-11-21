<!--
  トレンドの SVG の描画（制御された部品）。

  - 計算はすべて core（範囲・目盛り・縦軸・包絡・カーソル）に置き、ここは SVG の
    組み立てと操作の受け付けだけを持つ（後で Canvas に替えるとき、ここだけ替える）。
  - 表示範囲は利用側が持つ。拡大・縮小・パンの結果は `onRangeChange` で返すだけ。
  - 操作: Ctrl（mac は Cmd）+ホイールでポインタの時刻を基準に拡大・縮小、Shift+ホイールと
    ドラッグで横のパン。素のホイールはページのスクロールに任せる。キーボードは ← → で
    幅の 10% のパン、+ / - で中央基準の拡大・縮小、Esc でカーソルを消す。
  - 色はペンの `penColor`（`color` があればそれ、無ければ色の枠）、文字・線・背景は `--banto-*` のトークンだけ。
  - 線種・線幅・マーカーはペンの属性（`core/style.ts`）。線は `stroke-width`・`stroke-dasharray`
    （線なしは path を出さない）、マーカーは線の上に 1 本の path（`fill` = ペンの色、stroke なし）。
    凡例の見本は `TrendPenSwatch`（線 + 中央のマーカーの小さな SVG）。
  - 帯（`bands`）は指定したペンの縦軸で、描画域の背景（線の下）に描く（`core/bands.ts`）。
  - 管理線（`pen.limits`、`core/limits.ts`）はそのペンの縦軸で、帯の後・ペンの線の前に、描画域の
    幅いっぱいの細い破線と右端の内側の `label` で描く。越えた区間は、区間の矩形の clipPath で
    ペンの線・マーカーと同じ形を `colorVar` で重ね描きする（並びの順。線なしのペンは線を重ねず
    マーカーだけ）。どれも描画域の clip の内側。要約には管理線の値を足す（単位は付けない）。
  - bit のペンで `bitStyle: 'band'` のものは帯（タイムチャート）で描く（`core/bitBands.ts`）。描画域の
    下に、ペンの並びの順に積む（1 本目が一番下）。帯の分だけアナログの縦の長さは縮み（帯の合計は
    描画域の半分まで）、帯のペンは線・マーカー・管理線・縦軸の目盛りの持ち主から外れる。横の格子・
    カーソルの線は描画域の全体に引く。アナログの層（帯・管理線・線・マーカー・越えた区間）は
    `{uid}-analog-clip`（アナログの縦の長さまで）で切り、範囲外の値が bit の帯に重ならないようにする。True は濃い塗り、区間の中で変わったものは薄い塗り、帯の左端の
    内側にペンの `label`（帯が 10px 未満に縮んだら出さない。`showLabel`）。縦軸の持ち主が bit のペンなら目盛り（と横の格子）は 0・1 の 2 つだけ（`axisTickValues`）。bit のペンの値は False / True と出す（カーソル・凡例の範囲・要約。縦軸が
    bit のペンのときは 0・1 の目盛りも）。
  - 凡例は `onPenSelect` を渡すとペンを選ぶボタンになる（押した印は `selectedPenId`、
    利用側の印は `legendBadge`）。どのペンを選ぶと何が変わるかは利用側が決める。
-->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import { linearScale } from '@banto/charts';
	import { bandLayout, type TrendBands } from './core/bands';
	import { limitLabelY, limitLayout } from './core/limits';
	import {
		cursorDiff,
		cursorReadout,
		formatDuration,
		placeCursor,
		type CursorPair,
		type CursorValue
	} from './core/cursor';
	import { drawableSeries, type TrendData } from './core/data';
	import { envelopePath, hasFiniteValue } from './core/geometry';
	import {
		dashArray,
		markerPath,
		markerPoints,
		resolveLineStyle,
		resolveLineWidth,
		resolveMarkerKind,
		resolveMarkerSize
	} from './core/style';
	import TrendPenSwatch from './TrendPenSwatch.svelte';
	import { bitBandLayout, bitMissingSpans, bitSpans } from './core/bitBands';
	import {
		effectiveClip,
		formatObservationTime,
		samplesInRange,
		samplesMarkerPoints,
		samplesPath
	} from './core/samples';
	import {
		axisOwners,
		axisTickValues,
		BIT_RANGE_TEXT,
		formatValue,
		isBitBand,
		penColor,
		resolveYDomain
	} from './core/pens';
	import type { TrendPen, YDomain } from './core/pens';
	import { isValidRange, panRange, zoomRange, type TimeRange } from './core/range';
	import { timeTicks } from './core/timeTicks';

	interface Props {
		range: TimeRange;
		pens: readonly TrendPen[];
		/**
		 * `data.range` は取得した範囲。読み直しの間・失敗後も、手元のデータはこの範囲と各系列の幅で
		 * だけ描く（取得していない時刻に線やカーソルの値を出さない）。
		 */
		data: TrendData | null;
		height?: number;
		/** 図の名前（aria-label）。 */
		label: string;
		/** 描画域の横幅（px）。利用側へ返す（`binCount` に使う）。 */
		width?: number;
		loading?: boolean;
		emptyMessage?: string;
		/** パンの丸めに使う今（既定は Date.now()）。 */
		nowMs?: number;
		onRangeChange?: (range: TimeRange) => void;
		/** 横の帯（しきい値など）。`penId` のペンの縦軸で、線の下に描く。 */
		bands?: TrendBands | null;
		/** 凡例のペンを押したとき。渡すと凡例の各項目がボタンになる。 */
		onPenSelect?: (penId: string) => void;
		/** 凡例で押した印を付けるペン（`onPenSelect` を渡したときだけ使う）。 */
		selectedPenId?: string | null;
		/** 凡例の一覧の名前（aria-label）。 */
		legendLabel?: string;
		/** 凡例の項目の末尾に足す、利用側の印。 */
		legendBadge?: Snippet<[TrendPen]>;
	}

	let {
		range,
		pens,
		data,
		height = 360,
		label,
		width = $bindable(0),
		loading = false,
		emptyMessage = 'この期間のデータはありません',
		nowMs,
		onRangeChange,
		bands = null,
		onPenSelect,
		selectedPenId = null,
		legendLabel = '凡例',
		legendBadge
	}: Props = $props();

	const uid = $props.id();

	const MARGIN_TOP = 8;
	const MARGIN_BOTTOM = 24;
	const AXIS_W = 56;
	const EDGE_PAD = 8;
	/** キーボード・Shift+ホイールの 1 回のパン（幅に対する割合）。 */
	const PAN_STEP = 0.1;
	/** 1 回の拡大の倍率（縮小はその逆数）。 */
	const ZOOM_IN = 0.8;

	let outerWidth = $state(0);
	let plotEl = $state<HTMLDivElement | null>(null);
	let cursorMs = $state<number | null>(null);
	/** 固定カーソル A・B（時刻 UTC ms）。範囲が動いても時刻で持つので線もいっしょに流れる。 */
	let fixed = $state<CursorPair>({ a: null, b: null });
	let announce = $state('');
	/** この px 以上動いたらクリックではなくドラッグ（パン）。 */
	const CLICK_SLOP = 4;
	let drag: {
		pointerId: number;
		x0: number;
		y0: number;
		range0: TimeRange;
		moved: boolean;
		/** 押した位置が描画域の中か（外から押したときはカーソルを置かない）。 */
		inPlot: boolean;
	} | null = null;

	const owners = $derived(axisOwners(pens));
	const marginLeft = $derived(owners.left !== null ? AXIS_W : EDGE_PAD);
	const marginRight = $derived(owners.right !== null ? AXIS_W : EDGE_PAD);
	const plotWidth = $derived(Math.max(0, outerWidth - marginLeft - marginRight));
	const plotHeight = $derived(Math.max(0, height - MARGIN_TOP - MARGIN_BOTTOM));
	const valid = $derived(isValidRange(range) && plotWidth > 0 && plotHeight > 0);

	$effect(() => {
		width = plotWidth;
	});

	// 帯の置き場と、アナログの縦の長さ（帯の分を除く）。
	const bitLayout = $derived(bitBandLayout(pens, plotHeight));
	const analogHeight = $derived(bitLayout.analogHeight);
	const xScale = $derived(linearScale([range.startMs, range.endMs], [0, plotWidth]));
	// 取得範囲 ∩ 表示範囲と重なる区間だけを、縦軸・空の判定・線・カーソルで共用する。
	const drawable = $derived(drawableSeries(data, range));
	const emptyBins: never[] = [];
	// 縦軸・空の判定に使う区間。'samples' は線のために足した範囲の外の観測を除く。
	const extentBins = $derived(
		pens.map((p) => {
			const s = drawable.series[p.id];
			if (!s) return emptyBins;
			const eff = drawable.effective;
			return s.kind === 'samples' ? (eff ? samplesInRange(s.bins, eff) : emptyBins) : s.bins;
		})
	);
	const domains: YDomain[] = $derived(pens.map((p, i) => resolveYDomain(p, extentBins[i])));
	// 'samples' の線は有効範囲の x の幅で切る（外の観測は補間のために残しているだけ）。
	const lineClip = $derived(effectiveClip(drawable.effective, xScale, plotWidth));
	/** カーソルの吸着の幅（'samples' の系列）: 8px を時刻に直した ms。 */
	const SNAP_PX = 8;
	const snapMs = $derived(
		plotWidth > 0 && isValidRange(range) ? ((range.endMs - range.startMs) / plotWidth) * SNAP_PX : 0
	);
	const paths = $derived(
		valid
			? pens.map((p, i) => {
					const s = drawable.series[p.id];
					const eff = drawable.effective;
					if (!s || !eff || isBitBand(p)) return '';
					const ys = linearScale(domains[i], [analogHeight, 0]);
					if (s.kind === 'samples')
						return samplesPath(s.bins, s.binMs, xScale, ys, { step: !!p.bit });
					return envelopePath(s.bins, s.binMs, xScale, ys, {
						startMs: eff.startMs,
						endMs: eff.endMs
					});
				})
			: []
	);
	// マーカーは線と同じ区間・範囲で、ペンごとに 1 本の path（形は `core/style.ts`）。
	const markerPaths = $derived(
		valid
			? pens.map((p, i) => {
					const kind = resolveMarkerKind(p.marker);
					const s = drawable.series[p.id];
					const eff = drawable.effective;
					if (kind === 'none' || !s || !eff || isBitBand(p)) return '';
					const size = resolveMarkerSize(p.markerSize);
					const ys = linearScale(domains[i], [analogHeight, 0]);
					const plot = { width: plotWidth, height: analogHeight };
					const pts =
						s.kind === 'samples'
							? samplesMarkerPoints(s.bins, xScale, ys, eff, size, plot)
							: markerPoints(
									s.bins,
									s.binMs,
									xScale,
									ys,
									{ startMs: eff.startMs, endMs: eff.endMs },
									size,
									plot
								);
					return markerPath(pts, kind, size);
				})
			: []
	);
	const limitLayouts = $derived(
		valid
			? pens.map((p, i) =>
					isBitBand(p) ? { lines: [], rects: [] } : limitLayout(p.limits, domains[i], analogHeight)
				)
			: []
	);
	const bandRectList = $derived(valid ? bandLayout(bands, pens, domains, analogHeight) : []);
	// bit の帯: 位置と塗り。
	const bitBandViews = $derived(
		valid
			? bitLayout.bands.map((slot) => {
					const s = drawable.series[pens[slot.penIndex].id];
					const eff = drawable.effective;
					return {
						...slot,
						pen: pens[slot.penIndex],
						spans: s && eff ? bitSpans(s.bins, s.binMs, xScale, eff, s.kind) : [],
						missing: eff
							? bitMissingSpans(s?.bins ?? emptyBins, s?.binMs ?? 0, xScale, eff, s?.kind)
							: []
					};
				})
			: []
	);
	const xTicks = $derived(valid ? timeTicks(range, plotWidth) : []);
	const yTickCount = $derived(Math.max(2, Math.floor(analogHeight / 40)));

	function axisTicks(index: number | null): { v: number; y: number }[] {
		if (index === null || !valid) return [];
		const [lo, hi] = domains[index];
		const y = linearScale([lo, hi], [analogHeight, 0]);
		return axisTickValues(pens[index], [lo, hi], yTickCount).map((v) => ({ v, y: y(v) }));
	}
	const leftTicks = $derived(axisTicks(owners.left));
	const rightTicks = $derived(axisTicks(owners.right));
	const gridTicks = $derived(owners.left !== null ? leftTicks : rightTicks);
	/** 縦軸の目盛りの文字。持ち主が bit のペン（段差）のときは 0・1 を False・True と出す。 */
	function tickText(index: number | null, v: number): string {
		if (index !== null && pens[index].bit) {
			if (v === 0) return 'False';
			if (v === 1) return 'True';
		}
		return formatValue(v);
	}

	const hasData = $derived(pens.some((_, i) => hasFiniteValue(extentBins[i])));
	const readout = $derived(
		cursorMs !== null
			? cursorReadout(pens, drawable.series, cursorMs, drawable.effective, snapMs)
			: null
	);
	const cursorX = $derived(cursorMs !== null && valid ? xScale(cursorMs) : null);
	const readoutA = $derived(
		fixed.a !== null
			? cursorReadout(pens, drawable.series, fixed.a, drawable.effective, snapMs)
			: null
	);
	const readoutB = $derived(
		fixed.b !== null
			? cursorReadout(pens, drawable.series, fixed.b, drawable.effective, snapMs)
			: null
	);
	/** 表示範囲の中にある固定カーソルの x（外は null で線を描かない）。 */
	function fixedX(t: number | null): number | null {
		if (t === null || !valid || t < range.startMs || t > range.endMs) return null;
		return xScale(t);
	}
	const fixedAX = $derived(fixedX(fixed.a));
	const fixedBX = $derived(fixedX(fixed.b));

	const pad2 = (n: number) => String(n).padStart(2, '0');
	// withSeconds: カーソルは横軸の秒の目盛りと照合できるよう秒まで出す（範囲の要約は分まで）。
	function formatDateTime(tMs: number, withSeconds = false): string {
		const d = new Date(tMs);
		const base = `${d.getFullYear()}/${pad2(d.getMonth() + 1)}/${pad2(d.getDate())} ${pad2(d.getHours())}:${pad2(d.getMinutes())}`;
		return withSeconds ? `${base}:${pad2(d.getSeconds())}` : base;
	}
	function rangeText(pen: TrendPen, d: YDomain): string {
		if (pen.bit) return BIT_RANGE_TEXT;
		const unit = pen.unit ? ` ${pen.unit}` : '';
		return `${formatValue(d[0], pen.decimals)}〜${formatValue(d[1], pen.decimals)}${unit}`;
	}
	function limitsText(p: TrendPen): string {
		const ls = (p.limits ?? []).filter((l) => Number.isFinite(l.value));
		return ls.length > 0
			? `（管理線 ${ls.map((l) => `${l.label} ${formatValue(l.value, p.decimals)}`).join('・')}）`
			: '';
	}
	const summary = $derived(
		isValidRange(range)
			? `${formatDateTime(range.startMs)}〜${formatDateTime(range.endMs)}。ペン ${pens.length} 本` +
					(pens.length > 0
						? `: ${pens.map((p, i) => `${p.label}（${rangeText(p, domains[i])}）${limitsText(p)}`).join('、')}。`
						: '。') +
					(onRangeChange
						? '左右の矢印キーで移動、+ と - で拡大・縮小、Ctrl+ホイールで拡大・縮小、Shift+ホイールかドラッグで移動。'
						: '') +
					'クリックでカーソル A・B を置く（3 回目からは近い方を移す）、Esc でカーソルを消す。'
			: '表示範囲が不正です。'
	);

	const now = () => nowMs ?? Date.now();

	function emit(next: TimeRange) {
		if (!onRangeChange) return;
		if (next.startMs === range.startMs && next.endMs === range.endMs) return;
		onRangeChange(next);
	}
	/** 拡大・縮小の後、今を越えた分を戻す。 */
	function zoomAt(factor: number, anchorMs: number) {
		emit(panRange(zoomRange(range, factor, anchorMs), 0, now()));
	}
	function panBy(fraction: number) {
		emit(panRange(range, (range.endMs - range.startMs) * fraction, now()));
	}

	/** クライアント座標の x → 描画域の中の px（範囲外も返す）。 */
	function plotX(clientX: number): number | null {
		if (!plotEl || plotWidth <= 0) return null;
		return clientX - plotEl.getBoundingClientRect().left - marginLeft;
	}
	function plotY(clientY: number): number | null {
		if (!plotEl || plotHeight <= 0) return null;
		return clientY - plotEl.getBoundingClientRect().top - MARGIN_TOP;
	}
	function inPlotArea(clientX: number, clientY: number): boolean {
		const px = plotX(clientX);
		const py = plotY(clientY);
		return px !== null && py !== null && px >= 0 && px <= plotWidth && py >= 0 && py <= plotHeight;
	}
	function timeAt(clientX: number): number | null {
		const px = plotX(clientX);
		if (px === null || !isValidRange(range)) return null;
		return range.startMs + (px / plotWidth) * (range.endMs - range.startMs);
	}

	function onpointermove(e: PointerEvent) {
		const px = plotX(e.clientX);
		if (drag && drag.pointerId === e.pointerId && plotWidth > 0) {
			if (!drag.moved && Math.hypot(e.clientX - drag.x0, e.clientY - drag.y0) >= CLICK_SLOP)
				drag.moved = true;
			const w = drag.range0.endMs - drag.range0.startMs;
			if (drag.moved && onRangeChange)
				emit(panRange(drag.range0, (-(e.clientX - drag.x0) / plotWidth) * w, now()));
		}
		if (px !== null && px >= 0 && px <= plotWidth) cursorMs = timeAt(e.clientX);
		else if (!drag) cursorMs = null;
	}
	function onpointerdown(e: PointerEvent) {
		if (e.button !== 0 || !isValidRange(range)) return;
		drag = {
			pointerId: e.pointerId,
			x0: e.clientX,
			y0: e.clientY,
			range0: range,
			moved: false,
			inPlot: inPlotArea(e.clientX, e.clientY)
		};
		(e.currentTarget as Element).setPointerCapture?.(e.pointerId);
	}
	function place(tMs: number) {
		const r = placeCursor(fixed, tMs);
		fixed = r.state;
		announce = `カーソル ${r.placed === 'a' ? 'A' : 'B'} ${formatDateTime(tMs, true)}`;
	}
	function onpointerup(e: PointerEvent) {
		const click =
			drag !== null &&
			drag.pointerId === e.pointerId &&
			!drag.moved &&
			Math.hypot(e.clientX - drag.x0, e.clientY - drag.y0) < CLICK_SLOP;
		const downInPlot = drag?.inPlot ?? false;
		endDrag(e);
		if (!click || !downInPlot || !inPlotArea(e.clientX, e.clientY)) return;
		const t = timeAt(e.clientX);
		if (t !== null) place(t);
	}
	function endDrag(e: PointerEvent) {
		if (!drag || drag.pointerId !== e.pointerId) return;
		drag = null;
		const el = e.currentTarget as Element;
		if (el.hasPointerCapture?.(e.pointerId)) el.releasePointerCapture(e.pointerId);
	}
	function onpointerleave() {
		if (!drag) cursorMs = null;
	}

	function onkeydown(e: KeyboardEvent) {
		if (e.key === 'Escape') {
			if (cursorMs !== null || fixed.a !== null || fixed.b !== null) {
				if (fixed.a !== null || fixed.b !== null) announce = 'カーソルを消しました';
				cursorMs = null;
				fixed = { a: null, b: null };
				e.preventDefault();
			}
			return;
		}
		if (!onRangeChange || !isValidRange(range)) return;
		const center = (range.startMs + range.endMs) / 2;
		if (e.key === 'ArrowLeft') panBy(-PAN_STEP);
		else if (e.key === 'ArrowRight') panBy(PAN_STEP);
		else if (e.key === '+' || e.key === '=') zoomAt(ZOOM_IN, center);
		else if (e.key === '-' || e.key === '_') zoomAt(1 / ZOOM_IN, center);
		else return;
		e.preventDefault();
	}

	// ホイールは preventDefault が要るので、非 passive で自分で登録する。
	$effect(() => {
		const el = plotEl;
		if (!el) return;
		const onwheel = (e: WheelEvent) => {
			if (!onRangeChange || !isValidRange(range)) return;
			if (e.ctrlKey || e.metaKey) {
				if (e.deltaY === 0) return;
				e.preventDefault();
				const anchor = timeAt(e.clientX) ?? (range.startMs + range.endMs) / 2;
				zoomAt(e.deltaY > 0 ? 1 / ZOOM_IN : ZOOM_IN, anchor);
			} else if (e.shiftKey) {
				// Shift+ホイールは、ブラウザによって deltaX に載る。
				const d = e.deltaX !== 0 ? e.deltaX : e.deltaY;
				if (d === 0) return;
				e.preventDefault();
				panBy(Math.sign(d) * PAN_STEP);
			}
		};
		el.addEventListener('wheel', onwheel, { passive: false });
		return () => el.removeEventListener('wheel', onwheel);
	});
</script>

<div class="bi-trend">
	<!-- 図は独自のキー操作（← → + - Esc）を持つので role="application" にして
	     フォーカスを受ける。Svelte は application を操作できない役割と数えるので、
	     その 2 つの警告だけを止める。 -->
	<!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
	<div
		class="bi-trend__plot"
		class:bi-trend__plot--interactive={!!onRangeChange}
		bind:this={plotEl}
		bind:clientWidth={outerWidth}
		style:height="{height}px"
		role="application"
		aria-roledescription="トレンド"
		aria-label={label}
		aria-describedby="{uid}-summary"
		tabindex="0"
		{onkeydown}
		{onpointermove}
		{onpointerdown}
		{onpointerup}
		onpointercancel={endDrag}
		{onpointerleave}
	>
		<svg width={outerWidth} {height} aria-hidden="true" focusable="false">
			<defs>
				<clipPath id="{uid}-clip">
					<rect x="0" y="0" width={plotWidth} height={plotHeight} />
				</clipPath>
				<pattern
					id="{uid}-missing"
					width="6"
					height="6"
					patternUnits="userSpaceOnUse"
					patternTransform="rotate(45)"
				>
					<line class="bi-trend__missing-line" x1="0" y1="0" x2="0" y2="6" />
				</pattern>
				<clipPath id="{uid}-eff-clip">
					<rect x={lineClip?.x ?? 0} y="0" width={lineClip?.width ?? 0} height={plotHeight} />
				</clipPath>
				<clipPath id="{uid}-analog-clip">
					<rect x="0" y="0" width={plotWidth} height={analogHeight} />
				</clipPath>
				{#each limitLayouts as lay, i (pens[i].id)}
					{#each lay.rects as r, j (j)}
						<clipPath id="{uid}-lim-{i}-{j}">
							<rect x="0" y={r.y} width={plotWidth} height={r.height} />
						</clipPath>
					{/each}
				{/each}
			</defs>
			<g transform="translate({marginLeft},{MARGIN_TOP})">
				{#each gridTicks as t (t.v)}
					<line class="bi-trend__grid" x1="0" x2={plotWidth} y1={t.y} y2={t.y} />
				{/each}
				{#each xTicks as t (t.tMs)}
					<line
						class="bi-trend__grid"
						x1={xScale(t.tMs)}
						x2={xScale(t.tMs)}
						y1="0"
						y2={plotHeight}
					/>
					<text class="bi-trend__tick" x={xScale(t.tMs)} y={plotHeight + 16} text-anchor="middle"
						>{t.label}</text
					>
				{/each}
				<rect class="bi-trend__frame" x="0" y="0" width={plotWidth} height={plotHeight} />

				{#if owners.left !== null}
					{#each leftTicks as t (t.v)}
						<text class="bi-trend__tick" x="-6" y={t.y} text-anchor="end" dominant-baseline="middle"
							>{tickText(owners.left, t.v)}</text
						>
					{/each}
				{/if}
				{#if owners.right !== null}
					{#each rightTicks as t (t.v)}
						<text
							class="bi-trend__tick"
							x={plotWidth + 6}
							y={t.y}
							text-anchor="start"
							dominant-baseline="middle">{tickText(owners.right, t.v)}</text
						>
					{/each}
				{/if}

				<g clip-path="url(#{uid}-clip)">
					<g clip-path="url(#{uid}-analog-clip)">
						{#each bandRectList as b, i (i)}
							<rect
								class="bi-trend__band"
								x="0"
								y={b.y}
								width={plotWidth}
								height={b.height}
								fill={b.colorVar}><title>{b.label}</title></rect
							>
						{/each}
					</g>
					{#each bitBandViews as bb (bb.pen.id)}
						<rect class="bi-trend__bit-bg" x="0" y={bb.y} width={plotWidth} height={bb.height} />
						{#each bb.missing as ms, j (j)}
							<rect
								class="bi-trend__missing"
								x={ms.x}
								y={bb.y}
								width={ms.width}
								height={bb.height}
								fill="url(#{uid}-missing)"><title>記録なし</title></rect
							>
						{/each}
						{#each bb.spans as sp, j (j)}
							<rect
								x={sp.x}
								y={bb.y}
								width={sp.width}
								height={bb.height}
								fill={penColor(bb.pen)}
								opacity={sp.state === 'true' ? 1 : 0.4}
							/>
						{/each}
						{#if bb.showLabel}
							<text
								class="bi-trend__bit-label"
								x="4"
								y={bb.y + bb.height / 2}
								dominant-baseline="middle">{bb.pen.label}</text
							>
						{/if}
					{/each}
					<g clip-path="url(#{uid}-analog-clip)">
						{#each limitLayouts as lay, i (pens[i].id)}
							{#each lay.lines as l, j (j)}
								<line
									class="bi-trend__limit"
									x1="0"
									x2={plotWidth}
									y1={l.y}
									y2={l.y}
									stroke={l.colorVar}><title>{l.label}</title></line
								>
								<text
									class="bi-trend__limit-label"
									x={plotWidth - 4}
									y={limitLabelY(l.y)}
									text-anchor="end"
									fill={l.colorVar}>{l.label}</text
								>
							{/each}
						{/each}
						{#each pens as pen, i (pen.id)}
							{#if paths[i] && resolveLineStyle(pen.lineStyle) !== 'none'}
								<path
									class="bi-trend__line"
									d={paths[i]}
									clip-path="url(#{uid}-eff-clip)"
									stroke={penColor(pen)}
									stroke-width={resolveLineWidth(pen.lineWidth)}
									stroke-dasharray={dashArray(pen.lineStyle, pen.lineWidth) ?? undefined}
								/>
							{/if}
							{#if markerPaths[i]}
								<path class="bi-trend__marker" d={markerPaths[i]} fill={penColor(pen)} />
							{/if}
							{#each limitLayouts[i]?.rects ?? [] as r, j (j)}
								<g clip-path="url(#{uid}-lim-{i}-{j})">
									{#if paths[i] && resolveLineStyle(pen.lineStyle) !== 'none'}
										<path
											class="bi-trend__line"
											d={paths[i]}
											clip-path="url(#{uid}-eff-clip)"
											stroke={r.colorVar}
											stroke-width={resolveLineWidth(pen.lineWidth)}
											stroke-dasharray={dashArray(pen.lineStyle, pen.lineWidth) ?? undefined}
										/>
									{/if}
									{#if markerPaths[i]}
										<path class="bi-trend__marker" d={markerPaths[i]} fill={r.colorVar} />
									{/if}
								</g>
							{/each}
						{/each}
					</g>
					{#if cursorX !== null}
						<line class="bi-trend__cursor" x1={cursorX} x2={cursorX} y1="0" y2={plotHeight} />
					{/if}
					{#each [{ name: 'A', x: fixedAX }, { name: 'B', x: fixedBX }] as c (c.name)}
						{#if c.x !== null}
							<line
								class="bi-trend__fixed"
								x1={c.x}
								x2={c.x}
								y1="0"
								y2={plotHeight}
								data-cursor={c.name}
							/>
							<text
								class="bi-trend__fixed-name"
								x={c.x > plotWidth - 14 ? c.x - 3 : c.x + 3}
								text-anchor={c.x > plotWidth - 14 ? 'end' : 'start'}
								y="10">{c.name}</text
							>
						{/if}
					{/each}
				</g>

				{#if loading || (!hasData && valid)}
					<text
						class="bi-trend__message"
						x={plotWidth / 2}
						y={plotHeight / 2}
						text-anchor="middle"
						dominant-baseline="middle">{loading ? '読み込み中…' : emptyMessage}</text
					>
				{/if}
			</g>
		</svg>
	</div>

	<p id="{uid}-summary" class="bi-trend__sr">{label}。{summary}</p>
	<p class="bi-trend__sr" aria-live="polite">{loading ? '読み込み中' : ''}</p>
	<p class="bi-trend__sr" aria-live="polite">{announce}</p>

	{#if pens.length > 0}
		<ul class="bi-trend__legend" aria-label={legendLabel}>
			{#each pens as pen, i (pen.id)}
				<li class="bi-trend__legend-item">
					{#if onPenSelect}
						<button
							type="button"
							class="bi-trend__legend-button"
							aria-pressed={selectedPenId === pen.id}
							onclick={() => onPenSelect(pen.id)}
						>
							{@render legendBody(pen, i)}
						</button>
					{:else}
						{@render legendBody(pen, i)}
					{/if}
				</li>
			{/each}
		</ul>
	{/if}
	{#if cursorMs !== null}
		<p class="bi-trend__cursor-time">{formatDateTime(cursorMs, true)}</p>
	{/if}
	{#if fixed.a !== null || fixed.b !== null}
		<p class="bi-trend__cursor-time">
			{#if fixed.a !== null}A {formatDateTime(fixed.a, true)}{/if}
			{#if fixed.b !== null}
				B {formatDateTime(fixed.b, true)}{/if}
			{#if fixed.a !== null && fixed.b !== null}
				Δt {formatDuration(fixed.b - fixed.a)}{/if}
		</p>
	{/if}
</div>

{#snippet obsTime(v: CursorValue)}
	{#if typeof v.tMs === 'number'}<small class="bi-trend__muted">
			({formatObservationTime(v.tMs)})</small
		>{/if}
{/snippet}

{#snippet legendBody(pen: TrendPen, i: number)}
	<TrendPenSwatch {pen} />
	<span class="bi-trend__legend-label">{pen.label}</span>
	{#if pen.axis === 'right'}<span class="bi-trend__muted">（右軸）</span>{/if}
	<span class="bi-trend__muted">{rangeText(pen, domains[i])}</span>
	{#if readout}
		<span class="bi-trend__value">{readout[i].text}{@render obsTime(readout[i])}</span>
	{/if}
	{#if readoutA}<span class="bi-trend__value"
			>A {readoutA[i].text}{@render obsTime(readoutA[i])}</span
		>{/if}
	{#if readoutB}<span class="bi-trend__value"
			>B {readoutB[i].text}{@render obsTime(readoutB[i])}</span
		>{/if}
	{#if readoutA && readoutB}
		<span class="bi-trend__value">差 {cursorDiff(pen, readoutA[i], readoutB[i])}</span>
	{/if}
	{#if legendBadge}{@render legendBadge(pen)}{/if}
{/snippet}

<style>
	.bi-trend {
		display: flex;
		flex-direction: column;
		gap: var(--banto-space-1);
		color: var(--banto-text);
		min-width: 0;
	}
	.bi-trend__plot {
		position: relative;
		width: 100%;
		background: var(--banto-surface);
		touch-action: pan-y;
		user-select: none;
	}
	.bi-trend__plot--interactive {
		cursor: grab;
	}
	.bi-trend__plot:focus-visible {
		outline: none;
		box-shadow: var(--banto-focus-ring);
	}
	svg {
		display: block;
		overflow: visible;
	}
	.bi-trend__grid {
		stroke: var(--banto-chart-grid);
		stroke-width: 1;
	}
	.bi-trend__frame {
		fill: none;
		stroke: var(--banto-chart-axis);
		stroke-width: 1;
	}
	.bi-trend__tick {
		fill: var(--banto-text-muted);
		font-size: var(--banto-font-size-xs);
	}
	.bi-trend__line {
		fill: none;
		stroke-linejoin: round;
		stroke-linecap: round;
		vector-effect: non-scaling-stroke;
	}
	.bi-trend__marker {
		stroke: none;
	}
	.bi-trend__limit {
		stroke-width: 1;
		stroke-dasharray: 6 3;
		vector-effect: non-scaling-stroke;
	}
	.bi-trend__limit-label {
		font-size: var(--banto-font-size-xs);
	}
	.bi-trend__bit-bg {
		fill: none;
		stroke: var(--banto-chart-grid);
		stroke-width: 1;
	}
	.bi-trend__missing-line {
		stroke: var(--banto-chart-grid);
		stroke-width: 1.5;
	}
	.bi-trend__bit-label {
		fill: var(--banto-text-muted);
		font-size: var(--banto-font-size-xs);
	}
	.bi-trend__band {
		opacity: 0.12;
	}
	.bi-trend__cursor {
		stroke: var(--banto-text-muted);
		stroke-width: 1;
		stroke-dasharray: 4 3;
	}
	.bi-trend__fixed {
		stroke: var(--banto-text);
		stroke-width: 1.5;
	}
	.bi-trend__fixed-name {
		fill: var(--banto-text);
		font-size: var(--banto-font-size-xs);
		font-weight: 600;
	}
	.bi-trend__message {
		fill: var(--banto-text-muted);
		font-size: var(--banto-font-size-sm);
	}
	.bi-trend__legend {
		display: flex;
		flex-wrap: wrap;
		gap: var(--banto-space-1) var(--banto-space-2);
		margin: 0;
		padding: 0;
		list-style: none;
		font-size: var(--banto-font-size-sm);
	}
	.bi-trend__legend-item {
		display: inline-flex;
		align-items: center;
		gap: var(--banto-space-1);
	}
	.bi-trend__legend-button {
		display: inline-flex;
		align-items: center;
		gap: var(--banto-space-1);
		padding: 0.1rem 0.4rem;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
		background: transparent;
		color: var(--banto-text-muted);
		font: inherit;
		cursor: pointer;
	}
	.bi-trend__legend-button[aria-pressed='true'] {
		border-color: var(--banto-text);
		color: var(--banto-text);
	}
	.bi-trend__legend-button:focus-visible {
		outline: none;
		box-shadow: var(--banto-focus-ring);
	}
	.bi-trend__muted,
	.bi-trend__cursor-time {
		color: var(--banto-text-muted);
	}
	.bi-trend__cursor-time {
		margin: 0;
		font-size: var(--banto-font-size-xs);
	}
	.bi-trend__value {
		font-variant-numeric: tabular-nums;
	}
	.bi-trend__sr {
		position: absolute;
		width: 1px;
		height: 1px;
		margin: -1px;
		padding: 0;
		overflow: hidden;
		clip: rect(0 0 0 0);
		white-space: nowrap;
		border: 0;
	}
</style>
