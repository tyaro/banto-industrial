/**
 * しきい値などの帯（横の帯）の位置の計算。描画の層（SVG・後の Canvas）から切り離した計算だけ。
 *
 * 帯は「どのペンの縦軸で描くか」（`penId`）と、値の範囲の並び（`items`）で受ける。
 * ChronoGazer のしきい値（HH・H・L・LL）などの利用側の型はここに入れず、利用側が
 * この汎用の形に変換して渡す（設計書 §4.1）。管理線（設計書 §4.2 の 5、PR 7）に
 * 置き換えるときに見直す。
 *
 * 決め:
 * - `from`・`to` が `null` の側は開いた帯（縦軸の端まで伸ばす）。
 * - `from` > `to` なら入れ替える。非有限の数（NaN・±Infinity）は不正として、その帯を出さない。
 * - 帯はペンの縦軸の範囲 `[lo, hi]` で切り取る。縦軸の外の帯・切り取って幅 0 の帯は出さない。
 * - 帯を描くペンが居ない（`penId` がペンの組に無い）・`bands` が null なら空。
 */

import type { TrendPen, YDomain } from './pens';

/** 帯 1 つ。`from`・`to` は値（縦軸の単位）。`null` は開いた側。 */
export interface TrendBand {
	from: number | null;
	to: number | null;
	/** 塗りの色（CSS の値。例 `var(--banto-warning)`）。 */
	colorVar: string;
	/** 帯の名前（`<title>` などに使う）。 */
	label: string;
}

/** 帯の組。`penId` のペンの縦軸の範囲で描く。 */
export interface TrendBands {
	penId: string;
	items: readonly TrendBand[];
}

/** 描画域の中の帯の位置（px。y は上から）。 */
export interface BandRect {
	y: number;
	height: number;
	colorVar: string;
	label: string;
}

const isNum = (v: unknown): v is number => typeof v === 'number' && Number.isFinite(v);

/**
 * 帯の位置。`domain` はそのペンの縦軸の範囲（`resolveYDomain` の結果）、`plotHeight` は
 * 描画域の高さ（px）。範囲の下端が描画域の下（y = plotHeight）、上端が上（y = 0）。
 */
export function bandRects(
	items: readonly TrendBand[],
	domain: YDomain,
	plotHeight: number
): BandRect[] {
	const [lo, hi] = domain;
	if (!isNum(lo) || !isNum(hi) || !(hi > lo) || !isNum(plotHeight) || plotHeight <= 0) return [];
	const yOf = (v: number) => plotHeight - ((v - lo) / (hi - lo)) * plotHeight;
	const out: BandRect[] = [];
	for (const b of items) {
		if ((b.from !== null && !isNum(b.from)) || (b.to !== null && !isNum(b.to))) continue;
		let a = b.from ?? -Infinity;
		let z = b.to ?? Infinity;
		if (a > z) [a, z] = [z, a];
		const bottom = Math.max(a, lo);
		const top = Math.min(z, hi);
		if (!(top > bottom)) continue;
		const y = yOf(top);
		const height = yOf(bottom) - y;
		if (!(height > 0)) continue;
		out.push({ y, height, colorVar: b.colorVar, label: b.label });
	}
	return out;
}

/**
 * `bands` の帯を、そのペンの縦軸で置いた位置。`domains[i]` は `pens[i]` の縦軸の範囲。
 * `bands` が null・ペンが居ないなら空。
 */
export function bandLayout(
	bands: TrendBands | null | undefined,
	pens: readonly TrendPen[],
	domains: readonly YDomain[],
	plotHeight: number
): BandRect[] {
	if (!bands) return [];
	const index = pens.findIndex((p) => p.id === bands.penId);
	if (index < 0 || !domains[index]) return [];
	return bandRects(bands.items, domains[index], plotHeight);
}
