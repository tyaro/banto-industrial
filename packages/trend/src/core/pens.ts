/**
 * ペン（系列）の型と、縦軸の範囲・左右の軸・色・値の書式。
 *
 * 共通の契約は `Bin { tMs, min, max }` だけ（設計書 §4.3）。min・max が両方 null の
 * 区間は欠測で、線をつながない。片方だけ有限なら、その値 1 つの区間として扱う。
 */

import type { TrendLimit } from './limits';

/** 区間 1 つの値。`tMs` は区間の始まり（UTC ミリ秒）。 */
export interface Bin {
	tMs: number;
	min: number | null;
	max: number | null;
}

/** ペン 1 本の表示の定義。 */
export interface TrendPen {
	/** ペンの組の中で一意であること（`binsByPen` の鍵と、描画・凡例の each の鍵に使う）。 */
	id: string;
	label: string;
	/** 色の枠 1〜16（1〜8 は banto の `--banto-chart-N`、9〜16 は `--bi-chart-N`）。 */
	colorSlot: number;
	unit?: string | null;
	/** 小数の桁。未指定は有効数字 6 桁で出す。 */
	decimals?: number | null;
	/** 縦軸の側。未指定は左。 */
	axis?: 'left' | 'right';
	/** 縦軸の最小・最大の指定（片側だけでもよい）。 */
	yMin?: number | null;
	yMax?: number | null;
	/** 工学値レンジ。縦軸の指定が無い側に使う。 */
	engRange?: { min: number; max: number } | null;
	/** 線の種類。未指定・null は実線（`core/style.ts`）。 */
	lineStyle?: LineStyle | null;
	/** 線幅（px、1〜4）。未指定・null は 1.5。 */
	lineWidth?: number | null;
	/** マーカーの形。未指定・null はなし。 */
	marker?: MarkerKind | null;
	/** マーカーの大きさ（px、2〜8）。未指定・null は 4。 */
	markerSize?: number | null;
	/** 管理線。null・空 = 引かない。越えた区間の色は、並びの後ろのものが前のものの上に重なる（H → HH の順に渡せば HH の色が勝つ）。 */
	limits?: readonly TrendLimit[] | null;
	/** bit のペン。値 0 / 1 を False / True と出す（凡例の範囲・カーソル・要約）。 */
	bit?: boolean;
	/** bit の表し。'band' は描画域の下の帯（タイムチャート）。未指定・'step' は今までどおり線（段差）。bit でないペンでは無視。 */
	bitStyle?: 'step' | 'band' | null;
	/** 帯の太さ（px）。未指定は 16、8〜32 に丸める。 */
	bitBandHeight?: number | null;
}

export type LineStyle = 'solid' | 'dashed' | 'dashDot' | 'dashDotDot' | 'none';
export type MarkerKind = 'none' | 'circle' | 'square' | 'triangle' | 'diamond';

/** 縦軸の範囲 `[lo, hi]`（lo < hi）。 */
export type YDomain = [number, number];

const isNum = (v: unknown): v is number => typeof v === 'number' && Number.isFinite(v);

/** 区間の値の範囲（有限の値だけ）。値が 1 つも無ければ null。 */
export function dataExtent(bins: readonly Bin[]): YDomain | null {
	let lo = Infinity;
	let hi = -Infinity;
	for (const b of bins) {
		for (const v of [b.min, b.max]) {
			if (isNum(v)) {
				if (v < lo) lo = v;
				if (v > hi) hi = v;
			}
		}
	}
	return lo <= hi ? [lo, hi] : null;
}

/** 値 v の周りの余白（min == max のとき）。0 なら 1。 */
function padFor(v: number): number {
	return Math.abs(v) * 0.1 || 1;
}

/**
 * ペンの縦軸の範囲。側ごとに、ペンの指定（`yMin`/`yMax`）→ 工学値レンジ → データの
 * 範囲 → `[0, 1]` の順に決める。
 *
 * - データの範囲で決める側（ペンの指定も工学値レンジも無い側）だけ、`pen.limits` の有限の
 *   値も範囲に含める（下の側は最小、上の側は最大を広げる。データが無く管理線だけでも
 *   管理線の値で決める）。管理線が縦軸の外に出て見えなくなるのを防ぐ。ペンの指定・
 *   工学値レンジで決まる側は広げない。
 * - 結果が lo == hi なら幅を持たせる。ペンの指定で固定された側は動かさず、もう片方を
 *   広げる（両側とも指定・両側とも未指定なら上下に広げる）。
 * - lo > hi になったら（例: `yMin` だけ指定し、データがそれより下）、両側とも指定なら
 *   入れ替え、片側だけ指定なら指定の無い側を指定の側の外へ置き直す。
 */
export function resolveYDomain(pen: TrendPen, bins: readonly Bin[]): YDomain {
	const eng =
		pen.engRange && isNum(pen.engRange.min) && isNum(pen.engRange.max) ? pen.engRange : null;
	const data = dataExtent(bins);
	const fixedLo = isNum(pen.yMin);
	const fixedHi = isNum(pen.yMax);
	const limitValues = (pen.limits ?? []).map((l) => l.value).filter(isNum);
	let ext: [number, number] | null = data;
	if (limitValues.length > 0) {
		const lows = data ? [data[0], ...limitValues] : limitValues;
		const highs = data ? [data[1], ...limitValues] : limitValues;
		ext = [Math.min(...lows), Math.max(...highs)];
	}
	let lo = fixedLo ? (pen.yMin as number) : eng ? eng.min : ext ? ext[0] : 0;
	let hi = fixedHi ? (pen.yMax as number) : eng ? eng.max : ext ? ext[1] : 1;

	if (lo > hi) {
		if (fixedLo === fixedHi) [lo, hi] = [hi, lo];
		else if (fixedLo) hi = lo + padFor(lo) * 2;
		else lo = hi - padFor(hi) * 2;
	}
	if (lo === hi) {
		const pad = padFor(lo);
		if (fixedLo && !fixedHi) hi = lo + pad * 2;
		else if (fixedHi && !fixedLo) lo = hi - pad * 2;
		else {
			lo -= pad;
			hi += pad;
		}
	}
	return [lo, hi];
}

/** 帯（タイムチャート）で描く bit のペンか。帯のペンは縦軸・線・マーカー・管理線の持ち主にならない。 */
export function isBitBand(pen: Pick<TrendPen, 'bit' | 'bitStyle'>): boolean {
	return pen.bit === true && pen.bitStyle === 'band';
}

/** 左右の軸の目盛りを描くペン（その側の先頭。帯のペンは除く）の添字。無ければ null。 */
export function axisOwners(pens: readonly TrendPen[]): {
	left: number | null;
	right: number | null;
} {
	let left: number | null = null;
	let right: number | null = null;
	pens.forEach((p, i) => {
		if (isBitBand(p)) return;
		if ((p.axis ?? 'left') === 'right') {
			if (right === null) right = i;
		} else if (left === null) left = i;
	});
	return { left, right };
}

/** 色の枠の上限。 */
export const MAX_COLOR_SLOTS = 16;

/**
 * 色の枠の CSS 値。1〜8 は `var(--banto-chart-N)`、9〜16 は
 * `var(--bi-chart-N, var(--banto-chart-M))`（M = (N-1)%8+1。industrial のテーマが
 * `--bi-chart-N` を持たなくても色が出る）。範囲外は 1〜16 に丸め、非有限は 1。
 */
export function penColorVar(slot: number): string {
	const n = Number.isFinite(slot) ? Math.min(MAX_COLOR_SLOTS, Math.max(1, Math.round(slot))) : 1;
	if (n <= 8) return `var(--banto-chart-${n})`;
	return `var(--bi-chart-${n}, var(--banto-chart-${((n - 1) % 8) + 1}))`;
}

/** 欠測の表示。 */
export const MISSING_TEXT = '—';

/** bit のペンの凡例の範囲・要約の文字。 */
export const BIT_RANGE_TEXT = 'False / True';

/** bit の値の書式。0.5 以上を True とする。幅があって跨ぐ（区間の中で変わった）なら `False〜True`。欠測は「—」。 */
export function formatBitValue(
	min: number | null | undefined,
	max: number | null | undefined
): string {
	const lo = isNum(min) ? min : isNum(max) ? max : null;
	const hi = isNum(max) ? max : lo;
	if (lo === null || hi === null) return MISSING_TEXT;
	if (lo >= 0.5) return 'True';
	if (hi < 0.5) return 'False';
	return 'False〜True';
}

/**
 * 値の書式。null・非有限は「—」。`decimals` が 0〜10 の整数ならその桁で固定、
 * それ以外は有効数字 6 桁（末尾の 0 は落とす）。
 */
export function formatValue(v: number | null | undefined, decimals?: number | null): string {
	if (!isNum(v)) return MISSING_TEXT;
	if (isNum(decimals) && Number.isInteger(decimals) && decimals >= 0 && decimals <= 10) {
		return v.toFixed(decimals);
	}
	return String(Number(v.toPrecision(6)));
}
