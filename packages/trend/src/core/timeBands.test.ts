import { describe, expect, it } from 'vitest';
import { timeBandLayout, timeMarkLayout, timeSummaryText, type TrendTimeBand } from './timeBands';
import type { TrendPen } from './pens';
import type { BitBandSlot } from './bitBands';

const range = { startMs: 1000, endMs: 2000 };
const W = 100; // 10ms = 1px
const pen = (id: string, extra: Partial<TrendPen> = {}): TrendPen => ({
	id,
	label: id,
	colorSlot: 1,
	...extra
});
const band = (fromMs: number, toMs: number, penId?: string | null): TrendTimeBand => ({
	fromMs,
	toMs,
	colorVar: 'var(--x)',
	label: 'B',
	penId
});

describe('timeBandLayout', () => {
	it('表示範囲で切る（外・不正・幅 0 は出さない）', () => {
		const l = timeBandLayout(
			[
				band(500, 1500),
				band(1900, 3000),
				band(0, 900),
				band(2000, 3000),
				band(1500, 1500),
				band(1600, 1500),
				band(Number.NaN, 1500)
			],
			[],
			range,
			W,
			[],
			200
		);
		expect(l.background.map((r) => [r.x, r.width])).toEqual([
			[0, 50],
			[90, 10]
		]);
	});
	it('幅が 1px 未満は 1px に広げる', () => {
		const l = timeBandLayout([band(1500, 1503)], [], range, W, [], 200);
		expect(l.background[0].width).toBe(1);
	});
	it('帯が null・範囲が不正なら空', () => {
		const empty = { background: [], bitOverlays: [], lanes: [] };
		expect(timeBandLayout(null, [], range, W, [], 200)).toEqual(empty);
		expect(timeBandLayout([band(1100, 1200)], [], { startMs: 5, endMs: 5 }, W, [], 200)).toEqual(
			empty
		);
	});
	it('penId 付きのアナログのペンは上端の列に、帯を持つペンだけを並びの順に積む', () => {
		const pens = [pen('a'), pen('b'), pen('c')];
		const l = timeBandLayout(
			[band(1100, 1200, 'c'), band(1300, 1400, 'a')],
			pens,
			range,
			W,
			[],
			200
		);
		expect(l.lanes.map((r) => [r.y, r.height, r.x])).toEqual([
			[5, 4, 10],
			[0, 4, 30]
		]);
	});
	it('bit の帯のペンはそのペンの帯の高さの中', () => {
		const pens = [pen('a'), pen('bit', { bit: true, bitStyle: 'band' })];
		const slots: BitBandSlot[] = [{ penIndex: 1, y: 70, height: 16, showLabel: true }];
		const l = timeBandLayout([band(1100, 1200, 'bit')], pens, range, W, slots, 200);
		expect(l.bitOverlays).toEqual([
			{ x: 10, width: 10, y: 70, height: 16, colorVar: 'var(--x)', label: 'B' }
		]);
		expect(l.lanes).toEqual([]);
	});
	it('列が多くてアナログの域の半分を超えるときは縮めて収める', () => {
		const pens = Array.from({ length: 15 }, (_, i) => pen(`p${i}`));
		const bands = pens.map((p) => band(1100, 1200, p.id));
		const l = timeBandLayout(bands, pens, range, W, [], 52);
		expect(l.lanes).toHaveLength(15);
		const last = l.lanes[14];
		expect(last.y + last.height).toBeLessThanOrEqual(26 + 1e-9);
		expect(l.lanes[0].y).toBe(0);
	});
	it('analogHeight が 0 以下なら列は出さない（背景は出す）', () => {
		const l = timeBandLayout(
			[band(1100, 1200, 'a'), band(1100, 1200)],
			[pen('a')],
			range,
			W,
			[],
			0
		);
		expect(l.lanes).toEqual([]);
		expect(l.background).toHaveLength(1);
	});
	it('ペンが居ない penId は出さない', () => {
		const l = timeBandLayout([band(1100, 1200, 'zz')], [pen('a')], range, W, [], 200);
		expect(l).toEqual({ background: [], bitOverlays: [], lanes: [] });
	});
});

describe('timeMarkLayout', () => {
	const m = (tMs: number, extra = {}) => ({ tMs, label: 'M', ...extra });
	it('表示範囲 [start, end) の外は出さない', () => {
		const r = timeMarkLayout([m(999), m(1000), m(1500), m(2000)], [], range, W);
		expect(r.map((x) => x.x)).toEqual([0, 50]);
	});
	it('名前は前に出した印から 48px 未満なら省く（線は残す）', () => {
		const r = timeMarkLayout([m(1000), m(1300), m(1480), m(1490)], [], range, W);
		expect(r.map((x) => [x.x, x.showLabel])).toEqual([
			[0, true],
			[30, false],
			[48, true],
			[49, false]
		]);
	});
	it('名前の側: 右端の近くは左、左端・中ほどは右', () => {
		const r = timeMarkLayout([m(1000), m(1500), m(1990)], [], range, W);
		expect(r.map((x) => [x.x, x.labelSide])).toEqual([
			[0, 'right'],
			[50, 'right'],
			[99, 'left']
		]);
	});
	it('同じ時刻の印は 1 本にまとめ、名前をつなぐ（色は最初）', () => {
		const r = timeMarkLayout(
			[
				m(1500, { label: 'A', colorVar: 'var(--a)' }),
				m(1500, { label: 'B', colorVar: 'var(--b)' }),
				m(1500, { label: 'C' }),
				m(1600, { label: 'D' })
			],
			[],
			range,
			W
		);
		expect(r).toHaveLength(2);
		expect(r[0]).toMatchObject({ label: 'A・B・C', colorVar: 'var(--a)', count: 3 });
		expect(r[1].count).toBe(1);
	});
	it('色は colorVar、無ければペンの色、無ければ text-muted', () => {
		const pens = [pen('a', { color: '#123456' })];
		const r = timeMarkLayout(
			[
				m(1100, { colorVar: 'var(--c)', penId: 'a' }),
				m(1200, { penId: 'a' }),
				m(1300),
				m(1400, { penId: 'zz' })
			],
			pens,
			range,
			W
		);
		expect(r.map((x) => x.colorVar)).toEqual([
			'var(--c)',
			'#123456',
			'var(--banto-text-muted)',
			'var(--banto-text-muted)'
		]);
	});
});

describe('timeSummaryText', () => {
	it('0 件は空、あれば件数', () => {
		expect(timeSummaryText(0, 0)).toBe('');
		expect(timeSummaryText(3, 1)).toBe('時刻の帯 3・印 1。');
		expect(timeSummaryText(0, 2)).toBe('印 2。');
	});
});
