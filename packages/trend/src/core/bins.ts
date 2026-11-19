/**
 * 区間（bins）の数と幅。区間の数は描画域の横幅から決める（1 px に 1 区間前後、
 * 上限 2000。設計書 §4.3）。
 */

import { isValidRange, type TimeRange } from './range';

/** 1 回の読み出しの区間の数の上限。 */
export const MAX_BINS = 2000;

/** 描画域の幅（px）から区間の数。1〜`MAX_BINS`。非有限・0 以下は 1。 */
export function binCount(plotWidthPx: number): number {
	if (!Number.isFinite(plotWidthPx) || plotWidthPx < 1) return 1;
	return Math.min(MAX_BINS, Math.max(1, Math.round(plotWidthPx)));
}

/**
 * 範囲を `bins` 個に分けたときの区間の幅（ミリ秒の整数、切り上げ。`bins` 個で範囲を
 * 覆う）。範囲が不正なら 1、`bins` が不正なら 1 区間とみなす。
 */
export function binMs(range: TimeRange, bins: number): number {
	if (!isValidRange(range)) return 1;
	const n = Number.isFinite(bins) && bins >= 1 ? Math.floor(bins) : 1;
	return Math.max(1, Math.ceil((range.endMs - range.startMs) / n));
}
