/**
 * 包絡（MIN/MAX）の SVG path。描画の層（SVG・後の Canvas）から切り離した計算だけ。
 *
 * 描き方（決め）:
 * - 区間の x は区間の実際の範囲 `[max(tMs, startMs), min(tMs + binMs, endMs))` の中央
 *   （範囲を渡さなければ `tMs + binMs / 2`）。表示範囲の端で欠ける区間の x が範囲の外に出てクリップされないようにする。
 * - 範囲を渡したとき、範囲と重ならない区間は描かずに飛ばす（線は切らず `prevT` も進めない。範囲外は前後に並ぶだけ）。
 * - 各区間は min と max の縦線になり、隣の区間へは**前の点に近い方の端**から入る。
 *   1 px に 1 区間なら縦線が並んで帯に見え、拡大して区間が広いときは折れ線に見える。
 * - 値が 1 つ（min == max、または片方だけ有限）の区間は点 1 つ。
 * - 欠測（両方 null・非有限、座標が非有限）の区間で線を切り、次は `M` から始め直す。
 * - 前に描いた区間の `tMs` から次の区間の `tMs` までが `binMs * 1.5` を超えたら（区間の列の穴）欠測と同じく切る。
 *   `binMs` は利用側の計算とサーバーの区間の幅で端数が少しずれうるので、ぴったり一致ではなく 1.5 倍の余裕で判定する。
 *   `binMs` が正の有限でないときはこの判定をしない。
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
export function envelopePath(
	bins: readonly Bin[],
	binMs: number,
	x: Scale,
	y: Scale,
	options?: { startMs?: number; endMs?: number }
): string {
	const parts: string[] = [];
	let segLen = 0;
	let segStart = '';
	let prevY: number | null = null;
	let prevT: number | null = null;

	const endSegment = () => {
		if (segLen === 1) parts.push(`L${segStart}`);
		segLen = 0;
		prevY = null;
		prevT = null;
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

	const binOk = fin(binMs) && binMs > 0;
	const half = binOk ? binMs / 2 : 0;
	const startMs = options?.startMs;
	const endMs = options?.endMs;
	const centerOf = (tMs: number): number => {
		if (!binOk) return tMs;
		const s = fin(startMs) ? Math.max(tMs, startMs) : tMs;
		const e = fin(endMs) ? Math.min(tMs + binMs, endMs) : tMs + binMs;
		return (s + e) / 2;
	};
	// 表示範囲と重ならない区間か（範囲を渡したときだけ使う）。
	const outside = (tMs: number): boolean => {
		if (fin(startMs) && (binOk ? tMs + binMs <= startMs : tMs < startMs)) return true;
		return fin(endMs) && tMs >= endMs;
	};
	for (const b of bins) {
		if (options && fin(b.tMs) && outside(b.tMs)) continue;
		const lo = fin(b.min) ? b.min : fin(b.max) ? b.max : null;
		const hi = fin(b.max) ? b.max : lo;
		const px = fin(b.tMs) ? x(options ? centerOf(b.tMs) : b.tMs + half) : NaN;
		if (lo === null || hi === null || !Number.isFinite(px)) {
			endSegment();
			continue;
		}
		if (binOk && prevT !== null && b.tMs - prevT > binMs * 1.5) endSegment();
		prevT = b.tMs;
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
