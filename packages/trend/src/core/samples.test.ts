import { describe, expect, it } from 'vitest';
import { bitMissingSpans, bitSpans } from './bitBands';
import { cursorDiff, cursorReadout } from './cursor';
import { drawableSeries, type TrendData, type TrendSeries } from './data';
import type { Bin, TrendPen } from './pens';
import { dataExtent, resolveYDomain } from './pens';
import {
	effectiveClip,
	sampleAt,
	sampleIntervals,
	samplesForDraw,
	formatObservationTime,
	samplesInRange,
	samplesMarkerPoints,
	samplesPath
} from './samples';
import { markerPoints } from './style';

describe('samplesInRange の値の正規化・formatObservationTime', () => {
	it('min と max が違う観測も 1 値に揃え、縦軸が描いていない値で決まらない', () => {
		const bins: Bin[] = [
			{ tMs: 1000, min: 1, max: 1_000_000 },
			{ tMs: 2000, min: 2, max: 1_000_000 },
			{ tMs: 3000, min: null, max: null }
		];
		const inside = samplesInRange(bins, { startMs: 0, endMs: 10000 });
		expect(inside).toEqual([
			{ tMs: 1000, min: 1, max: 1 },
			{ tMs: 2000, min: 2, max: 2 },
			{ tMs: 3000, min: null, max: null }
		]);
		expect(dataExtent(inside)).toEqual([1, 2]);
	});

	it('秒未満の観測は別の文字列、ミリ秒 0 は秒まで', () => {
		const t = (ms: number) => new Date(2026, 10, 21, 12, 0, 0, ms).getTime();
		expect(formatObservationTime(t(100))).toBe('2026/11/21 12:00:00.100');
		expect(formatObservationTime(t(200))).toBe('2026/11/21 12:00:00.200');
		expect(formatObservationTime(t(5))).toBe('2026/11/21 12:00:00.005');
		expect(formatObservationTime(t(0))).toBe('2026/11/21 12:00:00');
	});
});

const obs = (tMs: number, v: number | null): Bin => ({ tMs, min: v, max: v });
const pen: TrendPen = { id: 'p', label: 'P', colorSlot: 1, unit: 'V', decimals: 1 };
const lin = (d0: number, d1: number, r0: number, r1: number) => (v: number) =>
	r0 + ((v - d0) / (d1 - d0)) * (r1 - r0);

describe('samplesPath / samplesMarkerPoints', () => {
	it('2 秒周期の観測は実時刻の x に置かれる（区間の中央にずらさない）', () => {
		const bins = [obs(0, 1), obs(2000, 3)];
		const x = lin(0, 10000, 0, 100);
		const y = (v: number) => v * 10;
		expect(samplesPath(bins, 2000, x, y)).toBe('M0,10L20,30');
		const pts = samplesMarkerPoints(bins, x, y, { startMs: 0, endMs: 10000 }, 4);
		expect(pts).toEqual([
			{ x: 0, y: 10 },
			{ x: 20, y: 30 }
		]);
		// 'bins' のマーカーは区間の中央（1 秒・3 秒）。
		const old = markerPoints(bins, 2000, x, y, { startMs: 0, endMs: 10000 }, 4);
		expect(old[0].x).toBe(10);
	});

	it('1 時間を 1000px に縮めても 2 秒だけのスパイクの点が残る', () => {
		const HOUR = 3_600_000;
		const bins: Bin[] = [];
		for (let t = 0; t < 60_000; t += 2000) bins.push(obs(t, t === 20_000 ? 100 : 1));
		const x = lin(0, HOUR, 0, 1000);
		const y = lin(0, 100, 100, 0);
		const pts = samplesMarkerPoints(bins, x, y, { startMs: 0, endMs: HOUR }, 4);
		expect(pts.some((p) => p.y === 0)).toBe(true);
		const thin = markerPoints(bins, 2000, x, y, { startMs: 0, endMs: HOUR }, 4);
		expect(thin.length).toBeLessThan(pts.length);
	});

	it('同じ座標の点だけ省く。描画域の外は省く', () => {
		const bins = [obs(0, 1), obs(1, 1), obs(2, 1), obs(1000, 1), obs(5000, 1)];
		const x = lin(0, 1000, 0, 100);
		const y = () => 5;
		const pts = samplesMarkerPoints(bins, x, y, { startMs: 0, endMs: 10000 }, 4, {
			width: 100,
			height: 10
		});
		expect(pts).toEqual([
			{ x: 0, y: 5 },
			{ x: 0.1, y: 5 },
			{ x: 0.2, y: 5 },
			{ x: 100, y: 5 }
		]);
		// 丸めて同じ座標になる点は 1 つ（x が 0.001px しか違わない）。
		const dup = samplesMarkerPoints(
			[obs(0, 1), obs(1, 1)],
			lin(0, 1_000_000, 0, 100),
			y,
			{ startMs: 0, endMs: 1_000_000 },
			4
		);
		expect(dup).toHaveLength(1);
	});

	it('binMs x 2 を超えた空きと null の観測で線が切れる', () => {
		const x = (t: number) => t;
		const y = (v: number) => v;
		expect(samplesPath([obs(0, 1), obs(2000, 2), obs(4000, 3)], 2000, x, y)).toBe(
			'M0,1L2000,2L4000,3'
		);
		// ちょうど 2 倍は切らない。超えたら切る（1 点の切れ端は点として出す）。
		expect(samplesPath([obs(0, 1), obs(4000, 2)], 2000, x, y)).toBe('M0,1L4000,2');
		expect(samplesPath([obs(0, 1), obs(4001, 2)], 2000, x, y)).toBe('M0,1L0,1M4001,2L4001,2');
		expect(samplesPath([obs(0, 1), obs(2000, null), obs(4000, 3)], 2000, x, y)).toBe(
			'M0,1L0,1M4000,3L4000,3'
		);
	});

	it('bit は前の値を次の観測まで保つ段差', () => {
		const x = (t: number) => t;
		const y = (v: number) => 10 - v * 10;
		expect(samplesPath([obs(0, 0), obs(2000, 1), obs(4000, 0)], 2000, x, y, { step: true })).toBe(
			'M0,10L2000,10L2000,0L4000,0L4000,10'
		);
	});
});

describe('sampleAt / cursorReadout', () => {
	const s: TrendSeries = {
		kind: 'samples',
		binMs: 2000,
		bins: [obs(0, 1), obs(2000, 5), obs(4000, null), obs(6000, 7)]
	};
	it('近い観測に吸着し、離れた所は null', () => {
		expect(sampleAt(s.bins, 100, 200)?.tMs).toBe(0);
		expect(sampleAt(s.bins, 1900, 200)?.tMs).toBe(2000);
		expect(sampleAt(s.bins, 1000, 200)).toBeNull();
		expect(sampleAt(s.bins, 2000, 0)?.tMs).toBe(2000);
	});
	it('最も近い観測が欠測なら、先の古い値には吸着せず null', () => {
		expect(sampleAt(s.bins, 4000, 2000)).toBeNull();
		const g = [obs(0, 5), obs(2000, null)];
		expect(sampleAt(g, 2000, 3000)).toBeNull();
	});
	it('欠測が遠く、有限の観測が近ければ吸着する', () => {
		expect(sampleAt(s.bins, 5500, 1000)?.tMs).toBe(6000);
		expect(sampleAt(s.bins, 3000, 1000)?.tMs).toBe(2000);
		expect(sampleAt(s.bins, 3000, 500)).toBeNull();
	});
	it('range を渡すと、範囲の外の隣には吸着しない', () => {
		const r = [obs(999, 3), obs(1100, 4), obs(2000, 6)];
		const range = { startMs: 1000, endMs: 2000 };
		// 左隣 999 は範囲の外、右隣 2000 は endMs 以後。範囲内は 1100 だけ。
		expect(sampleAt(r, 1000, 500, range)?.tMs).toBe(1100);
		expect(sampleAt(r, 1000, 500)?.tMs).toBe(999);
		expect(sampleAt(r, 1990, 1000, range)?.tMs).toBe(1100);
		expect(sampleAt(r, 1990, 1000)?.tMs).toBe(2000);
		expect(sampleAt([obs(999, 3)], 1000, 500, range)).toBeNull();
		expect(sampleAt([obs(2000, 3)], 1999, 500, range)).toBeNull();
	});
	it('cursorReadout: min・max が違う観測も描画と同じ 1 値（sampleValue）', () => {
		const eff = { startMs: 0, endMs: 10000 };
		const w: TrendSeries = {
			kind: 'samples',
			binMs: 2000,
			bins: [{ tMs: 1000, min: 0, max: 1 }]
		};
		const r = cursorReadout([pen], { p: w }, 1000, eff, 80)[0];
		expect(r.min).toBe(0);
		expect(r.max).toBe(0);
	});
	it('cursorReadout: 8px 相当の許容の中だけ値を出す', () => {
		const eff = { startMs: 0, endMs: 10000 };
		const series = { p: s };
		// 1000px で 10 秒 → 8px = 80ms。
		expect(cursorReadout([pen], series, 2050, eff, 80)[0].text).toBe('5.0 V');
		expect(cursorReadout([pen], series, 1000, eff, 80)[0].text).toBe('—');
		// 'bins' の系列は従来どおり区間の中なら値が出る。
		const b: TrendSeries = { binMs: 2000, bins: [{ tMs: 0, min: 1, max: 1 }] };
		expect(cursorReadout([pen], { p: b }, 1000, eff, 80)[0].text).toBe('1.0 V');
	});
});

describe('drawableSeries / 範囲', () => {
	const bins = [obs(0, 1), obs(2000, 2), obs(4000, 3), obs(6000, 4), obs(8000, 5)];
	const data: TrendData = {
		range: { startMs: 0, endMs: 10000 },
		series: { p: { kind: 'samples', binMs: 2000, bins } }
	};
	it('外の観測を 1 つずつ残し、kind を保つ。縦軸には外を含めない', () => {
		const d = drawableSeries(data, { startMs: 3000, endMs: 7000 });
		expect(d.series.p.kind).toBe('samples');
		expect(d.series.p.bins.map((b) => b.tMs)).toEqual([2000, 4000, 6000, 8000]);
		const inside = samplesInRange(d.series.p.bins, d.effective!);
		expect(inside.map((b) => b.tMs)).toEqual([4000, 6000]);
		expect(resolveYDomain(pen, inside)).toEqual([3, 4]);
	});
	it("'bins' の系列に kind は付かない", () => {
		const d = drawableSeries(
			{ range: { startMs: 0, endMs: 10 }, series: { p: { binMs: 5, bins: [] } } },
			{ startMs: 0, endMs: 10 }
		);
		expect('kind' in d.series.p).toBe(false);
	});
	it('samplesForDraw: 外が無ければ足さない', () => {
		expect(samplesForDraw(bins, { startMs: 0, endMs: 10000 })).toHaveLength(5);
	});
});

describe('bit の帯（samples）と欠測', () => {
	const x = (t: number) => t / 10;
	const range = { startMs: 0, endMs: 10000 };
	it('観測から次の観測までを値で塗る。OFF と欠測は別に出る', () => {
		// 0〜2000 ON、2000〜4000 OFF（記録あり）、6000〜8000 は空き（欠測）、8000 ON（最後は binMs 分）。
		const bins = [obs(0, 1), obs(2000, 0), obs(8000, 1)];
		expect(bitSpans(bins, 2000, x, range, 'samples')).toEqual([
			{ x: 0, width: 200, state: 'true' },
			{ x: 800, width: 200, state: 'true' }
		]);
		// 2000 の OFF は 6000 まで覆う（打ち切りは binMs x 2）。6000〜8000 が欠測、8000 の ON は 10000 まで覆う。
		expect(bitMissingSpans(bins, 2000, x, range, 'samples')).toEqual([{ x: 600, width: 200 }]);
	});
	it('null の観測の区間は欠測', () => {
		const bins = [obs(0, 1), obs(2000, null), obs(4000, 1)];
		expect(bitMissingSpans(bins, 2000, x, range, 'samples')).toEqual([
			{ x: 200, width: 200 },
			{ x: 600, width: 400 }
		]);
	});
	it("'bins' でも欠測が出る（OFF は覆う）", () => {
		const bins: Bin[] = [
			{ tMs: 0, min: 1, max: 1 },
			{ tMs: 1000, min: 0, max: 0 },
			{ tMs: 2000, min: null, max: null },
			{ tMs: 5000, min: 1, max: 1 }
		];
		expect(bitMissingSpans(bins, 1000, x, range)).toEqual([
			{ x: 200, width: 300 },
			{ x: 600, width: 400 }
		]);
	});
	it('有効範囲の外は何も描かない。データが無ければ範囲全体', () => {
		expect(bitMissingSpans([], 1000, x, { startMs: 2000, endMs: 4000 })).toEqual([
			{ x: 200, width: 200 }
		]);
		expect(bitMissingSpans([], 1000, x, { startMs: 5, endMs: 5 })).toEqual([]);
	});
	it('有効範囲より前の観測が範囲の中へ延びる', () => {
		const iv = sampleIntervals([obs(-1000, 1), obs(1000, 0)], 2000, { startMs: 0, endMs: 5000 });
		expect(iv).toEqual([
			{ s: 0, e: 1000, value: 1 },
			{ s: 1000, e: 3000, value: 0 }
		]);
	});
});

describe('指摘の修正（#1276）', () => {
	const eff = { startMs: 1000, endMs: 2000 };
	it('カーソルは有効範囲の外の観測に吸着しない（時刻 tMs を返す）', () => {
		const d = drawableSeries(
			{
				range: eff,
				series: {
					p: { kind: 'samples', binMs: 1000, bins: [obs(999, 9), obs(1100, 5), obs(2001, 8)] }
				}
			},
			{ startMs: 0, endMs: 3000 }
		);
		expect(d.series.p.bins.map((b) => b.tMs)).toEqual([999, 1100, 2001]);
		// 許容 200 でも 999（範囲の外）ではなく 1100 に吸着する。許容 50 なら範囲の中に候補が無く「—」。
		const r = cursorReadout([pen], d.series, 1000, d.effective, 200)[0];
		expect(r.tMs).toBe(1100);
		const r0 = cursorReadout([pen], d.series, 1000, d.effective, 50)[0];
		expect(r0.text).toBe('—');
		expect(r0.tMs).toBeNull();
		const r2 = cursorReadout([pen], d.series, 1150, d.effective, 200)[0];
		expect(r2.text).toBe('5.0 V');
		expect(r2.tMs).toBe(1100);
	});
	it('同じ観測は描画と同じ 1 値（min=0,max=1 は 0）。差も観測どうし', () => {
		const s: TrendSeries = {
			kind: 'samples',
			binMs: 1000,
			bins: [
				{ tMs: 1100, min: 0, max: 1 },
				{ tMs: 1600, min: 4, max: 9 }
			]
		};
		const bitPen: TrendPen = { ...pen, bit: true } as TrendPen;
		const [r] = cursorReadout([bitPen], { p: s }, 1100, eff, 0);
		expect([r.min, r.max]).toEqual([0, 0]);
		expect(r.text).not.toContain('〜');
		const a = cursorReadout([pen], { p: s }, 1100, eff, 0)[0];
		const b = cursorReadout([pen], { p: s }, 1600, eff, 0)[0];
		expect(a.text).toBe('0.0 V');
		expect(cursorDiff(pen, a, b)).toBe('+4.0 V');
	});
	it('線の clip は有効範囲の幅（取得 [1000,5000)、表示 [0,6000)）', () => {
		const x = (t: number) => (t / 6000) * 600;
		expect(effectiveClip({ startMs: 1000, endMs: 5000 }, x, 600)).toEqual({ x: 100, width: 400 });
		expect(effectiveClip({ startMs: -1000, endMs: 9000 }, x, 600)).toEqual({ x: 0, width: 600 });
		expect(effectiveClip(null, x, 600)).toBeNull();
	});
});
