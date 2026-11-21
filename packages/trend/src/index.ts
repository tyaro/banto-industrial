/**
 * `@banto-industrial/trend` — 記録計のトレンドの共通部品。
 *
 * 設計: [docs/chronogazer/trend-design.md](../../../docs/chronogazer/trend-design.md)
 * （§2・§4。ChronoGazer の監視・ヒストリカル・自由トレンドと、temp-scada の画面が使う）。
 *
 * 持つもの:
 * - 時刻の軸と表示範囲（`core/range.ts`・`core/timeTicks.ts`）。範囲は UTC ミリ秒の
 *   `[startMs, endMs)` で、拡大・縮小・パン・前へ・次へ・期間の候補・追従は純粋な関数。
 * - ペンと縦軸（`core/pens.ts`）。ペンごとの縦軸の範囲（指定 → 工学値レンジ → データ）、
 *   左右の 2 軸、16 色の枠。
 * - 区間（bins）の数と、MIN/MAX の包絡の形（`core/bins.ts`・`core/geometry.ts`）。
 * - 管理線と、越えた区間の線の色替えの位置（`core/limits.ts`）。ペンの属性 `limits`
 * - bit のペンの帯（タイムチャート）の置き場と塗り（`core/bitBands.ts`）。ペンの属性
 *   `bit`・`bitStyle`・`bitBandHeight`。bit の値は False / True と出す（`formatBitValue`）。
 * - 時刻の区間の帯 `timeBands` と時刻の印 `timeMarks` の位置の計算（`core/timeBands.ts`）。
 * - しきい値などの横の帯の位置（`core/bands.ts`）。指定したペンの縦軸で、線の下に描く
 *   （汎用の `{ from, to, colorVar, label }`。管理線に置き換えるときに見直す）。
 * - カーソルの値と、カーソル A・B（置き方・差・時間の差）（`core/cursor.ts`）と、範囲が変わったときの読み直しの段取り
 *   （`core/loader.ts`。debounce・abort・古い結果を捨てる）。
 * - ハイブリッドの純粋な核（設計書 §8）。ペンごとのリングバッファと `liveFrom`（`core/live.ts`）、
 *   履歴の区間とのつなぎ合わせ・境界をまたぐ区間の読み直しの計画と段取り（`core/hybrid.ts`）。
 * - SVG の描画（`TrendChart.svelte`）。計算は core に置き、描画の層は後で Canvas に
 *   替えられるよう組み立てだけを持つ。
 *
 * 持たないもの:
 * - データの取り方。利用側が `TrendFetch` を渡す（ChronoGazer の `collectAdmin.ts`・
 *   Tauri の呼び出し、SCADA の History API の型をここに入れない。設計書 §4.1・§4.3）。
 * - 表示範囲の状態。`TrendChart` は制御された部品で、操作の結果を `onRangeChange` で
 *   返すだけ。範囲は利用側が持つ。
 * - 平均・件数。共通の区間の契約は `Bin { tMs, min, max }` だけ（設計書 §4.3）。
 *
 * データの受け渡しの契約は「取得範囲 + ペンごとの `{ bins, binMs }`」（`core/data.ts` の
 * `TrendData`）。幅は API の実際の値をデータと組で持ち、描画は取得範囲と表示範囲の重なりだけで行う。
 *
 * banto の `LineChart` を使わない理由は設計書 §2 の 1（横軸が点の番号で、縦軸が全系列で
 * 共通。記録計の時刻の軸・ペンごとの縦軸・ハイブリッドに合わない）。banto の純粋な関数
 * （`linearScale`・`niceTicks`）とテーマのトークン（`--banto-*`）は使う。
 */

export {
	RANGE_PRESETS,
	MAX_RANGE_MS,
	MIN_RANGE_MS,
	isValidRange,
	clampWidth,
	presetRange,
	followRange,
	zoomRange,
	panRange,
	shiftPage,
	canShiftNext,
	isAtNow,
	type TimeRange,
	type RangePreset
} from './core/range';
export { timeTicks, pickTimeStep, TIME_TICK_STEPS_MS, type TimeTick } from './core/timeTicks';
export { MAX_BINS, binCount, binMs } from './core/bins';
export {
	MAX_COLOR_SLOTS,
	MISSING_TEXT,
	dataExtent,
	resolveYDomain,
	axisOwners,
	penColorVar,
	penColor,
	formatValue,
	formatBitValue,
	isBitBand,
	BIT_RANGE_TEXT,
	type Bin,
	type TrendPen,
	type LineStyle,
	type MarkerKind,
	type YDomain
} from './core/pens';
export { envelopePath, visibleBins, hasFiniteValue, binCenterMs } from './core/geometry';
export {
	DEFAULT_LINE_WIDTH,
	DEFAULT_MARKER_SIZE,
	resolveLineStyle,
	resolveLineWidth,
	resolveMarkerKind,
	resolveMarkerSize,
	dashArray,
	swatchDashArray,
	swatchWidth,
	markerPoints,
	markerPath,
	type MarkerPoint
} from './core/style';
export {
	intersectRange,
	drawableSeries,
	type TrendSeries,
	type TrendData,
	type DrawableSeries
} from './core/data';
export {
	bandRects,
	bandLayout,
	type TrendBand,
	type TrendBands,
	type BandRect
} from './core/bands';
export {
	limitLayout,
	limitLabelY,
	type TrendLimit,
	type LimitLine,
	type LimitRect,
	type LimitLayout
} from './core/limits';
export {
	BIT_BAND_DEFAULT,
	BIT_BAND_MIN,
	BIT_BAND_MAX,
	BIT_BAND_GAP,
	resolveBitBandHeight,
	bitBandLayout,
	bitSpans,
	bitMissingSpans,
	type BitMissingSpan,
	type BitBandSlot,
	type BitBandLayout,
	type BitSpan
} from './core/bitBands';
export {
	timeBandLayout,
	timeMarkLayout,
	timeSummaryText,
	TIME_LANE_HEIGHT,
	TIME_LANE_GAP,
	TIME_MARK_LABEL_MIN_GAP,
	TIME_MARK_DEFAULT_COLOR,
	type TrendTimeBand,
	type TrendTimeMark,
	type TimeBandRect,
	type TimeBandLayout,
	type TimeMarkLine
} from './core/timeBands';
export {
	SAMPLE_GAP_FACTOR,
	sampleAt,
	sampleIntervals,
	sampleValue,
	samplesForDraw,
	samplesInRange,
	samplesMarkerPoints,
	samplesPath,
	type SampleInterval
} from './core/samples';
export {
	binAt,
	cursorDiff,
	cursorReadout,
	formatDuration,
	placeCursor,
	type CursorPair,
	type CursorValue
} from './core/cursor';
export {
	createRangeLoader,
	isAbortError,
	type TrendFetch,
	type TrendFetchRequest,
	type TrendLoadRequest,
	type RangeLoader,
	type RangeLoaderOptions
} from './core/loader';
export {
	EMPTY_LIVE_PEN,
	LIVE_EVICT_SLACK,
	LIVE_MIN_RETAIN_MS,
	liveRetainMs,
	liveAppend,
	liveAppendAll,
	liveBreak,
	liveBreakAll,
	type LiveSample,
	type LivePen,
	type LiveBuffers,
	type LiveOptions
} from './core/live';
export {
	boundaryKey,
	boundaryPlan,
	boundaryValuesFrom,
	pruneBoundary,
	estimateIntervalMs,
	liveBinMs,
	liveBins,
	hybridData,
	createBoundaryLoader,
	type HybridSources,
	type HybridInput,
	type BoundaryValues,
	type BoundaryRequest,
	type BoundaryLoader,
	type BoundaryLoaderOptions
} from './core/hybrid';
export { default as TrendChart } from './TrendChart.svelte';
export { default as TrendPenSwatch } from './TrendPenSwatch.svelte';
