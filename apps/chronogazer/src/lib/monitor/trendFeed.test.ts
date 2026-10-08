/**
 * トレンドのバッファの持ち主（`trendFeed.svelte.ts`）のテスト。
 *
 * - 構成が同じなら作り直さない・履歴を読み直さない（無駄撃ちしない）。
 * - 履歴は flush の待ちの後に 1 回読み、格子に合わせる。現在値の行は上書きしない。
 * - 構成（グループ・窓）を変えたら、飛行中の履歴を abort し、遅れた応答を捨てる
 *   （グループをまたいで線を持ち越さない）。
 * - 返ってこない履歴は上限で打ち切り、「履歴なし・現在値だけ」にする。
 * - `unavailable`・失敗も「履歴なし」（0 件に潰さない）。
 * - 不明なタグ・シミュレーションのタグを拾う。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));

import type { CollectHistory, CollectHistoryParams, Readout } from '../banto/collectAdmin';
import { TrendFeed, trendConfigKey, type HistoryFetch, type TrendConfig } from './trendFeed.svelte';

const GRACE = 2000;
const TIMEOUT = 12_000;

function config(overrides: Partial<TrendConfig> = {}): TrendConfig {
	return { groupId: 1, windowMs: 5000, stepMs: 1000, penTagIds: [7], ...overrides };
}

function ready(data: Partial<CollectHistory> = {}): Readout<CollectHistory> {
	return {
		state: 'ready',
		data: { fromMs: 0, toMs: 0, series: [], unknownTagIds: [], ...data }
	};
}

interface Pending {
	params: CollectHistoryParams;
	signal: AbortSignal;
	resolve: (value: Readout<CollectHistory>) => void;
}

function controllableFetch(): { fetch: HistoryFetch; calls: Pending[] } {
	const calls: Pending[] = [];
	const fetch: HistoryFetch = (params, signal) =>
		new Promise((resolve) => calls.push({ params, signal, resolve }));
	return { fetch, calls };
}

const rowValues = (feed: TrendFeed) => feed.buffer?.rows.map((row) => row.values[0]) ?? null;

describe('TrendFeed', () => {
	beforeEach(() => {
		vi.useFakeTimers();
	});
	afterEach(() => {
		vi.useRealTimers();
	});

	it('flush の待ちの後に履歴を 1 回読み、現在値の行は上書きしない', async () => {
		const { fetch, calls } = controllableFetch();
		const feed = new TrendFeed({ fetchHistory: fetch, graceMs: GRACE, timeoutMs: TIMEOUT });
		feed.configure(config(), 14_000); // 10_000..14_000
		feed.append(14_000, [99]);
		expect(feed.historyState).toBe('loading');
		expect(calls).toHaveLength(0);

		await vi.advanceTimersByTimeAsync(GRACE);
		expect(calls).toHaveLength(1);
		expect(calls[0].params).toEqual({ tagIds: [7], fromMs: 10_000, toMs: 14_999, bins: 5 });

		calls[0].resolve(
			ready({
				series: [
					{
						tagId: 7,
						simulation: false,
						binMs: 1000,
						points: [10_000, 11_000, 12_000, 13_000, 14_000].map((tMs, i) => ({
							tMs,
							min: i,
							max: i
						}))
					}
				]
			})
		);
		await vi.advanceTimersByTimeAsync(0);
		expect(feed.historyState).toBe('ready');
		expect(rowValues(feed)).toEqual([0, 1, 2, 3, 99]);
	});

	it('同じ構成なら作り直さず、履歴も読み直さない', async () => {
		const { fetch, calls } = controllableFetch();
		const feed = new TrendFeed({ fetchHistory: fetch, graceMs: GRACE, timeoutMs: TIMEOUT });
		feed.configure(config(), 10_000);
		feed.append(10_000, [1]);
		feed.configure(config(), 11_000);
		await vi.advanceTimersByTimeAsync(GRACE);
		expect(calls).toHaveLength(1);
		expect(rowValues(feed)?.at(-1)).toBe(1);
	});

	it('グループを替えたら飛行中の履歴を abort し、遅れた応答を新しいグループに書かない', async () => {
		const { fetch, calls } = controllableFetch();
		const feed = new TrendFeed({ fetchHistory: fetch, graceMs: GRACE, timeoutMs: TIMEOUT });
		feed.configure(config({ groupId: 1 }), 14_000);
		await vi.advanceTimersByTimeAsync(GRACE);
		expect(calls).toHaveLength(1);

		feed.configure(config({ groupId: 2 }), 14_000);
		expect(calls[0].signal.aborted).toBe(true);
		expect(feed.groupId).toBe(2);
		expect(rowValues(feed)).toEqual([null, null, null, null, null]);

		calls[0].resolve(
			ready({
				series: [
					{ tagId: 7, simulation: true, binMs: 1000, points: [{ tMs: 12_000, min: 5, max: 5 }] }
				],
				unknownTagIds: [7]
			})
		);
		await vi.advanceTimersByTimeAsync(0);
		expect(rowValues(feed)).toEqual([null, null, null, null, null]);
		expect(feed.simulationTagIds).toEqual([]);
		expect(feed.unknownTagIds).toEqual([]);
		expect(feed.historyState).toBe('loading');

		// 新しいグループの分は改めて読む。
		await vi.advanceTimersByTimeAsync(GRACE);
		expect(calls).toHaveLength(2);
	});

	it('待ちの間に構成を変えたら、前の構成の履歴は要求すらしない', async () => {
		const { fetch, calls } = controllableFetch();
		const feed = new TrendFeed({ fetchHistory: fetch, graceMs: GRACE, timeoutMs: TIMEOUT });
		feed.configure(config({ windowMs: 5000 }), 14_000);
		await vi.advanceTimersByTimeAsync(GRACE / 2);
		feed.configure(config({ windowMs: 3000 }), 14_000);
		await vi.advanceTimersByTimeAsync(GRACE);
		expect(calls).toHaveLength(1);
		expect(calls[0].params.bins).toBe(3);
	});

	it('返ってこない履歴は上限で打ち切って「履歴なし」にし、要求も abort する', async () => {
		const { fetch, calls } = controllableFetch();
		const feed = new TrendFeed({ fetchHistory: fetch, graceMs: GRACE, timeoutMs: TIMEOUT });
		feed.configure(config(), 14_000);
		await vi.advanceTimersByTimeAsync(GRACE);
		expect(feed.historyState).toBe('loading');
		await vi.advanceTimersByTimeAsync(TIMEOUT);
		expect(feed.historyState).toBe('unavailable');
		expect(calls[0].signal.aborted).toBe(true);
		// 現在値は書き足せる（履歴が無くても線は出る）。
		feed.append(15_000, [3]);
		expect(rowValues(feed)?.at(-1)).toBe(3);
	});

	it.each<[string, HistoryFetch]>([
		['unavailable', () => Promise.resolve({ state: 'unavailable' })],
		['失敗', () => Promise.reject(new Error('422'))]
	])('%s も「履歴なし」（0 件に潰さない）', async (_name, fetch) => {
		const feed = new TrendFeed({ fetchHistory: fetch, graceMs: GRACE, timeoutMs: TIMEOUT });
		feed.configure(config(), 14_000);
		await vi.advanceTimersByTimeAsync(GRACE);
		expect(feed.historyState).toBe('unavailable');
	});

	it('不明なタグ（このグループのペンだけ）とシミュレーションのタグを拾う', async () => {
		const feed = new TrendFeed({
			fetchHistory: () =>
				Promise.resolve(
					ready({
						series: [
							{ tagId: 7, simulation: true, binMs: 1000, points: [] },
							{ tagId: 8, simulation: false, binMs: 1000, points: [] }
						],
						unknownTagIds: [9, 42]
					})
				),
			graceMs: GRACE,
			timeoutMs: TIMEOUT
		});
		feed.configure(config({ penTagIds: [7, 8, 9] }), 14_000);
		await vi.advanceTimersByTimeAsync(GRACE);
		expect(feed.simulationTagIds).toEqual([7]);
		expect(feed.unknownTagIds).toEqual([9]);
	});

	it('reset で止めて捨てる。飛行中の応答も書かない', async () => {
		const { fetch, calls } = controllableFetch();
		const feed = new TrendFeed({ fetchHistory: fetch, graceMs: GRACE, timeoutMs: TIMEOUT });
		feed.configure(config(), 14_000);
		await vi.advanceTimersByTimeAsync(GRACE);
		feed.reset();
		expect(calls[0].signal.aborted).toBe(true);
		calls[0].resolve(ready());
		await vi.advanceTimersByTimeAsync(0);
		expect(feed.buffer).toBeNull();
		expect(feed.historyState).toBe('idle');
		expect(feed.key).toBeNull();
	});

	it('構成のキーはグループ・窓・刻み・ペンの並びで変わる', () => {
		const base = trendConfigKey(config());
		expect(trendConfigKey(config({ groupId: 2 }))).not.toBe(base);
		expect(trendConfigKey(config({ windowMs: 60_000 }))).not.toBe(base);
		expect(trendConfigKey(config({ stepMs: 2000 }))).not.toBe(base);
		expect(trendConfigKey(config({ penTagIds: [8] }))).not.toBe(base);
		expect(trendConfigKey(config({ periodMsOf: () => 1 }))).toBe(base);
	});
});
