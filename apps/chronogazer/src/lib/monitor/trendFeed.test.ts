/**
 * 監視のトレンドのデータの持ち主（`trendFeed.svelte.ts`、ハイブリッド）のテスト。
 *
 * - 履歴は構成を当ててから flush の待ちの後に、表示範囲で 1 回読む。追従中に毎秒は読み直さない。
 * - 現在値はリングバッファに書き足し、描画データ（`data`）に出る。`null` は欠測のサンプル。
 * - 構成（グループ・ペンの並び）を変えたら全部捨て、飛行中の履歴を abort し、遅れた応答を捨てる
 *   （グループをまたいで線を持ち越さない）。
 * - 返ってこない・`unavailable` の履歴は「履歴なし」にし、リングバッファだけで描く。
 * - 途切れ（`breakLive`）でリングバッファを捨て、書き直し始めたら待ってから履歴を読み直す。
 * - 左へパンで追従をやめて範囲を出たら読み直し、「最新へ」・時間窓の選び直しで追従に戻る。
 * - 境界の区間を読み直す。幅 2 ms 未満はサーバーを呼ばない。
 * - 不明なタグ・シミュレーションのタグを拾う。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));

import type { TrendFetch } from '@banto-industrial/trend';
import type { CollectHistory, CollectHistoryParams, Readout } from '../banto/collectAdmin';
import {
	TrendFeed,
	skipTinyRanges,
	trendConfigKey,
	type HistoryFetch,
	type TrendConfig
} from './trendFeed.svelte';
import { trendPenId } from './trendPens';

const GRACE = 2000;
const DEBOUNCE = 200;
const TIMEOUT = 12_000;
const MIN = 60_000;
const NOW = 1_000 * 3_600_000;
const PEN = trendPenId(0, 7);

function config(overrides: Partial<TrendConfig> = {}): TrendConfig {
	return {
		groupId: 1,
		penTagIds: [7],
		windowMs: 10 * MIN,
		plotWidthPx: 600,
		sampleIntervalMs: [1000],
		...overrides
	};
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

function newFeed(fetch: HistoryFetch): TrendFeed {
	return new TrendFeed({
		fetchHistory: fetch,
		graceMs: GRACE,
		timeoutMs: TIMEOUT,
		debounceMs: DEBOUNCE
	});
}

/** 1 本のタグの履歴（`points` は `[tMs, 値]`）。 */
function series(tagId: number, binMs: number, points: [number, number | null][]) {
	return {
		tagId,
		simulation: false,
		binMs,
		points: points.map(([tMs, v]) => ({ tMs, min: v, max: v }))
	};
}

const historyCalls = (calls: Pending[]) => calls.filter((c) => c.params.bins > 1);

describe('TrendFeed', () => {
	beforeEach(() => {
		vi.useFakeTimers();
	});
	afterEach(() => {
		vi.useRealTimers();
	});

	it('小数の nowMs でも、取得の範囲とサンプルの時刻は整数のミリ秒になる', async () => {
		const { fetch, calls } = controllableFetch();
		const feed = newFeed(fetch);
		feed.configure(config(), NOW + 0.7);
		feed.append(NOW + 0.7, [1]);
		expect(Number.isInteger(feed.view!.startMs)).toBe(true);
		expect(Number.isInteger(feed.view!.endMs)).toBe(true);
		const pen = Object.values(feed.live)[0] as { samples?: { tMs: number }[] } | undefined;
		expect(pen).toBeDefined();
		expect(JSON.stringify(pen)).toContain(String(NOW));
		expect(JSON.stringify(pen)).not.toMatch(/\d\.\d/);
		await vi.advanceTimersByTimeAsync(GRACE + DEBOUNCE);
		expect(calls).toHaveLength(1);
		expect(Number.isInteger(calls[0].params.fromMs)).toBe(true);
		expect(Number.isInteger(calls[0].params.toMs)).toBe(true);
	});

	it('flush の待ちの後に表示範囲の履歴を 1 回読み、追従中に毎秒は読み直さない', async () => {
		const { fetch, calls } = controllableFetch();
		const feed = newFeed(fetch);
		expect(feed.configure(config(), NOW)).toBe(true);
		feed.append(NOW, [0]);
		expect(feed.view).toEqual({ startMs: NOW - 10 * MIN, endMs: NOW });
		expect(feed.following).toBe(true);
		expect(feed.historyState).toBe('loading');
		await vi.advanceTimersByTimeAsync(GRACE - 1);
		expect(calls).toHaveLength(0);
		await vi.advanceTimersByTimeAsync(1 + DEBOUNCE);
		expect(calls).toHaveLength(1);
		expect(calls[0].params).toMatchObject({
			tagIds: [7],
			fromMs: NOW - 10 * MIN,
			toMs: NOW - 1
		});
		expect(calls[0].params.bins).toBeGreaterThan(1);

		// 毎秒の現在値: 右端は進むが読み直さない。
		for (let s = 1; s <= 120; s++) {
			feed.sync(config(), NOW + s * 1000, {}, [s]);
			await vi.advanceTimersByTimeAsync(1000);
		}
		expect(feed.view).toEqual({ startMs: NOW + 120_000 - 10 * MIN, endMs: NOW + 120_000 });
		expect(historyCalls(calls)).toHaveLength(1);
		// 同じ構成なら作り直さない。
		expect(feed.configure(config(), NOW + 121_000)).toBe(false);
	});

	it('現在値をリングバッファに書き足し、描画データに出る。履歴より前は履歴で描く', async () => {
		const { fetch, calls } = controllableFetch();
		const feed = newFeed(fetch);
		feed.sync(config(), NOW, {}, [10]);
		feed.sync(config(), NOW + 1000, {}, [null]);
		feed.sync(config(), NOW + 2000, {}, [12]);
		expect(feed.live[PEN]?.liveFrom).toBe(NOW);
		expect(feed.live[PEN]?.samples.map((s) => s.value)).toEqual([10, null, 12]);
		// 履歴が届く前もリングバッファだけで描く。
		expect(feed.data?.series[PEN]?.bins.some((b) => b.min === 10)).toBe(true);

		await vi.advanceTimersByTimeAsync(GRACE + DEBOUNCE);
		const req = historyCalls(calls)[0];
		req.resolve(
			ready({
				series: [
					series(7, 1000, [
						[NOW - 5000, 1],
						[NOW - 4000, 2]
					])
				]
			})
		);
		await vi.advanceTimersByTimeAsync(0);
		expect(feed.historyState).toBe('ready');
		const bins = feed.data?.series[PEN]?.bins ?? [];
		expect(bins.find((b) => b.tMs === NOW - 5000)?.min).toBe(1);
		expect(bins.find((b) => b.tMs === NOW)?.min).toBe(10);
	});

	it('グループを替えたら全部捨て、飛行中の履歴を abort し、遅れた応答を新しいグループに書かない', async () => {
		const { fetch, calls } = controllableFetch();
		const feed = newFeed(fetch);
		feed.sync(config(), NOW, {}, [10]);
		await vi.advanceTimersByTimeAsync(GRACE + DEBOUNCE);
		expect(calls).toHaveLength(1);

		const snapshot = {};
		feed.sync(config({ groupId: 2, penTagIds: [8] }), NOW + 1000, snapshot, [20]);
		expect(calls[0].signal.aborted).toBe(true);
		expect(feed.groupId).toBe(2);
		expect(feed.penIds).toEqual([trendPenId(0, 8)]);
		expect(feed.live[PEN]).toBeUndefined();
		expect(feed.live[trendPenId(0, 8)]?.samples).toEqual([{ tMs: NOW + 1000, value: 20 }]);
		expect(feed.historyState).toBe('loading');

		calls[0].resolve(ready({ series: [series(7, 1000, [[NOW - 5000, 1]])] }));
		await vi.advanceTimersByTimeAsync(0);
		expect(feed.history).toBeNull();
		expect(feed.historyState).toBe('loading');

		// 新しい構成の履歴は、また待ってから読む。
		await vi.advanceTimersByTimeAsync(GRACE + DEBOUNCE);
		expect(calls).toHaveLength(2);
		expect(calls[1].params.tagIds).toEqual([8]);
	});

	it('待ちの間に構成を変えたら、前の構成の履歴は要求すらしない', async () => {
		const { fetch, calls } = controllableFetch();
		const feed = newFeed(fetch);
		feed.configure(config(), NOW);
		await vi.advanceTimersByTimeAsync(GRACE / 2);
		feed.configure(config({ penTagIds: [7, 9] }), NOW + 1000);
		await vi.advanceTimersByTimeAsync(GRACE + DEBOUNCE);
		expect(calls).toHaveLength(1);
		expect(calls[0].params.tagIds).toEqual([7, 9]);
	});

	it('返ってこない履歴は上限で打ち切って「履歴なし」にし、リングバッファだけで描く', async () => {
		const { fetch, calls } = controllableFetch();
		const feed = newFeed(fetch);
		feed.sync(config(), NOW, {}, [10]);
		await vi.advanceTimersByTimeAsync(GRACE + DEBOUNCE);
		expect(calls).toHaveLength(1);
		await vi.advanceTimersByTimeAsync(TIMEOUT);
		expect(calls[0].signal.aborted).toBe(true);
		expect(feed.historyState).toBe('unavailable');
		expect(feed.history).toBeNull();
		expect(feed.data?.series[PEN]?.bins.some((b) => b.min === 10)).toBe(true);
		// 失敗した要求は、表示範囲を出るまで繰り返さない。
		feed.sync(config(), NOW + 1000, {}, [11]);
		await vi.advanceTimersByTimeAsync(GRACE + DEBOUNCE);
		expect(calls).toHaveLength(1);
	});

	it('unavailable も「履歴なし」（0 件に潰さない）', async () => {
		const { fetch, calls } = controllableFetch();
		const feed = newFeed(fetch);
		feed.configure(config(), NOW);
		await vi.advanceTimersByTimeAsync(GRACE + DEBOUNCE);
		calls[0].resolve({ state: 'unavailable', reason: 'x' } as unknown as Readout<CollectHistory>);
		await vi.advanceTimersByTimeAsync(0);
		expect(feed.historyState).toBe('unavailable');
	});

	it('途切れでリングバッファを捨て、書き直し始めたら flush を待ってから履歴を読み直す', async () => {
		const { fetch, calls } = controllableFetch();
		const feed = newFeed(fetch);
		feed.sync(config(), NOW, {}, [10]);
		await vi.advanceTimersByTimeAsync(GRACE + DEBOUNCE);
		calls[0].resolve(ready({ series: [series(7, 1000, [[NOW - 5000, 1]])] }));
		await vi.advanceTimersByTimeAsync(0);
		feed.sync(config(), NOW + 1000, {}, [11]);

		feed.breakLive();
		expect(feed.live[PEN]?.liveFrom).toBeNull();
		expect(feed.live[PEN]?.samples).toEqual([]);

		// 30 秒後に書き直し始めた。
		feed.sync(config(), NOW + 31_000, {}, [12]);
		expect(feed.live[PEN]?.liveFrom).toBe(NOW + 31_000);
		await vi.advanceTimersByTimeAsync(DEBOUNCE);
		expect(historyCalls(calls)).toHaveLength(1);
		// 待ちの間に時計が進む。
		feed.sync(config(), NOW + 33_100, {}, [13]);
		await vi.advanceTimersByTimeAsync(GRACE + DEBOUNCE);
		const again = historyCalls(calls);
		expect(again).toHaveLength(2);
		expect(again[1].params.toMs).toBe(NOW + 33_100 - 1);
	});

	it('liveFrom より前へパンして読み済みなら、現在値が届いても同じ履歴を読み直さない', async () => {
		const { fetch, calls } = controllableFetch();
		const feed = newFeed(fetch);
		feed.sync(config(), NOW, {}, [10]);
		feed.sync(config(), NOW + 1000, {}, [11]);
		await vi.advanceTimersByTimeAsync(GRACE + DEBOUNCE);
		calls[0].resolve(ready());
		await vi.advanceTimersByTimeAsync(0);

		const past = { startMs: NOW - 30 * MIN, endMs: NOW - 20 * MIN };
		feed.rangeChange(past);
		await vi.advanceTimersByTimeAsync(DEBOUNCE);
		expect(historyCalls(calls)).toHaveLength(2);
		calls[1].resolve(ready());
		await vi.advanceTimersByTimeAsync(0);
		expect(feed.historyState).toBe('ready');

		for (let i = 2; i < 7; i++) {
			feed.sync(config(), NOW + i * 1000, {}, [i]);
			await vi.advanceTimersByTimeAsync(GRACE + DEBOUNCE);
		}
		expect(historyCalls(calls)).toHaveLength(2);
		expect(feed.historyState).toBe('ready');
	});

	it('左へパンで追従をやめ、範囲を出たら読み直す。「最新へ」で追従に戻る', async () => {
		const { fetch, calls } = controllableFetch();
		const feed = newFeed(fetch);
		feed.sync(config(), NOW, {}, [10]);
		await vi.advanceTimersByTimeAsync(GRACE + DEBOUNCE);
		calls[0].resolve(ready());
		await vi.advanceTimersByTimeAsync(0);

		const past = { startMs: NOW - 30 * MIN, endMs: NOW - 20 * MIN };
		feed.rangeChange(past);
		expect(feed.following).toBe(false);
		expect(feed.view).toEqual(past);
		// 止めている間も現在値は書き足し、範囲は動かない。
		feed.sync(config(), NOW + 1000, {}, [11]);
		expect(feed.view).toEqual(past);
		expect(feed.live[PEN]?.samples).toHaveLength(2);
		await vi.advanceTimersByTimeAsync(DEBOUNCE);
		expect(historyCalls(calls)).toHaveLength(2);
		expect(calls[1].params).toMatchObject({ fromMs: past.startMs, toMs: past.endMs - 1 });

		feed.followLatest();
		expect(feed.following).toBe(true);
		expect(feed.view).toEqual({ startMs: NOW + 1000 - 10 * MIN, endMs: NOW + 1000 });
	});

	it('右端を今まで戻すと追従を再開し、時間窓を替えたら窓の幅で追従に戻る', () => {
		const { fetch } = controllableFetch();
		const feed = newFeed(fetch);
		feed.configure(config(), NOW);
		feed.rangeChange({ startMs: NOW - 20 * MIN, endMs: NOW - 10 * MIN });
		expect(feed.following).toBe(false);
		feed.rangeChange({ startMs: NOW - 5 * MIN, endMs: NOW });
		expect(feed.following).toBe(true);
		expect(feed.view).toEqual({ startMs: NOW - 5 * MIN, endMs: NOW });

		feed.rangeChange({ startMs: NOW - 20 * MIN, endMs: NOW - 10 * MIN });
		feed.configure(config({ windowMs: MIN }), NOW + 1000);
		expect(feed.following).toBe(true);
		expect(feed.view).toEqual({ startMs: NOW + 1000 - MIN, endMs: NOW + 1000 });
	});

	it('履歴の区間が liveFrom をまたぐとき、境界の区間を 1 区間で読み直して足す', async () => {
		const { fetch, calls } = controllableFetch();
		const feed = newFeed(fetch);
		feed.sync(config(), NOW, {}, [10]);
		feed.sync(config(), NOW + 2000, {}, [10]);
		await vi.advanceTimersByTimeAsync(GRACE + DEBOUNCE);
		// サーバーが幅を 60 秒に広げた。最後の区間 [NOW-30s, NOW+30s) が liveFrom（NOW）をまたぐ。
		calls[0].resolve(
			ready({
				series: [
					series(7, 60_000, [
						[NOW - 90_000, 1],
						[NOW - 30_000, 2]
					])
				]
			})
		);
		await vi.advanceTimersByTimeAsync(0);
		await vi.advanceTimersByTimeAsync(DEBOUNCE);
		const boundary = calls.filter((c) => c.params.bins === 1);
		expect(boundary).toHaveLength(1);
		expect(boundary[0].params).toEqual({
			tagIds: [7],
			fromMs: NOW - 30_000,
			toMs: NOW - 1,
			bins: 1
		});
		// 読み直しが届くまで、その区間は欠測。
		const before = feed.data?.series[PEN]?.bins.find((b) => b.tMs === NOW - 30_000);
		expect(before).toEqual({ tMs: NOW - 30_000, min: null, max: null });
		boundary[0].resolve(ready({ series: [series(7, 30_000, [[NOW - 30_000, 5]])] }));
		await vi.advanceTimersByTimeAsync(0);
		const after = feed.data?.series[PEN]?.bins.find((b) => b.tMs === NOW - 30_000);
		expect(after).toEqual({ tMs: NOW - 30_000, min: 5, max: 5 });
	});

	it('不明なタグ（このグループのペンだけ）とシミュレーションのタグを拾う', async () => {
		const { fetch, calls } = controllableFetch();
		const feed = newFeed(fetch);
		feed.configure(config({ penTagIds: [7, 9] }), NOW);
		await vi.advanceTimersByTimeAsync(GRACE + DEBOUNCE);
		calls[0].resolve(
			ready({
				series: [{ ...series(7, 1000, [[NOW - 5000, 1]]), simulation: true }],
				unknownTagIds: [9, 99]
			})
		);
		await vi.advanceTimersByTimeAsync(0);
		expect(feed.unknownTagIds).toEqual([9]);
		expect(feed.simulationTagIds).toEqual([7]);
	});

	it('sync: 同じスナップショットは 2 回書かない。reset で止めて捨て、飛行中の応答も書かない', async () => {
		const { fetch, calls } = controllableFetch();
		const feed = newFeed(fetch);
		const snapshot = {};
		feed.sync(config(), NOW, snapshot, [1]);
		feed.sync(config(), NOW + 1000, snapshot, [2]);
		expect(feed.live[PEN]?.samples).toEqual([{ tMs: NOW, value: 1 }]);
		expect(feed.nowMs).toBe(NOW + 1000);

		await vi.advanceTimersByTimeAsync(GRACE + DEBOUNCE);
		feed.reset();
		expect(calls[0].signal.aborted).toBe(true);
		calls[0].resolve(ready({ series: [series(7, 1000, [[NOW - 5000, 1]])] }));
		await vi.advanceTimersByTimeAsync(0);
		expect(feed.key).toBeNull();
		expect(feed.history).toBeNull();
		expect(feed.view).toBeNull();
		expect(feed.data).toBeNull();
		expect(feed.nowMs).toBeNull();
		expect(feed.historyState).toBe('idle');
	});

	it('構成のキーはグループとペンの並びで変わり、時間窓・幅では変わらない', () => {
		const base = trendConfigKey(config());
		expect(trendConfigKey(config({ groupId: 2 }))).not.toBe(base);
		expect(trendConfigKey(config({ penTagIds: [7, 7] }))).not.toBe(base);
		expect(trendConfigKey(config({ windowMs: MIN }))).toBe(base);
		expect(trendConfigKey(config({ plotWidthPx: 100 }))).toBe(base);
	});
});

describe('skipTinyRanges（境界の読み直しの 2 ms 未満）', () => {
	it('幅 2 ms 未満はサーバーを呼ばず空の結果（欠測）、2 ms 以上は呼ぶ', async () => {
		const inner = vi.fn<TrendFetch>(async () => ({ x: { bins: [], binMs: 1 } }));
		const fetch = skipTinyRanges(inner);
		const signal = new AbortController().signal;
		await expect(
			fetch({ range: { startMs: 10, endMs: 11 }, penIds: ['x'], bins: 1, signal })
		).resolves.toEqual({});
		expect(inner).not.toHaveBeenCalled();
		await fetch({ range: { startMs: 10, endMs: 12 }, penIds: ['x'], bins: 1, signal });
		expect(inner).toHaveBeenCalledTimes(1);
	});
});
