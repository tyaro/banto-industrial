/**
 * 包絡（MIN/MAX）の SVG path。描画の層（SVG・後の Canvas）から切り離した計算だけ。
 *
 * 描き方（決め）:
 * - 区間の終わりは `min(tMs + binMs, 次の区間の tMs)`（`binEndMs`。収集周期が途中で変わって `binMs` より狭い
 *   間隔の点が並んでも、各点は次の点の時刻で打ち切る。最後の区間・次が不正（同じか小さい・非有限）なら `tMs + binMs`）。
 * - 区間の x は区間の実際の範囲 `[max(tMs, startMs), min(終わり, endMs))` の中央
 *   （範囲を渡さなければ `tMs + (終わり - tMs) / 2`）。表示範囲の端で欠ける区間の x が範囲の外に出てクリップされないようにする。
 * - 範囲を渡したとき、範囲と重ならない区間は描かずに飛ばす（線は切らず `prevT` も進めない。範囲外は前後に並ぶだけ）。
 * - 縦軸の自動範囲と「データなし」の判定も同じ区間だけを見る（`visibleBins`。範囲外の値に縦軸が引っ張られない）。
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
 * - `visibleBins` で絞った列を渡しても結果は変わらない（重なる区間は連続した部分列で、最後の区間の次は
 *   表示範囲の外にあり、終わりを `endMs` で切る結果は同じ）。
 */

import type { Bin } from './pens';

type Scale = (v: number) => number;

const fin = (v: number | null | undefined): v is number =>
	typeof v === 'number' && Number.isFinite(v);
const r2 = (v: number) => Math.round(v * 100) / 100;

/**
 * 区間の終わり（排他）。`min(tMs + binMs, 次の区間の tMs)`。`binMs` が正の有限でないときは `tMs`。
 * 次が無い・非有限・`tMs` 以下（異常）なら `tMs + binMs`。
 */
export function binEndMs(tMs: number, binMs: number, nextTMs?: number): number {
	if (!(fin(binMs) && binMs > 0)) return tMs;
	const e = tMs + binMs;
	return fin(nextTMs) && nextTMs > tMs && nextTMs < e ? nextTMs : e;
}

/** 区間が表示範囲 `[startMs, endMs)` と重ならないか（`envelopePath`・`visibleBins` の共通の判定）。 */
export function isOutsideRange(
	tMs: number,
	binMs: number,
	nextTMs: number | undefined,
	startMs: number | undefined,
	endMs: number | undefined
): boolean {
	const binOk = fin(binMs) && binMs > 0;
	if (fin(startMs) && (binOk ? binEndMs(tMs, binMs, nextTMs) <= startMs : tMs < startMs))
		return true;
	return fin(endMs) && tMs >= endMs;
}

/** 表示範囲と重なる区間だけ（`tMs` が非有限の区間は除く）。縦軸・空の判定・カーソルもこれを使う。 */
export function visibleBins(
	bins: readonly Bin[],
	binMs: number,
	range: { startMs: number; endMs: number }
): Bin[] {
	return bins.filter(
		(b, i) =>
			fin(b.tMs) && !isOutsideRange(b.tMs, binMs, bins[i + 1]?.tMs, range.startMs, range.endMs)
	);
}

/** 有限の min か max を持つ区間が 1 つでもあるか。 */
export function hasFiniteValue(bins: readonly Bin[]): boolean {
	return bins.some((b) => fin(b.min) || fin(b.max));
}

/**
 * 区間の x に使う時刻（区間の実際の範囲の中央）。`range` を渡すと表示範囲で切った範囲の中央。
 * `binMs` が正の有限でないときは `tMs`。`envelopePath` とマーカーの点（`core/style.ts`）で共用する。
 */
export function binCenterMs(
	tMs: number,
	binMs: number,
	nextTMs: number | undefined,
	range?: { startMs?: number; endMs?: number }
): number {
	if (!(fin(binMs) && binMs > 0)) return tMs;
	const s = range && fin(range.startMs) ? Math.max(tMs, range.startMs) : tMs;
	const end = binEndMs(tMs, binMs, nextTMs);
	const e = range && fin(range.endMs) ? Math.min(end, range.endMs) : end;
	return (s + e) / 2;
}

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
	const startMs = options?.startMs;
	const endMs = options?.endMs;
	for (let i = 0; i < bins.length; i++) {
		const b = bins[i];
		const nextT = bins[i + 1]?.tMs;
		if (options && fin(b.tMs) && isOutsideRange(b.tMs, binMs, nextT, startMs, endMs)) continue;
		const lo = fin(b.min) ? b.min : fin(b.max) ? b.max : null;
		const hi = fin(b.max) ? b.max : lo;
		const px = fin(b.tMs) ? x(binCenterMs(b.tMs, binMs, nextT, options)) : NaN;
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
