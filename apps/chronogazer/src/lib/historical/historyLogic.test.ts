/**
 * ヒストリカル画面のロジック（`historyLogic.ts`）のテスト。
 *
 * - 期間の検証（前後・31 日）・「前へ / 次へ」（現在を超えない）・URL の読み書き（おかしければ既定）。
 * - `bins` は描画幅・2000・ペン数 × bins ≤ 8000 に収まり、行数（bins の 2 倍）が描画幅を超えない。
 * - 格子は包絡（区間の最小・最大）で、欠測は `null`、系列ごとの時刻・`binMs` の違いを吸収する。
 */
import { describe, expect, it } from 'vitest';
import { ProviderError } from '@banto/admin-core';
import { HISTORY_MAX_BINS, HISTORY_MAX_POINTS, HISTORY_MAX_WINDOW_MS } from '../banto/collectAdmin';
import { trendPlotWidthPx } from '../monitor/trendLogic';
import {
	HISTORY_LIMIT_MESSAGE,
	HISTORY_PRESETS,
	RANGE_INVALID_MESSAGE,
	RANGE_ORDER_MESSAGE,
	RANGE_TOO_WIDE_MESSAGE,
	activePresetWidth,
	buildHistoryGrid,
	canShiftNext,
	checkRange,
	defaultRange,
	fromLocalInputValue,
	historyBins,
	historyErrorMessage,
	historyQuery,
	historyRequestFor,
	historyTimeLabel,
	isEmptyGrid,
	parseMsParam,
	presetRange,
	resolveRange,
	shiftRange,
	toLocalInputValue,
	unknownTagsNote
} from './historyLogic';

const NOW = 1_800_000_000_000;
const HOUR = 3_600_000;

describe('期間の候補・既定', () => {
	it('候補は 1・8・24 時間と 7・31 日で、どれも 31 日以内', () => {
		expect(HISTORY_PRESETS.map((p) => p.widthMs / HOUR)).toEqual([1, 8, 24, 168, 744]);
		for (const p of HISTORY_PRESETS) {
			expect(checkRange(presetRange(p.widthMs, NOW)).ok).toBe(true);
		}
	});

	it('既定は直近 1 時間（現在で終わる）', () => {
		expect(defaultRange(NOW)).toEqual({ fromMs: NOW - HOUR, toMs: NOW });
	});

	it('幅が候補に当たるときだけ印が付く', () => {
		expect(activePresetWidth(presetRange(8 * HOUR, NOW))).toBe(8 * HOUR);
		expect(activePresetWidth({ fromMs: NOW - 5 * HOUR, toMs: NOW })).toBeNull();
	});
});

describe('checkRange', () => {
	it('開始 < 終了・幅 31 日以内なら ok（ちょうど 31 日も ok）', () => {
		expect(checkRange({ fromMs: 1, toMs: 2 }).ok).toBe(true);
		expect(checkRange({ fromMs: 0, toMs: HISTORY_MAX_WINDOW_MS }).ok).toBe(true);
	});

	it('開始 >= 終了は順序の案内', () => {
		expect(checkRange({ fromMs: 5, toMs: 5 })).toEqual({ ok: false, message: RANGE_ORDER_MESSAGE });
		expect(checkRange({ fromMs: 9, toMs: 5 })).toEqual({ ok: false, message: RANGE_ORDER_MESSAGE });
	});

	it('31 日を 1 ms でも超えると幅の案内', () => {
		expect(checkRange({ fromMs: 0, toMs: HISTORY_MAX_WINDOW_MS + 1 })).toEqual({
			ok: false,
			message: RANGE_TOO_WIDE_MESSAGE
		});
	});

	it('数でない・負は入力の案内', () => {
		expect(checkRange({ fromMs: Number.NaN, toMs: 5 })).toEqual({
			ok: false,
			message: RANGE_INVALID_MESSAGE
		});
		expect(checkRange({ fromMs: -1, toMs: 5 }).ok).toBe(false);
	});
});

describe('shiftRange / canShiftNext', () => {
	const range = { fromMs: NOW - 10 * HOUR, toMs: NOW - 5 * HOUR };

	it('前へは同じ幅だけ過去へ', () => {
		expect(shiftRange(range, 'prev', NOW)).toEqual({
			fromMs: NOW - 15 * HOUR,
			toMs: NOW - 10 * HOUR
		});
	});

	it('次へは同じ幅だけ未来へ', () => {
		expect(shiftRange(range, 'next', NOW)).toEqual({ fromMs: NOW - 5 * HOUR, toMs: NOW });
	});

	it('次へは現在を超えない（超えるなら終わりを現在にして幅を保つ）', () => {
		const late = { fromMs: NOW - 3 * HOUR, toMs: NOW - HOUR };
		const next = shiftRange(late, 'next', NOW);
		expect(next.toMs).toBe(NOW);
		expect(next.toMs - next.fromMs).toBe(2 * HOUR);
	});

	it('終わりが現在以後なら次へは動かず、押せない', () => {
		const atNow = { fromMs: NOW - HOUR, toMs: NOW };
		expect(shiftRange(atNow, 'next', NOW)).toEqual(atNow);
		expect(canShiftNext(atNow, NOW)).toBe(false);
		expect(canShiftNext(range, NOW)).toBe(true);
	});

	it('前へは 0 より前へ行かず、幅を保つ', () => {
		expect(shiftRange({ fromMs: 1000, toMs: 5000 }, 'prev', NOW)).toEqual({
			fromMs: 0,
			toMs: 4000
		});
	});

	it('ずらした結果も 31 日以内のまま', () => {
		const wide = presetRange(HISTORY_MAX_WINDOW_MS, NOW - 40 * 24 * HOUR);
		expect(checkRange(shiftRange(wide, 'prev', NOW)).ok).toBe(true);
		expect(checkRange(shiftRange(wide, 'next', NOW)).ok).toBe(true);
	});
});

describe('datetime-local の変換', () => {
	it('往復で分までが保たれる', () => {
		const ms = new Date(2026, 10, 19, 9, 5).getTime();
		const text = toLocalInputValue(ms);
		expect(text).toBe('2026-11-19T09:05');
		expect(fromLocalInputValue(text)).toBe(ms);
	});

	it('空・壊れた値は null', () => {
		expect(fromLocalInputValue('')).toBeNull();
		expect(fromLocalInputValue('2026-11-19')).toBeNull();
		expect(fromLocalInputValue('2026-13-45T99:99')).toBeNull();
	});
});

describe('URL の読み書き', () => {
	it('parseMsParam は数字だけ', () => {
		expect(parseMsParam('123')).toBe(123);
		expect(parseMsParam(null)).toBeNull();
		expect(parseMsParam('-1')).toBeNull();
		expect(parseMsParam('1.5')).toBeNull();
		expect(parseMsParam('abc')).toBeNull();
		expect(parseMsParam('')).toBeNull();
	});

	it('from・to が正しければそのまま', () => {
		const r = resolveRange(String(NOW - 2 * HOUR), String(NOW - HOUR), NOW);
		expect(r).toEqual({ range: { fromMs: NOW - 2 * HOUR, toMs: NOW - HOUR }, fellBack: false });
	});

	it('両方無ければ既定（戻した扱いにしない）', () => {
		expect(resolveRange(null, null, NOW)).toEqual({ range: defaultRange(NOW), fellBack: false });
	});

	it('片方だけ・前後が逆・31 日超・数でないは既定に戻す', () => {
		const fallback = { range: defaultRange(NOW), fellBack: true };
		expect(resolveRange('100', null, NOW)).toEqual(fallback);
		expect(resolveRange('200', '100', NOW)).toEqual(fallback);
		expect(resolveRange('0', String(HISTORY_MAX_WINDOW_MS + 1), NOW)).toEqual(fallback);
		expect(resolveRange('x', 'y', NOW)).toEqual(fallback);
	});

	it('historyQuery を resolveRange が読み戻せる', () => {
		const range = { fromMs: NOW - 3 * HOUR, toMs: NOW - HOUR };
		const params = new URLSearchParams(historyQuery(7, range));
		expect(params.get('group')).toBe('7');
		expect(resolveRange(params.get('from'), params.get('to'), NOW).range).toEqual(range);
	});
});

describe('historyBins / historyRequestFor', () => {
	it('行数（bins の 2 倍）が描画域の幅を超えない', () => {
		for (const width of [200, 400, 800, 1200, 1900, 4000]) {
			for (const tags of [1, 3, 8]) {
				const bins = historyBins(width, tags);
				expect(bins * 2).toBeLessThanOrEqual(Math.max(2, trendPlotWidthPx(width)));
			}
		}
	});

	it('2000 以下・ペン数 × bins ≤ 8000 に収まる', () => {
		for (const width of [100, 1000, 5000, 20000]) {
			for (let tags = 1; tags <= 8; tags++) {
				const bins = historyBins(width, tags);
				expect(bins).toBeGreaterThanOrEqual(1);
				expect(bins).toBeLessThanOrEqual(HISTORY_MAX_BINS);
				expect(bins * tags).toBeLessThanOrEqual(HISTORY_MAX_POINTS);
			}
		}
	});

	it('50 以上は 25 刻み（リサイズのたびに読み直さない）', () => {
		expect(historyBins(1000, 2) % 25).toBe(0);
	});

	it('要求はタグの重複を除き、8 本までに切る', () => {
		const req = historyRequestFor({ fromMs: 0, toMs: HOUR }, [1, 1, 2, 3, 4, 5, 6, 7, 8, 9], 1000);
		expect(req?.tagIds).toEqual([1, 2, 3, 4, 5, 6, 7, 8]);
		expect(req?.fromMs).toBe(0);
		expect(req?.toMs).toBe(HOUR);
	});

	it('タグが無い・幅が無い・期間が外れているなら null', () => {
		const range = { fromMs: 0, toMs: HOUR };
		expect(historyRequestFor(range, [], 1000)).toBeNull();
		expect(historyRequestFor(range, [1], 0)).toBeNull();
		expect(historyRequestFor({ fromMs: 5, toMs: 5 }, [1], 1000)).toBeNull();
	});
});

describe('buildHistoryGrid', () => {
	const range = { fromMs: 1000, toMs: 1999 }; // 幅 1000 ms（両端を含む）

	it('区間 k は 2 行（最小・最大）で、行数は bins の 2 倍', () => {
		const grid = buildHistoryGrid(range, 10, [1], {
			series: [
				{
					tagId: 1,
					simulation: false,
					binMs: 100,
					points: [
						{ tMs: 1000, min: 1, max: 5 },
						{ tMs: 1100, min: 2, max: 3 }
					]
				}
			]
		});
		expect(grid.stepMs).toBe(100);
		expect(grid.bins).toBe(10);
		expect(grid.rows).toHaveLength(20);
		expect(grid.rows[0]).toEqual({ t: 1000, values: [1] });
		expect(grid.rows[1]).toEqual({ t: 1050, values: [5] });
		expect(grid.rows[2].values).toEqual([2]);
		expect(grid.rows[3].values).toEqual([3]);
		// 点の無い区間は欠測（0 にしない）。
		expect(grid.rows[4].values).toEqual([null]);
		expect(grid.rows[5].values).toEqual([null]);
	});

	it('欠測の点（null）・非有限は線を切る', () => {
		const grid = buildHistoryGrid(range, 10, [1], {
			series: [
				{
					tagId: 1,
					simulation: false,
					binMs: 100,
					points: [
						{ tMs: 1000, min: null, max: null },
						{ tMs: 1100, min: Number.NaN, max: 4 }
					]
				}
			]
		});
		expect(isEmptyGrid(grid)).toBe(true);
	});

	it('同じ区間に重なる点は最小の最小・最大の最大にする（山を落とさない）', () => {
		const grid = buildHistoryGrid(range, 2, [1], {
			series: [
				{
					tagId: 1,
					simulation: false,
					binMs: 10,
					points: [
						{ tMs: 1000, min: 3, max: 4 },
						{ tMs: 1010, min: 1, max: 9 },
						{ tMs: 1020, min: 5, max: 6 }
					]
				}
			]
		});
		expect(grid.rows[0].values).toEqual([1]);
		expect(grid.rows[1].values).toEqual([9]);
	});

	it('系列ごとに binMs・時刻が違っても、ペンの列に正しく入る', () => {
		const grid = buildHistoryGrid(range, 10, [1, 2], {
			series: [
				{ tagId: 1, simulation: false, binMs: 100, points: [{ tMs: 1000, min: 1, max: 2 }] },
				// 粗い系列: 300 ms 幅の点 1 つが 3 区間にまたがる。
				{ tagId: 2, simulation: false, binMs: 300, points: [{ tMs: 1200, min: 7, max: 8 }] }
			]
		});
		expect(grid.rows[0].values).toEqual([1, null]);
		expect(grid.rows[4].values).toEqual([null, 7]);
		expect(grid.rows[8].values).toEqual([null, 7]);
		expect(grid.rows[9].values).toEqual([null, 8]);
		expect(grid.rows[10].values).toEqual([null, null]);
	});

	it('系列の無いペン（削除されたタグ）は null のまま', () => {
		const grid = buildHistoryGrid(range, 4, [1, 99], {
			series: [{ tagId: 1, simulation: false, binMs: 250, points: [{ tMs: 1000, min: 1, max: 1 }] }]
		});
		expect(grid.rows.every((row) => row.values[1] === null)).toBe(true);
	});

	it('期間の外の点は無視し、末尾を超えない', () => {
		const grid = buildHistoryGrid(range, 10, [1], {
			series: [
				{
					tagId: 1,
					simulation: false,
					binMs: 100,
					points: [
						{ tMs: 0, min: 1, max: 1 },
						{ tMs: 1900, min: 4, max: 5 },
						{ tMs: 5000, min: 9, max: 9 }
					]
				}
			]
		});
		expect(grid.rows).toHaveLength(20);
		expect(grid.rows[18].values).toEqual([4]);
		expect(grid.rows[19].values).toEqual([5]);
	});
});

describe('表示・エラーの文言', () => {
	it('時刻の書式は期間で変わる', () => {
		const at = new Date(2026, 10, 19, 9, 5, 7).getTime();
		expect(historyTimeLabel(at, HOUR)).toBe('09:05:07');
		expect(historyTimeLabel(at, 8 * HOUR)).toBe('09:05');
		expect(historyTimeLabel(at, 7 * 24 * HOUR)).toBe('11/19 09:05');
	});

	it('422（validation）は日本語の案内にする', () => {
		const err = new ProviderError({ kind: 'validation', field_errors: [] });
		expect(historyErrorMessage(err)).toBe(HISTORY_LIMIT_MESSAGE);
	});

	it('それ以外のエラーは理由を添える', () => {
		expect(historyErrorMessage(new Error('boom'))).toContain('boom');
		const other = new ProviderError({ kind: 'other', message: 'net down' });
		expect(historyErrorMessage(other)).toContain('net down');
	});

	it('不明なタグの注記は名前を挙げ、無ければ null', () => {
		expect(unknownTagsNote([], (id) => String(id))).toBeNull();
		expect(unknownTagsNote([3, 4], (id) => `タグ ID ${id}`)).toContain('タグ ID 3、タグ ID 4');
	});
});

describe('buildHistoryGrid: 収集周期が区間より長いタグ', () => {
	// 1 時間窓・600 区間 = 1 区間 6 秒。周期 60 秒で点は 60 秒ごと。
	const range = { fromMs: 0, toMs: 3_599_999 };
	const series = (points: { tMs: number; min: number | null; max: number | null }[]) => ({
		series: [{ tagId: 1, simulation: false, binMs: 6000, points }]
	});
	const at = (k: number) => 60_000 * k;
	const filled = (grid: ReturnType<typeof buildHistoryGrid>, from: number, to: number) =>
		grid.rows.slice(from * 2, to * 2).every((row) => row.values[0] !== null);

	it('周期が無いと、点の間の区間は null（従来どおり）', () => {
		const grid = buildHistoryGrid(
			range,
			600,
			[1],
			series([
				{ tMs: at(0), min: 1, max: 2 },
				{ tMs: at(1), min: 3, max: 4 }
			])
		);
		expect(grid.rows[2].values).toEqual([null]); // 区間 1
	});

	it('周期 60 秒なら、次の点まで区間が埋まって null にならない', () => {
		const grid = buildHistoryGrid(
			range,
			600,
			[1],
			series([
				{ tMs: at(0), min: 1, max: 2 },
				{ tMs: at(1), min: 3, max: 4 }
			]),
			() => 60_000
		);
		expect(filled(grid, 0, 10)).toBe(true); // 区間 0〜9 = 最初の 60 秒
		expect(grid.rows[0].values).toEqual([1]);
		expect(grid.rows[19].values).toEqual([2]);
		expect(grid.rows[20].values).toEqual([3]); // 区間 10 = 2 点目
		// 最後の点は周期分だけ延びる。その先は null。
		expect(filled(grid, 10, 20)).toBe(true);
		expect(grid.rows[40].values).toEqual([null]);
	});

	it('サーバーの欠測点がある区間は、周期でも埋めず null のまま', () => {
		const grid = buildHistoryGrid(
			range,
			600,
			[1],
			series([
				{ tMs: at(0), min: 1, max: 2 },
				{ tMs: at(1), min: null, max: null },
				{ tMs: at(2), min: 5, max: 6 }
			]),
			() => 60_000
		);
		expect(filled(grid, 0, 10)).toBe(true);
		expect(grid.rows[20].values).toEqual([null]); // 区間 10（欠測点）
		expect(grid.rows[38].values).toEqual([null]); // 区間 19
		expect(grid.rows[40].values).toEqual([5]); // 区間 20
	});

	it('点の間隔が binMs より狭くても、欠測の点の区間は null（周期を途中で変えた素通し）', () => {
		// binMs = 60 秒。良い点 1 秒、欠測の点 11 秒・21 秒。1 区間 6 秒。
		const grid = buildHistoryGrid(
			range,
			600,
			[1],
			{
				series: [
					{
						tagId: 1,
						simulation: false,
						binMs: 60_000,
						points: [
							{ tMs: 1000, min: 1, max: 2 },
							{ tMs: 11_000, min: null, max: null },
							{ tMs: 21_000, min: null, max: null }
						]
					}
				]
			},
			() => 60_000
		);
		expect(grid.rows[0].values).toEqual([1]); // 区間 0
		expect(grid.rows[2].values).toEqual([1]); // 区間 1（〜11 秒）
		expect(grid.rows[4].values).toEqual([null]); // 区間 2（12 秒〜）
		expect(grid.rows[8].values).toEqual([null]); // 区間 4
		expect(grid.rows[20].values).toEqual([null]); // 区間 10（60 秒）
	});
});
