/**
 * Pure aggregation helpers for the M4 dashboard (spec §6 integration): all
 * derived from the `items` list resource, kept side-effect free so they stay
 * type-checked and trivially testable even though this milestone only wires
 * them into `+page.svelte` (no dedicated Vitest suite for this app-level
 * glue - the chart math itself is covered in @banto/charts).
 */
import type { Item } from './sampleData';

export interface StatTiles {
	count: number;
	stockTotal: number;
	avgPrice: number;
	lowStockCount: number;
}

const LOW_STOCK_THRESHOLD = 50;

/** 商品数 / 在庫合計 / 平均価格 (rounded mean) / 在庫僅少 (stock < 50) counts. */
export function computeStatTiles(items: Item[]): StatTiles {
	const count = items.length;
	const stockTotal = items.reduce((sum, item) => sum + item.stock, 0);
	const avgPrice =
		count === 0 ? 0 : Math.round(items.reduce((sum, item) => sum + item.price, 0) / count);
	const lowStockCount = items.filter((item) => item.stock < LOW_STOCK_THRESHOLD).length;
	return { count, stockTotal, avgPrice, lowStockCount };
}

export interface CategoryStock {
	category: string;
	stock: number;
}

/** Category = the item name's first whitespace-separated token (e.g. "緑茶 500ml" -> "緑茶"). */
export function categoryOf(name: string): string {
	return name.split(/\s+/)[0] || name;
}

/**
 * Sum of stock per category, ALL categories, sorted desc. No 「その他」 fold:
 * the demo dataset's category set is small and bounded (12), and folding the
 * tail into a single bar summed several categories into one bar ~5x longer
 * than any real one - a misleading chart. Folding is only warranted when the
 * category count is unbounded. Note the 8-color-slot design rule constrains
 * SERIES count, not bar count; this feeds a single-series (single-hue) bar
 * chart, so any number of bars is fine.
 */
export function byCategory(items: Item[]): CategoryStock[] {
	const totals = new Map<string, number>();
	for (const item of items) {
		const cat = categoryOf(item.name);
		totals.set(cat, (totals.get(cat) ?? 0) + item.stock);
	}
	return [...totals.entries()]
		.map(([category, stock]) => ({ category, stock }))
		.sort((a, b) => b.stock - a.stock);
}

export interface PriceBucket {
	bucket: string;
	count: number;
}

const PRICE_BUCKET_LABELS = ['〜¥99', '¥100〜149', '¥150〜199', '¥200〜'] as const;

/** 〜¥99 / ¥100〜149 / ¥150〜199 / ¥200〜 item counts. */
export function priceBuckets(items: Item[]): PriceBucket[] {
	const counts = [0, 0, 0, 0];
	for (const item of items) {
		if (item.price <= 99) counts[0]++;
		else if (item.price <= 149) counts[1]++;
		else if (item.price <= 199) counts[2]++;
		else counts[3]++;
	}
	return PRICE_BUCKET_LABELS.map((bucket, i) => ({ bucket, count: counts[i] }));
}

export interface MonthCount {
	/** `YYYY-MM`. */
	month: string;
	count: number;
}

/**
 * Count of items per updatedAt MONTH (`YYYY-MM` = first 7 chars of the ISO
 * date), ascending; lexicographic sort of `YYYY-MM` is chronological. Daily
 * granularity (~900 points over the demo dataset's 2.5-year span) rendered
 * as an unreadable noise band in a ~300px-wide card, so the dashboard
 * aggregates monthly (~30 points) instead.
 */
export function updatesByMonth(items: Item[]): MonthCount[] {
	const counts = new Map<string, number>();
	for (const item of items) {
		const month = item.updatedAt.slice(0, 7);
		counts.set(month, (counts.get(month) ?? 0) + 1);
	}
	return [...counts.entries()]
		.map(([month, count]) => ({ month, count }))
		.sort((a, b) => (a.month < b.month ? -1 : a.month > b.month ? 1 : 0));
}

const SCATTER_SAMPLE_SIZE = 500;

/** First N rows, deterministic (no shuffling) - a stable sample for the scatter chart. */
export function scatterSample(items: Item[], limit: number = SCATTER_SAMPLE_SIZE): Item[] {
	return items.slice(0, limit);
}

export interface MonthWithMovingAvg {
	/** `YYYY-MM`. */
	month: string;
	count: number;
	/** Trailing average of `count` over this month and up to the 2 preceding
	 * months, rounded to 1 decimal. The first 2 months of the series have
	 * fewer than 3 months of history, so their window is shorter (2, then 1) -
	 * a documented simplification rather than leaving them undefined. */
	avg3: number;
}

const MOVING_AVG_WINDOW = 3;

/** §6.1 ComboChart demo data: monthly update counts plus their 3-month trailing moving average (spec §6.1 dashboard integration). */
export function monthlyWithMovingAvg(items: Item[]): MonthWithMovingAvg[] {
	const months = updatesByMonth(items);
	return months.map((m, i) => {
		const windowStart = Math.max(0, i - (MOVING_AVG_WINDOW - 1));
		const window = months.slice(windowStart, i + 1);
		const avg = window.reduce((sum, w) => sum + w.count, 0) / window.length;
		return { month: m.month, count: m.count, avg3: Math.round(avg * 10) / 10 };
	});
}

export const WEEKDAY_LABELS = ['日', '月', '火', '水', '木', '金', '土'] as const;

export interface WeekdayMonthCount {
	/** `日`..`土`, fixed order (spec §6.1 Heatmap demo: y-axis order is not sorted alphabetically, it's the calendar week order). */
	weekday: (typeof WEEKDAY_LABELS)[number];
	/** `YYYY-MM`. */
	month: string;
	count: number;
}

const HEATMAP_MONTHS = 12;

/**
 * §6.1 Heatmap demo data: item-update counts by weekday x month, for the
 * LAST 12 months only (all ~30 months would make an unreadably narrow-column
 * heatmap). Every (weekday, month) combination in that window gets an
 * explicit row - including a `count` of 0 - so the heatmap renders a full
 * rectangle instead of leaving genuinely-zero cells indistinguishable from
 * "no data" gaps. Row order is deliberately weekday-major (all of 日's months,
 * then all of 月's, ...) and month-ascending within each weekday, so
 * `heatmapGrid`'s first-appearance category ordering comes out as
 * `weekday: 日..土` / `month: ascending` with no extra sorting needed downstream.
 */
export function weekdayMonthHeat(items: Item[]): WeekdayMonthCount[] {
	const lastMonths = updatesByMonth(items)
		.map((m) => m.month)
		.slice(-HEATMAP_MONTHS);
	const monthSet = new Set(lastMonths);

	const key = (weekday: string, month: string) => `${weekday} ${month}`;
	const counts = new Map<string, number>();
	for (const weekday of WEEKDAY_LABELS) {
		for (const month of lastMonths) counts.set(key(weekday, month), 0);
	}

	for (const item of items) {
		const month = item.updatedAt.slice(0, 7);
		if (!monthSet.has(month)) continue;
		// `updatedAt` is a UTC-generated `YYYY-MM-DD` (see sampleData.ts); read
		// it back as UTC so the weekday doesn't shift with the local timezone.
		const weekday = WEEKDAY_LABELS[new Date(`${item.updatedAt}T00:00:00Z`).getUTCDay()];
		const k = key(weekday, month);
		counts.set(k, (counts.get(k) ?? 0) + 1);
	}

	const rows: WeekdayMonthCount[] = [];
	for (const weekday of WEEKDAY_LABELS) {
		for (const month of lastMonths) {
			rows.push({ weekday, month, count: counts.get(key(weekday, month)) ?? 0 });
		}
	}
	return rows;
}

export interface CategoryCount {
	category: string;
	count: number;
}

/** §6.1 RadarChart demo data: top-N categories by ITEM COUNT (not stock, unlike `byCategory`), desc. */
export function categoryCountsTop(items: Item[], n: number = 5): CategoryCount[] {
	const counts = new Map<string, number>();
	for (const item of items) {
		const cat = categoryOf(item.name);
		counts.set(cat, (counts.get(cat) ?? 0) + 1);
	}
	return [...counts.entries()]
		.map(([category, count]) => ({ category, count }))
		.sort((a, b) => b.count - a.count)
		.slice(0, n);
}

// --- M13 SPC panel demo data (roadmap.md M13: histogram / Pareto / box plot) ---

/** All item prices, unfiltered - `Histogram` bins these itself (`core/bins.ts`). */
export function priceValues(items: Item[]): number[] {
	return items.map((item) => item.price);
}

export interface CategoryCountItem {
	label: string;
	value: number;
}

/** ALL categories with their item counts, unsorted - `ParetoChart` sorts descending itself (`core/pareto.ts`). Unlike `categoryCountsTop` this is not truncated: a Pareto chart's whole point is showing the full distribution against the 80% line. */
export function categoryCounts(items: Item[]): CategoryCountItem[] {
	const counts = new Map<string, number>();
	for (const item of items) {
		const cat = categoryOf(item.name);
		counts.set(cat, (counts.get(cat) ?? 0) + 1);
	}
	return [...counts.entries()].map(([label, value]) => ({ label, value }));
}

export interface PriceGroup {
	label: string;
	values: number[];
}

const BOX_PLOT_TOP_CATEGORIES = 6;

/** Top-N categories by item count (reuses `categoryCountsTop`), each with its raw price array - `BoxPlot` computes five-number summaries itself (`core/boxplot.ts`). Truncated to keep the box plot legible (unlike `categoryCounts` above). */
export function priceByCategoryGroups(
	items: Item[],
	topN: number = BOX_PLOT_TOP_CATEGORIES
): PriceGroup[] {
	const top = categoryCountsTop(items, topN).map((c) => c.category);
	const byCategory = new Map<string, number[]>(top.map((cat) => [cat, []]));
	for (const item of items) {
		const cat = categoryOf(item.name);
		byCategory.get(cat)?.push(item.price);
	}
	return top.map((label) => ({ label, values: byCategory.get(label) ?? [] }));
}

// --- M13 trend panel demo data (roadmap.md M13: zoom/pan, bands, markers, second axis, streaming) ---

export interface TrendPoint {
	/** Sequential sample index - the trend panel's x-axis is an ordered category (`core/ticks-time.ts`), not real elapsed time. */
	t: number;
	/** °C, plotted on the left axis. */
	temperature: number;
	/** MPa, plotted on the right axis (different unit - the M13 第2Y軸 demo). */
	pressure: number;
}

const TREND_BASE_TEMPERATURE = 68;
const TREND_BASE_PRESSURE = 1.2;

/**
 * One streaming sample: a small random walk from `prev` (or the baseline
 * when `prev` is undefined, i.e. the first seeded point) so the demo trend
 * has plausible SPC-like noise instead of a flat line. `random` is injectable
 * (defaults to `Math.random`) so the walk can be made deterministic in tests.
 */
export function nextTrendPoint(
	prev: TrendPoint | undefined,
	random: () => number = Math.random
): TrendPoint {
	return {
		t: (prev?.t ?? -1) + 1,
		temperature: (prev?.temperature ?? TREND_BASE_TEMPERATURE) + (random() - 0.5) * 1.5,
		pressure: (prev?.pressure ?? TREND_BASE_PRESSURE) + (random() - 0.5) * 0.05
	};
}

/** Seed `count` consecutive trend points (see `nextTrendPoint`), for the panel's initial (pre-streaming) render. */
export function seedTrendPoints(count: number, random: () => number = Math.random): TrendPoint[] {
	const points: TrendPoint[] = [];
	let prev: TrendPoint | undefined;
	for (let i = 0; i < count; i++) {
		prev = nextTrendPoint(prev, random);
		points.push(prev);
	}
	return points;
}
