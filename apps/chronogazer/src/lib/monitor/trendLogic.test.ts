/**
 * 監視画面のトレンドの純関数（`trendLogic.ts`）の表。
 *
 * 追従の状態遷移（追従・左へパンで止める・右端を今へ戻すと再開）・履歴を読み直す条件
 * （追従中に毎秒は読まない・範囲を出たら・幅が変わったら・途切れの後）・しきい値の帯
 * （無い・同じ値・端の無い側）・端末ごとの時間窓（壊れた localStorage を含む）・説明文と注記・
 * 表示の時計。
 */
import { describe, expect, it } from 'vitest';
import {
	hybridData,
	liveAppend,
	liveRetainMs,
	type LivePen,
	type TrendSeries,
	type TrendData
} from '@banto-industrial/trend';
import { TIME_WINDOW_OPTIONS } from '../../routes/(app)/groups/groupsPageLogic';
import {
	DEFAULT_TREND_WINDOW_SEC,
	FOLLOW_TOLERANCE_MS,
	HISTORY_UNAVAILABLE_NOTE,
	SIMULATION_NOTE,
	TREND_DANGER_COLOR,
	TREND_WARNING_COLOR,
	TREND_WINDOWS_SEC,
	TREND_WINDOW_STORAGE_KEY,
	groupDefaultWindowSec,
	loadTrendWindowOverride,
	needsHistoryReload,
	penLegendLabel,
	rangeChangeFollow,
	resolveBandPen,
	resolveTrendWindowSec,
	saveTrendWindowOverride,
	startFollow,
	observeServerClock,
	serverClockNow,
	tickFollow,
	type ServerClock,
	thresholdBands,
	trendDescription,
	trendNotices,
	trendPenInfos,
	isAllBitTrend,
	trendWindowLabel,
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

const MIN = 60_000;

/** 説明文の試験の描画データ: ペンごとの値の列を 1 秒刻みの区間（min = max）にする。 */
function dataOf(penIds: readonly string[], columns: readonly (number | null)[][]): TrendData {
	const series: Record<string, TrendSeries> = {};
	penIds.forEach((id, i) => {
		series[id] = {
			binMs: 1000,
			bins: columns.map((row, k) => ({ tMs: k * 1000, min: row[i], max: row[i] }))
		};
	});
	return { range: { startMs: 0, endMs: columns.length * 1000 }, series };
}

/** 追従中の説明文（表示範囲は窓の幅、値はペンの並びどおりの列）。 */
function describeOf(
	windowSec: number,
	pens: readonly TrendPenInfo[],
	columns: readonly (number | null)[][],
	bandTagId: number | null
): string {
	const penIds = pens.map((_, i) => `p${i}`);
	return trendDescription({
		windowSec,
		view: { startMs: 0, endMs: windowSec * 1000 },
		following: true,
		pens,
		penIds,
		data: columns.length > 0 ? dataOf(penIds, columns) : null,
		bandTagId
	});
}

describe('時間窓の選択肢', () => {
	it('「表示グループ」ページ（= Rust の TREND_TIME_WINDOWS_SEC）と同じ', () => {
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

describe('追従の状態（startFollow / tickFollow / rangeChangeFollow）', () => {
	const NOW = 10 * 3_600_000;

	it('追従中は右端を今にして幅を保って進む。変わらなければ同じオブジェクト', () => {
		const s = startFollow(10 * MIN, NOW);
		expect(s).toEqual({
			following: true,
			widthMs: 10 * MIN,
			view: { startMs: NOW - 10 * MIN, endMs: NOW }
		});
		expect(tickFollow(s, NOW)).toBe(s);
		expect(tickFollow(s, NOW + 1000).view).toEqual({
			startMs: NOW + 1000 - 10 * MIN,
			endMs: NOW + 1000
		});
	});

	it('左へパン（右端が今から外れる）で追従をやめ、止めている間は時計が進んでも動かない', () => {
		const s = startFollow(10 * MIN, NOW);
		const panned = { startMs: NOW - 15 * MIN, endMs: NOW - 5 * MIN };
		const stopped = rangeChangeFollow(s, panned, NOW);
		expect(stopped).toEqual({ following: false, widthMs: 10 * MIN, view: panned });
		expect(tickFollow(stopped, NOW + 60_000)).toBe(stopped);
	});

	it('今から離れた位置での拡大は追従をやめ、右端を今に付けた拡大・縮小はその幅で追従を続ける', () => {
		const s = startFollow(10 * MIN, NOW);
		const zoomedPast = { startMs: NOW - 8 * MIN, endMs: NOW - 2 * MIN };
		expect(rangeChangeFollow(s, zoomedPast, NOW).following).toBe(false);
		const zoomedAtNow = { startMs: NOW - 30 * MIN, endMs: NOW };
		expect(rangeChangeFollow(s, zoomedAtNow, NOW)).toEqual({
			following: true,
			widthMs: 30 * MIN,
			view: zoomedAtNow
		});
	});

	it('右端を今へ戻すと（許容 FOLLOW_TOLERANCE_MS 以内）追従を再開する。不正な範囲は無視', () => {
		const stopped = rangeChangeFollow(
			startFollow(10 * MIN, NOW),
			{ startMs: NOW - 20 * MIN, endMs: NOW - 10 * MIN },
			NOW
		);
		const back = rangeChangeFollow(
			stopped,
			{ startMs: NOW - 10 * MIN - 500, endMs: NOW - 500 },
			NOW
		);
		expect(FOLLOW_TOLERANCE_MS).toBeGreaterThan(500);
		expect(back.following).toBe(true);
		expect(back.view).toEqual({ startMs: NOW - 10 * MIN, endMs: NOW });
		expect(rangeChangeFollow(stopped, { startMs: 5, endMs: 5 }, NOW)).toBe(stopped);
	});

	it('幅は部品の範囲（1 分〜31 日）に丸める', () => {
		expect(startFollow(10, NOW).widthMs).toBe(MIN);
		expect(startFollow(400 * 24 * 3_600_000, NOW).widthMs).toBe(31 * 24 * 3_600_000);
	});
});

describe('履歴を読み直すか（needsHistoryReload）', () => {
	const NOW = 10 * 3_600_000;
	const view = { startMs: NOW - 10 * MIN, endMs: NOW };
	const requested = { range: view, binMs: 1000 };
	const base = { requested, view, binMs: 1000, following: true, liveFroms: [NOW - 9 * MIN] };

	it('まだ要求していなければ読む。ペンが無い・範囲が不正なら読まない', () => {
		expect(needsHistoryReload({ ...base, requested: null })).toBe(true);
		expect(needsHistoryReload({ ...base, requested: null, liveFroms: [] })).toBe(false);
		expect(needsHistoryReload({ ...base, requested: null, view: { startMs: 1, endMs: 1 } })).toBe(
			false
		);
	});

	it('追従中は、左端が要求した範囲の中にあり幅が同じなら、毎秒は読まない', () => {
		for (let s = 0; s < 120; s++) {
			const v = { startMs: view.startMs + s * 1000, endMs: view.endMs + s * 1000 };
			expect(needsHistoryReload({ ...base, view: v })).toBe(false);
		}
		// リングバッファが無いペン（値が bad のまま）があっても、追従中は右端の伸びで読まない。
		expect(
			needsHistoryReload({
				...base,
				view: { startMs: view.startMs + 5000, endMs: view.endMs + 5000 },
				liveFroms: [null]
			})
		).toBe(false);
	});

	it('左端が要求した範囲を出たら読む（左へパン・縮小、追従で窓 1 つぶん進んだ）', () => {
		const left = { startMs: view.startMs - 1, endMs: view.endMs - 1 };
		expect(needsHistoryReload({ ...base, view: left, following: false })).toBe(true);
		const ahead = { startMs: view.endMs, endMs: view.endMs + 10 * MIN };
		expect(needsHistoryReload({ ...base, view: ahead, liveFroms: [null] })).toBe(true);
	});

	it('区間の幅が変わったら読む（拡大・縮小・描画域の幅）', () => {
		expect(needsHistoryReload({ ...base, binMs: 2000 })).toBe(true);
	});

	it('表示範囲を全部リングバッファで描けるなら、範囲を出ても読まない', () => {
		const ahead = { startMs: view.endMs, endMs: view.endMs + 10 * MIN };
		expect(needsHistoryReload({ ...base, view: ahead, liveFroms: [view.endMs - MIN] })).toBe(false);
	});

	it('途切れの後に書き直し始めた（liveFrom が要求した範囲の終わりより後）なら読む', () => {
		const later = { startMs: view.startMs + 30_000, endMs: view.endMs + 30_000 };
		expect(needsHistoryReload({ ...base, view: later, liveFroms: [NOW + 20_000] })).toBe(true);
		// 一度読めば（要求した範囲の終わり ≥ liveFrom）繰り返さない。
		expect(
			needsHistoryReload({
				...base,
				requested: { range: later, binMs: 1000 },
				view: { startMs: later.startMs + 1000, endMs: later.endMs + 1000 },
				liveFroms: [NOW + 20_000]
			})
		).toBe(false);
	});

	it('追従を止めている間は、履歴で描く部分が要求した範囲を出たら読む', () => {
		const zoomOut = { startMs: view.startMs, endMs: view.endMs + 5 * MIN };
		expect(
			needsHistoryReload({ ...base, view: zoomOut, following: false, liveFroms: [null] })
		).toBe(true);
		// ライブで描ける部分（liveFrom 以後）は履歴が無くてよい。
		expect(needsHistoryReload({ ...base, view: zoomOut, following: false })).toBe(false);
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
			[10, null],
			[85.25, 3]
		];
		expect(describeOf(600, pens, rows, 1)).toBe(
			'直近 10 分のトレンド。ペン: 温度（℃）、圧力。縦軸はペンごとの自動スケール。表示中の値の範囲: 温度 10.0〜85.3、圧力 3。しきい値の帯: 温度（HH 90.0 以上 危険、H 80.0〜90.0 注意）。'
		);
		expect(describeOf(60, pens, [], 2)).toBe(
			'直近 1 分のトレンド。ペン: 温度（℃）、圧力。表示できる値はまだありません。しきい値の帯: 圧力（しきい値の設定なし）。'
		);
		expect(describeOf(60, pens, [], null)).toContain('しきい値の帯: なし。');
	});

	it('説明文: ペンごとの範囲を言い、合算の範囲は言わない。値が無いペンは値なし', () => {
		const mk = (tagId: number, name: string) => ({
			tagId,
			name,
			unit: '',
			decimals: 0,
			isBit: false,
			colorSlot: 0,
			thresholds: null
		});
		const pens = [mk(1, 'A'), mk(2, 'B'), mk(3, 'C')] as unknown as Parameters<
			typeof trendDescription
		>[0]['pens'];
		const text = describeOf(
			60,
			pens,
			[
				[0, 100, null],
				[10, 200, null]
			],
			null
		);
		expect(text).toContain('表示中の値の範囲: A 0〜10、B 100〜200、C 値なし。');
		expect(text).not.toContain('A 0〜200');
		expect(text).not.toContain('全ペン共通');
	});

	it('bit のタグ: isBit、全ペン bit なら説明文の縦軸は 0 と 1、混在は数値の範囲（#551）', () => {
		const withBit = [
			...tags,
			{ id: 3, name: '運転中', unit: '', decimals: 0, dataType: 'bit', ...NONE },
			{ id: 4, name: '異常', unit: '', decimals: 0, dataType: 'bit', ...NONE }
		] as unknown as Parameters<typeof trendPenInfos>[1];
		const g = (...ids: number[]) => ({ pens: ids.map((tagId) => ({ tagId, colorSlot: null })) });
		const bits = trendPenInfos(g(3, 4), withBit);
		expect(bits.map((p) => p.isBit)).toEqual([true, true]);
		expect(isAllBitTrend(bits)).toBe(true);

		const rows = [
			[0, 1],
			[1, 1]
		];
		expect(describeOf(60, bits, rows, null)).toBe(
			'直近 1 分のトレンド。ペン: 運転中、異常。縦軸は 0 と 1 で、表示中の値は 0〜1。しきい値の帯: なし。'
		);
		// 値が 1 種類だけのとき。
		expect(describeOf(60, bits, [[1, 1]], null)).toContain('表示中の値は 1。');

		// 混在: 数値の範囲で言う。
		const mixed = trendPenInfos(g(2, 3), withBit);
		expect(isAllBitTrend(mixed)).toBe(false);
		const text = describeOf(60, mixed, [[5, 1]], null);
		expect(text).toContain('縦軸はペンごとの自動スケール。表示中の値の範囲: 圧力 5、運転中 1。');
		expect(text).toContain('bit のペン（運転中）は 0 が False、1 が True。');

		// ペンが無いときは全 bit ではない。
		expect(isAllBitTrend([])).toBe(false);
	});

	it('説明文: 表示範囲の外の値は数えない。追従を止めていれば期間を言う。min・max の包絡も読む', () => {
		const pens = trendPenInfos({ pens: [{ tagId: 2, colorSlot: null }] }, tags);
		const data: TrendData = {
			range: { startMs: 0, endMs: 4000 },
			series: {
				p0: {
					binMs: 1000,
					bins: [
						{ tMs: 0, min: 1, max: 1 },
						{ tMs: 1000, min: 2, max: 7 },
						{ tMs: 2000, min: 3, max: 3 },
						{ tMs: 3000, min: 100, max: 100 }
					]
				}
			}
		};
		const text = trendDescription({
			windowSec: 60,
			view: { startMs: 1000, endMs: 3000 },
			following: false,
			pens,
			penIds: ['p0'],
			data,
			bandTagId: null
		});
		expect(text).toMatch(/^.+〜.+ のトレンド（追従を止めています）。ペン: 圧力。/);
		expect(text).toContain('表示中の値の範囲: 圧力 2〜7。');
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
	 * 収集周期 60 秒・ポーリング 5 秒で、開いた時点のサンプルの古さ（0 / 30 / 55 秒）によらず、
	 * good のサンプルが続く間は線が切れない（応答 → `applyValuesOutcome` → 時計 → `liveAppend` →
	 * `hybridData` を通す）。古さ 55 秒は「開いて 5 秒後に次のサンプル」で、`ptimeMs` を時計に
	 * すると時計が跳んで間が欠測になった（再レビュー P2）。
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
		let state: ValuesState = INITIAL_VALUES_STATE;
		let clock: ServerClock = null;
		let pen: LivePen | undefined;
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
			pen = liveAppend(pen, { tMs: now, value: 42 }, { retainMs: liveRetainMs(windowMs) });
		}
		const data = hybridData({
			history: null,
			live: { p: pen },
			penIds: ['p'],
			binMs: Math.ceil(windowMs / 600),
			// サンプル間隔 = 収集周期とポーリングの周期の長い方。
			sampleIntervalMs: { p: 60_000 }
		});
		const bins = data?.series.p?.bins ?? [];
		expect(bins.length).toBeGreaterThan(1);
		expect(bins.every((b) => b.min === 42 && b.max === 42)).toBe(true);
	});
});
