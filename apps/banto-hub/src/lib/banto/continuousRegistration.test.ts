/**
 * `continuousRegistration.ts` の `buildContinuousParams`/`generateContinuousTags` ほかに
 * 対するユニットテスト（`tagFormNumeric.test.ts` と同じ describe/it スタイル）。
 * R2b-6d-2a 以降、アドレスの計算は REST の結果を受け取る純関数になったので、REST の結果は偽で与える。
 *
 * 回帰本体（TAG-P0-1、docs/banto-hub-desktop-plan.md §9）: Svelte 5 の
 * `<input type="number" bind:value>` は `continuousForm.count` へ
 * `number | null` を代入するが、旧 `continuousParams()` は
 * `form.count.trim()` を呼んでいたため `TypeError: count.trim is not a
 * function` でクラッシュしていた。`buildContinuousParams` はこれを
 * `parseOptionalNumber` 経由のパースに置き換えて修正している。
 */
import { describe, expect, it, vi } from 'vitest';

// 純関数の外側の `addressSpan.ts` は REST のヘルパー（`tagRegistryAdmin.ts`）を既定の呼び口として
// import する。`$state` を使うモジュールの読み込みで通信が始まらないよう、`tagCsvDiff.test.ts` と
// 同じ軽量なフェイクに差し替える（このテストは REST を偽の `api` で差し替えて呼ぶ）。
vi.mock('@banto/admin-core', () => ({
	getAuthProvider: () => ({ getToken: () => 'test-token' }),
	ProviderError: class ProviderError extends Error {}
}));
vi.mock('./setup', () => ({ CSRF_HEADER: { 'X-Banto-Client': 'banto' } }));
vi.mock('./deferredDelete.svelte', () => ({ deferredDelete: { flush: async () => {} } }));
import {
	buildContinuousOffsetItems,
	buildContinuousParams,
	computeContinuousTags,
	generateContinuousTags,
	MAX_CONTINUOUS_COUNT,
	namePatternFromParsed,
	nextNamePatternOnParsed,
	nextStartNumberOnParsed,
	startNumberFromParsed,
	type ContinuousFormState
} from './continuousRegistration';
import type { OffsetAddressItem, ParsedAddress } from './tagRegistryAdmin';
import { fakeAddressApi } from './addressFake';

/** REST の結果（`ok: true`）の偽。 */
function okParsed(
	canonical: string,
	area: string | null,
	index: number | null,
	wordCount = 1,
	bit: number | null = null
): ParsedAddress {
	return { ok: true, canonical, area, writableArea: true, index, bit, wordCount };
}

/** `D100` から 1 点ずつの偽のアドレス列。 */
function addressesFor(count: number): ParsedAddress[] {
	return Array.from({ length: Math.max(0, Math.ceil(count)) }, (_, i) =>
		okParsed(`D${100 + i}`, 'D', 100 + i)
	);
}

/** 実フォーム（`+page.svelte` の `blankContinuousForm()`）の初期値に、
 * 生成に必要な最低限の入力（対象グループ・開始アドレス）を埋めたもの。
 * `count` だけ呼び出し側で上書きしてテストする。 */
function baseForm(overrides: Partial<ContinuousFormState> = {}): ContinuousFormState {
	return {
		collectionGroupId: '1',
		namePattern: 'temp{n}',
		startNumber: '1',
		startAddress: 'D100',
		count: '1',
		dataType: 'i16',
		stringLength: '',
		stringEncoding: 'utf8',
		unit: '',
		decimals: '0',
		rawLo: '',
		rawHi: '',
		engLo: '',
		engHi: '',
		enabled: true,
		writable: false,
		...overrides
	};
}

describe('buildContinuousParams', () => {
	it('count が number（Svelte 5 の number input 実代入値）でもクラッシュしない - 回帰本体', () => {
		const params = buildContinuousParams(baseForm({ count: 5 }));
		expect(params?.count).toBe(5);
	});

	it('count が null（number input を空欄にした実代入値）でもクラッシュせず null を返す - 回帰本体', () => {
		expect(buildContinuousParams(baseForm({ count: null }))).toBeNull();
	});

	it('count が undefined でもクラッシュせず null を返す', () => {
		expect(buildContinuousParams(baseForm({ count: undefined as unknown as null }))).toBeNull();
	});

	it('count が空文字列（text 入力互換）でも null を返す', () => {
		expect(buildContinuousParams(baseForm({ count: '' }))).toBeNull();
	});

	it('count が数値化できない文字列でも null を返す', () => {
		expect(buildContinuousParams(baseForm({ count: 'abc' }))).toBeNull();
	});

	it('collectionGroupId が未選択（空文字列）なら null', () => {
		expect(buildContinuousParams(baseForm({ collectionGroupId: '' }))).toBeNull();
	});

	it('namePattern が空白のみなら null', () => {
		expect(buildContinuousParams(baseForm({ namePattern: '   ' }))).toBeNull();
	});

	it('startAddress が空白のみなら null', () => {
		expect(buildContinuousParams(baseForm({ startAddress: '   ' }))).toBeNull();
	});

	it('startNumber/decimals が null でも 0 として組み立てられる（number input クリア時）', () => {
		const params = buildContinuousParams(baseForm({ startNumber: null, decimals: null }));
		expect(params?.startNumber).toBe(0);
		expect(params?.decimals).toBe(0);
	});

	it('スケーリングが null でも組み立てられ、送信時は未設定（null）になる', () => {
		const params = buildContinuousParams(baseForm({ rawLo: null, rawHi: 100 }));
		expect(params?.rawLo).toBeNull();
		expect(params?.rawHi).toBe(100);
	});

	it('#533: 組み立てた入力にしきい値の項目は無い（Hub はしきい値を持たない）', () => {
		const params = buildContinuousParams(baseForm({}));
		expect(params).not.toBeNull();
		expect(Object.keys(params ?? {}).filter((key) => key.startsWith('threshold'))).toEqual([]);
	});
});

describe('buildContinuousParams -> generateContinuousTags（点数と行数の一致 - 回帰本体）', () => {
	it.each([1, 2, 1000])('count=%i で対応件数(%i件)の行が生成される', (count) => {
		const params = buildContinuousParams(baseForm({ count }));
		expect(params).not.toBeNull();
		const result = generateContinuousTags(params!, addressesFor(params!.count));
		expect(result.ok).toBe(true);
		if (result.ok) {
			expect(result.rows).toHaveLength(count);
			expect(result.tags).toHaveLength(count);
		}
	});

	it('MAX_CONTINUOUS_COUNT は 1000 である（このテストの前提）', () => {
		expect(MAX_CONTINUOUS_COUNT).toBe(1000);
	});
});

describe('generateContinuousTags: 不正な点数は ok:false + 人間可読メッセージ', () => {
	it('0 はエラーになる', () => {
		const params = buildContinuousParams(baseForm({ count: 0 }));
		expect(params).not.toBeNull();
		const result = generateContinuousTags(params!, addressesFor(params!.count));
		expect(result.ok).toBe(false);
		if (!result.ok) {
			expect(result.error).toMatch(/1以上/);
		}
	});

	it('負数はエラーになる', () => {
		const params = buildContinuousParams(baseForm({ count: -1 }));
		expect(params).not.toBeNull();
		const result = generateContinuousTags(params!, addressesFor(params!.count));
		expect(result.ok).toBe(false);
		if (!result.ok) {
			expect(result.error).toMatch(/1以上/);
		}
	});

	it('小数（1.5）はエラーになる', () => {
		const params = buildContinuousParams(baseForm({ count: 1.5 }));
		expect(params).not.toBeNull();
		const result = generateContinuousTags(params!, addressesFor(params!.count));
		expect(result.ok).toBe(false);
		if (!result.ok) {
			expect(result.error).toMatch(/整数/);
		}
	});

	it(`上限超え（${MAX_CONTINUOUS_COUNT + 1}）はエラーになる`, () => {
		const params = buildContinuousParams(baseForm({ count: MAX_CONTINUOUS_COUNT + 1 }));
		expect(params).not.toBeNull();
		const result = generateContinuousTags(params!, addressesFor(params!.count));
		expect(result.ok).toBe(false);
		if (!result.ok) {
			expect(result.error).toMatch(new RegExp(`${MAX_CONTINUOUS_COUNT}以下`));
		}
	});
});

// --- R2b-6d-2a: アドレスの計算は REST（offset-addresses / parse-addresses）の結果を使う --------

describe('generateContinuousTags: REST の結果をそのまま行にする', () => {
	it('結果の canonical を、名前の連番と対にして行・タグにする', () => {
		const params = buildContinuousParams(baseForm({ count: 3, startNumber: '10' }));
		const result = generateContinuousTags(params!, [
			okParsed('X1E', 'X', 30),
			okParsed('X1F', 'X', 31),
			okParsed('X20', 'X', 32)
		]);
		expect(result.ok).toBe(true);
		if (result.ok) {
			expect(result.rows).toEqual([
				{ name: 'temp10', address: 'X1E' },
				{ name: 'temp11', address: 'X1F' },
				{ name: 'temp12', address: 'X20' }
			]);
			expect(result.tags.map((t) => t.address)).toEqual(['X1E', 'X1F', 'X20']);
		}
	});

	it('Modbus の 40001 形式は正規形（400001）がそのまま保存の形になる', () => {
		const params = buildContinuousParams(baseForm({ count: 2, startAddress: '40001' }));
		const result = generateContinuousTags(params!, [
			okParsed('400001', 'holding_register', 0),
			okParsed('400002', 'holding_register', 1)
		]);
		expect(result.ok && result.rows.map((r) => r.address)).toEqual(['400001', '400002']);
	});

	it('算出できなかった件は、API のメッセージつきでその件目のエラーにする（末尾の数字を進めて推測しない）', () => {
		const params = buildContinuousParams(baseForm({ count: 3 }));
		const result = generateContinuousTags(params!, [
			okParsed('D100', 'D', 100),
			{ ok: false, message: 'アドレスが範囲外です' },
			okParsed('D102', 'D', 102)
		]);
		expect(result.ok).toBe(false);
		if (!result.ok) {
			expect(result.error).toContain('2件目');
			expect(result.error).toContain('アドレスが範囲外です');
		}
	});

	it('結果が足りない件（応答の欠落）もエラーにする', () => {
		const params = buildContinuousParams(baseForm({ count: 2 }));
		const result = generateContinuousTags(params!, [okParsed('D100', 'D', 100)]);
		expect(result.ok).toBe(false);
		if (!result.ok) expect(result.error).toContain('2件目');
	});

	it('string 型は文字列長の指定が必要（API を呼ぶ前の検証）', () => {
		const params = buildContinuousParams(baseForm({ dataType: 'string', stringLength: '' }));
		const result = generateContinuousTags(params!, addressesFor(1));
		expect(result.ok).toBe(false);
		if (!result.ok) expect(result.error).toMatch(/文字列長/);
	});

	it('string 型は stringLength・stringEncoding をタグへ引き継ぐ', () => {
		const params = buildContinuousParams(
			baseForm({ dataType: 'string', stringLength: '4', stringEncoding: 'shift_jis', count: 1 })
		);
		const result = generateContinuousTags(params!, addressesFor(1));
		expect(result.ok && result.tags[0]).toMatchObject({
			stringLength: 4,
			stringEncoding: 'shift_jis'
		});
	});
});

describe('buildContinuousOffsetItems / computeContinuousTags（REST を呼ぶ薄い層）', () => {
	it('i 件目は n = i・unit = points で、接続と型をそのまま渡す', () => {
		const params = buildContinuousParams(baseForm({ count: 3, dataType: 'f32' }));
		const items = buildContinuousOffsetItems(params!, 7);
		expect(items).toEqual([
			expect.objectContaining({
				connectionId: 7,
				address: 'D100',
				dataType: 'f32',
				n: 0,
				unit: 'points'
			}),
			expect.objectContaining({ n: 1, unit: 'points' }),
			expect.objectContaining({ n: 2, unit: 'points' })
		]);
		expect(items.every((i) => i.tagKind === 'plc')).toBe(true);
	});

	it('全件を 1 回の offset-addresses で求めて行にする', async () => {
		const calls: OffsetAddressItem[][] = [];
		const params = buildContinuousParams(baseForm({ count: 2 }));
		const result = await computeContinuousTags(params!, 3, {
			offset: async (items) => {
				calls.push(items);
				return addressesFor(items.length);
			}
		});
		expect(calls).toHaveLength(1);
		expect(calls[0]).toHaveLength(2);
		expect(result.ok && result.rows.map((r) => r.address)).toEqual(['D100', 'D101']);
	});

	it('入力が不正なら API を呼ばない', async () => {
		let called = false;
		const params = buildContinuousParams(baseForm({ count: 0 }));
		const result = await computeContinuousTags(params!, 3, {
			offset: async () => {
				called = true;
				return [];
			}
		});
		expect(called).toBe(false);
		expect(result.ok).toBe(false);
	});

	it('通信の失敗はエラーにする（推測で埋めない）', async () => {
		const params = buildContinuousParams(baseForm({ count: 2 }));
		const result = await computeContinuousTags(params!, 3, {
			offset: async () => {
				throw new Error('network down');
			}
		});
		expect(result.ok).toBe(false);
		if (!result.ok) expect(result.error).toContain('network down');
	});
});

describe('namePatternFromParsed（§7.1: canonical が area で始まれば {area}{n}、そうでなければ tag{n}）', () => {
	it('D3000 → D{n}', () => {
		expect(namePatternFromParsed(okParsed('D3000', 'D', 3000))).toBe('D{n}');
	});

	it('ワード内ビット D100.5 → D{n}', () => {
		expect(namePatternFromParsed(okParsed('D100.5', 'D', 100, 1, 5))).toBe('D{n}');
	});

	it('canonical と area の大文字小文字は無視する（area が小文字でも一致）', () => {
		expect(namePatternFromParsed(okParsed('W1FF', 'w', 511))).toBe('w{n}');
	});

	it('Modbus の 400001 は領域名が現れないので tag{n}', () => {
		expect(namePatternFromParsed(okParsed('400001', 'holding_register', 0))).toBe('tag{n}');
	});

	it('area が無い（plc 以外・位置を持たないドライバ）なら tag{n}', () => {
		expect(namePatternFromParsed(okParsed('ABC1', null, null))).toBe('tag{n}');
	});

	it('解析に失敗した・結果が無いときは null', () => {
		expect(namePatternFromParsed({ ok: false, message: 'x' })).toBeNull();
		expect(namePatternFromParsed(undefined)).toBeNull();
	});
});

describe('startNumberFromParsed（§7.1: canonical が area で始まれば index、そうでなければ canonical の数字）', () => {
	it('D3000 → 3000', () => {
		expect(startNumberFromParsed(okParsed('D3000', 'D', 3000))).toBe(3000);
	});

	it('16 進のデバイスは 10 進の index（W1FF → 511）', () => {
		expect(startNumberFromParsed(okParsed('W1FF', 'W', 511))).toBe(511);
	});

	it('ワード内ビットはワードの番号（D100.5 → 100）', () => {
		expect(startNumberFromParsed(okParsed('D100.5', 'D', 100, 1, 5))).toBe(100);
	});

	it('Modbus の 40001 は正規形 400001 の数字を 10 進で読む', () => {
		expect(startNumberFromParsed(okParsed('400001', 'holding_register', 0))).toBe(400001);
	});

	it('area で始まるのに index が無ければ null', () => {
		expect(startNumberFromParsed(okParsed('D3000', 'D', null))).toBeNull();
	});

	it('数字で終わらない canonical は null / 解析失敗も null', () => {
		expect(startNumberFromParsed(okParsed('ABC', null, null))).toBeNull();
		expect(startNumberFromParsed({ ok: false, message: 'x' })).toBeNull();
		expect(startNumberFromParsed(undefined)).toBeNull();
	});
});

describe('nextNamePatternOnParsed / nextStartNumberOnParsed（touched 追跡）', () => {
	const d3000 = okParsed('D3000', 'D', 3000);

	it('touched が false なら導出した値を返す（開始番号は文字列）', () => {
		expect(nextNamePatternOnParsed('D3000', d3000, false)).toBe('D{n}');
		expect(nextStartNumberOnParsed('D3000', d3000, false)).toBe('3000');
	});

	it('touched が true なら null（呼び出し側は欄に触れない）', () => {
		expect(nextNamePatternOnParsed('D3000', d3000, true)).toBeNull();
		expect(nextStartNumberOnParsed('D3000', d3000, true)).toBeNull();
	});

	it('アドレスが空・解析できない間は空文字列（欄を空に戻す）', () => {
		expect(nextNamePatternOnParsed('', undefined, false)).toBe('');
		expect(nextStartNumberOnParsed('', undefined, false)).toBe('');
		expect(nextNamePatternOnParsed('Q?', { ok: false, message: 'x' }, false)).toBe('');
		expect(nextStartNumberOnParsed('Q?', { ok: false, message: 'x' }, false)).toBe('');
	});
});

describe('computeContinuousTags + 偽のドライバ: unit = points は型のワード数ぶん動く', () => {
	it('f32 の 3 点は D100 / D102 / D104（words ではなく points で呼ぶ）', async () => {
		const api = fakeAddressApi();
		const params = buildContinuousParams(baseForm({ count: 3, dataType: 'f32' }));
		const result = await computeContinuousTags(params!, 1, api);
		expect(result.ok && result.rows.map((r) => r.address)).toEqual(['D100', 'D102', 'D104']);
		expect(api.offsetCalls[0].every((i) => i.unit === 'points')).toBe(true);
	});

	it('f64 の 3 点は 4 ワードずつ（D100 / D104 / D108）', async () => {
		const params = buildContinuousParams(baseForm({ count: 3, dataType: 'f64' }));
		const result = await computeContinuousTags(params!, 1, fakeAddressApi());
		expect(result.ok && result.rows.map((r) => r.address)).toEqual(['D100', 'D104', 'D108']);
	});

	it('ドライバが解釈できない開始アドレスは、フォールバックせずエラーにする', async () => {
		const params = buildContinuousParams(baseForm({ count: 2, startAddress: 'oops' }));
		const result = await computeContinuousTags(params!, 1, fakeAddressApi());
		expect(result.ok).toBe(false);
		if (!result.ok) expect(result.error).toContain('解釈できません');
	});
});
