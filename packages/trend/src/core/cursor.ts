/**
 * カーソルの位置の値（凡例に出す）。
 */

import type { TrendSeries } from './data';
import { binEndMs } from './geometry';
import { formatBitValue, formatValue, MISSING_TEXT, type Bin, type TrendPen } from './pens';
import type { TimeRange } from './range';

const fin = (v: number | null | undefined): v is number =>
	typeof v === 'number' && Number.isFinite(v);

/**
 * `tMs` を含む区間（`[b.tMs, min(b.tMs + binMs, 次の区間の tMs))`）。`bins` は `tMs` の昇順（契約）。
 * 範囲外・欠測（有限の値が無い）は null。
 */
export function binAt(bins: readonly Bin[], binMs: number, tMs: number): Bin | null {
	if (!fin(tMs) || !fin(binMs) || binMs <= 0 || bins.length === 0) return null;
	let lo = 0;
	let hi = bins.length - 1;
	let found = -1;
	while (lo <= hi) {
		const mid = (lo + hi) >> 1;
		if (bins[mid].tMs <= tMs) {
			found = mid;
			lo = mid + 1;
		} else {
			hi = mid - 1;
		}
	}
	if (found < 0) return null;
	const b = bins[found];
	if (tMs >= binEndMs(b.tMs, binMs, bins[found + 1]?.tMs)) return null;
	if (!fin(b.min) && !fin(b.max)) return null;
	return b;
}

export interface CursorValue {
	penId: string;
	min: number | null;
	max: number | null;
	/** 書式化した値（単位付き）。欠測は「—」。 */
	text: string;
}

/**
 * ペンごとのカーソル位置の値。値が 1 つなら `値 単位`、幅があれば `最小〜最大 単位`。
 * 区間の幅はペンごと（`series[id].binMs`）。`tMs` が有効範囲（取得範囲 ∩ 表示範囲）の外、
 * または有効範囲が null なら全ペン「—」。
 */
export function cursorReadout(
	pens: readonly TrendPen[],
	series: Readonly<Record<string, TrendSeries | undefined>>,
	tMs: number,
	effective: TimeRange | null
): CursorValue[] {
	const inside = effective !== null && tMs >= effective.startMs && tMs < effective.endMs;
	return pens.map((pen) => {
		const s = series[pen.id];
		const b = inside && s ? binAt(s.bins, s.binMs, tMs) : null;
		const min = b ? (fin(b.min) ? b.min : fin(b.max) ? b.max : null) : null;
		const max = b ? (fin(b.max) ? b.max : min) : null;
		if (min === null || max === null)
			return { penId: pen.id, min: null, max: null, text: MISSING_TEXT };
		if (pen.bit) return { penId: pen.id, min, max, text: formatBitValue(min, max) };
		const unit = pen.unit ? ` ${pen.unit}` : '';
		const a = formatValue(min, pen.decimals);
		const z = formatValue(max, pen.decimals);
		return { penId: pen.id, min, max, text: a === z ? `${a}${unit}` : `${a}〜${z}${unit}` };
	});
}

/** 固定カーソル A・B の時刻（UTC ms）。置いていない側は null。 */
export interface CursorPair {
	a: number | null;
	b: number | null;
}

/**
 * クリックの時刻に A・B を置く規則。1 回目は A、2 回目は B。
 * 3 回目以降は、クリックの時刻に近い方を移す（同じ距離なら A）。
 */
export function placeCursor(
	state: CursorPair,
	tMs: number
): { state: CursorPair; placed: 'a' | 'b' } {
	if (state.a === null) return { state: { a: tMs, b: state.b }, placed: 'a' };
	if (state.b === null) return { state: { a: state.a, b: tMs }, placed: 'b' };
	const placed = Math.abs(tMs - state.a) <= Math.abs(tMs - state.b) ? 'a' : 'b';
	return {
		state: placed === 'a' ? { a: tMs, b: state.b } : { a: state.a, b: tMs },
		placed
	};
}

/**
 * ペンごとの差（B − A）。符号は常に付ける。どちらかが欠測なら「—」、bit は差を出さず「—」。
 * 両方が 1 つの値（min === max）なら `B − A`、どちらかに幅があれば中点どうしの差に「約」を付ける。
 */
export function cursorDiff(pen: TrendPen, a: CursorValue, b: CursorValue): string {
	if (pen.bit) return MISSING_TEXT;
	if (!fin(a.min) || !fin(a.max) || !fin(b.min) || !fin(b.max)) return MISSING_TEXT;
	const exact = a.min === a.max && b.min === b.max;
	const d = (b.min + b.max) / 2 - (a.min + a.max) / 2;
	const body = formatValue(Math.abs(d), pen.decimals);
	const sign = d < 0 && /[1-9]/.test(body) ? '-' : '+';
	const unit = pen.unit ? ` ${pen.unit}` : '';
	return `${exact ? '' : '約 '}${sign}${body}${unit}`;
}

/**
 * 時間の差（ミリ秒、符号付き）。`1時間02分03秒`・`5分00秒`・`12秒`、1 日以上は `2日03時間04分`。
 * 秒の端数は切り捨て。0 は `0秒`。
 */
export function formatDuration(ms: number): string {
	if (!fin(ms)) return MISSING_TEXT;
	const total = Math.floor(Math.abs(ms) / 1000);
	const sign = ms < 0 && total > 0 ? '-' : '';
	const d = Math.floor(total / 86400);
	const h = Math.floor((total % 86400) / 3600);
	const m = Math.floor((total % 3600) / 60);
	const s = total % 60;
	const p2 = (n: number) => String(n).padStart(2, '0');
	if (d > 0) return `${sign}${d}日${p2(h)}時間${p2(m)}分`;
	if (h > 0) return `${sign}${h}時間${p2(m)}分${p2(s)}秒`;
	if (m > 0) return `${sign}${m}分${p2(s)}秒`;
	return `${sign}${s}秒`;
}
