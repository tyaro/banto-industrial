/**
 * 時刻の軸の目盛り。
 *
 * 刻みは候補（1 秒〜7 日）から、隣のラベルが `minLabelPx` 以上離れる最小のものを選び、
 * **ローカル時刻**の区切りに揃える（1 日未満の刻みはその日のローカルの 0 時から数え、
 * 1 日はローカルの 0 時、7 日は月曜のローカルの 0 時）。日ごとに 0 時から数え直すので、
 * 夏時間のある時間帯でも各日の目盛りは 0 時から始まる。
 *
 * ラベル: 1 分未満の刻みは `HH:mm:ss`、1 日未満は `HH:mm`、範囲がローカルの日を
 * またぐときは頭に `M/D ` を付ける。1 日以上の刻みは `M/D`。
 */

import { isValidRange, type TimeRange } from './range';

export interface TimeTick {
	tMs: number;
	label: string;
}

const SECOND = 1000;
const MINUTE = 60 * SECOND;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

/** 刻みの候補（小さい順）。1 日未満はどれも 1 日を割り切る。 */
export const TIME_TICK_STEPS_MS: readonly number[] = Object.freeze([
	SECOND,
	5 * SECOND,
	10 * SECOND,
	30 * SECOND,
	MINUTE,
	5 * MINUTE,
	10 * MINUTE,
	15 * MINUTE,
	30 * MINUTE,
	HOUR,
	2 * HOUR,
	3 * HOUR,
	6 * HOUR,
	12 * HOUR,
	DAY,
	7 * DAY
]);

/** 目盛りの数の上限（壊れた入力で巨大な配列を作らない）。 */
const MAX_TICKS = 500;

/** ラベルが `minLabelPx` 以上離れる最小の刻み。どれも足りなければ最大の刻み。 */
export function pickTimeStep(widthMs: number, plotWidthPx: number, minLabelPx = 80): number {
	const pxPerMs = plotWidthPx / widthMs;
	for (const step of TIME_TICK_STEPS_MS) {
		if (step * pxPerMs >= minLabelPx) return step;
	}
	return TIME_TICK_STEPS_MS[TIME_TICK_STEPS_MS.length - 1];
}

function localMidnight(tMs: number): number {
	const d = new Date(tMs);
	d.setHours(0, 0, 0, 0);
	return d.getTime();
}

/** ローカルの翌日の 0 時（夏時間で 23・25 時間の日も正しく進む）。 */
function nextLocalMidnight(midnightMs: number, days = 1): number {
	const d = new Date(midnightMs);
	d.setDate(d.getDate() + days);
	d.setHours(0, 0, 0, 0);
	return d.getTime();
}

const pad2 = (n: number) => String(n).padStart(2, '0');

function formatTick(tMs: number, stepMs: number, withDate: boolean): string {
	const d = new Date(tMs);
	const date = `${d.getMonth() + 1}/${d.getDate()}`;
	if (stepMs >= DAY) return date;
	let time = `${pad2(d.getHours())}:${pad2(d.getMinutes())}`;
	if (stepMs < MINUTE) time += `:${pad2(d.getSeconds())}`;
	return withDate ? `${date} ${time}` : time;
}

/** 範囲の目盛り。範囲が不正か、描画域の幅が正でなければ空。 */
export function timeTicks(range: TimeRange, plotWidthPx: number, minLabelPx = 80): TimeTick[] {
	if (!isValidRange(range) || !Number.isFinite(plotWidthPx) || plotWidthPx <= 0) return [];
	const minPx = Number.isFinite(minLabelPx) && minLabelPx > 0 ? minLabelPx : 80;
	const { startMs, endMs } = range;
	const step = pickTimeStep(endMs - startMs, plotWidthPx, minPx);
	const withDate = localMidnight(startMs) !== localMidnight(endMs - 1);

	const out: number[] = [];
	if (step < DAY) {
		let day = localMidnight(startMs);
		while (day <= endMs && out.length < MAX_TICKS) {
			const next = nextLocalMidnight(day);
			const first = Math.max(day, day + Math.ceil((startMs - day) / step) * step);
			for (let t = first; t < next && t <= endMs && out.length < MAX_TICKS; t += step) {
				if (t >= startMs) out.push(t);
			}
			day = next;
		}
	} else {
		const days = Math.round(step / DAY);
		let t = localMidnight(startMs);
		if (days === 7) {
			// 月曜に揃える（getDay: 0=日曜）。
			const dow = new Date(t).getDay();
			t = nextLocalMidnight(t, -((dow + 6) % 7));
		}
		while (t < startMs) t = nextLocalMidnight(t, days);
		while (t <= endMs && out.length < MAX_TICKS) {
			out.push(t);
			t = nextLocalMidnight(t, days);
		}
	}
	return out.map((tMs) => ({ tMs, label: formatTick(tMs, step, withDate) }));
}
