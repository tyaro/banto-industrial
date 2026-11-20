/**
 * 管理線（HH・H・L・LL など）の位置と、越えた区間の矩形の計算。描画の層（SVG・後の Canvas）から
 * 切り離した計算だけ（設計書 §4.2 の 5）。
 *
 * 決め:
 * - 管理線はペンごとの属性（`TrendPen.limits`）で、そのペンの縦軸の範囲 `[lo, hi]` で置く。
 * - `upper` は値より上、`lower` は値より下を「越えた」とする。
 * - 非有限の値の線は捨てる。縦軸の範囲の外の線は引かない（`lines` に入れない）が、越えた区間
 *   には効く（上限が縦軸より上なら越える区間は無い。下限が縦軸より上なら全部が越えている）。
 * - 越えた区間は描画域 `[0, plotHeight]` で切り、高さ 0 は出さない。並びの順を保つ
 *   （後ろの線の色が前の上に重なる。H → HH の順に渡せば HH の色が勝つ）。
 * - 縦軸の範囲が不正（有限でない・lo >= hi）・`plotHeight` が 0 以下・`limits` が null なら空。
 */

import type { YDomain } from './pens';

/** 管理線 1 本。upper は値より上、lower は値より下を「越えた」とする。 */
export interface TrendLimit {
	value: number;
	side: 'upper' | 'lower';
	/** 線と、越えた区間の線の色（CSS の値。例 `var(--banto-warning)`）。 */
	colorVar: string;
	/** 線の名前（例 'HH'）。線の端の文字・`<title>`・要約に使う。 */
	label: string;
}

/** 描画域の中の管理線の位置（px。y は上から）。 */
export interface LimitLine {
	y: number;
	colorVar: string;
	label: string;
}

/** 越えた区間の矩形（px。y は上から）。 */
export interface LimitRect {
	y: number;
	height: number;
	colorVar: string;
}

export interface LimitLayout {
	lines: LimitLine[];
	rects: LimitRect[];
}

const isNum = (v: unknown): v is number => typeof v === 'number' && Number.isFinite(v);

export function limitLayout(
	limits: readonly TrendLimit[] | null | undefined,
	domain: YDomain,
	plotHeight: number
): LimitLayout {
	const out: LimitLayout = { lines: [], rects: [] };
	if (!limits || limits.length === 0 || !domain) return out;
	const [lo, hi] = domain;
	if (!isNum(lo) || !isNum(hi) || !(hi > lo) || !isNum(plotHeight) || plotHeight <= 0) return out;
	const yOf = (v: number) => plotHeight - ((v - lo) / (hi - lo)) * plotHeight;
	for (const l of limits) {
		if (!isNum(l.value)) continue;
		if (l.value >= lo && l.value <= hi) {
			out.lines.push({ y: yOf(l.value), colorVar: l.colorVar, label: l.label });
		}
		let top: number;
		let bottom: number;
		if (l.side === 'upper') {
			top = 0;
			bottom = yOf(Math.max(l.value, lo));
			if (l.value >= hi) continue;
		} else {
			top = yOf(Math.min(l.value, hi));
			bottom = plotHeight;
			if (l.value <= lo) continue;
		}
		const y = Math.max(0, top);
		const height = Math.min(plotHeight, bottom) - y;
		if (height > 0) out.rects.push({ y, height, colorVar: l.colorVar });
	}
	return out;
}
