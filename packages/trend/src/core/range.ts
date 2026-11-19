/**
 * 表示範囲（時刻の窓）の純粋な関数。
 *
 * 範囲は UTC のミリ秒の半開区間 `[startMs, endMs)`。拡大・縮小・パン・前へ・次へ・
 * 期間の候補・追従は、すべて範囲を受けて新しい範囲を返すだけで、状態は持たない
 * （範囲は利用側が持つ。`TrendChart` は制御された部品）。
 *
 * 不正な入力の扱い（決め）:
 * - 範囲を作る関数（`presetRange`・`followRange`）は、時刻が非有限なら `RangeError`
 *   を投げる（時計の値の誤りは呼び出し側の不具合なので黙って直さない）。幅は
 *   `[MIN_RANGE_MS, MAX_RANGE_MS]` に丸め、非有限の幅は `MIN_RANGE_MS` にする。
 * - 範囲を変える関数（`zoomRange`・`panRange`・`shiftPage`）は、範囲が不正（非有限・
 *   逆順・幅 0）か、引数が非有限なら、受けた範囲をそのまま返す（ホイールやポインタの
 *   値から来るので、例外で画面を止めない）。
 * - 結果の端はミリ秒の整数に丸める。
 */

/** 表示範囲。UTC のミリ秒で `[startMs, endMs)`。 */
export interface TimeRange {
	startMs: number;
	endMs: number;
}

const SECOND = 1000;
const MINUTE = 60 * SECOND;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

/** 表示範囲の幅の上限（31 日。履歴の口の上限に合わせる）。 */
export const MAX_RANGE_MS = 31 * DAY;
/** 表示範囲の幅の下限（1 分）。 */
export const MIN_RANGE_MS = MINUTE;

/** 期間の候補（設計書 §4.2）。 */
export interface RangePreset {
	widthMs: number;
	label: string;
}

export const RANGE_PRESETS: readonly RangePreset[] = Object.freeze([
	{ widthMs: 10 * MINUTE, label: '10 分' },
	{ widthMs: 30 * MINUTE, label: '30 分' },
	{ widthMs: HOUR, label: '1 時間' },
	{ widthMs: 2 * HOUR, label: '2 時間' },
	{ widthMs: 4 * HOUR, label: '4 時間' },
	{ widthMs: 8 * HOUR, label: '8 時間' },
	{ widthMs: 12 * HOUR, label: '12 時間' },
	{ widthMs: DAY, label: '1 日' },
	{ widthMs: 7 * DAY, label: '7 日' },
	{ widthMs: 31 * DAY, label: '31 日' }
]);

/** 端が有限で、終わりが始まりより後なら true。 */
export function isValidRange(range: TimeRange): boolean {
	return (
		Number.isFinite(range.startMs) && Number.isFinite(range.endMs) && range.endMs > range.startMs
	);
}

/** 幅を `[MIN_RANGE_MS, MAX_RANGE_MS]` に丸める。非有限は `MIN_RANGE_MS`。 */
export function clampWidth(widthMs: number): number {
	if (!Number.isFinite(widthMs)) return MIN_RANGE_MS;
	return Math.min(MAX_RANGE_MS, Math.max(MIN_RANGE_MS, Math.round(widthMs)));
}

function requireFinite(name: string, v: number): void {
	if (!Number.isFinite(v)) throw new RangeError(`${name} が有限の数ではない: ${v}`);
}

/** 終わりを `endMs` にした幅 `widthMs` の範囲。 */
export function presetRange(widthMs: number, endMs: number): TimeRange {
	requireFinite('endMs', endMs);
	const end = Math.round(endMs);
	return { startMs: end - clampWidth(widthMs), endMs: end };
}

/** 右端を今にした範囲（追従）。 */
export function followRange(widthMs: number, nowMs: number): TimeRange {
	return presetRange(widthMs, nowMs);
}

/**
 * 拡大・縮小。`factor` < 1 で拡大（幅が狭まる）、> 1 で縮小。`anchorMs` の時刻が
 * 範囲の中で同じ相対位置に留まる（範囲外の anchor は端に寄せる）。幅は
 * `[MIN_RANGE_MS, MAX_RANGE_MS]` に丸める。今を越えないための丸めはしない
 * （必要なら結果を `panRange(r, 0, now)` に通す）。
 */
export function zoomRange(range: TimeRange, factor: number, anchorMs: number): TimeRange {
	if (!isValidRange(range) || !Number.isFinite(factor) || factor <= 0) return range;
	if (!Number.isFinite(anchorMs)) return range;
	const width = range.endMs - range.startMs;
	const ratio = Math.min(1, Math.max(0, (anchorMs - range.startMs) / width));
	const anchor = range.startMs + ratio * width;
	const next = clampWidth(width * factor);
	const startMs = Math.round(anchor - ratio * next);
	return { startMs, endMs: startMs + next };
}

/** 横のパン。終わりが `nowMs` を越えないよう丸める。幅は保つ。 */
export function panRange(range: TimeRange, deltaMs: number, nowMs: number): TimeRange {
	if (!isValidRange(range) || !Number.isFinite(deltaMs) || !Number.isFinite(nowMs)) return range;
	const width = range.endMs - range.startMs;
	let endMs = Math.round(range.endMs + deltaMs);
	const now = Math.round(nowMs);
	if (endMs > now) endMs = now;
	return { startMs: endMs - width, endMs };
}

/** 前へ・次へ（幅 1 つ分）。次へは今で止まる。 */
export function shiftPage(range: TimeRange, direction: 'prev' | 'next', nowMs: number): TimeRange {
	if (!isValidRange(range)) return range;
	const width = range.endMs - range.startMs;
	return panRange(range, direction === 'prev' ? -width : width, nowMs);
}

/** 「次へ」で動けるか（終わりが今より前か）。 */
export function canShiftNext(range: TimeRange, nowMs: number): boolean {
	return isValidRange(range) && Number.isFinite(nowMs) && range.endMs < nowMs;
}

/**
 * 右端が今に付いているか（追従の判定）。終わりが `nowMs - toleranceMs` 以上なら true
 * （今より先に出ていても true）。
 */
export function isAtNow(range: TimeRange, nowMs: number, toleranceMs: number): boolean {
	if (!isValidRange(range) || !Number.isFinite(nowMs)) return false;
	const tol = Number.isFinite(toleranceMs) && toleranceMs > 0 ? toleranceMs : 0;
	return range.endMs >= nowMs - tol;
}
