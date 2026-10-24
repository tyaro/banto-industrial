/**
 * `addressSpan.ts` のユニットテスト（R2b-6d-2a）。占有の重なり判定（設計 §3.6）と、既存タグ全件の
 * 占有を引く薄い層（接続ごとにまとめる・10,000 件の上限で分ける）を、`addressFake.ts` の偽の REST で確かめる。
 */
import { describe, expect, it, vi } from 'vitest';

// `addressSpan.ts` は REST のヘルパー（`tagRegistryAdmin.ts`）を既定の呼び口として import する。
// `$state` を使うモジュールの読み込みで通信が始まらないよう、`tagCsvDiff.test.ts` と同じ軽量な
// フェイクに差し替える（このテストは REST を偽の `api` で差し替えて呼ぶ）。
vi.mock('@banto/admin-core', () => ({
	getAuthProvider: () => ({ getToken: () => 'test-token' }),
	ProviderError: class ProviderError extends Error {}
}));
vi.mock('./setup', () => ({ CSRF_HEADER: { 'X-Banto-Client': 'banto' } }));
vi.mock('./deferredDelete.svelte', () => ({ deferredDelete: { flush: async () => {} } }));

import {
	chunked,
	fetchExistingSpans,
	spanOfParsed,
	spansOverlap,
	type AddressSpan,
	type ExistingTagForSpan
} from './addressSpan';
import { fakeAddressApi } from './addressFake';

const span = (
	canonical: string,
	area: string | null,
	index: number | null,
	wordCount = 1
): AddressSpan => ({ canonical, area, index, wordCount });

describe('spanOfParsed', () => {
	it('成功した結果から占有を作り、wordCount は最低 1', () => {
		expect(
			spanOfParsed({
				ok: true,
				canonical: 'D100',
				area: 'D',
				writableArea: true,
				index: 100,
				bit: null,
				wordCount: 0
			})
		).toEqual(span('D100', 'D', 100, 1));
	});

	it('失敗・undefined は null', () => {
		expect(spanOfParsed({ ok: false, message: 'x' })).toBeNull();
		expect(spanOfParsed(undefined)).toBeNull();
	});
});

describe('spansOverlap（§3.6）', () => {
	it('同じ area で範囲が重なれば true・隣り合うだけなら false', () => {
		expect(spansOverlap(span('D100', 'D', 100, 2), span('D101', 'D', 101))).toBe(true);
		expect(spansOverlap(span('D100', 'D', 100, 2), span('D102', 'D', 102))).toBe(false);
		expect(spansOverlap(span('D101', 'D', 101), span('D100', 'D', 100, 2))).toBe(true);
	});

	it('別の area は false', () => {
		expect(spansOverlap(span('D100', 'D', 100), span('W100', 'W', 100))).toBe(false);
	});

	it('index が無ければ canonical の一致だけ（area が同じでも）', () => {
		expect(spansOverlap(span('X.1', 'X', null), span('X.1', 'X', null))).toBe(true);
		expect(spansOverlap(span('X.1', 'X', null), span('X.2', 'X', null))).toBe(false);
		expect(spansOverlap(span('D100', 'D', 100), span('D100', 'D', null))).toBe(true);
		expect(spansOverlap(span('D100', 'D', 100), span('D101', 'D', null))).toBe(false);
	});

	it('area が無ければ canonical の一致だけ', () => {
		expect(spansOverlap(span('A', null, null), span('A', null, null))).toBe(true);
		expect(spansOverlap(span('A', null, null), span('B', null, null))).toBe(false);
	});
});

describe('chunked', () => {
	it('size 件ずつに分ける', () => {
		expect(chunked([1, 2, 3, 4, 5], 2)).toEqual([[1, 2], [3, 4], [5]]);
		expect(chunked([], 3)).toEqual([]);
	});
});

describe('fetchExistingSpans', () => {
	const t = (id: number, address: string, extra: Partial<ExistingTagForSpan> = {}) => ({
		id,
		collectionGroupId: 1,
		address,
		dataType: 'i16' as const,
		stringLength: null,
		stringEncoding: 'utf8' as const,
		tagKind: 'plc' as const,
		...extra
	});

	it('接続ごとにまとめて 1 回ずつ引き、plc 以外・接続の分からないものは対象外', async () => {
		const api = fakeAddressApi();
		const spans = await fetchExistingSpans(
			[
				t(1, 'D100'),
				t(2, 'D200', { collectionGroupId: 2 }),
				t(3, 'D300', { tagKind: 'computed' }),
				t(4, 'D400', { collectionGroupId: 9 })
			],
			(g) => (g === 1 ? 10 : g === 2 ? 20 : undefined),
			api
		);
		expect(api.parseCalls.map((c) => c.connectionId).sort()).toEqual([10, 20]);
		expect([...spans.keys()].sort()).toEqual([1, 2]);
		expect(spans.get(1)).toEqual(span('D100', 'D', 100, 1));
	});

	it('10,000 件を超えるときは分けて呼ぶ', async () => {
		const api = fakeAddressApi();
		const tags = Array.from({ length: 10_001 }, (_, i) => t(i + 1, `D${i}`));
		const spans = await fetchExistingSpans(tags, () => 10, api);
		expect(api.parseCalls.map((c) => c.items.length)).toEqual([10_000, 1]);
		expect(spans.size).toBe(10_001);
	});

	it('解析できなかったタグは、保存済みの文字列を canonical にした位置なしの占有にする', async () => {
		const api = fakeAddressApi();
		const spans = await fetchExistingSpans([t(1, '???')], () => 10, api);
		expect(spans.get(1)).toEqual(span('???', null, null, 1));
	});

	it('通信の失敗は例外（衝突なしに倒さない）', async () => {
		const api = fakeAddressApi();
		api.parse = async () => {
			throw new Error('down');
		};
		await expect(fetchExistingSpans([t(1, 'D100')], () => 10, api)).rejects.toThrow('down');
	});
});
