/**
 * 監視画面（R1-D の D-1）の判断の表テスト（`monitorLogic.ts`）。
 *
 * とくに固定したいこと:
 * - **null を 0 と区別する**（#414 段階2）。値 0 は「0」、値 null は「—」。
 * - `invalid`（設定不正）・未収集（キーが無い）・`bad`・`stale` を別の状態・
 *   別の文言にする。`bad` / `stale` では最後の値を出さない（Q2）。
 * - しきい値の判定が収集のしきい値イベント（`classify_threshold`）と同じ向き・
 *   優先順位である。
 */
import { describe, expect, it } from 'vitest';
import type { CurrentSampleView } from '../banto/collectAdmin';
import type { DisplayGroup } from '../banto/displayGroupsAdmin';
import type { Tag } from '../banto/tagRegistryAdmin';
import {
	LAST_GROUP_STORAGE_KEY,
	MONITOR_POLL_DEFAULT_MS,
	NEVER_RECEIVED_LABEL,
	NO_VALUE,
	UNCOLLECTED_LABEL,
	formatValue,
	groupPenViews,
	isKindRendered,
	lastReceivedText,
	loadLastGroup,
	parseGroupParam,
	penView,
	pollPeriodMs,
	saveLastGroup,
	selectGroup,
	sortDisplayGroups,
	thresholdLevel,
	thresholdLevelLabel,
	valuesStaleNote,
	type PenState,
	type ThresholdLevel
} from './monitorLogic';

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

function group(overrides: Partial<DisplayGroup> = {}): DisplayGroup {
	return {
		id: 1,
		name: 'G',
		sortOrder: 0,
		kind: 'digital',
		attributes: {},
		pens: [],
		revision: 1,
		...overrides
	};
}

const PEN = { tagId: 1, colorSlot: null };

describe('sortDisplayGroups / parseGroupParam / selectGroup', () => {
	it('sortOrder 昇順、同じなら ID 昇順。元の配列は変えない', () => {
		const input = [
			group({ id: 3, sortOrder: 1 }),
			group({ id: 2, sortOrder: 0 }),
			group({ id: 1, sortOrder: 1 })
		];
		expect(sortDisplayGroups(input).map((g) => g.id)).toEqual([2, 1, 3]);
		expect(input.map((g) => g.id)).toEqual([3, 2, 1]);
	});

	it.each([
		[null, null],
		['', null],
		['7', 7],
		['007', 7],
		['-1', null],
		['1.5', null],
		['abc', null],
		['99999999999999999999', null]
	])('parseGroupParam(%j) = %j', (raw, expected) => {
		expect(parseGroupParam(raw)).toBe(expected);
	});

	const sorted = [group({ id: 5 }), group({ id: 6 }), group({ id: 7 })];
	it.each([
		// requested, remembered, id, missingRequested
		[6, 7, 6, null],
		[null, 7, 7, null],
		[null, null, 5, null],
		[null, 99, 5, null],
		[99, 7, 7, 99],
		[99, null, 5, 99]
	])('URL %j・端末 %j → %j（見つからない指定 %j）', (requested, remembered, id, missing) => {
		expect(selectGroup(sorted, requested, remembered)).toEqual({ id, missingRequested: missing });
	});

	it('グループが 0 件なら null', () => {
		expect(selectGroup([], 1, 1)).toEqual({ id: null, missingRequested: 1 });
		expect(selectGroup([], null, null)).toEqual({ id: null, missingRequested: null });
	});
});

describe('loadLastGroup / saveLastGroup（ストレージが投げても止めない）', () => {
	it('往復できる', () => {
		const map = new Map<string, string>();
		const storage = {
			getItem: (k: string) => map.get(k) ?? null,
			setItem: (k: string, v: string) => void map.set(k, v)
		};
		saveLastGroup(storage, 12);
		expect(map.get(LAST_GROUP_STORAGE_KEY)).toBe('12');
		expect(loadLastGroup(storage)).toBe(12);
	});

	it('壊れた値・無いストレージ・投げるストレージは null / 何もしない', () => {
		expect(loadLastGroup({ getItem: () => 'x' })).toBeNull();
		expect(loadLastGroup(undefined)).toBeNull();
		expect(
			loadLastGroup({
				getItem: () => {
					throw new Error('SecurityError');
				}
			})
		).toBeNull();
		expect(() =>
			saveLastGroup(
				{
					setItem: () => {
						throw new Error('QuotaExceeded');
					}
				},
				1
			)
		).not.toThrow();
		expect(() => saveLastGroup(undefined, 1)).not.toThrow();
	});
});

describe('pollPeriodMs（最短の収集周期を 500ms〜5s に丸める）', () => {
	const tags = [
		{ id: 1, collectionGroupId: 10 },
		{ id: 2, collectionGroupId: 20 },
		{ id: 3, collectionGroupId: 30 }
	];
	const cgs = [
		{ id: 10, periodMs: 1000 },
		{ id: 20, periodMs: 100 },
		{ id: 30, periodMs: 60000 }
	];
	it.each([
		[[1], 1000],
		[[1, 2], 500],
		[[2], 500],
		[[3], 5000],
		[[1, 3], 1000],
		[[], MONITOR_POLL_DEFAULT_MS],
		[[99], MONITOR_POLL_DEFAULT_MS]
	])('タグ %j → %i ms', (tagIds, expected) => {
		const pens = tagIds.map((tagId) => ({ tagId, colorSlot: null }));
		expect(pollPeriodMs({ pens }, tags, cgs)).toBe(expected);
	});

	it('収集グループが読めていなければ既定値', () => {
		expect(pollPeriodMs({ pens: [PEN] }, tags, [])).toBe(MONITOR_POLL_DEFAULT_MS);
	});
});

describe('formatValue', () => {
	it.each([
		[0, 0, '0'],
		[0, 2, '0.00'],
		[-0, 1, '0.0'],
		[-0.04, 1, '0.0'],
		[12.345, 1, '12.3'],
		[12.35, 0, '12'],
		[-3.5, 2, '-3.50'],
		[1234567, 0, '1234567'],
		[1.5, -1, '2'],
		[1.5, 99, '1.500000000000000'],
		[1.25, 1.5, '1']
	])('formatValue(%d, %d) = %s', (value, decimals, expected) => {
		expect(formatValue(value, decimals)).toBe(expected);
	});
});

describe('thresholdLevel（classify_threshold と同じ向き・優先順位）', () => {
	const all = { thresholdLl: 10, thresholdL: 20, thresholdH: 80, thresholdHh: 90 };
	it.each<[number | null, Parameters<typeof thresholdLevel>[1], ThresholdLevel]>([
		[50, all, 'normal'],
		[90, all, 'HH'],
		[95, all, 'HH'],
		[89.9, all, 'H'],
		[80, all, 'H'],
		[20, all, 'L'],
		[10, all, 'LL'],
		[-5, all, 'LL'],
		[20.1, all, 'normal'],
		[0, all, 'LL'],
		[null, all, 'none'],
		[50, undefined, 'none'],
		[50, { thresholdLl: null, thresholdL: null, thresholdH: null, thresholdHh: null }, 'none'],
		// HH だけ: H 相当の値でも HH 未満なら範囲内。
		[85, { thresholdLl: null, thresholdL: null, thresholdH: null, thresholdHh: 90 }, 'normal'],
		// H と L が同じ値（ll <= l <= h <= hh で許される）- 上側が優先。
		[50, { thresholdLl: null, thresholdL: 50, thresholdH: 50, thresholdHh: null }, 'H'],
		// しきい値 0 を「無い」と扱わない。
		[0, { thresholdLl: null, thresholdL: 0, thresholdH: null, thresholdHh: null }, 'L'],
		[1, { thresholdLl: null, thresholdL: 0, thresholdH: null, thresholdHh: null }, 'normal']
	])('値 %j → %s', (value, thresholds, expected) => {
		expect(thresholdLevel(value, thresholds)).toBe(expected);
	});

	it('文言: none 以外は文字がある（色だけで伝えない）', () => {
		for (const level of ['HH', 'H', 'normal', 'L', 'LL'] as const) {
			expect(thresholdLevelLabel(level)).toMatch(/\S/);
		}
		expect(thresholdLevelLabel('HH')).toContain('HH');
		expect(thresholdLevelLabel('LL')).toContain('LL');
		expect(thresholdLevelLabel('none')).toBeNull();
	});
});

describe('penView（値・品質・しきい値の総当たり）', () => {
	const withThresholds = tag({ thresholdH: 80, thresholdHh: 90 });
	const sample = (s: Partial<CurrentSampleView>): CurrentSampleView => ({
		value: 1,
		ptimeMs: 1000,
		quality: 'good',
		lastGoodMs: 1000,
		...s
	});
	/** 時刻の表示（テストでは区別できれば足りる）。 */
	const at = (ms: number) => `T${ms}`;

	it.each<
		[
			string,
			CurrentSampleView | undefined,
			Tag | undefined,
			{
				display: string;
				state: PenState;
				stateLabel: string;
				level: ThresholdLevel;
				last: string | null;
				link: boolean;
			}
		]
	>([
		[
			'good・値あり（小数桁・単位）',
			sample({ value: 23.456 }),
			tag(),
			{ display: '23.5', state: 'good', stateLabel: '正常', level: 'none', last: null, link: false }
		],
		[
			'good・値 0 は 0（null と区別する）',
			sample({ value: 0 }),
			tag(),
			{ display: '0.0', state: 'good', stateLabel: '正常', level: 'none', last: null, link: false }
		],
		[
			'good・HH',
			sample({ value: 95 }),
			withThresholds,
			{ display: '95.0', state: 'good', stateLabel: '正常', level: 'HH', last: null, link: false }
		],
		[
			'good・範囲内',
			sample({ value: 50 }),
			withThresholds,
			{
				display: '50.0',
				state: 'good',
				stateLabel: '正常',
				level: 'normal',
				last: null,
				link: false
			}
		],
		[
			'good・値 null（来ない約束だが 0 にしない）',
			sample({ value: null, ptimeMs: 7000, lastGoodMs: 1000 }),
			tag(),
			{
				display: NO_VALUE,
				state: 'bad',
				stateLabel: '値なし',
				level: 'none',
				last: '最後に受け取った値: T1000',
				link: false
			}
		],
		[
			'bad: 値があっても出さず、最後に good だった時刻を添える（読みに行った時刻ではない）',
			sample({ value: 42, quality: 'bad', ptimeMs: 5000, lastGoodMs: 1000 }),
			withThresholds,
			{
				display: NO_VALUE,
				state: 'bad',
				stateLabel: '通信エラー',
				level: 'none',
				last: '最後に受け取った値: T1000',
				link: false
			}
		],
		[
			'bad: 一度も受け取っていなければ時刻を出さない（#531）',
			sample({ value: null, quality: 'bad', ptimeMs: 5000, lastGoodMs: null }),
			withThresholds,
			{
				display: NO_VALUE,
				state: 'bad',
				stateLabel: '通信エラー',
				level: 'none',
				last: NEVER_RECEIVED_LABEL,
				link: false
			}
		],
		[
			'stale: 最後の値を出さず、時刻を添える',
			sample({ value: 95, quality: 'stale', ptimeMs: 6000, lastGoodMs: 4000 }),
			withThresholds,
			{
				display: NO_VALUE,
				state: 'stale',
				stateLabel: '更新停止',
				level: 'none',
				last: '最後に受け取った値: T4000',
				link: false
			}
		],
		[
			'invalid: 設定不正、タグ設定へ案内',
			{ value: null, ptimeMs: null, quality: 'invalid', lastGoodMs: null },
			tag(),
			{
				display: NO_VALUE,
				state: 'invalid',
				stateLabel: '設定不正（収集対象外）',
				level: 'none',
				last: null,
				link: true
			}
		],
		[
			'キーが無い: 未収集',
			undefined,
			tag(),
			{
				display: NO_VALUE,
				state: 'uncollected',
				stateLabel: UNCOLLECTED_LABEL,
				level: 'none',
				last: null,
				link: false
			}
		],
		[
			'タグ情報なし: 値は出す（小数桁を使わない）',
			sample({ value: 1.25 }),
			undefined,
			{ display: '1.25', state: 'good', stateLabel: '正常', level: 'none', last: null, link: false }
		]
	])('%s', (_label, input, tagDef, expected) => {
		const view = penView(PEN, 0, input, tagDef);
		expect({
			display: view.display,
			state: view.state,
			stateLabel: view.stateLabel,
			level: view.level,
			last: lastReceivedText(view, at),
			link: view.linkToTags
		}).toEqual(expected);
		expect(view.levelLabel).toBe(thresholdLevelLabel(expected.level));
	});

	it('名前・単位・色の枠', () => {
		expect(penView(PEN, 0, undefined, tag()).name).toBe('温度');
		expect(penView(PEN, 0, undefined, undefined).name).toBe('タグ ID 1');
		expect(penView(PEN, 0, undefined, tag()).unit).toBe('℃');
		expect(penView(PEN, 0, undefined, tag({ unit: '' })).unit).toBeNull();
		expect(penView(PEN, 0, undefined, tag({ unit: null })).unit).toBeNull();
		expect(penView(PEN, 2, undefined, tag()).colorSlot).toBe(3);
		expect(penView({ tagId: 1, colorSlot: 5 }, 0, undefined, tag()).colorSlot).toBe(5);
	});
});

describe('penView の value（バー・計器の描画用、D-2）', () => {
	const sample = (s: Partial<CurrentSampleView>): CurrentSampleView => ({
		value: 1,
		ptimeMs: 1000,
		quality: 'good',
		lastGoodMs: 1000,
		...s
	});
	const pen = { tagId: 1, colorSlot: null };

	it.each<[string, CurrentSampleView | undefined, number | null]>([
		['good・値あり', sample({ value: 12.5 }), 12.5],
		['good・値 0 は 0（null にしない）', sample({ value: 0 }), 0],
		['good・値 null', sample({ value: null }), null],
		['bad は値があっても null（最後の値を描かない）', sample({ value: 42, quality: 'bad' }), null],
		['stale は値があっても null', sample({ value: 42, quality: 'stale' }), null],
		['invalid', sample({ value: null, quality: 'invalid', ptimeMs: null, lastGoodMs: null }), null],
		['未収集（キーが無い）', undefined, null]
	])('%s', (_label, s, expected) => {
		const view = penView(pen, 0, s, tag());
		expect(view.value).toBe(expected);
		// 「—」と value null は同じ条件（描画と文字が食い違わない）。
		expect(view.display === NO_VALUE).toBe(expected === null);
	});
});

describe('groupPenViews', () => {
	const g = group({
		pens: [
			{ tagId: 1, colorSlot: null },
			{ tagId: 2, colorSlot: null }
		]
	});
	it('値がまだ無ければ空（画面は状態の説明を出す）', () => {
		expect(groupPenViews(g, null, [tag()])).toEqual([]);
	});
	it('ペンの順に、キー tag:<id> で引く', () => {
		const views = groupPenViews(
			g,
			{ 'tag:2': { value: 3, ptimeMs: 1, quality: 'good', lastGoodMs: 1 } },
			[tag(), tag({ id: 2, name: '圧力', decimals: 0 })]
		);
		expect(views.map((v) => [v.name, v.display, v.state])).toEqual([
			['温度', NO_VALUE, 'uncollected'],
			['圧力', '3', 'good']
		]);
	});
});

describe('isKindRendered / valuesStaleNote', () => {
	it('デジタル（D-1）とバー・計器（D-2）を描く。トレンドは D-3 まで描かない', () => {
		expect(isKindRendered('digital')).toBe(true);
		expect(isKindRendered('trend')).toBe(false);
		expect(isKindRendered('bar')).toBe(true);
		expect(isKindRendered('gauge')).toBe(true);
	});

	it('いつの表示かを必ず出す', () => {
		expect(valuesStaleNote(null, () => 'X')).toContain('まだ一度も取得できていません');
		expect(valuesStaleNote(1, () => '12:00:00')).toContain('12:00:00に取得したもの');
	});
});
