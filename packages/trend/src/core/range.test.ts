import { describe, expect, it } from 'vitest';
import {
	canShiftNext,
	clampWidth,
	followRange,
	isAtNow,
	isValidRange,
	MAX_RANGE_MS,
	MIN_RANGE_MS,
	panRange,
	presetRange,
	RANGE_PRESETS,
	shiftPage,
	zoomRange
} from './range';

const H = 3_600_000;
const NOW = Date.UTC(2026, 10, 20, 3, 0, 0);

describe('RANGE_PRESETS', () => {
	it('10 分〜31 日の 10 個で、幅は上限・下限の中', () => {
		expect(RANGE_PRESETS.map((p) => p.label)).toEqual([
			'10 分',
			'30 分',
			'1 時間',
			'2 時間',
			'4 時間',
			'8 時間',
			'12 時間',
			'1 日',
			'7 日',
			'31 日'
		]);
		for (const p of RANGE_PRESETS) {
			expect(p.widthMs).toBeGreaterThanOrEqual(MIN_RANGE_MS);
			expect(p.widthMs).toBeLessThanOrEqual(MAX_RANGE_MS);
		}
		expect(RANGE_PRESETS.at(-1)?.widthMs).toBe(MAX_RANGE_MS);
	});
});

describe('presetRange / followRange', () => {
	it('終わりを指定の時刻にする', () => {
		expect(presetRange(H, NOW)).toEqual({ startMs: NOW - H, endMs: NOW });
		expect(followRange(H, NOW)).toEqual({ startMs: NOW - H, endMs: NOW });
	});
	it('幅を上限・下限に丸める。非有限の幅は下限', () => {
		expect(presetRange(10, NOW).startMs).toBe(NOW - MIN_RANGE_MS);
		expect(presetRange(MAX_RANGE_MS * 2, NOW).startMs).toBe(NOW - MAX_RANGE_MS);
		expect(presetRange(NaN, NOW).startMs).toBe(NOW - MIN_RANGE_MS);
		expect(clampWidth(Infinity)).toBe(MIN_RANGE_MS);
	});
	it('時刻が非有限なら RangeError', () => {
		expect(() => presetRange(H, NaN)).toThrow(RangeError);
		expect(() => followRange(H, Infinity)).toThrow(RangeError);
	});
});

describe('isValidRange', () => {
	it('逆順・幅 0・非有限は不正', () => {
		expect(isValidRange({ startMs: 0, endMs: 1 })).toBe(true);
		expect(isValidRange({ startMs: 1, endMs: 1 })).toBe(false);
		expect(isValidRange({ startMs: 2, endMs: 1 })).toBe(false);
		expect(isValidRange({ startMs: NaN, endMs: 1 })).toBe(false);
	});
});

describe('zoomRange', () => {
	const r = { startMs: 0, endMs: 4 * H };
	it('anchor の相対位置を保って拡大する', () => {
		expect(zoomRange(r, 0.5, H)).toEqual({ startMs: H / 2, endMs: H / 2 + 2 * H });
	});
	it('中央基準で縮小する', () => {
		expect(zoomRange(r, 2, 2 * H)).toEqual({ startMs: -2 * H, endMs: 6 * H });
	});
	it('幅を上限・下限に丸める', () => {
		const z = zoomRange(r, 1e-9, 2 * H);
		expect(z.endMs - z.startMs).toBe(MIN_RANGE_MS);
		expect(z.startMs + MIN_RANGE_MS / 2).toBe(2 * H);
		const big = zoomRange({ startMs: 0, endMs: MAX_RANGE_MS }, 3, 0);
		expect(big).toEqual({ startMs: 0, endMs: MAX_RANGE_MS });
	});
	it('範囲外の anchor は端に寄せる', () => {
		expect(zoomRange(r, 0.5, -100 * H)).toEqual({ startMs: 0, endMs: 2 * H });
		expect(zoomRange(r, 0.5, 100 * H)).toEqual({ startMs: 2 * H, endMs: 4 * H });
	});
	it('不正な入力ではそのまま返す', () => {
		expect(zoomRange(r, NaN, 0)).toBe(r);
		expect(zoomRange(r, 0, 0)).toBe(r);
		expect(zoomRange(r, -1, 0)).toBe(r);
		expect(zoomRange(r, 0.5, NaN)).toBe(r);
		const bad = { startMs: 5, endMs: 1 };
		expect(zoomRange(bad, 0.5, 3)).toBe(bad);
	});
});

describe('panRange / shiftPage / canShiftNext', () => {
	const r = { startMs: NOW - 3 * H, endMs: NOW - 2 * H };
	it('幅を保って動かす', () => {
		expect(panRange(r, -H, NOW)).toEqual({ startMs: NOW - 4 * H, endMs: NOW - 3 * H });
	});
	it('終わりが今を越えないよう丸める', () => {
		expect(panRange(r, 10 * H, NOW)).toEqual({ startMs: NOW - H, endMs: NOW });
		// 今を越えた範囲は delta 0 でも今に戻す
		expect(panRange({ startMs: NOW, endMs: NOW + H }, 0, NOW)).toEqual({
			startMs: NOW - H,
			endMs: NOW
		});
	});
	it('前へ・次へは幅 1 つ分、次へは今で止まる', () => {
		expect(shiftPage(r, 'prev', NOW)).toEqual({ startMs: NOW - 4 * H, endMs: NOW - 3 * H });
		expect(shiftPage(r, 'next', NOW)).toEqual({ startMs: NOW - 2 * H, endMs: NOW - H });
		expect(shiftPage({ startMs: NOW - 2 * H, endMs: NOW - H / 2 }, 'next', NOW)).toEqual({
			startMs: NOW - 1.5 * H,
			endMs: NOW
		});
	});
	it('canShiftNext は終わりが今より前のときだけ', () => {
		expect(canShiftNext(r, NOW)).toBe(true);
		expect(canShiftNext({ startMs: NOW - H, endMs: NOW }, NOW)).toBe(false);
		expect(canShiftNext(r, NaN)).toBe(false);
	});
	it('不正な入力ではそのまま返す', () => {
		expect(panRange(r, NaN, NOW)).toBe(r);
		expect(panRange(r, H, NaN)).toBe(r);
		const bad = { startMs: 2, endMs: 1 };
		expect(shiftPage(bad, 'prev', NOW)).toBe(bad);
	});
});

describe('isAtNow', () => {
	it('終わりが now - tolerance 以上なら追従中', () => {
		expect(isAtNow({ startMs: NOW - H, endMs: NOW - 500 }, NOW, 1000)).toBe(true);
		expect(isAtNow({ startMs: NOW - H, endMs: NOW - 1500 }, NOW, 1000)).toBe(false);
		expect(isAtNow({ startMs: NOW - H, endMs: NOW + 10 }, NOW, 0)).toBe(true);
	});
	it('不正な範囲・now は false、不正な tolerance は 0 とみなす', () => {
		expect(isAtNow({ startMs: 1, endMs: 0 }, NOW, 1000)).toBe(false);
		expect(isAtNow({ startMs: NOW - H, endMs: NOW }, NaN, 1000)).toBe(false);
		expect(isAtNow({ startMs: NOW - H, endMs: NOW - 1 }, NOW, NaN)).toBe(false);
	});
});
