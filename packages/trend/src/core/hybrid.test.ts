import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { TrendData, TrendSeries } from './data';
import { envelopePath } from './geometry';
import type { TrendFetchRequest } from './loader';
import {
	boundaryKey,
	boundaryPlan,
	boundaryValuesFrom,
	createBoundaryLoader,
	estimateIntervalMs,
	hybridData,
	liveBinMs,
	liveBins,
	pruneBoundary,
	type BoundaryRequest
} from './hybrid';
import { EMPTY_LIVE_PEN, liveAppend, liveBreak, type LivePen, type LiveSample } from './live';
import type { Bin } from './pens';

const M = 60_000;
// 5 分と 1 分の格子に揃った基準の時刻。
const T0 = 1000 * 5 * M;
const id = (v: number) => v;

function pen(samples: LiveSample[]): LivePen {
	let p: LivePen = EMPTY_LIVE_PEN;
	for (const s of samples) p = liveAppend(p, s, { retainMs: 120 * M });
	return p;
}
function every(from: number, to: number, stepMs: number, value: (t: number) => number | null) {
	const out: LiveSample[] = [];
	for (let t = from; t < to; t += stepMs) out.push({ tMs: t, value: value(t) });
	return out;
}
function minuteBins(from: number, to: number, value: (t: number) => number | null): Bin[] {
	const out: Bin[] = [];
	for (let t = from; t < to; t += M) {
		const v = value(t);
		out.push({ tMs: t, min: v, max: v });
	}
	return out;
}
/** 線の切れ目の数（`M` の数）。 */
const segments = (s: TrendSeries) =>
	(envelopePath(s.bins, s.binMs, id, id).match(/M/g) ?? []).length;
const tms = (s: TrendSeries) => s.bins.map((b) => b.tMs);
const increasing = (xs: number[]) => xs.every((x, i) => i === 0 || x > xs[i - 1]);

describe('liveBinMs / estimateIntervalMs / liveBins', () => {
	it('区間の幅をサンプル間隔の 1.5 倍以上になるまで整数倍に広げる', () => {
		expect(liveBinMs(1000, 1000)).toBe(2000);
		expect(liveBinMs(M, 1000)).toBe(M);
		expect(liveBinMs(500, 1000)).toBe(1500);
		expect(liveBinMs(1000, undefined)).toBe(1000);
		expect(liveBinMs(NaN, 1000)).toBe(1500);
	});

	it('サンプル間隔は直近の差の中央値', () => {
		expect(estimateIntervalMs([{ tMs: 0, value: 1 }])).toBeUndefined();
		const s = [0, 1000, 2000, 2900, 4000, 9000].map((tMs) => ({ tMs, value: 1 }));
		expect(estimateIntervalMs(s)).toBe(1000);
	});

	it('格子でまとめ、最初の区間は liveFrom で始め、値の無い区間は欠測', () => {
		const s: LiveSample[] = [
			{ tMs: T0 + 30_000, value: 2 },
			{ tMs: T0 + 50_000, value: 4 },
			{ tMs: T0 + M + 10_000, value: null },
			{ tMs: T0 + 2 * M, value: 7 }
		];
		expect(liveBins(s, T0 + 30_000, M)).toEqual([
			{ tMs: T0 + 30_000, min: 2, max: 4 },
			{ tMs: T0 + M, min: null, max: null },
			{ tMs: T0 + 2 * M, min: 7, max: 7 }
		]);
	});

	it('間隔のゆらぎで値の無い格子ができず、線が 1 本のまま', () => {
		// 間隔 1 秒 ± 0.3 秒、履歴の幅 1 秒 → 2 秒に広げる。
		const s: LiveSample[] = [];
		for (let i = 0; i < 300; i++) s.push({ tMs: T0 + i * 1000 + (i % 2 ? 300 : -300), value: i });
		s[0].tMs = T0;
		const p = pen(s);
		const out = hybridData({
			history: null,
			live: { a: p },
			penIds: ['a'],
			binMs: 1000,
			sampleIntervalMs: { a: 1000 }
		})!;
		expect(out.series.a!.binMs).toBe(2000);
		expect(segments(out.series.a!)).toBe(1);
	});
});

describe('hybridData — 設計書 §8 の受け入れ', () => {
	it('(1) 末尾が未保存: 履歴の最後より新しい値がリングバッファにだけあるとき、その値が描かれる', () => {
		// 履歴は 10 分前〜今。最後の 5 分はまだ保存されておらず欠測。
		const history: TrendData = {
			range: { startMs: T0 - 10 * M, endMs: T0 + 10 * M },
			series: {
				a: {
					bins: minuteBins(T0 - 10 * M, T0 + 10 * M, (t) => (t < T0 + 5 * M ? 1 : null)),
					binMs: M
				}
			}
		};
		const live = { a: pen(every(T0, T0 + 10 * M, 10_000, () => 5)) };
		const out = hybridData({ history, live, penIds: ['a'], binMs: M, nowMs: T0 + 10 * M })!;
		const s = out.series.a!;
		const last = s.bins[s.bins.length - 1];
		expect(last).toEqual({ tMs: T0 + 9 * M, min: 5, max: 5 });
		expect(s.bins.filter((b) => b.tMs >= T0).every((b) => b.min === 5)).toBe(true);
		expect(segments(s)).toBe(1);
		expect(out.range).toEqual({ startMs: T0 - 10 * M, endMs: T0 + 10 * M });
	});

	it('(2) 欠測: 履歴の途中とリングバッファの途中の欠測が、どちらも線を切る', () => {
		const history: TrendData = {
			range: { startMs: T0 - 10 * M, endMs: T0 },
			series: {
				a: { bins: minuteBins(T0 - 10 * M, T0, (t) => (t === T0 - 5 * M ? null : 1)), binMs: M }
			}
		};
		const live = {
			a: pen(every(T0, T0 + 10 * M, 10_000, (t) => (t >= T0 + 3 * M && t < T0 + 4 * M ? null : 2)))
		};
		const out = hybridData({ history, live, penIds: ['a'], binMs: M })!;
		const s = out.series.a!;
		expect(s.bins.find((b) => b.tMs === T0 - 5 * M)).toEqual({
			tMs: T0 - 5 * M,
			min: null,
			max: null
		});
		expect(s.bins.find((b) => b.tMs === T0 + 3 * M)).toEqual({
			tMs: T0 + 3 * M,
			min: null,
			max: null
		});
		// 切れ目 2 つで 3 本（つなぎ目 T0 では切れない）。
		expect(segments(s)).toBe(3);
	});

	it('(3) 重複: liveFrom 以後の時刻が両方にあるとき、線が 1 本だけ描かれる', () => {
		const history: TrendData = {
			range: { startMs: T0 - 5 * M, endMs: T0 + 5 * M },
			series: {
				a: { bins: minuteBins(T0 - 5 * M, T0 + 5 * M, (t) => (t < T0 ? 1 : 999)), binMs: M }
			}
		};
		const live = { a: pen(every(T0, T0 + 5 * M, 10_000, () => 3)) };
		const out = hybridData({ history, live, penIds: ['a'], binMs: M })!;
		const s = out.series.a!;
		expect(s.bins.some((b) => b.max === 999)).toBe(false);
		expect(increasing(tms(s))).toBe(true);
		expect(tms(s)).toEqual(minuteBins(T0 - 5 * M, T0 + 5 * M, () => 0).map((b) => b.tMs));
		expect(segments(s)).toBe(1);
	});

	describe('(4) 境界をまたぐ区間', () => {
		// 履歴は 5 分の区間。liveFrom は 10:03 相当（T0 + 3 分）。
		const history: TrendData = {
			range: { startMs: T0 - 5 * M, endMs: T0 + 10 * M },
			series: {
				a: {
					bins: [
						{ tMs: T0 - 5 * M, min: 1, max: 2 },
						{ tMs: T0, min: 0, max: 100 }, // 10:03 以後の値（100）が混ざっている
						{ tMs: T0 + 5 * M, min: 50, max: 50 }
					],
					binMs: 5 * M
				}
			}
		};
		const liveFrom = T0 + 3 * M;
		const live = {
			a: pen(every(liveFrom, T0 + 10 * M, 1000, (t) => (t === T0 + 4 * M ? 9 : 7)))
		};
		const src = { history, live, penIds: ['a'] };

		it('[区間の始まり, liveFrom) を 1 区間で読む計画を出す', () => {
			expect(boundaryPlan(src)).toEqual([
				{ range: { startMs: T0, endMs: liveFrom }, penIds: ['a'] }
			]);
		});

		it('読み直す前は、その区間を欠測にして履歴の値を数えない', () => {
			const s = hybridData({ ...src, binMs: M })!.series.a!;
			expect(s.bins[1]).toEqual({ tMs: T0, min: null, max: null });
			expect(s.bins.some((b) => b.max === 100 || b.max === 50)).toBe(false);
		});

		it('liveFrom より前は履歴（読み直し）から、後はリングバッファから、どちらも 1 回だけ集計する', () => {
			const [req] = boundaryPlan(src);
			const boundary = boundaryValuesFrom(req, {
				a: { bins: [{ tMs: T0, min: 1, max: 3 }], binMs: 3 * M }
			});
			expect(boundaryPlan(src, boundary)).toEqual([]);
			const s = hybridData({ ...src, binMs: M, boundary })!.series.a!;
			expect(s.binMs).toBe(5 * M);
			expect(s.bins).toEqual([
				{ tMs: T0 - 5 * M, min: 1, max: 2 },
				{ tMs: T0, min: 1, max: 3 },
				{ tMs: liveFrom, min: 7, max: 9 },
				{ tMs: T0 + 5 * M, min: 7, max: 7 }
			]);
			// 両側に値があるので、つなぎ目で線が切れない。
			expect(segments(s)).toBe(1);
		});
	});

	it('(5) 追い出し: 古い値を追い出した後も、追い出した範囲が履歴で欠けずに描かれる', () => {
		const retain = { retainMs: 10 * M };
		let p: LivePen = EMPTY_LIVE_PEN;
		const end = T0 + 11 * M + 10_000;
		for (let t = T0; t <= end; t += 10_000) p = liveAppend(p, { tMs: t, value: 2 }, retain);
		// 11 分を超えたところで 10 分までまとめて追い出した。
		expect(p.liveFrom).toBe(T0 + M + 10_000);

		const history: TrendData = {
			range: { startMs: T0 - 5 * M, endMs: end + 1 },
			series: { a: { bins: minuteBins(T0 - 5 * M, end + 1, () => 1), binMs: M } }
		};
		const src = { history, live: { a: p }, penIds: ['a'] };
		const plan = boundaryPlan(src);
		expect(plan).toEqual([{ range: { startMs: T0 + M, endMs: T0 + M + 10_000 }, penIds: ['a'] }]);
		const boundary = boundaryValuesFrom(plan[0], {
			a: { bins: [{ tMs: T0 + M, min: 1, max: 1 }], binMs: M }
		});
		const s = hybridData({ ...src, binMs: M, boundary })!.series.a!;
		// 追い出した範囲（T0 〜 liveFrom）の区間が履歴で全部ある。
		const evicted = s.bins.filter((b) => b.tMs >= T0 && b.tMs < p.liveFrom!);
		expect(evicted.map((b) => b.tMs)).toEqual([T0, T0 + M]);
		expect(evicted.every((b) => b.min === 1)).toBe(true);
		expect(s.bins.filter((b) => b.tMs >= p.liveFrom!).every((b) => b.min === 2)).toBe(true);
		expect(segments(s)).toBe(1);
	});
});

describe('hybridData — そのほか', () => {
	it('履歴が null なら liveFrom からだけ描き、範囲の右端は nowMs', () => {
		const live = { a: pen(every(T0 + 30_000, T0 + 3 * M, 10_000, () => 1)) };
		const out = hybridData({
			history: null,
			live,
			penIds: ['a', 'b'],
			binMs: M,
			nowMs: T0 + 4 * M
		})!;
		expect(out.range).toEqual({ startMs: T0 + 30_000, endMs: T0 + 4 * M });
		expect(out.series.a!.bins[0].tMs).toBe(T0 + 30_000);
		expect(out.series.b).toBeUndefined();
	});

	it('履歴もリングバッファも無ければ null', () => {
		expect(hybridData({ history: null, live: {}, penIds: ['a'], binMs: M })).toBeNull();
		expect(
			hybridData({ history: null, live: { a: EMPTY_LIVE_PEN }, penIds: ['a'], binMs: M })
		).toBeNull();
	});

	it('リングバッファが始まりを待っているペンは履歴のまま', () => {
		const series = { bins: minuteBins(T0, T0 + 3 * M, () => 1), binMs: M };
		const history: TrendData = { range: { startMs: T0, endMs: T0 + 3 * M }, series: { a: series } };
		const out = hybridData({
			history,
			live: { a: EMPTY_LIVE_PEN },
			penIds: ['a'],
			binMs: M,
			nowMs: T0 + 9 * M
		})!;
		expect(out.series.a).toBe(series);
		expect(out.range).toEqual(history.range);
	});

	it('途切れた後の liveFrom で、それより前は履歴で描く', () => {
		let p = liveBreak(pen(every(T0, T0 + 2 * M, 10_000, () => 5)));
		p = liveAppend(p, { tMs: T0 + 4 * M, value: 6 }, { retainMs: 30 * M });
		const history: TrendData = {
			range: { startMs: T0, endMs: T0 + 5 * M },
			series: { a: { bins: minuteBins(T0, T0 + 5 * M, () => 1), binMs: M } }
		};
		const s = hybridData({ history, live: { a: p }, penIds: ['a'], binMs: M })!.series.a!;
		expect(s.bins).toEqual([
			...minuteBins(T0, T0 + 4 * M, () => 1),
			{ tMs: T0 + 4 * M, min: 6, max: 6 }
		]);
	});

	it('pruneBoundary はいま要るキーだけを残す', () => {
		const history: TrendData = {
			range: { startMs: T0, endMs: T0 + 5 * M },
			series: { a: { bins: [{ tMs: T0, min: 1, max: 1 }], binMs: 5 * M } }
		};
		const src = { history, live: { a: pen([{ tMs: T0 + M, value: 1 }]) }, penIds: ['a'] };
		const keep = boundaryKey('a', T0, T0 + M);
		const old = boundaryKey('a', T0, T0 + 30_000);
		const v = { [keep]: { tMs: T0, min: 1, max: 1 }, [old]: { tMs: T0, min: 2, max: 2 } };
		expect(Object.keys(pruneBoundary(v, src))).toEqual([keep]);
		const kept = pruneBoundary(v, src);
		expect(pruneBoundary(kept, src)).toBe(kept);
	});

	it('同じ範囲のペンは 1 つの要求にまとめる', () => {
		const series = { bins: [{ tMs: T0, min: 1, max: 1 }], binMs: 5 * M };
		const history: TrendData = {
			range: { startMs: T0, endMs: T0 + 5 * M },
			series: { a: series, b: series, c: series }
		};
		const live = {
			a: pen([{ tMs: T0 + M, value: 1 }]),
			b: pen([{ tMs: T0 + M, value: 1 }]),
			c: pen([{ tMs: T0 + 2 * M, value: 1 }])
		};
		expect(boundaryPlan({ history, live, penIds: ['a', 'b', 'c'] })).toEqual([
			{ range: { startMs: T0, endMs: T0 + M }, penIds: ['a', 'b'] },
			{ range: { startMs: T0, endMs: T0 + 2 * M }, penIds: ['c'] }
		]);
	});
});

describe('createBoundaryLoader', () => {
	beforeEach(() => vi.useFakeTimers());
	afterEach(() => vi.useRealTimers());

	const req = (end: number, penIds = ['a']): BoundaryRequest => ({
		range: { startMs: 0, endMs: end },
		penIds
	});

	it('debounce の後に区間 1 つで読み、同じ計画を何度渡しても読み直さない', async () => {
		const fetch = vi.fn(async (_req: TrendFetchRequest) => ({
			a: { bins: [{ tMs: 0, min: 1, max: 2 }], binMs: 10 }
		}));
		const onResult = vi.fn();
		const loader = createBoundaryLoader({ fetch, onResult });
		loader.request([req(10)]);
		loader.request([req(10)]);
		await vi.advanceTimersByTimeAsync(199);
		expect(fetch).not.toHaveBeenCalled();
		loader.request([req(10)]);
		await vi.advanceTimersByTimeAsync(1);
		expect(fetch).toHaveBeenCalledTimes(1);
		expect(fetch.mock.calls[0][0]).toMatchObject({ range: { startMs: 0, endMs: 10 }, bins: 1 });
		await vi.runAllTimersAsync();
		expect(onResult).toHaveBeenCalledWith(
			{ [boundaryKey('a', 0, 10)]: { tMs: 0, min: 1, max: 2 } },
			req(10)
		);
	});

	it('計画から消えた要求は abort し、遅れて返っても渡さない', async () => {
		let resolve!: (v: Record<string, TrendSeries>) => void;
		const signals: AbortSignal[] = [];
		const fetch = vi.fn(({ signal }: { signal: AbortSignal }) => {
			signals.push(signal);
			return new Promise<Record<string, TrendSeries>>((r) => (resolve = r));
		});
		const onResult = vi.fn();
		const loader = createBoundaryLoader({ fetch, onResult });
		loader.request([req(10)]);
		await vi.advanceTimersByTimeAsync(200);
		loader.request([req(20)]);
		expect(signals[0].aborted).toBe(true);
		resolve({ a: { bins: [], binMs: 10 } });
		await vi.advanceTimersByTimeAsync(0);
		expect(onResult).not.toHaveBeenCalled();
		await vi.advanceTimersByTimeAsync(200);
		expect(fetch).toHaveBeenCalledTimes(2);
		loader.dispose();
		expect(signals[1].aborted).toBe(true);
	});

	it('失敗した要求は onError に渡し、計画から消えるまで読み直さない', async () => {
		const fetch = vi.fn(async () => {
			throw new Error('boom');
		});
		const onError = vi.fn();
		const loader = createBoundaryLoader({ fetch, onResult: vi.fn(), onError });
		loader.request([req(10)]);
		await vi.runAllTimersAsync();
		expect(onError).toHaveBeenCalledTimes(1);
		loader.request([req(10)]);
		await vi.runAllTimersAsync();
		expect(fetch).toHaveBeenCalledTimes(1);
		loader.request([]);
		loader.request([req(10)]);
		await vi.runAllTimersAsync();
		expect(fetch).toHaveBeenCalledTimes(2);
	});
});
