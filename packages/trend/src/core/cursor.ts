/**
 * カーソルの位置の値（凡例に出す）。
 */

import type { TrendSeries } from './data';
import { binEndMs } from './geometry';
import { formatValue, MISSING_TEXT, type Bin, type TrendPen } from './pens';
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
		const unit = pen.unit ? ` ${pen.unit}` : '';
		const a = formatValue(min, pen.decimals);
		const z = formatValue(max, pen.decimals);
		return { penId: pen.id, min, max, text: a === z ? `${a}${unit}` : `${a}〜${z}${unit}` };
	});
}
