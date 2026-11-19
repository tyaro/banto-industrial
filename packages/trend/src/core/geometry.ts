/**
 * 包絡（MIN/MAX）の SVG path。描画の層（SVG・後の Canvas）から切り離した計算だけ。
 *
 * 描き方（決め）:
 * - 区間の x は区間の中央（`tMs + binMs / 2`）。
 * - 各区間は min と max の縦線になり、隣の区間へは**前の点に近い方の端**から入る。
 *   1 px に 1 区間なら縦線が並んで帯に見え、拡大して区間が広いときは折れ線に見える。
 * - 値が 1 つ（min == max、または片方だけ有限）の区間は点 1 つ。
 * - 欠測（両方 null・非有限、座標が非有限）の区間で線を切り、次は `M` から始め直す。
 * - 1 点だけの切れ端は同じ点への `L` を足す（`stroke-linecap: round` で点として見える）。
 * - 座標は小数 2 桁に丸める。
 * - `bins` は `tMs` の昇順であること（契約）。並べ替えはしない。
 */

import type { Bin } from './pens';

type Scale = (v: number) => number;

const fin = (v: number | null | undefined): v is number =>
	typeof v === 'number' && Number.isFinite(v);
const r2 = (v: number) => Math.round(v * 100) / 100;

/** 包絡の path の `d`。点が無ければ空文字。 */
export function envelopePath(bins: readonly Bin[], binMs: number, x: Scale, y: Scale): string {
	const parts: string[] = [];
	let segLen = 0;
	let segStart = '';
	let prevY: number | null = null;

	const endSegment = () => {
		if (segLen === 1) parts.push(`L${segStart}`);
		segLen = 0;
		prevY = null;
	};
	const push = (px: number, py: number) => {
		const pt = `${r2(px)},${r2(py)}`;
		if (segLen === 0) {
			parts.push(`M${pt}`);
			segStart = pt;
		} else {
			parts.push(`L${pt}`);
		}
		segLen++;
		prevY = py;
	};

	const half = fin(binMs) && binMs > 0 ? binMs / 2 : 0;
	for (const b of bins) {
		const lo = fin(b.min) ? b.min : fin(b.max) ? b.max : null;
		const hi = fin(b.max) ? b.max : lo;
		const px = fin(b.tMs) ? x(b.tMs + half) : NaN;
		if (lo === null || hi === null || !Number.isFinite(px)) {
			endSegment();
			continue;
		}
		const yLo = y(Math.min(lo, hi));
		const yHi = y(Math.max(lo, hi));
		if (!Number.isFinite(yLo) || !Number.isFinite(yHi)) {
			endSegment();
			continue;
		}
		if (yLo === yHi) {
			push(px, yLo);
			continue;
		}
		const p = prevY;
		if (p !== null && Math.abs(yHi - p) < Math.abs(yLo - p)) {
			push(px, yHi);
			push(px, yLo);
		} else {
			push(px, yLo);
			push(px, yHi);
		}
	}
	endSegment();
	return parts.join('');
}
