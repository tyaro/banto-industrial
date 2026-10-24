/**
 * `structRegistration.ts` のユニットテスト（R2b-6d-2a）。アドレスの計算は REST
 * （`parse-addresses` / `offset-addresses`）の結果を受け取る純関数と、REST を呼ぶ薄い層に分かれたので、
 * REST は SLMP の `D` 系（10 進のワード・`.N` のビット）を真似た偽の `api`（`addressFake.ts`）で差し替える。
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
	allocateStructFields,
	assembleAutoStructRows,
	checkStructBase,
	cumulativeWordOffsets,
	detectStructAddressCollisions,
	manualStructRows,
	structFieldWordCounts,
	structRowsToTagInputs,
	STRUCT_BASE_BIT_ERROR,
	type ExistingTagForCollision,
	type StructAllocatedRow,
	type StructField
} from './structRegistration';
import { fakeAddressApi, fakeParse } from './addressFake';
import type { ParsedAddress } from './tagRegistryAdmin';

const f = (name: string, dataType: StructField['dataType'], extra: Partial<StructField> = {}) => ({
	name,
	dataType,
	...extra
});

describe('allocateStructFields: ワード累積の自動割付（REST は偽）', () => {
	it('i16 3 フィールドは +1 ワードずつ連続する', async () => {
		const api = fakeAddressApi();
		const result = await allocateStructFields(
			1,
			'D3000',
			[f('a', 'i16'), f('b', 'i16'), f('c', 'i16')],
			api
		);
		expect(result.ok && result.rows.map((r) => r.address)).toEqual(['D3000', 'D3001', 'D3002']);
	});

	it('u16 の直後に f32 を置く混在型: 累積オフセットは前のフィールドまでのワード数の和', async () => {
		const api = fakeAddressApi();
		const result = await allocateStructFields(
			1,
			'D100',
			[f('a', 'u16'), f('b', 'f32'), f('c', 'u16'), f('d', 'f64'), f('e', 'i16')],
			api
		);
		expect(result.ok && result.rows.map((r) => r.address)).toEqual([
			'D100',
			'D101',
			'D103',
			'D104',
			'D108'
		]);
	});

	it('累積のオフセットは unit = words で offset-addresses に渡し、接続を項目ごとに付ける', async () => {
		const api = fakeAddressApi();
		await allocateStructFields(7, 'D100', [f('a', 'f32'), f('b', 'u16'), f('c', 'i16')], api);
		expect(api.offsetCalls).toHaveLength(1);
		expect(api.offsetCalls[0].map((i) => [i.connectionId, i.n, i.unit])).toEqual([
			[7, 0, 'words'],
			[7, 2, 'words'],
			[7, 3, 'words']
		]);
	});

	it('string 型は文字列長ぶんのワードを占有し、stringEncoding を引き継ぐ', async () => {
		const api = fakeAddressApi();
		const result = await allocateStructFields(
			1,
			'D100',
			[f('s', 'string', { stringLength: 4, stringEncoding: 'shift_jis' }), f('n', 'i16')],
			api
		);
		expect(result.ok && result.rows.map((r) => r.address)).toEqual(['D100', 'D104']);
		expect(result.ok && result.rows[0]).toMatchObject({
			stringLength: 4,
			stringEncoding: 'shift_jis'
		});
	});

	it('bit フィールドは 1 ワード占有として割り付ける（ビット位置のパッキングはしない）', async () => {
		const api = fakeAddressApi();
		const result = await allocateStructFields(1, 'D100', [f('x', 'bit'), f('y', 'bit')], api);
		expect(result.ok && result.rows.map((r) => r.address)).toEqual(['D100', 'D101']);
	});

	it('ベースアドレスがワード内ビット（D100.5）なら断る（bit が null でない）', async () => {
		const result = await allocateStructFields(1, 'D100.5', [f('a', 'i16')], fakeAddressApi());
		expect(result).toEqual({ ok: false, error: STRUCT_BASE_BIT_ERROR });
	});

	it('ドライバが解釈できないベースは、API のメッセージつきで断る（推測しない）', async () => {
		const result = await allocateStructFields(1, '???', [f('a', 'i16')], fakeAddressApi());
		expect(result.ok).toBe(false);
		if (!result.ok) expect(result.error).toContain('解釈できません');
	});

	it('累積の途中で範囲外になった行は、その行のエラー（API のメッセージつき）', async () => {
		const api = fakeAddressApi();
		api.offset = async (items) =>
			items.map((item, i) =>
				i === 1
					? { ok: false as const, message: 'アドレスが範囲外です' }
					: fakeParse({ tagKind: 'plc', address: item.address, dataType: item.dataType })
			);
		const result = await allocateStructFields(1, 'D100', [f('a', 'i16'), f('b', 'i16')], api);
		expect(result.ok).toBe(false);
		if (!result.ok) {
			expect(result.error).toContain('2行目（b）');
			expect(result.error).toContain('アドレスが範囲外です');
		}
	});

	it('ベースアドレスが空・フィールド 0 件・名前が空・string の長さ不正は API を呼ばずにエラー', async () => {
		const api = fakeAddressApi();
		expect((await allocateStructFields(1, '  ', [f('a', 'i16')], api)).ok).toBe(false);
		expect((await allocateStructFields(1, 'D100', [], api)).ok).toBe(false);
		expect((await allocateStructFields(1, 'D100', [f('', 'i16')], api)).ok).toBe(false);
		expect((await allocateStructFields(1, 'D100', [f('s', 'string')], api)).ok).toBe(false);
		const fractional = await allocateStructFields(
			1,
			'D100',
			[f('s', 'string', { stringLength: 1.5 })],
			api
		);
		expect(fractional.ok).toBe(false);
		expect(api.parseCalls).toHaveLength(0);
		expect(api.offsetCalls).toHaveLength(0);
	});

	it('通信の失敗はエラーにする（推測で埋めない）', async () => {
		const api = fakeAddressApi();
		api.parse = async () => {
			throw new Error('network down');
		};
		const result = await allocateStructFields(1, 'D100', [f('a', 'i16')], api);
		expect(result.ok).toBe(false);
		if (!result.ok) expect(result.error).toContain('network down');
	});
});

describe('manualStructRows: 手動割付', () => {
	it('各フィールドの明示アドレスを解析して正規形にする', async () => {
		const result = await manualStructRows(
			1,
			[f('a', 'i16', { address: 'd200' }), f('b', 'f32', { address: 'D300' })],
			fakeAddressApi()
		);
		expect(result.ok && result.rows.map((r) => r.address)).toEqual(['D200', 'D300']);
		expect(result.ok && result.rows[1].span).toMatchObject({ area: 'D', index: 300, wordCount: 2 });
	});

	it('解釈できない行は、その行のエラー（API のメッセージつき）', async () => {
		const result = await manualStructRows(
			1,
			[f('a', 'i16', { address: 'D200' }), f('b', 'i16', { address: 'oops' })],
			fakeAddressApi()
		);
		expect(result.ok).toBe(false);
		if (!result.ok) expect(result.error).toContain('2行目（b）');
	});

	it('アドレス未入力の行・フィールド 0 件はエラー', async () => {
		expect((await manualStructRows(1, [f('a', 'i16', { address: '' })], fakeAddressApi())).ok).toBe(
			false
		);
		expect((await manualStructRows(1, [], fakeAddressApi())).ok).toBe(false);
	});
});

describe('純関数: checkStructBase / structFieldWordCounts / cumulativeWordOffsets / assembleAutoStructRows', () => {
	const ok = (canonical: string, bit: number | null = null, wordCount = 1): ParsedAddress => ({
		ok: true,
		canonical,
		area: 'D',
		writableArea: true,
		index: 100,
		bit,
		wordCount
	});

	it('checkStructBase: どちらかの型で通れば良く、bit が付いていれば断る', () => {
		expect(checkStructBase([ok('D100'), { ok: false, message: 'x' }])).toEqual({ ok: true });
		expect(checkStructBase([{ ok: false, message: 'x' }, ok('D100')])).toEqual({ ok: true });
		expect(checkStructBase([ok('D100.5', 5), ok('D100.5', 5)])).toEqual({
			ok: false,
			error: STRUCT_BASE_BIT_ERROR
		});
		const both = checkStructBase([
			{ ok: false, message: '範囲外' },
			{ ok: false, message: '範囲外' }
		]);
		expect(both.ok).toBe(false);
	});

	it('structFieldWordCounts: bit 型は 1、それ以外は解析結果の wordCount、失敗は行のエラー', () => {
		const fields = [f('a', 'bit'), f('b', 'f32'), f('c', 'i64')];
		expect(
			structFieldWordCounts(fields, [undefined, ok('D100', null, 2), ok('D100', null, 4)])
		).toEqual({ ok: true, counts: [1, 2, 4] });
		const failed = structFieldWordCounts(fields, [
			undefined,
			ok('D100', null, 2),
			{ ok: false, message: 'この型は使えません' }
		]);
		expect(failed.ok).toBe(false);
		if (!failed.ok) expect(failed.error).toContain('3行目（c）');
	});

	it('cumulativeWordOffsets: 先頭は 0、以降は前までの和', () => {
		expect(cumulativeWordOffsets([1, 2, 4, 1])).toEqual([0, 1, 3, 7]);
		expect(cumulativeWordOffsets([])).toEqual([]);
	});

	it('assembleAutoStructRows: 結果の欠落は行のエラー', () => {
		const result = assembleAutoStructRows([f('a', 'i16'), f('b', 'i16')], [ok('D100')]);
		expect(result.ok).toBe(false);
		if (!result.ok) expect(result.error).toContain('2行目（b）');
	});
});

// --- 衝突検出 ---------------------------------------------------------------------------------

type SpanLike = { area: string | null; index: number | null; wordCount: number };

function row(name: string, address: string, span: SpanLike): StructAllocatedRow {
	return { name, address, dataType: 'i16', span: { canonical: address, ...span } };
}

function existing(
	name: string,
	groupId: number,
	canonical: string,
	span: SpanLike | null
): ExistingTagForCollision {
	return { name, collectionGroupId: groupId, span: span && { canonical, ...span } };
}

describe('detectStructAddressCollisions: フィールド間・名前', () => {
	it('同じ area で [index, index+wordCount-1] が重なると field-address-overlap', () => {
		const rows = [
			row('a', 'D100', { area: 'D', index: 100, wordCount: 2 }),
			row('b', 'D101', { area: 'D', index: 101, wordCount: 1 })
		];
		const collisions = detectStructAddressCollisions(rows, [], 1);
		expect(collisions.map((c) => [c.index, c.kind])).toEqual([
			[0, 'field-address-overlap'],
			[1, 'field-address-overlap']
		]);
	});

	it('隣り合うだけ（D100 の 2 ワードと D102）は重ならない', () => {
		const rows = [
			row('a', 'D100', { area: 'D', index: 100, wordCount: 2 }),
			row('b', 'D102', { area: 'D', index: 102, wordCount: 1 })
		];
		expect(detectStructAddressCollisions(rows, [], 1)).toEqual([]);
	});

	it('別の area は index が同じでも重ならない', () => {
		const rows = [
			row('a', 'D100', { area: 'D', index: 100, wordCount: 1 }),
			row('b', 'W100', { area: 'W', index: 100, wordCount: 1 })
		];
		expect(detectStructAddressCollisions(rows, [], 1)).toEqual([]);
	});

	it('ワード内ビットは bit を見ずにワードを占める（同じワードの別のビットも重なる）', () => {
		const rows = [
			row('a', 'D100.1', { area: 'D', index: 100, wordCount: 1 }),
			row('b', 'D100.2', { area: 'D', index: 100, wordCount: 1 })
		];
		expect(detectStructAddressCollisions(rows, [], 1).map((c) => c.kind)).toEqual([
			'field-address-overlap',
			'field-address-overlap'
		]);
	});

	it('index が無い（位置を持たないドライバ）ものは canonical の一致だけを見る', () => {
		const same = [
			row('a', 'FOO.1', { area: 'FOO', index: null, wordCount: 1 }),
			row('b', 'FOO.1', { area: 'FOO', index: null, wordCount: 1 })
		];
		expect(detectStructAddressCollisions(same, [], 1)).toHaveLength(2);
		const different = [
			row('a', 'FOO.1', { area: 'FOO', index: null, wordCount: 1 }),
			row('b', 'FOO.2', { area: 'FOO', index: null, wordCount: 1 })
		];
		expect(detectStructAddressCollisions(different, [], 1)).toEqual([]);
	});

	it('構造体内でフィールド名が重複すると field-name-duplicate', () => {
		const rows = [
			row('a', 'D100', { area: 'D', index: 100, wordCount: 1 }),
			row('a', 'D101', { area: 'D', index: 101, wordCount: 1 })
		];
		expect(detectStructAddressCollisions(rows, [], 1).map((c) => c.kind)).toEqual([
			'field-name-duplicate'
		]);
	});
});

describe('detectStructAddressCollisions: 既存タグとの衝突（収集グループで絞る）', () => {
	const r = [row('x', 'D100', { area: 'D', index: 100, wordCount: 2 })];

	it('同じグループの既存タグと占有が重なると existing-address-overlap', () => {
		const found = detectStructAddressCollisions(
			r,
			[existing('old', 1, 'D101', { area: 'D', index: 101, wordCount: 1 })],
			1
		);
		expect(found.map((c) => c.kind)).toEqual(['existing-address-overlap']);
		expect(found[0].message).toContain('既存タグ「old」');
	});

	it('別の収集グループの既存タグとは、同じ PLC でも衝突しない', () => {
		expect(
			detectStructAddressCollisions(
				r,
				[existing('old', 2, 'D101', { area: 'D', index: 101, wordCount: 1 })],
				1
			)
		).toEqual([]);
	});

	it('同名の既存タグは existing-name-duplicate（占有が分からない既存タグでも名前は見る）', () => {
		const found = detectStructAddressCollisions(r, [existing('x', 1, 'x', null)], 1);
		expect(found.map((c) => c.kind)).toEqual(['existing-name-duplicate']);
	});

	it('占有が分からない既存タグはアドレスでは衝突しない', () => {
		expect(detectStructAddressCollisions(r, [existing('old', 1, 'D100', null)], 1)).toEqual([]);
	});

	it('衝突がなければ空配列', () => {
		expect(
			detectStructAddressCollisions(
				r,
				[existing('old', 1, 'D200', { area: 'D', index: 200, wordCount: 1 })],
				1
			)
		).toEqual([]);
	});
});

describe('structRowsToTagInputs', () => {
	it('割付結果を TagInput 配列へ変換する（アドレスは正規形）', () => {
		const rows: StructAllocatedRow[] = [
			{
				name: 'a',
				address: 'D100',
				dataType: 'i16',
				span: { canonical: 'D100', area: 'D', index: 100, wordCount: 1 }
			},
			{
				name: 's',
				address: 'D101',
				dataType: 'string',
				stringLength: 4,
				stringEncoding: 'shift_jis',
				span: { canonical: 'D101', area: 'D', index: 101, wordCount: 4 }
			}
		];
		const inputs = structRowsToTagInputs(rows, {
			collectionGroupId: 3,
			enabled: true,
			writable: false
		});
		expect(inputs[0]).toMatchObject({
			name: 'a',
			collectionGroupId: 3,
			address: 'D100',
			dataType: 'i16',
			decimals: 0,
			enabled: true,
			writable: false
		});
		expect(inputs[1]).toMatchObject({ stringLength: 4, stringEncoding: 'shift_jis' });
	});
});
