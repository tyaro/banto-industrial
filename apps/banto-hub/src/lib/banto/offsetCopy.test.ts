/**
 * `offsetCopy.ts` のユニットテスト（R2b-6d-2a）。コピー先のアドレスは REST（`offset-addresses`、
 * `unit: "words"`）の結果を受け取る純関数 {@link assembleOffsetCopy} と、REST を呼ぶ薄い層
 * {@link computeOffsetCopy} に分かれたので、REST は `addressFake.ts` の偽で差し替える。
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
	assembleOffsetCopy,
	computeOffsetCopy,
	isDeviceNameBasedName,
	offsetCopyName,
	offsetCopyRowsToTagInputs,
	type OffsetCopyRow
} from './offsetCopy';
import { fakeAddressApi, fakeParse } from './addressFake';
import type { ExistingTagForCollision } from './structRegistration';
import type { ParsedAddress, Tag } from './tagRegistryAdmin';

function tag(overrides: Partial<Tag> & Pick<Tag, 'id' | 'name' | 'address'>): Tag {
	return {
		collectionGroupId: 1,
		dataType: 'i16',
		stringLength: null,
		stringEncoding: 'utf8',
		rawLo: null,
		rawHi: null,
		engLo: null,
		engHi: null,
		unit: null,
		decimals: 0,
		enabled: true,
		writable: false,
		tagKind: 'plc',
		expression: null,
		...overrides
	} as Tag;
}

const ok = (canonical: string, area = 'D', index = 0, wordCount = 1): ParsedAddress => ({
	ok: true,
	canonical,
	area,
	writableArea: true,
	index,
	bit: null,
	wordCount
});

describe('isDeviceNameBasedName（名前が元のアドレスの正規形と大小文字を無視して一致）', () => {
	it('大文字・小文字を無視して true', () => {
		expect(isDeviceNameBasedName('D3000', 'D3000')).toBe(true);
		expect(isDeviceNameBasedName('d3000', 'D3000')).toBe(true);
	});

	it('ワード内ビットは bit を含む正規形と一致するときだけ true', () => {
		expect(isDeviceNameBasedName('D100.5', 'D100.5')).toBe(true);
		expect(isDeviceNameBasedName('D100', 'D100.5')).toBe(false);
	});

	it('Modbus は正規形（400001）と一致するときだけ true（40001 では false）', () => {
		expect(isDeviceNameBasedName('400001', '400001')).toBe(true);
		expect(isDeviceNameBasedName('40001', '400001')).toBe(false);
	});

	it('意味のある名前は false・前後の空白は無視・空は false', () => {
		expect(isDeviceNameBasedName('temp01', 'D3000')).toBe(false);
		expect(isDeviceNameBasedName('  D3000  ', 'D3000')).toBe(true);
		expect(isDeviceNameBasedName('', 'D3000')).toBe(false);
		expect(isDeviceNameBasedName('D3000', '')).toBe(false);
	});
});

describe('offsetCopyName', () => {
	it('アドレス由来（大文字）は新アドレスの大文字表記・（小文字）は小文字表記になる', () => {
		expect(offsetCopyName({ name: 'D3000', address: 'D3000' }, 'D3100', new Set())).toBe('D3100');
		expect(offsetCopyName({ name: 'd3000', address: 'D3000' }, 'D3100', new Set())).toBe('d3100');
	});

	it('Modbus のアドレス由来は新しい正規形そのものになる', () => {
		expect(offsetCopyName({ name: '400001', address: '400001' }, '400011', new Set())).toBe(
			'400011'
		);
	});

	it('末尾に数字を持つ意味名は +1・持たない意味名は 2 から', () => {
		expect(offsetCopyName({ name: 'temp01', address: 'D3000' }, 'D3100', new Set())).toBe('temp02');
		expect(offsetCopyName({ name: 'pressure', address: 'D3000' }, 'D3100', new Set())).toBe(
			'pressure2'
		);
	});

	it('taken と衝突する場合は衝突しない数字まで進め、採った名前を taken に積む', () => {
		const taken = new Set(['temp02', 'temp03']);
		expect(offsetCopyName({ name: 'temp01', address: 'D3000' }, 'D3100', taken)).toBe('temp04');
		expect(taken.has('temp04')).toBe(true);
	});
});

describe('assembleOffsetCopy（REST の結果を受け取る純関数）', () => {
	const a = tag({ id: 1, name: 'D3000', address: 'D3000' });
	const b = tag({ id: 2, name: 'temp01', address: 'D3001' });

	it('REST が返したアドレスをコピー先にし、名前は命名ルールで決める', () => {
		const result = assembleOffsetCopy(
			[a, b],
			100,
			[ok('D3100', 'D', 3100), ok('D3101', 'D', 3101)],
			[]
		);
		expect(result.errors).toEqual([]);
		expect(result.rows.map((r) => [r.sourceId, r.name, r.address])).toEqual([
			[1, 'D3100', 'D3100'],
			[2, 'temp02', 'D3101']
		]);
	});

	it('API が失敗した source は行を作らず、API のメッセージつきのエラーにする（推測しない）', () => {
		const result = assembleOffsetCopy(
			[a, b],
			100,
			[ok('D3100', 'D', 3100), { ok: false, message: 'アドレスが範囲外です' }],
			[]
		);
		expect(result.rows.map((r) => r.sourceId)).toEqual([1]);
		expect(result.errors).toHaveLength(1);
		expect(result.errors[0]).toMatchObject({ sourceId: 2 });
		expect(result.errors[0].message).toContain('アドレスが範囲外です');
	});

	it('結果が無い（応答の欠落）source もエラー', () => {
		const result = assembleOffsetCopy([a], 100, [], []);
		expect(result.rows).toEqual([]);
		expect(result.errors).toHaveLength(1);
	});

	it.each([0, -1, -1000, 1.5])(
		'オフセット %s は「1以上の整数」を満たさず、全 source をエラーにする',
		(offset) => {
			const result = assembleOffsetCopy([a, b], offset, [ok('D1'), ok('D2')], []);
			expect(result.rows).toEqual([]);
			expect(result.errors.map((e) => e.sourceId)).toEqual([1, 2]);
			expect(result.errors[0].message).toContain('1以上の整数');
		}
	);

	it('既存タグと占有が重なるコピー先は衝突エラーにする（行は残す）', () => {
		const existing: ExistingTagForCollision[] = [
			{
				name: 'other',
				collectionGroupId: 1,
				span: { canonical: 'D3100', area: 'D', index: 3100, wordCount: 1 }
			}
		];
		const result = assembleOffsetCopy([a], 100, [ok('D3100', 'D', 3100)], existing);
		expect(result.rows).toHaveLength(1);
		expect(result.errors).toHaveLength(1);
		expect(result.errors[0].message).toContain('既存タグ「other」');
	});

	it('#325: 4 ワード占有の既存タグの範囲内に入るコピー先とも重なりを検出する', () => {
		const existing: ExistingTagForCollision[] = [
			{
				name: 'wide',
				collectionGroupId: 1,
				span: { canonical: 'D3098', area: 'D', index: 3098, wordCount: 4 }
			}
		];
		const result = assembleOffsetCopy([a], 100, [ok('D3100', 'D', 3100)], existing);
		expect(result.errors).toHaveLength(1);
	});

	it('別グループの既存タグとはアドレスが重なっても衝突扱いしない', () => {
		const existing: ExistingTagForCollision[] = [
			{
				name: 'other',
				collectionGroupId: 2,
				span: { canonical: 'D3100', area: 'D', index: 3100, wordCount: 1 }
			}
		];
		const result = assembleOffsetCopy([a], 100, [ok('D3100', 'D', 3100)], existing);
		expect(result.errors).toEqual([]);
	});

	it('同一バッチ内で 2 つの選択タグが同じアドレスへ衝突するとエラー', () => {
		const result = assembleOffsetCopy(
			[a, b],
			100,
			[ok('D3100', 'D', 3100), ok('D3100', 'D', 3100)],
			[]
		);
		expect(result.errors.length).toBeGreaterThanOrEqual(1);
		expect(result.errors.some((e) => e.message.includes('重なって'))).toBe(true);
	});

	it('占有が分からない既存タグ（span null）は名前の衝突だけ見る', () => {
		const existing: ExistingTagForCollision[] = [
			{ name: 'D3100', collectionGroupId: 1, span: null }
		];
		const result = assembleOffsetCopy([a], 100, [ok('D3100', 'D', 3100)], existing);
		expect(result.errors.map((e) => e.message).join()).toContain('既に登録されています');
	});

	it('string タグは stringEncoding を source から引き継ぐ', () => {
		const s = tag({
			id: 3,
			name: 'str',
			address: 'D10',
			dataType: 'string',
			stringLength: 4,
			stringEncoding: 'shift_jis'
		});
		const result = assembleOffsetCopy([s], 10, [ok('D20', 'D', 20, 4)], []);
		expect(result.rows[0]).toMatchObject({ stringLength: 4, stringEncoding: 'shift_jis' });
	});
});

describe('computeOffsetCopy（REST を呼ぶ薄い層）', () => {
	const group1Conn = (g: number) => (g === 1 ? 10 : g === 2 ? 20 : undefined);

	it('全件を unit = words・n = オフセットで 1 回の offset-addresses に渡し、接続は収集グループから引く', async () => {
		const api = fakeAddressApi();
		const sources = [
			tag({ id: 1, name: 'a', address: 'D100', collectionGroupId: 1 }),
			tag({ id: 2, name: 'b', address: 'D200', collectionGroupId: 2 })
		];
		const result = await computeOffsetCopy(sources, 5, sources, group1Conn, api);
		expect(api.offsetCalls).toHaveLength(1);
		expect(api.offsetCalls[0].map((i) => [i.connectionId, i.address, i.n, i.unit])).toEqual([
			[10, 'D100', 5, 'words'],
			[20, 'D200', 5, 'words']
		]);
		expect(result.rows.map((r) => r.address)).toEqual(['D105', 'D205']);
	});

	it('ワード内ビットはワードの部分だけ進め、ビットを保つ（D100.5 + 1 → D101.5）', async () => {
		const api = fakeAddressApi();
		const bit = tag({ id: 1, name: 'flag', address: 'D100.5', dataType: 'bit' });
		const result = await computeOffsetCopy([bit], 1, [bit], group1Conn, api);
		expect(result.rows.map((r) => r.address)).toEqual(['D101.5']);
	});

	it('既存タグ（対象のグループのもの）の占有を parse-addresses で引いて衝突を検出する', async () => {
		const api = fakeAddressApi();
		const src = tag({ id: 1, name: 'a', address: 'D100' });
		const occupied = tag({ id: 2, name: 'other', address: 'D105' });
		const elsewhere = tag({ id: 3, name: 'far', address: 'D105', collectionGroupId: 2 });
		const result = await computeOffsetCopy([src], 5, [src, occupied, elsewhere], group1Conn, api);
		expect(api.parseCalls).toHaveLength(1);
		expect(api.parseCalls[0].connectionId).toBe(10);
		expect(api.parseCalls[0].items.map((i) => i.address)).toEqual(['D100', 'D105']);
		expect(result.errors).toHaveLength(1);
		expect(result.errors[0].message).toContain('既存タグ「other」');
	});

	it('既存タグが 10,000 件を超えるときは分けて呼ぶ', async () => {
		const api = fakeAddressApi();
		const src = tag({ id: 1, name: 'a', address: 'D100' });
		const many = Array.from({ length: 10_001 }, (_, i) =>
			tag({ id: 100 + i, name: `t${i}`, address: `D${5000 + i}` })
		);
		await computeOffsetCopy([src], 5, [src, ...many], group1Conn, api);
		expect(api.parseCalls.map((c) => c.items.length)).toEqual([10_000, 2]);
	});

	it('接続の分からない収集グループの source はそのタグだけエラー', async () => {
		const api = fakeAddressApi();
		const orphan = tag({ id: 1, name: 'a', address: 'D100', collectionGroupId: 99 });
		const fine = tag({ id: 2, name: 'b', address: 'D200', collectionGroupId: 1 });
		const result = await computeOffsetCopy([orphan, fine], 5, [], group1Conn, api);
		expect(result.rows.map((r) => r.sourceId)).toEqual([2]);
		expect(result.errors).toHaveLength(1);
		expect(result.errors[0]).toMatchObject({ sourceId: 1 });
		expect(result.errors[0].message).toContain('接続が見つかりません');
	});

	it('不正なオフセットは API を呼ばずに全件エラー', async () => {
		const api = fakeAddressApi();
		const src = tag({ id: 1, name: 'a', address: 'D100' });
		const result = await computeOffsetCopy([src], 0, [src], group1Conn, api);
		expect(api.offsetCalls).toHaveLength(0);
		expect(result.errors).toHaveLength(1);
	});

	it('offset-addresses の通信の失敗は、衝突なしに倒さず全件のエラーにする', async () => {
		const api = fakeAddressApi();
		api.offset = async () => {
			throw new Error('network down');
		};
		const src = tag({ id: 1, name: 'a', address: 'D100' });
		const result = await computeOffsetCopy([src], 5, [src], group1Conn, api);
		expect(result.rows).toEqual([]);
		expect(result.errors[0].message).toContain('network down');
	});

	it('既存タグの占有を引けなかったときも、衝突なしに倒さず全件のエラーにする', async () => {
		const api = fakeAddressApi();
		api.parse = async () => {
			throw new Error('parse down');
		};
		const src = tag({ id: 1, name: 'a', address: 'D100' });
		const result = await computeOffsetCopy([src], 5, [src], group1Conn, api);
		expect(result.rows).toEqual([]);
		expect(result.errors[0].message).toContain('parse down');
	});
});

describe('offsetCopyRowsToTagInputs', () => {
	const base: OffsetCopyRow = {
		sourceId: 1,
		sourceName: 'a',
		sourceAddress: 'D100',
		name: 'a2',
		address: 'D105',
		collectionGroupId: 1,
		dataType: 'i16',
		unit: 'C',
		decimals: 1,
		rawLo: 0,
		rawHi: 100,
		engLo: 0,
		engHi: 10,
		enabled: true,
		writable: true
	};

	it('TagInput 相当へ変換し、単位・スケーリングを保持する', () => {
		expect(offsetCopyRowsToTagInputs([base])[0]).toMatchObject({
			name: 'a2',
			address: 'D105',
			unit: 'C',
			decimals: 1,
			rawHi: 100,
			engHi: 10,
			writable: true
		});
	});

	it('string タグの stringEncoding をそのまま引き継ぐ', () => {
		const input = offsetCopyRowsToTagInputs([
			{ ...base, dataType: 'string', stringLength: 4, stringEncoding: 'shift_jis' }
		])[0];
		expect(input).toMatchObject({ stringLength: 4, stringEncoding: 'shift_jis' });
	});
});

// fakeParse を使うことで偽の解析と同じ規則を確かめる（偽自体の健全性）。
describe('addressFake', () => {
	it('D 系のアドレスを解析し、解釈できないものは ok:false', () => {
		expect(fakeParse({ tagKind: 'plc', address: 'd10.f', dataType: 'bit' })).toMatchObject({
			ok: true,
			canonical: 'D10.F',
			bit: 15
		});
		expect(fakeParse({ tagKind: 'plc', address: '??', dataType: 'i16' }).ok).toBe(false);
	});
});
