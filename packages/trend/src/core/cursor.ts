/**
 * カーソルの位置の値（凡例に出す）。
 */

import { formatValue, MISSING_TEXT, type Bin, type TrendPen } from './pens';

const fin = (v: number | null | undefined): v is number =>
	typeof v === 'number' && Number.isFinite(v);

/**
 * `tMs` を含む区間（`[b.tMs, b.tMs + binMs)`）。`bins` は `tMs` の昇順（契約）。
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
	if (tMs >= b.tMs + binMs) return null;
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

/** ペンごとのカーソル位置の値。値が 1 つなら `値 単位`、幅があれば `最小〜最大 単位`。 */
export function cursorReadout(
	pens: readonly TrendPen[],
	binsByPen: Readonly<Record<string, readonly Bin[] | undefined>>,
	binMs: number,
	tMs: number
): CursorValue[] {
	return pens.map((pen) => {
		const b = binAt(binsByPen[pen.id] ?? [], binMs, tMs);
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
