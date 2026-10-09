/**
 * リアルタイムトレンドの純関数（`trendLogic.ts`）の表。
 *
 * 格子（刻みの選び方・揃え方）・進めると古い行を捨てる・値の無い刻みは `null`・
 * 履歴と現在値の合わせ方・しきい値の帯（無い・同じ値・端の無い側）・端末ごとの
 * 時間窓（壊れた localStorage を含む）・説明文と注記。
 */
import { describe, expect, it } from 'vitest';
import { HISTORY_MAX_BINS } from '../banto/collectAdmin';
import { TIME_WINDOW_OPTIONS } from '../../routes/(app)/groups/groupsPageLogic';
import {
	DEFAULT_TREND_WINDOW_SEC,
	HISTORY_UNAVAILABLE_NOTE,
	SIMULATION_NOTE,
	TREND_DANGER_COLOR,
	TREND_PLOT_MARGIN_PX,
	TREND_WARNING_COLOR,
	TREND_WINDOWS_SEC,
	TREND_WINDOW_STORAGE_KEY,
	advanceTrend,
	alignToStep,
	appendLive,
	chooseStepMs,
	emptyTrendBuffer,
	groupDefaultWindowSec,
	historyPointValue,
	historyRequest,
	loadTrendWindowOverride,
	maxTrendRows,
	mergeHistory,
	penLegendLabel,
	resolveBandPen,
	resolveTrendWindowSec,
	saveTrendWindowOverride,
	observeServerClock,
	serverClockNow,
	type ServerClock,
	thresholdBands,
	trendDescription,
	trendNotices,
	trendPenInfos,
	trendYFormatter,
	isAllBitTrend,
	trendWindowLabel,
	valueExtent,
	type TrendBuffer,
	type TrendPenInfo
} from './trendLogic';
import type { ThresholdFields } from '../banto/tagThresholdsAdmin';
import { INITIAL_VALUES_STATE, applyValuesOutcome, type ValuesState } from './valuesPoller.svelte';

const NONE: ThresholdFields = {
	thresholdHh: null,
	thresholdH: null,
	thresholdL: null,
	thresholdLl: null
};

const values = (buffer: TrendBuffer) => buffer.rows.map((row) => row.values);
const times = (buffer: TrendBuffer) => buffer.rows.map((row) => row.t);

describe('時間窓の選択肢', () => {
	it('グループ設定画面（= Rust の TREND_TIME_WINDOWS_SEC）と同じ', () => {
		expect([...TREND_WINDOWS_SEC]).toEqual(TIME_WINDOW_OPTIONS.map((o) => o.value));
		expect(TREND_WINDOWS_SEC).toContain(DEFAULT_TREND_WINDOW_SEC);
	});

	it.each([
		[60, '1 分'],
		[600, '10 分'],
		[3600, '1 時間']
	])('%i 秒は「%s」', (sec, label) => {
		expect(trendWindowLabel(sec)).toBe(label);
	});
});

describe('chooseStepMs / maxTrendRows', () => {
	it.each([
		// [窓(秒), 幅, 周期, 本数, 刻み]
		['10 分・1280px・500ms・1 本 → 1 秒（600 行）', 600, 1280, 500, 1, 1000],
		['1 分・1280px・500ms → 1 秒（周期の 2 倍が下限）', 60, 1280, 500, 1, 1000],
		['1 分・1280px・1 秒 → 2 秒', 60, 1280, 1000, 1, 2000],
		['10 分・400px・500ms → 2 秒（336 行に収める）', 600, 400, 500, 1, 2000],
		['1 時間・1280px・1 秒 → 5 秒（720 行）', 3600, 1280, 1000, 1, 5000],
		['1 時間・2600px・500ms・8 本 → 5 秒（8000/8 = 1000 行以下）', 3600, 2600, 500, 8, 5000],
		['1 時間・100px → 最大の 60 秒', 3600, 100, 500, 1, 60_000],
		['5 分・1280px・5 秒周期 → 10 秒', 300, 1280, 5000, 1, 10_000]
	])('%s', (_name, windowSec, widthPx, pollPeriodMs, tagCount, expected) => {
		const step = chooseStepMs({ windowMs: windowSec * 1000, widthPx, pollPeriodMs, tagCount });
		expect(step).toBe(expected);
		// 選んだ刻みの行数は（最大の候補で諦めたとき以外）上限以下。
		if (step !== 60_000) {
			expect((windowSec * 1000) / step).toBeLessThanOrEqual(maxTrendRows(widthPx, tagCount));
		}
	});

	it('行数の上限は描画域の幅・HISTORY_MAX_BINS・本数あたりの予算の最小', () => {
		expect(maxTrendRows(1000, 1)).toBe(1000 - TREND_PLOT_MARGIN_PX);
		expect(maxTrendRows(5000, 1)).toBe(HISTORY_MAX_BINS);
		expect(maxTrendRows(5000, 8)).toBe(1000);
		expect(maxTrendRows(0, 1)).toBe(1);
	});

	it('どの窓でも、行数が整数になる', () => {
		for (const sec of TREND_WINDOWS_SEC) {
			for (const width of [300, 800, 1280, 2000]) {
				for (const period of [500, 1000, 2000, 5000]) {
					const step = chooseStepMs({
						windowMs: sec * 1000,
						widthPx: width,
						pollPeriodMs: period,
						tagCount: 3
					});
					expect(Number.isInteger((sec * 1000) / step)).toBe(true);
				}
			}
		}
	});
});

describe('格子（emptyTrendBuffer / advanceTrend）', () => {
	it('空の格子は窓の行数ぶんの null で、最後の行が今の刻み', () => {
		const b = emptyTrendBuffer(1000, 5000, 2, 10_500);
		expect(times(b)).toEqual([6000, 7000, 8000, 9000, 10_000]);
		expect(values(b).every((v) => v.length === 2 && v.every((x) => x === null))).toBe(true);
		expect(alignToStep(10_999, 1000)).toBe(10_000);
	});

	it.each([
		['同じ刻みなら変わらない', 10_900, [6000, 7000, 8000, 9000, 10_000]],
		['時計が戻っても変わらない', 3000, [6000, 7000, 8000, 9000, 10_000]],
		[
			'2 刻み進めると古い 2 行を捨て、null を 2 行足す',
			12_100,
			[8000, 9000, 10_000, 11_000, 12_000]
		],
		['窓より長く空いたら作り直す', 60_000, [56_000, 57_000, 58_000, 59_000, 60_000]]
	])('%s', (_name, now, expected) => {
		const b = emptyTrendBuffer(1000, 5000, 1, 10_500);
		const next = advanceTrend(b, now);
		expect(times(next)).toEqual(expected);
		expect(next.rows).toHaveLength(5);
	});

	it('進めても古い行の値は残り、足した刻みは null（値の無い刻み = 線が切れる）', () => {
		let b = emptyTrendBuffer(1000, 5000, 1, 10_000);
		b = appendLive(b, 10_000, [1]);
		b = appendLive(b, 13_200, [4]);
		expect(values(b)).toEqual([[null], [1], [null], [null], [4]]);
	});
});

describe('appendLive', () => {
	it('同じ刻みに 2 回来たら後の値。undefined はそのまま、非有限は null', () => {
		let b = emptyTrendBuffer(1000, 3000, 3, 10_000);
		b = appendLive(b, 10_100, [1, 2, 3]);
		b = appendLive(b, 10_600, [5, undefined as unknown as null, Number.NaN]);
		expect(values(b).at(-1)).toEqual([5, 2, null]);
		expect(b.liveFromT).toBe(10_000);
	});

	it('0 は 0 のまま（null と区別する）', () => {
		const b = appendLive(emptyTrendBuffer(1000, 3000, 2, 0), 0, [0, null]);
		expect(values(b).at(-1)).toEqual([0, null]);
	});

	it('liveFromT は最初に書いた刻みのまま', () => {
		let b = emptyTrendBuffer(1000, 5000, 1, 10_000);
		b = appendLive(b, 10_000, [1]);
		b = appendLive(b, 12_000, [2]);
		expect(b.liveFromT).toBe(10_000);
	});
});

describe('historyPointValue（最小と最大の中点）', () => {
	it.each([
		[{ min: 2, max: 4 }, 3],
		[{ min: 5, max: 5 }, 5],
		[{ min: null, max: null }, null],
		[{ min: null, max: 7 }, 7],
		[{ min: 1, max: Number.POSITIVE_INFINITY }, 1],
		[{ min: 0, max: 0 }, 0]
	])('%o → %s', (point, expected) => {
		expect(historyPointValue(point)).toBe(expected);
	});
});

describe('mergeHistory（初期窓 + 現在値）', () => {
	const series = (tagId: number, binMs: number, points: [number, number | null][]) => ({
		tagId,
		simulation: false,
		binMs,
		points: points.map(([tMs, v]) => ({ tMs, min: v, max: v }))
	});

	it('点を刻みに書く。欠測は null、現在値の行（liveFromT 以後）は上書きしない', () => {
		let b = emptyTrendBuffer(1000, 6000, 1, 15_000); // 10_000..15_000
		b = appendLive(b, 15_000, [99]);
		const merged = mergeHistory(b, [7], {
			series: [
				series(7, 1000, [
					[10_000, 1],
					[11_000, 2],
					[12_000, null],
					[13_000, 4],
					[14_000, 5],
					[15_000, 6]
				])
			]
		});
		expect(values(merged)).toEqual([[1], [2], [null], [4], [5], [99]]);
	});

	it('収集周期が刻みより長いタグは、次の点まで値を保つ（線が細切れにならない）', () => {
		const b = emptyTrendBuffer(1000, 6000, 1, 15_000);
		const merged = mergeHistory(
			b,
			[7],
			{
				series: [
					series(7, 1, [
						[10_000, 1],
						[13_000, 2]
					])
				]
			},
			() => 3000
		);
		expect(values(merged)).toEqual([[1], [1], [1], [2], [2], [2]]);
	});

	it('窓より前の点は捨て、ペンの並びどおりに入れる。系列の無いペンは null のまま', () => {
		const b = emptyTrendBuffer(1000, 3000, 3, 12_000); // 10_000..12_000
		const merged = mergeHistory(b, [2, 1, 9], {
			series: [
				series(1, 1000, [
					[5000, 100],
					[10_000, 10],
					[11_000, 11]
				]),
				series(2, 1000, [[12_000, 20]])
			]
		});
		expect(values(merged)).toEqual([
			[null, 10, null],
			[null, 11, null],
			[20, null, null]
		]);
	});

	it('区間の幅が刻みより狭いときは同じ刻みの後の点', () => {
		const b = emptyTrendBuffer(1000, 2000, 1, 11_000);
		const merged = mergeHistory(b, [1], {
			series: [
				series(1, 500, [
					[10_000, 1],
					[10_500, 2],
					[11_000, 3]
				])
			]
		});
		expect(values(merged)).toEqual([[2], [3]]);
	});

	it('同じタグのペンが 2 本あれば両方に入る', () => {
		const b = emptyTrendBuffer(1000, 1000, 2, 10_000);
		const merged = mergeHistory(b, [1, 1], { series: [series(1, 1000, [[10_000, 4]])] });
		expect(values(merged)).toEqual([[4, 4]]);
	});
});

describe('historyRequest / valueExtent', () => {
	it('期間は格子の全体（両端を含む）、区間の数は行数、タグは重複を除く', () => {
		const b = emptyTrendBuffer(1000, 600_000, 2, 1_000_000);
		expect(historyRequest(b, [3, 3, 4])).toEqual({
			tagIds: [3, 4],
			fromMs: 401_000,
			toMs: 1_000_999,
			bins: 600
		});
		expect(historyRequest(b, [])).toBeNull();
	});

	it('値の範囲は null を飛ばす。値が無ければ null', () => {
		expect(
			valueExtent([
				{ t: 0, values: [null, 3] },
				{ t: 1, values: [-1, null] }
			])
		).toEqual({ min: -1, max: 3 });
		expect(valueExtent([{ t: 0, values: [null] }])).toBeNull();
	});
});

describe('thresholdBands', () => {
	it('設定なし・null は帯なし', () => {
		expect(thresholdBands(NONE)).toEqual([]);
		expect(thresholdBands(null)).toEqual([]);
	});

	it('4 つとも: HH..+∞ 危険、H..HH 注意、LL..L 注意、-∞..LL 危険', () => {
		expect(
			thresholdBands({ thresholdHh: 90, thresholdH: 80, thresholdL: 20, thresholdLl: 10 })
		).toEqual([
			{ from: 90, to: null, label: 'HH', colorVar: TREND_DANGER_COLOR },
			{ from: 80, to: 90, label: 'H', colorVar: TREND_WARNING_COLOR },
			{ from: 10, to: 20, label: 'L', colorVar: TREND_WARNING_COLOR },
			{ from: null, to: 10, label: 'LL', colorVar: TREND_DANGER_COLOR }
		]);
	});

	it('H だけ・L だけは反対の端が開く', () => {
		expect(thresholdBands({ ...NONE, thresholdH: 0 })).toEqual([
			{ from: 0, to: null, label: 'H', colorVar: TREND_WARNING_COLOR }
		]);
		expect(thresholdBands({ ...NONE, thresholdL: 5 })).toEqual([
			{ from: null, to: 5, label: 'L', colorVar: TREND_WARNING_COLOR }
		]);
	});

	it('同じ値（H = HH、L = LL）は幅 0 の注意の帯を出さない', () => {
		expect(
			thresholdBands({ thresholdHh: 60, thresholdH: 60, thresholdL: 30, thresholdLl: 30 })
		).toEqual([
			{ from: 60, to: null, label: 'HH', colorVar: TREND_DANGER_COLOR },
			{ from: null, to: 30, label: 'LL', colorVar: TREND_DANGER_COLOR }
		]);
	});

	it('L = H（正しい設定）は上下の注意の帯が接するだけ', () => {
		expect(thresholdBands({ ...NONE, thresholdH: 50, thresholdL: 50 })).toEqual([
			{ from: 50, to: null, label: 'H', colorVar: TREND_WARNING_COLOR },
			{ from: null, to: 50, label: 'L', colorVar: TREND_WARNING_COLOR }
		]);
	});
});

describe('resolveBandPen', () => {
	const pens = [
		{ tagId: 1, thresholds: null },
		{ tagId: 2, thresholds: { ...NONE, thresholdH: 5 } },
		{ tagId: 3, thresholds: { ...NONE, thresholdL: 1 } }
	];

	it.each([
		['選んでいなければ、しきい値のある最初のペン', null, 2],
		['選んだペン（しきい値が無くても選べる）', 1, 1],
		['選んだペンがグループに居なければ既定', 42, 2]
	])('%s', (_name, selected, expected) => {
		expect(resolveBandPen(pens, selected)).toBe(expected);
	});

	it('どのペンにもしきい値が無ければ null', () => {
		expect(resolveBandPen([{ tagId: 1, thresholds: null }], null)).toBeNull();
	});
});

describe('端末ごとの時間窓（Q4）', () => {
	function memoryStorage(initial: Record<string, string> = {}) {
		const data = new Map(Object.entries(initial));
		return {
			getItem: (key: string) => data.get(key) ?? null,
			setItem: (key: string, value: string) => void data.set(key, value),
			data
		};
	}

	it('覚えが無ければグループの既定、それも無ければ 10 分', () => {
		expect(resolveTrendWindowSec({ timeWindowSec: 300 }, null)).toBe(300);
		expect(resolveTrendWindowSec({}, null)).toBe(DEFAULT_TREND_WINDOW_SEC);
		expect(groupDefaultWindowSec({ timeWindowSec: 42 })).toBe(DEFAULT_TREND_WINDOW_SEC);
		expect(resolveTrendWindowSec({ timeWindowSec: 300 }, 60)).toBe(60);
	});

	it('グループごとに覚え、既定と同じ窓を選んだら覚えを消す', () => {
		const storage = memoryStorage();
		saveTrendWindowOverride(storage, 1, 60, 600);
		saveTrendWindowOverride(storage, 2, 3600, 600);
		expect(loadTrendWindowOverride(storage, 1)).toBe(60);
		expect(loadTrendWindowOverride(storage, 2)).toBe(3600);
		expect(loadTrendWindowOverride(storage, 3)).toBeNull();
		saveTrendWindowOverride(storage, 1, 600, 600);
		expect(loadTrendWindowOverride(storage, 1)).toBeNull();
		expect(JSON.parse(storage.data.get(TREND_WINDOW_STORAGE_KEY) ?? '')).toEqual({ '2': 3600 });
	});

	it.each([
		['壊れた JSON', '{'],
		['配列', '[60]'],
		['選択肢に無い値', '{"1": 42}'],
		['文字列の値', '{"1": "60"}']
	])('%s は覚え無しとして読む', (_name, raw) => {
		expect(loadTrendWindowOverride(memoryStorage({ [TREND_WINDOW_STORAGE_KEY]: raw }), 1)).toBe(
			null
		);
	});

	it('壊れた覚えがあっても保存できる。選択肢に無い窓は保存しない', () => {
		const storage = memoryStorage({ [TREND_WINDOW_STORAGE_KEY]: '{' });
		saveTrendWindowOverride(storage, 1, 60, 600);
		expect(loadTrendWindowOverride(storage, 1)).toBe(60);
		saveTrendWindowOverride(storage, 1, 42, 600);
		expect(loadTrendWindowOverride(storage, 1)).toBe(60);
	});

	it('ストレージが無い・投げるときも画面を止めない', () => {
		const throwing = {
			getItem: () => {
				throw new Error('denied');
			},
			setItem: () => {
				throw new Error('denied');
			}
		};
		expect(loadTrendWindowOverride(undefined, 1)).toBeNull();
		expect(loadTrendWindowOverride(throwing, 1)).toBeNull();
		expect(() => saveTrendWindowOverride(throwing, 1, 60, 600)).not.toThrow();
		expect(() => saveTrendWindowOverride(undefined, 1, 60, 600)).not.toThrow();
	});
});

describe('パネルの表示（trendPenInfos / 説明文 / 注記）', () => {
	const tags = [
		{
			id: 1,
			name: '温度',
			unit: '℃',
			decimals: 1,
			thresholdHh: 90,
			thresholdH: 80,
			thresholdL: null,
			thresholdLl: null
		},
		{ id: 2, name: '圧力', unit: '', decimals: 0, ...NONE }
	] as unknown as Parameters<typeof trendPenInfos>[1];

	it('タグの名前・単位・しきい値。読めないタグは「タグ ID n」、色の枠は既定で位置 + 1', () => {
		const pens = trendPenInfos(
			{
				pens: [
					{ tagId: 1, colorSlot: 5 },
					{ tagId: 2, colorSlot: null },
					{ tagId: 9, colorSlot: null }
				]
			},
			tags
		);
		expect(pens).toEqual<TrendPenInfo[]>([
			{
				tagId: 1,
				name: '温度',
				unit: '℃',
				decimals: 1,
				isBit: false,
				colorSlot: 5,
				thresholds: { thresholdHh: 90, thresholdH: 80, thresholdL: null, thresholdLl: null }
			},
			{
				tagId: 2,
				name: '圧力',
				unit: null,
				decimals: 0,
				isBit: false,
				colorSlot: 2,
				thresholds: null
			},
			{
				tagId: 9,
				name: 'タグ ID 9',
				unit: null,
				decimals: null,
				isBit: false,
				colorSlot: 3,
				thresholds: null
			}
		]);
		expect(penLegendLabel(pens[0])).toBe('温度（℃）');
		expect(penLegendLabel(pens[1])).toBe('圧力');
	});

	it('説明文: ペン・単位・窓・値の範囲・選んだペンの帯', () => {
		const pens = trendPenInfos(
			{
				pens: [
					{ tagId: 1, colorSlot: null },
					{ tagId: 2, colorSlot: null }
				]
			},
			tags
		);
		const rows = [
			{ t: 0, values: [10, null] },
			{ t: 1, values: [85.25, 3] }
		];
		expect(trendDescription({ windowSec: 600, pens, rows, bandTagId: 1 })).toBe(
			'直近 10 分のトレンド。ペン: 温度（℃）、圧力。縦軸は全ペン共通の自動スケールで、表示中の値の範囲は 3.0〜85.3。しきい値の帯: 温度（HH 90.0 以上 危険、H 80.0〜90.0 注意）。'
		);
		expect(trendDescription({ windowSec: 60, pens, rows: [], bandTagId: 2 })).toBe(
			'直近 1 分のトレンド。ペン: 温度（℃）、圧力。表示できる値はまだありません。しきい値の帯: 圧力（しきい値の設定なし）。'
		);
		expect(trendDescription({ windowSec: 60, pens, rows: [], bandTagId: null })).toContain(
			'しきい値の帯: なし。'
		);
	});

	it('bit のタグ: isBit、全ペン bit なら縦軸・説明文は False / True、混在は数値のまま（#551）', () => {
		const withBit = [
			...tags,
			{ id: 3, name: '運転中', unit: '', decimals: 0, dataType: 'bit', ...NONE },
			{ id: 4, name: '異常', unit: '', decimals: 0, dataType: 'bit', ...NONE }
		] as unknown as Parameters<typeof trendPenInfos>[1];
		const g = (...ids: number[]) => ({ pens: ids.map((tagId) => ({ tagId, colorSlot: null })) });
		const bits = trendPenInfos(g(3, 4), withBit);
		expect(bits.map((p) => p.isBit)).toEqual([true, true]);
		expect(isAllBitTrend(bits)).toBe(true);

		const fy = trendYFormatter(bits);
		expect([0, 1, 0.2, 0.5].map(fy)).toEqual(['False', 'True', '', '']);

		const rows = [
			{ t: 0, values: [0, 1] },
			{ t: 1, values: [1, 1] }
		];
		expect(trendDescription({ windowSec: 60, pens: bits, rows, bandTagId: null })).toBe(
			'直近 1 分のトレンド。ペン: 運転中、異常。縦軸は False（0）と True（1）で、表示中の値は False〜True。しきい値の帯: なし。'
		);
		// 値が 1 種類だけのとき。
		expect(
			trendDescription({
				windowSec: 60,
				pens: bits,
				rows: [{ t: 0, values: [1, 1] }],
				bandTagId: null
			})
		).toContain('表示中の値は True。');

		// 混在: 数値のまま（他のタグの 0 / 1 まで True / False に見せない）。
		const mixed = trendPenInfos(g(2, 3), withBit);
		expect(isAllBitTrend(mixed)).toBe(false);
		expect([0, 1].map(trendYFormatter(mixed))).toEqual(['0', '1']);
		const text = trendDescription({
			windowSec: 60,
			pens: mixed,
			rows: [{ t: 0, values: [5, 1] }],
			bandTagId: null
		});
		expect(text).toContain('表示中の値の範囲は 1〜5。');
		expect(text).toContain('bit のペン（運転中）は 0 が False、1 が True。');

		// ペンが無いときは全 bit ではない。
		expect(isAllBitTrend([])).toBe(false);
		expect(trendYFormatter([])(1)).toBe('1');
	});

	it('注記: 履歴を読めない・不明なタグ・シミュレーションを別々に', () => {
		expect(trendNotices({ historyState: 'ready', unknownNames: [], simulationNames: [] })).toEqual(
			[]
		);
		expect(
			trendNotices({ historyState: 'loading', unknownNames: [], simulationNames: [] })
		).toEqual([]);
		expect(
			trendNotices({
				historyState: 'unavailable',
				unknownNames: ['タグ ID 9'],
				simulationNames: ['温度', '圧力']
			})
		).toEqual([
			HISTORY_UNAVAILABLE_NOTE,
			'次のタグは登録が見つからないため履歴がありません: タグ ID 9',
			`${SIMULATION_NOTE}（温度、圧力）。`
		]);
	});
});

describe('表示の時計（observeServerClock / serverClockNow、PR #543 のレビュー P2）', () => {
	it('サーバーの時刻を受け取るまでは時計が無い（端末の時計を使わない）', () => {
		expect(observeServerClock(null, null, 123_456)).toBeNull();
		expect(serverClockNow(null, 123_456)).toBeNull();
	});

	it('基準 + 端末の経過時間で進み、応答のたびに付け替える。表示の時刻は戻さない', () => {
		let clock: ServerClock = observeServerClock(null, 1_000_000, 50);
		expect(serverClockNow(clock, 50)).toBe(1_000_000);
		expect(serverClockNow(clock, 5050)).toBe(1_005_000);
		// 次の応答: サーバーの時刻に付け替える。
		clock = observeServerClock(clock, 1_005_200, 5050);
		expect(serverClockNow(clock, 5050)).toBe(1_005_200);
		// 見積もりより前のサーバーの時刻（端末の時計が速い）: 表示は戻さず、追いつくまで止める。
		clock = observeServerClock(clock, 1_009_000, 10_050); // 見積もりは 1_010_200
		expect(serverClockNow(clock, 10_050)).toBe(1_010_200);
		expect(serverClockNow(clock, 11_050)).toBe(1_010_200);
		expect(serverClockNow(clock, 12_050)).toBe(1_011_000);
		// serverNowMs の無い応答では変えない。端末の時刻が戻っても基準より前にしない。
		expect(observeServerClock(clock, null, 13_000)).toBe(clock);
		expect(serverClockNow(clock, 0)).toBe(1_010_200);
	});

	/**
	 * 収集周期 60 秒・ポーリング 5 秒・刻み 10 秒で、開いた時点のサンプルの古さ（0 / 30 / 55 秒）
	 * によらず、good のサンプルが続く間は線が切れない（応答 → `applyValuesOutcome` → 時計 →
	 * `appendLive` を通す）。古さ 55 秒は「開いて 5 秒後に次のサンプル」で、`ptimeMs` を時計に
	 * すると時計が跳んで間の刻みが `null` になった（再レビュー P2）。
	 */
	it.each([
		[0, 60_000],
		[30_000, 60_000],
		[55_000, 60_000],
		[0, 600_000],
		[30_000, 600_000],
		[55_000, 600_000]
	])('サンプルの古さ %i ms・窓 %i ms', (ageMs, windowMs) => {
		const T0 = 3_600_000_000;
		const localOffset = 987_654; // 端末の単調な時刻はサーバーの時刻と無関係
		const step = 10_000;
		let state: ValuesState = INITIAL_VALUES_STATE;
		let clock: ServerClock = null;
		let buffer: TrendBuffer | null = null;
		let lastNow = -Infinity;
		for (let k = 0; k <= 36; k++) {
			// 5 秒ごと + 少し揺らぐ（応答を待ってから次を予約する）。
			const t = k * 5000 + (k % 3) * 37;
			// 最後に読んだ時刻: 開いた時点（t = 0）で ageMs だけ古く、60 秒ごとに進む。
			const ptime = T0 - ageMs + Math.floor((t + ageMs) / 60_000) * 60_000;
			state = applyValuesOutcome(
				state,
				{
					kind: 'ok',
					value: {
						state: 'ready',
						data: { 'tag:1': { value: 42, ptimeMs: ptime, quality: 'good', lastGoodMs: ptime } },
						serverNowMs: T0 + t
					}
				},
				t
			);
			clock = observeServerClock(clock, state.serverNowMs, localOffset + t);
			const now = serverClockNow(clock, localOffset + t);
			if (now === null) throw new Error('時計が無い');
			expect(now).toBeGreaterThanOrEqual(lastNow);
			lastNow = now;
			buffer ??= emptyTrendBuffer(step, windowMs, 1, now);
			buffer = appendLive(buffer, now, [42]);
		}
		const live = buffer!.rows.filter((row) => row.t >= (buffer!.liveFromT ?? Infinity));
		expect(live.length).toBeGreaterThan(5);
		expect(live.every((row) => row.values[0] === 42)).toBe(true);
	});

	it('ポーリングの失敗の間は書き足さないので、刻みは null（線が切れる）のまま', () => {
		let clock: ServerClock = observeServerClock(null, 1_000_000, 0);
		let buffer = emptyTrendBuffer(10_000, 60_000, 1, serverClockNow(clock, 0)!);
		buffer = appendLive(buffer, serverClockNow(clock, 0)!, [1]);
		// 30 秒失敗して、次の成功。
		clock = observeServerClock(clock, 1_030_000, 30_000);
		buffer = appendLive(buffer, serverClockNow(clock, 30_000)!, [2]);
		expect(values(buffer).flat()).toEqual([null, null, 1, null, null, 2]);
	});
});
