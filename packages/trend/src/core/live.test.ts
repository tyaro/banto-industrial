import { describe, expect, it } from 'vitest';
import {
	EMPTY_LIVE_PEN,
	LIVE_MIN_RETAIN_MS,
	liveAppend,
	liveAppendAll,
	liveBreak,
	liveBreakAll,
	liveRetainMs,
	type LivePen
} from './live';

const M = 60_000;
const opts = { retainMs: 10 * M };

function fill(pen: LivePen | undefined, from: number, to: number, stepMs: number, value = 1) {
	let p = pen ?? EMPTY_LIVE_PEN;
	for (let t = from; t <= to; t += stepMs) p = liveAppend(p, { tMs: t, value }, opts);
	return p;
}

describe('liveRetainMs', () => {
	it('追従の窓の 2 倍か 30 分の長い方', () => {
		expect(liveRetainMs(10 * M)).toBe(LIVE_MIN_RETAIN_MS);
		expect(liveRetainMs(60 * M)).toBe(120 * M);
		expect(liveRetainMs(NaN)).toBe(LIVE_MIN_RETAIN_MS);
	});
});

describe('liveAppend', () => {
	it('最初の値の時刻が liveFrom になる', () => {
		const p = liveAppend(undefined, { tMs: 1000, value: 3 }, opts);
		expect(p).toEqual({ liveFrom: 1000, samples: [{ tMs: 1000, value: 3 }] });
	});

	it('null のサンプルでは liveFrom が動かない（欠測として残る）', () => {
		let p = liveAppend(undefined, { tMs: 1000, value: 3 }, opts);
		p = liveAppend(p, { tMs: 2000, value: null }, opts);
		p = liveAppend(p, { tMs: 3000, value: 4 }, opts);
		expect(p.liveFrom).toBe(1000);
		expect(p.samples.map((s) => s.value)).toEqual([3, null, 4]);
	});

	it('始まりを待っている間の null は捨て、次の値から始める', () => {
		let p = liveAppend(undefined, { tMs: 1000, value: null }, opts);
		expect(p).toBe(EMPTY_LIVE_PEN);
		p = liveAppend(p, { tMs: 2000, value: 5 }, opts);
		expect(p.liveFrom).toBe(2000);
	});

	it('非有限の値は null、時刻が前以下・非有限のサンプルは捨てて同じオブジェクトを返す', () => {
		let p = liveAppend(undefined, { tMs: 1000, value: 1 }, opts);
		p = liveAppend(p, { tMs: 2000, value: NaN }, opts);
		expect(p.samples[1]).toEqual({ tMs: 2000, value: null });
		expect(liveAppend(p, { tMs: 2000, value: 9 }, opts)).toBe(p);
		expect(liveAppend(p, { tMs: 1500, value: 9 }, opts)).toBe(p);
		expect(liveAppend(p, { tMs: NaN, value: 9 }, opts)).toBe(p);
	});

	it('途切れ（liveBreak）の後は、その後の最初の値の時刻に liveFrom が進む', () => {
		let p = fill(undefined, 0, 60_000, 1000);
		expect(p.liveFrom).toBe(0);
		p = liveBreak(p);
		expect(p).toBe(EMPTY_LIVE_PEN);
		p = liveAppend(p, { tMs: 90_000, value: null }, opts);
		p = liveAppend(p, { tMs: 95_000, value: 2 }, opts);
		expect(p.liveFrom).toBe(95_000);
		expect(p.samples).toEqual([{ tMs: 95_000, value: 2 }]);
	});

	it('保持時間を一定の割合超えたら、保持時間までまとめて追い出し liveFrom を進める', () => {
		// 10 分保持・余裕 1.1 倍。11 分ちょうどまでは追い出さない。
		let p = fill(undefined, 0, 11 * M, 1000);
		expect(p.liveFrom).toBe(0);
		p = liveAppend(p, { tMs: 11 * M + 1000, value: 1 }, opts);
		expect(p.liveFrom).toBe(M + 1000);
		expect(p.samples[0].tMs).toBe(M + 1000);
		expect(p.samples[p.samples.length - 1].tMs - p.liveFrom!).toBe(10 * M);
	});

	it('追い出しはまとめて行うので、liveFrom は書き足しのたびには動かない', () => {
		let p: LivePen = EMPTY_LIVE_PEN;
		const seen = new Set<number>();
		let appends = 0;
		for (let t = 0; t <= 40 * M; t += 1000) {
			p = liveAppend(p, { tMs: t, value: 1 }, opts);
			seen.add(p.liveFrom!);
			appends++;
		}
		// 40 分で、11 分以降は約 1 分（保持 10 分 × 0.1）に 1 回だけ進む。
		expect(appends).toBe(2401);
		expect(seen.size).toBeLessThanOrEqual(31);
		expect(seen.size).toBeGreaterThan(20);

		// 余裕 0 なら毎回動く（比較のため）。
		let q: LivePen = EMPTY_LIVE_PEN;
		const seen0 = new Set<number>();
		for (let t = 0; t <= 40 * M; t += 1000) {
			q = liveAppend(q, { tMs: t, value: 1 }, { retainMs: 10 * M, evictSlack: 0 });
			seen0.add(q.liveFrom!);
		}
		expect(seen0.size).toBeGreaterThan(1500);
	});

	it('追い出した後の先頭の null は捨て、liveFrom は残った最初の値の時刻', () => {
		let p = liveAppend(undefined, { tMs: 0, value: 1 }, opts);
		p = liveAppend(p, { tMs: 2 * M, value: null }, opts);
		p = liveAppend(p, { tMs: 3 * M, value: 2 }, opts);
		p = liveAppend(p, { tMs: 12 * M, value: 3 }, opts);
		// cutoff = 2 分。先頭の null（2 分）を捨てて 3 分から。
		expect(p.liveFrom).toBe(3 * M);
		expect(p.samples.map((s) => s.tMs)).toEqual([3 * M, 12 * M]);
	});
});

describe('liveAppendAll / liveBreakAll', () => {
	it('変わったペンだけ差し替え、何も変わらなければ同じオブジェクト', () => {
		const b0 = {};
		const b1 = liveAppendAll(
			b0,
			{ a: { tMs: 1000, value: 1 }, b: { tMs: 1000, value: null } },
			opts
		);
		expect(b1.a?.liveFrom).toBe(1000);
		expect(b1.b).toBeUndefined();
		expect(liveAppendAll(b1, { a: { tMs: 1000, value: 1 } }, opts)).toBe(b1);
		const b2 = liveBreakAll(b1, ['a']);
		expect(b2.a).toBe(EMPTY_LIVE_PEN);
		expect(liveBreakAll(b2)).toBe(b2);
	});
});
