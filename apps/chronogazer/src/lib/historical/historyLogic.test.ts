/**
 * ヒストリカル画面のロジック（`historyLogic.ts`）のテスト。
 *
 * - 期間の検証（前後・31 日）・「前へ / 次へ」（現在を超えない）・URL の読み書き（おかしければ既定）。
 * - `bins` は描画幅・2000・ペン数 × bins ≤ 8000 に収まる。
 * - 期間の 2 つの形（両端を含む ⇔ 半開区間）の変換、部品の `TrendFetch` の形
 *   （ペンごとの系列・API の `binMs`・`unknownTagIds` の付帯情報・`ready` でないときの失敗）。
 */
import { describe, expect, it } from 'vitest';
import { ProviderError } from '@banto/admin-core';
import {
	HISTORY_MAX_BINS,
	HISTORY_MAX_POINTS,
	HISTORY_MAX_WINDOW_MS,
	type CollectHistory,
	type CollectHistoryParams,
	type Readout
} from '../banto/collectAdmin';
import {
	HISTORY_LIMIT_MESSAGE,
	HISTORY_PRESETS,
	RANGE_INVALID_MESSAGE,
	RANGE_ORDER_MESSAGE,
	RANGE_TOO_WIDE_MESSAGE,
	HistoryUnavailableError,
	activePresetWidth,
	canShiftNext,
	checkRange,
	createHistoryFetch,
	defaultRange,
	fromLocalInputValue,
	historyBins,
	historyDescription,
	historyErrorMessage,
	historyExtent,
	historyQuery,
	historyRequestFor,
	parseMsParam,
	presetRange,
	resolveRange,
	sameHistoryRange,
	shiftRange,
	toHistoryRange,
	toLocalInputValue,
	toTimeRange,
	unknownTagsNote,
	type HistoryExtras
} from './historyLogic';
import { trendPenId } from '../monitor/trendPens';

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
	it('区間の数が描画域の幅を超えない', () => {
		for (const width of [200, 400, 800, 1200, 1900, 4000]) {
			for (const tags of [1, 3, 8]) {
				expect(historyBins(width, tags)).toBeLessThanOrEqual(width);
			}
		}
	});

	it('2000 以下・ペン数 × bins ≤ 8000 に収まる', () => {
		for (const width of [0, 1, 100, 1000, 5000, 20000]) {
			for (let tags = 1; tags <= 8; tags++) {
				const bins = historyBins(width, tags);
				expect(bins).toBeGreaterThanOrEqual(1);
				expect(bins).toBeLessThanOrEqual(HISTORY_MAX_BINS);
				expect(bins * tags).toBeLessThanOrEqual(HISTORY_MAX_POINTS);
			}
		}
	});

	it('50 以上は 25 刻み（リサイズのたびに読み直さない）', () => {
		expect(historyBins(1013, 2)).toBe(1000);
		expect(historyBins(49, 1)).toBe(49);
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

describe('期間の 2 つの形（両端を含む ⇔ 半開区間）', () => {
	it('toTimeRange は終わりを 1 ms 先にし、toHistoryRange で戻る', () => {
		const r = { fromMs: 1000, toMs: 1999 };
		expect(toTimeRange(r)).toEqual({ startMs: 1000, endMs: 2000 });
		expect(toHistoryRange(toTimeRange(r))).toEqual(r);
		expect(sameHistoryRange(r, { fromMs: 1000, toMs: 1999 })).toBe(true);
		expect(sameHistoryRange(r, { fromMs: 1000, toMs: 2000 })).toBe(false);
	});

	it('31 日ちょうどの期間は、戻しても 31 日以内のまま', () => {
		const r = { fromMs: NOW - HISTORY_MAX_WINDOW_MS, toMs: NOW };
		expect(checkRange(toHistoryRange(toTimeRange(r))).ok).toBe(true);
	});
});

describe('createHistoryFetch', () => {
	const signal = new AbortController().signal;
	const ready = (data: CollectHistory): Readout<CollectHistory> => ({ state: 'ready', data });

	it('タグの重複を除いて 1 回で読み、ペンごとの系列（昇順・API の binMs）にする', async () => {
		const calls: CollectHistoryParams[] = [];
		const extras = new WeakMap<object, HistoryExtras>();
		const fetch = createHistoryFetch(async (params) => {
			calls.push(params);
			return ready({
				fromMs: 1000,
				toMs: 1999,
				series: [
					{
						tagId: 5,
						simulation: false,
						binMs: 60_000,
						points: [
							{ tMs: 1500, min: 2, max: 3 },
							{ tMs: 1000, min: 1, max: 1 }
						]
					}
				],
				unknownTagIds: [9]
			});
		}, extras);
		const ids = [trendPenId(0, 5), trendPenId(1, 5), trendPenId(2, 9)];
		const out = await fetch({
			range: { startMs: 1000, endMs: 2000 },
			penIds: ids,
			bins: 10,
			signal
		});
		expect(calls).toEqual([{ tagIds: [5, 9], fromMs: 1000, toMs: 1999, bins: 10 }]);
		expect(out[ids[0]]).toEqual({
			binMs: 60_000,
			bins: [
				{ tMs: 1000, min: 1, max: 1 },
				{ tMs: 1500, min: 2, max: 3 }
			]
		});
		expect(out[ids[1]]).toEqual(out[ids[0]]);
		expect(out[ids[2]]).toBeUndefined();
		expect(extras.get(out)).toEqual({ unknownTagIds: [9], simulationTagIds: [] });
	});

	it('ready でなければ HistoryUnavailableError を投げる（空に潰さない）', async () => {
		const fetch = createHistoryFetch(async () => ({ state: 'unavailable' }), new WeakMap());
		await expect(
			fetch({ range: { startMs: 0, endMs: 10 }, penIds: [trendPenId(0, 1)], bins: 1, signal })
		).rejects.toBeInstanceOf(HistoryUnavailableError);
	});
});

describe('説明文', () => {
	it('表示中の値の範囲は取得範囲 ∩ 表示範囲だけを見る', () => {
		const data = {
			range: { startMs: 0, endMs: 2000 },
			series: {
				a: {
					binMs: 1000,
					bins: [
						{ tMs: 0, min: 1, max: 2 },
						{ tMs: 1000, min: 5, max: 9 }
					]
				}
			}
		};
		expect(historyExtent(data, { startMs: 0, endMs: 2000 })).toEqual({ min: 1, max: 9 });
		expect(historyExtent(data, { startMs: 1000, endMs: 2000 })).toEqual({ min: 5, max: 9 });
		expect(historyExtent(null, { startMs: 0, endMs: 2000 })).toBeNull();
	});

	it('値が無ければその旨、帯のペンを言う', () => {
		const range = { fromMs: 0, toMs: HOUR };
		expect(
			historyDescription({ range, penLabels: ['A'], extent: null, bandPenName: null })
		).toContain('この期間に表示できる値はありません。しきい値の帯: なし。');
		expect(
			historyDescription({ range, penLabels: ['A'], extent: { min: 1, max: 2 }, bandPenName: 'A' })
		).toContain('表示中の値の範囲は 1〜2。しきい値の帯: A。');
	});
});

describe('表示・エラーの文言', () => {
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
