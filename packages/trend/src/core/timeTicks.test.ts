import { describe, expect, it } from 'vitest';
import { pickTimeStep, timeTicks } from './timeTicks';

// vitest.config.ts で TZ=Asia/Tokyo（UTC+9、夏時間なし）に固定している。
const jst = (y: number, mo: number, d: number, h = 0, mi = 0, s = 0) =>
	Date.UTC(y, mo - 1, d, h - 9, mi, s);
const MIN = 60_000;
const H = 60 * MIN;
const DAY = 24 * H;

describe('timeTicks', () => {
	it('時間帯が Asia/Tokyo に固定されている', () => {
		expect(new Date(jst(2026, 11, 20)).getHours()).toBe(0);
	});

	it('1 時間を 800px なら 10 分刻みで、ローカルの区切りに揃える', () => {
		const ticks = timeTicks(
			{ startMs: jst(2026, 11, 20, 9, 3), endMs: jst(2026, 11, 20, 10, 3) },
			800
		);
		expect(ticks.map((t) => t.label)).toEqual([
			'09:10',
			'09:20',
			'09:30',
			'09:40',
			'09:50',
			'10:00'
		]);
		expect(ticks[0].tMs).toBe(jst(2026, 11, 20, 9, 10));
	});

	it('ラベルが minLabelPx 以上離れる最小の刻みを選ぶ', () => {
		expect(pickTimeStep(H, 800, 80)).toBe(10 * MIN); // 10 分 = 133px
		expect(pickTimeStep(H, 800, 140)).toBe(15 * MIN);
		expect(pickTimeStep(60_000, 800, 80)).toBe(10_000);
		// どれも足りなければ 7 日
		expect(pickTimeStep(365 * DAY, 100, 80)).toBe(7 * DAY);
	});

	it('1 分未満の刻みは秒まで出す', () => {
		const ticks = timeTicks(
			{ startMs: jst(2026, 11, 20, 9, 0, 0), endMs: jst(2026, 11, 20, 9, 1, 0) },
			800
		);
		expect(ticks[0].label).toBe('09:00:00');
		expect(ticks[1].label).toBe('09:00:10');
	});

	it('日をまたぐ範囲では日付を付ける', () => {
		const ticks = timeTicks({ startMs: jst(2026, 11, 20, 20), endMs: jst(2026, 11, 21, 4) }, 400);
		expect(ticks.map((t) => t.label)).toEqual([
			'11/20 20:00',
			'11/20 22:00',
			'11/21 00:00',
			'11/21 02:00',
			'11/21 04:00'
		]);
	});

	it('日刻みはローカルの 0 時に揃え、M/D で出す', () => {
		const ticks = timeTicks({ startMs: jst(2026, 11, 1, 5), endMs: jst(2026, 11, 8, 5) }, 700);
		expect(ticks.map((t) => t.label)).toEqual([
			'11/2',
			'11/3',
			'11/4',
			'11/5',
			'11/6',
			'11/7',
			'11/8'
		]);
		expect(ticks[0].tMs).toBe(jst(2026, 11, 2));
	});

	it('7 日刻みは月曜の 0 時に揃える', () => {
		// 2026-11-02 は月曜
		const ticks = timeTicks({ startMs: jst(2026, 10, 31), endMs: jst(2026, 11, 30) }, 300);
		expect(ticks.map((t) => t.label)).toEqual(['11/2', '11/9', '11/16', '11/23', '11/30']);
		for (const t of ticks) expect(new Date(t.tMs).getDay()).toBe(1);
	});

	it('不正な入力では空', () => {
		expect(timeTicks({ startMs: 1, endMs: 0 }, 800)).toEqual([]);
		expect(timeTicks({ startMs: 0, endMs: H }, 0)).toEqual([]);
		expect(timeTicks({ startMs: 0, endMs: H }, NaN)).toEqual([]);
	});

	it('目盛りは範囲の中で昇順', () => {
		const r = { startMs: jst(2026, 11, 20, 1, 7), endMs: jst(2026, 11, 21, 9, 13) };
		const ticks = timeTicks(r, 1200);
		expect(ticks.length).toBeGreaterThan(2);
		for (let i = 0; i < ticks.length; i++) {
			expect(ticks[i].tMs).toBeGreaterThanOrEqual(r.startMs);
			expect(ticks[i].tMs).toBeLessThanOrEqual(r.endMs);
			if (i > 0) expect(ticks[i].tMs).toBeGreaterThan(ticks[i - 1].tMs);
		}
	});
});
