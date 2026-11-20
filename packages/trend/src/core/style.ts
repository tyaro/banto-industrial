/**
 * ペンの線種・線幅・マーカーの計算。描画の層（SVG・後の Canvas）から切り離した純粋な関数だけ。
 *
 * 決め（設計書 §5。値の名前は表示グループのペンの属性 `PenStyle` と同じ）:
 * - 未指定・null・不正な値は既定（実線・1.5 px・マーカーなし・4 px）。範囲外は範囲へ丸める
 *   （線幅 1〜4、マーカーの大きさ 2〜8 の整数）。
 * - 破線は `stroke-linecap: round` 前提。ダッシュは両端に w/2 ずつ伸びるので、長さ 0 のダッシュが
 *   直径 w の点になる。パターンは線幅 w に比例（dashed = `5w 3w`、dashDot = `5w 2.5w 0 2.5w`、
 *   dashDotDot = `5w 2.5w 0 2.5w 0 2.5w`）。線なし（none）は線を描かない（マーカーだけを描ける）。
 * - マーカーの点は区間ごとに、値が 1 つ、または min と max の y の差が size 未満なら 1 点（y は中点）、
 *   それ以外は min と max の 2 点。x は `envelopePath` と同じ区間の中央。欠測は飛ばす。
 *   前に印を付けた区間の x から `max(size * 2.5, 8)` px 未満の区間は飛ばす（印が詰まらない）。
 * - マーカーの形は 1 本の path にまとめる（丸は arc 2 本、三角は上向きで外接の高さ size、
 *   ひし形は対角 size）。座標は小数 2 桁。
 */

import { binCenterMs, isOutsideRange } from './geometry';
import type { Bin, LineStyle, MarkerKind } from './pens';

const fin = (v: unknown): v is number => typeof v === 'number' && Number.isFinite(v);
const r2 = (v: number) => Math.round(v * 100) / 100;

export const DEFAULT_LINE_WIDTH = 1.5;
export const DEFAULT_MARKER_SIZE = 4;

const LINE_STYLES: readonly LineStyle[] = ['solid', 'dashed', 'dashDot', 'dashDotDot', 'none'];
const MARKER_KINDS: readonly MarkerKind[] = ['none', 'circle', 'square', 'triangle', 'diamond'];

export function resolveLineStyle(v: unknown): LineStyle {
	return LINE_STYLES.includes(v as LineStyle) ? (v as LineStyle) : 'solid';
}

export function resolveMarkerKind(v: unknown): MarkerKind {
	return MARKER_KINDS.includes(v as MarkerKind) ? (v as MarkerKind) : 'none';
}

/** 線幅（px）。非有限・null は 1.5、範囲外は 1〜4 に丸める。 */
export function resolveLineWidth(v: unknown): number {
	return fin(v) ? Math.min(4, Math.max(1, v)) : DEFAULT_LINE_WIDTH;
}

/** マーカーの大きさ（px）。非有限・null は 4、範囲外は 2〜8 に丸め、整数にする。 */
export function resolveMarkerSize(v: unknown): number {
	return fin(v) ? Math.min(8, Math.max(2, Math.round(v))) : DEFAULT_MARKER_SIZE;
}

/** `stroke-dasharray` の値。実線・線なしは null。 */
export function dashArray(style: unknown, width: unknown): string | null {
	const s = resolveLineStyle(style);
	if (s === 'solid' || s === 'none') return null;
	const w = resolveLineWidth(width);
	const f = (k: number) => String(r2(w * k));
	if (s === 'dashed') return `${f(5)} ${f(3)}`;
	if (s === 'dashDot') return `${f(5)} ${f(2.5)} 0 ${f(2.5)}`;
	return `${f(5)} ${f(2.5)} 0 ${f(2.5)} 0 ${f(2.5)}`;
}

export interface MarkerPoint {
	x: number;
	y: number;
}

/** マーカーを置く点。範囲の判定・x は `envelopePath` と同じ。 */
export function markerPoints(
	bins: readonly Bin[],
	binMs: number,
	x: (v: number) => number,
	y: (v: number) => number,
	options: { startMs?: number; endMs?: number } | undefined,
	size: unknown
): MarkerPoint[] {
	const sz = resolveMarkerSize(size);
	const minGap = Math.max(sz * 2.5, 8);
	const out: MarkerPoint[] = [];
	let lastX: number | null = null;
	for (let i = 0; i < bins.length; i++) {
		const b = bins[i];
		if (!fin(b.tMs)) continue;
		const nextT = bins[i + 1]?.tMs;
		if (options && isOutsideRange(b.tMs, binMs, nextT, options.startMs, options.endMs)) continue;
		const lo = fin(b.min) ? b.min : fin(b.max) ? b.max : null;
		const hi = fin(b.max) ? b.max : lo;
		if (lo === null || hi === null) continue;
		const px = x(binCenterMs(b.tMs, binMs, nextT, options));
		const yLo = y(Math.min(lo, hi));
		const yHi = y(Math.max(lo, hi));
		if (!Number.isFinite(px) || !Number.isFinite(yLo) || !Number.isFinite(yHi)) continue;
		if (lastX !== null && Math.abs(px - lastX) < minGap) continue;
		lastX = px;
		if (Math.abs(yLo - yHi) < sz) out.push({ x: r2(px), y: r2((yLo + yHi) / 2) });
		else out.push({ x: r2(px), y: r2(yLo) }, { x: r2(px), y: r2(yHi) });
	}
	return out;
}

/** マーカーの形の path の `d`（全部の点を 1 本に）。'none'・点なしは空文字。 */
export function markerPath(points: readonly MarkerPoint[], kind: unknown, size: unknown): string {
	const k = resolveMarkerKind(kind);
	if (k === 'none' || points.length === 0) return '';
	const sz = resolveMarkerSize(size);
	const h = sz / 2;
	const parts: string[] = [];
	for (const p of points) {
		const { x, y } = p;
		if (k === 'circle') {
			parts.push(`M${r2(x - h)},${r2(y)}a${h},${h} 0 1,0 ${sz},0a${h},${h} 0 1,0 ${-sz},0Z`);
		} else if (k === 'square') {
			parts.push(`M${r2(x - h)},${r2(y - h)}h${sz}v${sz}h${-sz}Z`);
		} else if (k === 'triangle') {
			parts.push(`M${r2(x)},${r2(y - h)}L${r2(x + h)},${r2(y + h)}L${r2(x - h)},${r2(y + h)}Z`);
		} else {
			parts.push(
				`M${r2(x)},${r2(y - h)}L${r2(x + h)},${r2(y)}L${r2(x)},${r2(y + h)}L${r2(x - h)},${r2(y)}Z`
			);
		}
	}
	return parts.join('');
}
