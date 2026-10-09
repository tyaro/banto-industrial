/**
 * バー・計器（R1-D の D-2）の判断の表テスト（`meterLogic.ts`）。
 *
 * とくに固定したいこと:
 * - レンジの決め方（Q3）: 工学値レンジ → LL..HH → 未設定。タグを読めていない
 *   ときは「未設定」と言わない。
 * - 値が無いとき（`bad` / `stale` / `invalid` / 未収集）は棒を描かない（0 の
 *   位置に描かない。#414 段階2）。
 * - レンジ外は端に丸め、文字で知らせる。
 * - 色の段階（HH/LL → danger、H/L → warning）と、計器（banto の `gaugeColorVar`）の
 *   色が文字（`thresholdLevel`）と必ず一致する。
 */
import { gaugeColorVar } from '@banto/charts';
import { describe, expect, it } from 'vitest';
import type { CurrentSampleView } from '../banto/collectAdmin';
// #532: しきい値はタグではなく記録計の側の設定。画面はタグにしきい値を添えた形
// （`withThresholds`）で判定するので、テストもその形で組む。
import type { TagWithThresholds as Tag } from '../banto/tagThresholdsAdmin';
import {
	OVER_RANGE_LABEL,
	RANGE_NO_TAG_MESSAGE,
	RANGE_UNSET_MESSAGE,
	UNDER_RANGE_LABEL,
	barGeometry,
	gaugeThresholds,
	groupThresholdMarks,
	levelTone,
	meterDescription,
	meterView,
	scaleText,
	thresholdText,
	meterViews,
	resolveMeterRange,
	scaleTicks,
	thresholdMarks,
	toneColorVar,
	type LevelTone,
	type MeterRange
} from './meterLogic';
import { penView, thresholdLevel, type ThresholdLevel } from './monitorLogic';

function tag(overrides: Partial<Tag> = {}): Tag {
	return {
		id: 1,
		name: '温度',
		collectionGroupId: 10,
		address: '40001',
		dataType: 'u16',
		rawLo: null,
		rawHi: null,
		engLo: null,
		engHi: null,
		unit: '℃',
		decimals: 1,
		thresholdH: null,
		thresholdHh: null,
		thresholdL: null,
		thresholdLl: null,
		enabled: true,
		revision: 1,
		...overrides
	};
}

const scaled = (engLo: number, engHi: number) => tag({ rawLo: 0, rawHi: 4095, engLo, engHi });

describe('resolveMeterRange（Q3: 工学値レンジ → LL..HH → 未設定）', () => {
	it.each<[string, Tag | undefined, MeterRange]>([
		['工学値レンジ', scaled(0, 100), { kind: 'ok', min: 0, max: 100, source: 'eng' }],
		[
			'工学値レンジが逆向き（engLo > engHi）でも小さい方が下端',
			scaled(100, -20),
			{ kind: 'ok', min: -20, max: 100, source: 'eng' }
		],
		[
			'工学値レンジがしきい値より優先',
			tag({ rawLo: 0, rawHi: 10, engLo: 0, engHi: 200, thresholdLl: 10, thresholdHh: 90 }),
			{ kind: 'ok', min: 0, max: 200, source: 'eng' }
		],
		[
			'工学値レンジの幅 0 はレンジにならず LL..HH へ',
			tag({ rawLo: 0, rawHi: 10, engLo: 5, engHi: 5, thresholdLl: 10, thresholdHh: 90 }),
			{ kind: 'ok', min: 10, max: 90, source: 'thresholds' }
		],
		[
			'工学値レンジが無ければ LL..HH',
			tag({ thresholdLl: -5, thresholdL: 0, thresholdH: 80, thresholdHh: 90 }),
			{ kind: 'ok', min: -5, max: 90, source: 'thresholds' }
		],
		['LL だけ（HH 無し）は未設定', tag({ thresholdLl: 0, thresholdH: 80 }), { kind: 'unset' }],
		['HH だけ（LL 無し）は未設定', tag({ thresholdHh: 90 }), { kind: 'unset' }],
		[
			'LL === HH は未設定（幅が無い）',
			tag({ thresholdLl: 50, thresholdHh: 50 }),
			{ kind: 'unset' }
		],
		[
			'H/L だけでは決めない（LL..HH のみ）',
			tag({ thresholdL: 0, thresholdH: 100 }),
			{ kind: 'unset' }
		],
		['何も無ければ未設定', tag(), { kind: 'unset' }],
		// #551: bit は工学値レンジが無くても 0〜1（False〜True）。
		['bit は既定で 0〜1', tag({ dataType: 'bit' }), { kind: 'ok', min: 0, max: 1, source: 'bit' }],
		[
			'bit に LL..HH を置いても 0〜1',
			tag({ dataType: 'bit', thresholdLl: 5, thresholdHh: 9 }),
			{ kind: 'ok', min: 0, max: 1, source: 'bit' }
		],
		[
			'bit でも工学値レンジがあればそれが先',
			tag({ dataType: 'bit', rawLo: 0, rawHi: 1, engLo: 0, engHi: 10 }),
			{ kind: 'ok', min: 0, max: 10, source: 'eng' }
		],
		[
			'bit 以外は従来どおり未設定（i16 は 0〜1 にしない）',
			tag({ dataType: 'i16' }),
			{ kind: 'unset' }
		],
		['タグを読めていなければ noTag（未設定と言わない）', undefined, { kind: 'noTag' }]
	])('%s', (_label, t, expected) => {
		expect(resolveMeterRange(t)).toEqual(expected);
	});
});

describe('levelTone / toneColorVar（HH/LL → danger、H/L → warning）', () => {
	it.each<[ThresholdLevel, LevelTone, string]>([
		['HH', 'danger', 'var(--banto-danger)'],
		['LL', 'danger', 'var(--banto-danger)'],
		['H', 'warning', 'var(--banto-warning)'],
		['L', 'warning', 'var(--banto-warning)'],
		['normal', 'normal', 'var(--banto-primary)'],
		['none', 'normal', 'var(--banto-primary)']
	])('%s → %s', (level, tone, colorVar) => {
		expect(levelTone(level)).toBe(tone);
		expect(toneColorVar(levelTone(level))).toBe(colorVar);
	});
});

describe('gaugeThresholds（計器の色が文字と必ず一致する）', () => {
	it('H/HH/L/LL を warning/danger/warningLow/dangerLow に写す', () => {
		expect(
			gaugeThresholds(tag({ thresholdLl: 5, thresholdL: 10, thresholdH: 80, thresholdHh: 90 }))
		).toEqual({ warning: 80, danger: 90, warningLow: 10, dangerLow: 5 });
		expect(gaugeThresholds(tag({ thresholdH: 80 }))).toEqual({ warning: 80 });
		expect(gaugeThresholds(tag())).toEqual({});
		expect(gaugeThresholds(undefined)).toEqual({});
	});

	// タグの検証は LL <= L <= H <= HH（等号あり）。等号の組み合わせを含めて、
	// 値を境目の上下・ちょうどで総当たりし、banto の gaugeColorVar の色が
	// thresholdLevel → levelTone の色と一致することを確かめる。
	const tags: [string, Tag][] = [
		['4 つ全部', tag({ thresholdLl: 5, thresholdL: 10, thresholdH: 80, thresholdHh: 90 })],
		['上側だけ', tag({ thresholdH: 80, thresholdHh: 90 })],
		['下側だけ', tag({ thresholdLl: 5, thresholdL: 10 })],
		['H だけ', tag({ thresholdH: 0 })],
		[
			'LL === L === H（等号）',
			tag({ thresholdLl: 50, thresholdL: 50, thresholdH: 50, thresholdHh: 60 })
		],
		['LL === H（L なし）', tag({ thresholdLl: 50, thresholdH: 50 })],
		['L === H', tag({ thresholdLl: 10, thresholdL: 50, thresholdH: 50, thresholdHh: 90 })],
		['H === HH', tag({ thresholdH: 90, thresholdHh: 90 })],
		['LL === L', tag({ thresholdLl: 10, thresholdL: 10 })],
		['全部同じ', tag({ thresholdLl: 50, thresholdL: 50, thresholdH: 50, thresholdHh: 50 })],
		['負の値', tag({ thresholdLl: -50, thresholdL: -40, thresholdH: -10, thresholdHh: 0 })]
	];
	for (const [label, t] of tags) {
		it(label, () => {
			const points = [t.thresholdLl, t.thresholdL, t.thresholdH, t.thresholdHh].filter(
				(v): v is number => v !== null
			);
			const values = [-1000, 1000, ...points.flatMap((p) => [p - 0.5, p, p + 0.5, p - 1e-9])];
			for (const v of values) {
				const expected = toneColorVar(levelTone(thresholdLevel(v, t)));
				expect(gaugeColorVar(v, gaugeThresholds(t)), `値 ${v}`).toBe(expected);
			}
		});
	}
});

describe('barGeometry（値なしは描かない、レンジ外は端に丸める）', () => {
	const range = { min: 0, max: 200 };
	it.each<[string, number | null, ReturnType<typeof barGeometry>]>([
		['中', 50, { fill: 0.25, out: null }],
		['下端ちょうど（0 は 0 の高さで描く）', 0, { fill: 0, out: null }],
		['上端ちょうど', 200, { fill: 1, out: null }],
		['上限超え', 250, { fill: 1, out: 'over' }],
		['下限未満', -1, { fill: 0, out: 'under' }],
		['値なし（null）は描かない', null, null],
		['非有限は描かない', Number.NaN, null],
		['+∞ も描かない', Number.POSITIVE_INFINITY, null]
	])('%s', (_label, value, expected) => {
		expect(barGeometry(value, range)).toEqual(expected);
	});

	it('負のレンジ', () => {
		expect(barGeometry(-25, { min: -50, max: 0 })).toEqual({ fill: 0.5, out: null });
	});

	it('幅の無いレンジは描かない', () => {
		expect(barGeometry(1, { min: 1, max: 1 })).toBeNull();
	});
});

describe('scaleTicks / thresholdMarks', () => {
	it('目盛は両端を含み、間はきりのよい値', () => {
		expect(scaleTicks({ min: 0, max: 100 }).map((t) => t.value)).toEqual([0, 20, 40, 60, 80, 100]);
		const ticks = scaleTicks({ min: -5, max: 90 });
		expect(ticks[0]).toEqual({ value: -5, position: 0 });
		expect(ticks[ticks.length - 1]).toEqual({ value: 90, position: 1 });
		for (const t of ticks) {
			expect(t.value).toBeGreaterThanOrEqual(-5);
			expect(t.value).toBeLessThanOrEqual(90);
		}
		// 位置は値に比例する。
		const mid = ticks.find((t) => t.value === 40);
		expect(mid?.position).toBeCloseTo(45 / 95);
	});

	it('幅の無いレンジは目盛なし', () => {
		expect(scaleTicks({ min: 3, max: 3 })).toEqual([]);
	});

	it('しきい値の印はレンジの中（両端を含む）だけ、上から', () => {
		const t = tag({ thresholdLl: -10, thresholdL: 0, thresholdH: 80, thresholdHh: 100 });
		expect(thresholdMarks(t, { min: 0, max: 100 })).toEqual([
			{ level: 'HH', value: 100, position: 1, tone: 'danger' },
			{ level: 'H', value: 80, position: 0.8, tone: 'warning' },
			{ level: 'L', value: 0, position: 0, tone: 'warning' }
		]);
		expect(thresholdMarks(undefined, { min: 0, max: 100 })).toEqual([]);
	});
});

describe('meterView（ペン 1 本のバー・計器の表示）', () => {
	const sample = (s: Partial<CurrentSampleView>): CurrentSampleView => ({
		value: 1,
		ptimeMs: 1000,
		quality: 'good',
		lastGoodMs: 1000,
		...s
	});
	const pen = { tagId: 1, colorSlot: null };
	const withRange = tag({
		rawLo: 0,
		rawHi: 4095,
		engLo: 0,
		engHi: 100,
		thresholdH: 80,
		thresholdHh: 90
	});

	it('値あり・範囲内: 棒を描き、色は primary', () => {
		const v = meterView(penView(pen, 0, sample({ value: 50 }), withRange), withRange);
		expect(v.range).toEqual({ kind: 'ok', min: 0, max: 100, source: 'eng' });
		expect(v.bar).toEqual({ fill: 0.5, out: null });
		expect(v.tone).toBe('normal');
		expect(v.outLabel).toBeNull();
		expect(v.rangeMessage).toBeNull();
		expect(v.gaugeThresholds).toEqual({ warning: 80, danger: 90 });
		expect(v.decimals).toBe(1);
	});

	it('HH を超えてレンジ外: danger・上限超え', () => {
		const v = meterView(penView(pen, 0, sample({ value: 120 }), withRange), withRange);
		expect(v.level).toBe('HH');
		expect(v.tone).toBe('danger');
		expect(v.bar).toEqual({ fill: 1, out: 'over' });
		expect(v.outLabel).toBe(OVER_RANGE_LABEL);
	});

	it('下限未満', () => {
		const v = meterView(penView(pen, 0, sample({ value: -3 }), withRange), withRange);
		expect(v.outLabel).toBe(UNDER_RANGE_LABEL);
	});

	it.each<[string, CurrentSampleView | undefined]>([
		['bad', sample({ value: 42, quality: 'bad' })],
		['stale', sample({ value: 42, quality: 'stale' })],
		['invalid', sample({ value: null, quality: 'invalid', ptimeMs: null, lastGoodMs: null })],
		['未収集', undefined],
		['good で値なし', sample({ value: null })]
	])('値が無い（%s）: 棒を描かず「—」、レンジ外とも言わない', (_label, s) => {
		const v = meterView(penView(pen, 0, s, withRange), withRange);
		expect(v.display).toBe('—');
		expect(v.value).toBeNull();
		expect(v.bar).toBeNull();
		expect(v.outLabel).toBeNull();
		expect(v.tone).toBe('normal');
		// レンジ・目盛は出す（枠は描き、棒だけ描かない）。
		expect(v.range.kind).toBe('ok');
		expect(v.ticks.length).toBeGreaterThan(0);
	});

	it('レンジ未設定: 棒も目盛も無く、理由の文言。値の文字は出す', () => {
		const t = tag({ thresholdH: 0 });
		const v = meterView(penView(pen, 0, sample({ value: 7 }), t), t);
		expect(v.range).toEqual({ kind: 'unset' });
		expect(v.rangeMessage).toBe(RANGE_UNSET_MESSAGE);
		expect(v.bar).toBeNull();
		expect(v.ticks).toEqual([]);
		expect(v.marks).toEqual([]);
		expect(v.display).toBe('7.0');
		expect(v.level).toBe('H');
		expect(v.tone).toBe('warning');
	});

	it('タグを読めていない: noTag の文言（未設定と言わない）、小数桁は不明', () => {
		const v = meterView(penView(pen, 0, sample({ value: 7 }), undefined), undefined);
		expect(v.rangeMessage).toBe(RANGE_NO_TAG_MESSAGE);
		expect(v.decimals).toBeNull();
		expect(v.display).toBe('7');
	});

	it('meterViews はペンの順でタグを引き当てる', () => {
		const a = tag({ id: 1, rawLo: 0, rawHi: 1, engLo: 0, engHi: 10 });
		const b = tag({ id: 2 });
		const views = meterViews(
			[
				penView({ tagId: 2, colorSlot: null }, 0, sample({ value: 1 }), b),
				penView({ tagId: 1, colorSlot: null }, 1, sample({ value: 5 }), a),
				penView({ tagId: 3, colorSlot: null }, 2, sample({ value: 5 }), undefined)
			],
			[a, b]
		);
		expect(views.map((v) => [v.tagId, v.range.kind])).toEqual([
			[2, 'unset'],
			[1, 'ok'],
			[3, 'noTag']
		]);
	});
});

describe('groupThresholdMarks（同じ値のしきい値の名前を 1 つにまとめる、#535）', () => {
	const range = { min: 0, max: 100 };
	const groups = (t: Tag) =>
		groupThresholdMarks(thresholdMarks(t, range)).map((g) => [g.label, g.value, g.tone]);

	it.each<[string, Tag, [string, number, LevelTone][]]>([
		[
			'4 つとも別の値: まとめない（上から）',
			tag({ thresholdLl: 5, thresholdL: 10, thresholdH: 80, thresholdHh: 90 }),
			[
				['HH', 90, 'danger'],
				['H', 80, 'warning'],
				['L', 10, 'warning'],
				['LL', 5, 'danger']
			]
		],
		[
			'LL === L === H: 1 つ（下から LL/L/H、重い方の danger）',
			tag({ thresholdLl: 50, thresholdL: 50, thresholdH: 50, thresholdHh: 60 }),
			[
				['HH', 60, 'danger'],
				['LL/L/H', 50, 'danger']
			]
		],
		[
			'L === H: warning どうし',
			tag({ thresholdLl: 10, thresholdL: 50, thresholdH: 50, thresholdHh: 90 }),
			[
				['HH', 90, 'danger'],
				['L/H', 50, 'warning'],
				['LL', 10, 'danger']
			]
		],
		['H === HH', tag({ thresholdH: 90, thresholdHh: 90 }), [['H/HH', 90, 'danger']]],
		['LL === L', tag({ thresholdLl: 10, thresholdL: 10 }), [['LL/L', 10, 'danger']]],
		[
			'全部同じ',
			tag({ thresholdLl: 50, thresholdL: 50, thresholdH: 50, thresholdHh: 50 }),
			[['LL/L/H/HH', 50, 'danger']]
		],
		['しきい値なし', tag(), []]
	])('%s', (_label, t, expected) => {
		expect(groups(t)).toEqual(expected);
	});

	it('meterView の markGroups に入る（位置はまとめた値の位置）', () => {
		const t = tag({
			rawLo: 0,
			rawHi: 1,
			engLo: 0,
			engHi: 100,
			thresholdLl: 50,
			thresholdL: 50,
			thresholdH: 50
		});
		const v = meterView(penView({ tagId: 1, colorSlot: null }, 0, undefined, t), t);
		expect(v.marks).toHaveLength(3);
		expect(v.markGroups).toEqual([
			{ levels: ['LL', 'L', 'H'], label: 'LL/L/H', value: 50, position: 0.5, tone: 'danger' }
		]);
	});
});

describe('bit のバー・計器（#551）', () => {
	const bit = tag({ dataType: 'bit', unit: null, decimals: 0 });
	const sample = (value: number | null): CurrentSampleView => ({
		value,
		ptimeMs: 1000,
		quality: 'good',
		lastGoodMs: 1000
	});
	const view = (value: number | null, t: Tag = bit) =>
		meterView(penView({ tagId: 1, colorSlot: null }, 0, sample(value), t), t);

	it('「レンジ未設定」にならず、棒は 0 で空・1 で満タン、目盛は両端の 2 本だけ', () => {
		const off = view(0);
		const on = view(1);
		expect(off.rangeMessage).toBeNull();
		expect(off.range.kind).toBe('ok');
		expect(off.bar).toEqual({ fill: 0, out: null });
		expect(on.bar).toEqual({ fill: 1, out: null });
		expect(on.ticks).toEqual([
			{ value: 0, position: 0 },
			{ value: 1, position: 1 }
		]);
		expect(off.display).toBe('False');
		expect(on.display).toBe('True');
	});

	it('非 bit の 0〜1 のレンジの目盛は従来どおり（間の目盛が出る）', () => {
		const t = tag({ rawLo: 0, rawHi: 1, engLo: 0, engHi: 1 });
		expect(view(0, t).ticks.length).toBeGreaterThan(2);
	});

	it('scaleText: bit の既定レンジは False / True、それ以外は数値', () => {
		expect(scaleText(view(1), 0)).toBe('False');
		expect(scaleText(view(1), 1)).toBe('True');
		const num = view(1, tag({ rawLo: 0, rawHi: 1, engLo: 0, engHi: 1 }));
		expect(scaleText(num, 1)).toBe('1.0');
		// しきい値の数値は bit でも数値のまま。
		expect(thresholdText(view(1), 1)).toBe('1');
	});

	it('値なしは棒なし（0 の位置に描かない）でレンジは 0〜1', () => {
		const v = view(null);
		expect(v.bar).toBeNull();
		expect(v.range).toEqual({ kind: 'ok', min: 0, max: 1, source: 'bit' });
	});

	it('支援技術向けの説明は False〜True', () => {
		expect(meterDescription(view(1))).toBe('レンジ False〜True（bit の既定）。しきい値: なし');
	});
});

describe('meterDescription（レンジとしきい値を文で伝える、#535）', () => {
	const view = (t: Tag | undefined) =>
		meterView(penView({ tagId: 1, colorSlot: null }, 0, undefined, t), t);

	it.each<[string, Tag | undefined, string]>([
		[
			'工学値レンジ + 4 つのしきい値（単位・小数桁つき）',
			tag({
				rawLo: 0,
				rawHi: 1,
				engLo: 0,
				engHi: 100,
				thresholdLl: 5,
				thresholdL: 10,
				thresholdH: 80,
				thresholdHh: 90
			}),
			'レンジ 0.0 ℃〜100.0 ℃（工学値レンジ）。しきい値: HH 90.0 ℃、H 80.0 ℃、L 10.0 ℃、LL 5.0 ℃'
		],
		[
			'LL..HH のレンジ',
			tag({ unit: null, decimals: 0, thresholdLl: 0, thresholdHh: 1 }),
			'レンジ 0〜1（しきい値の LL〜HH）。しきい値: HH 1、LL 0'
		],
		[
			'レンジの外のしきい値も言う（印は出ないので文で補う）',
			tag({ unit: null, decimals: 0, rawLo: 0, rawHi: 1, engLo: 0, engHi: 10, thresholdH: 50 }),
			'レンジ 0〜10（工学値レンジ）。しきい値: H 50'
		],
		[
			'同じ値のしきい値も 1 つずつ言う',
			tag({
				unit: null,
				decimals: 0,
				thresholdLl: 50,
				thresholdL: 50,
				thresholdH: 50,
				thresholdHh: 60
			}),
			'レンジ 50〜60（しきい値の LL〜HH）。しきい値: HH 60、H 50、L 50、LL 50'
		],
		['レンジ未設定・しきい値なし', tag({ unit: null }), `${RANGE_UNSET_MESSAGE}。しきい値: なし`],
		['タグを読めていない', undefined, `${RANGE_NO_TAG_MESSAGE}。しきい値: なし`]
	])('%s', (_label, t, expected) => {
		expect(meterDescription(view(t))).toBe(expected);
	});
});
