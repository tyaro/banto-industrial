/**
 * bit のペンの帯（タイムチャート）の置き場と中身の計算。描画の層（SVG・後の Canvas）から
 * 切り離した計算だけ（設計書 §4.2）。
 *
 * 決め:
 * - 帯になるのは `bit && bitStyle === 'band'` のペンだけ（`isBitBand`）。描画域の下から、ペンの
 *   並びの順に積む（1 本目が一番下）。帯の間は 2px。太さはペンの `bitBandHeight`（既定 16、
 *   8〜32 に丸める。非有限は既定）。
 * - 帯の合計（太さの和 + 間）が描画域の高さの半分を超えたら、太さも間も同じ割合で縮めて半分に
 *   収める。アナログの描画域の高さは「描画域の高さ − 帯の合計」で、アナログの線と帯は重ならない。
 *   帯が無ければ描画域の高さのまま。
 * - 帯の中身は、表示範囲と重なる区間ごとに（`envelopePath` と同じ範囲の判定。x の範囲は区間の
 *   実際の範囲 `[max(tMs, startMs), min(終わり, endMs))`）、min・max がともに 0.5 以上なら True
 *   （濃い塗り）、min < 0.5 <= max（区間の中で変わった）なら薄い塗り、ともに 0.5 未満の False と
 *   欠測は塗らない。時間でつながる同じ状態の区間は 1 つの矩形にまとめる（x・幅は px）。
 */

import { binEndMs, isOutsideRange } from './geometry';
import { isBitBand, type Bin, type TrendPen } from './pens';

/** 帯の太さの既定・下限・上限（px）と、帯の間（px）。 */
export const BIT_BAND_DEFAULT = 16;
export const BIT_BAND_MIN = 8;
export const BIT_BAND_MAX = 32;
export const BIT_BAND_GAP = 2;

/** 帯の合計が描画域の高さに占める上限。 */
const MAX_SHARE = 0.5;

/** 帯 1 本の位置（px。y は描画域の上から）。`penIndex` は `pens` の添字。 */
export interface BitBandSlot {
	penIndex: number;
	y: number;
	height: number;
}

export interface BitBandLayout {
	bands: BitBandSlot[];
	/** アナログの線を描く縦の長さ（px）。 */
	analogHeight: number;
}

const fin = (v: number | null | undefined): v is number =>
	typeof v === 'number' && Number.isFinite(v);

/** 帯の太さ。未指定・非有限は 16、8〜32 に丸める。 */
export function resolveBitBandHeight(h: number | null | undefined): number {
	if (!fin(h)) return BIT_BAND_DEFAULT;
	return Math.min(BIT_BAND_MAX, Math.max(BIT_BAND_MIN, h));
}

export function bitBandLayout(pens: readonly TrendPen[], plotHeight: number): BitBandLayout {
	const h = fin(plotHeight) && plotHeight > 0 ? plotHeight : 0;
	const idx: number[] = [];
	pens.forEach((p, i) => {
		if (isBitBand(p)) idx.push(i);
	});
	if (idx.length === 0 || h === 0) return { bands: [], analogHeight: h };
	const heights = idx.map((i) => resolveBitBandHeight(pens[i].bitBandHeight));
	const raw = heights.reduce((a, b) => a + b, 0) + BIT_BAND_GAP * (idx.length - 1);
	const k = raw > h * MAX_SHARE ? (h * MAX_SHARE) / raw : 1;
	const gap = BIT_BAND_GAP * k;
	const bands: BitBandSlot[] = [];
	let bottom = h;
	idx.forEach((penIndex, n) => {
		const height = heights[n] * k;
		bands.push({ penIndex, y: bottom - height, height });
		bottom -= height + gap;
	});
	return { bands, analogHeight: h - raw * k };
}

/** 帯の塗り 1 つ。`state` の `mixed` は区間の中で変わったもの（薄く塗る）。 */
export interface BitSpan {
	x: number;
	width: number;
	state: 'true' | 'mixed';
}

/**
 * 帯の塗りの並び。`range` は有効範囲（取得範囲 ∩ 表示範囲）、`x` は時刻 → px の scale。
 * `bins` は `tMs` の昇順（契約）。
 */
export function bitSpans(
	bins: readonly Bin[],
	binMs: number,
	x: (tMs: number) => number,
	range: { startMs: number; endMs: number }
): BitSpan[] {
	const out: { s: number; e: number; state: 'true' | 'mixed' }[] = [];
	for (let i = 0; i < bins.length; i++) {
		const b = bins[i];
		const next = bins[i + 1]?.tMs;
		if (!fin(b.tMs) || isOutsideRange(b.tMs, binMs, next, range.startMs, range.endMs)) continue;
		const lo = fin(b.min) ? b.min : fin(b.max) ? b.max : null;
		const hi = fin(b.max) ? b.max : lo;
		if (lo === null || hi === null) continue;
		const state = lo >= 0.5 ? 'true' : hi >= 0.5 ? 'mixed' : null;
		if (state === null) continue;
		const s = Math.max(b.tMs, range.startMs);
		const e = Math.min(binEndMs(b.tMs, binMs, next), range.endMs);
		if (!(e > s)) continue;
		const prev = out[out.length - 1];
		if (prev && prev.state === state && prev.e >= s) prev.e = Math.max(prev.e, e);
		else out.push({ s, e, state });
	}
	const r2 = (v: number) => Math.round(v * 100) / 100;
	return out
		.map((o) => {
			const x0 = x(o.s);
			return { x: r2(x0), width: r2(x(o.e) - x0), state: o.state };
		})
		.filter((o) => Number.isFinite(o.x) && Number.isFinite(o.width) && o.width > 0);
}
