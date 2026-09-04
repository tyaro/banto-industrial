/**
 * `structRegistration.ts` の `allocateStructFields`/`manualStructRows`/
 * `detectStructAddressCollisions`/`structRowsToTagInputs` に対するユニット
 * テスト（`continuousRegistration.test.ts` と同じ describe/it スタイル）。
 */
import { describe, expect, it } from 'vitest';
import {
	allocateStructFields,
	detectStructAddressCollisions,
	manualStructRows,
	structRowsToTagInputs,
	type ExistingTagForCollision,
	type StructField
} from './structRegistration';

describe('allocateStructFields: ワード累積の自動割付', () => {
	it('i16 3フィールドは +1 ワードずつ連続する', () => {
		const fields: StructField[] = [
			{ name: 'a', dataType: 'i16' },
			{ name: 'b', dataType: 'i16' },
			{ name: 'c', dataType: 'i16' }
		];
		const result = allocateStructFields('D3000', fields);
		expect(result.ok).toBe(true);
		if (result.ok) {
			expect(result.rows.map((r) => r.address)).toEqual(['D3000', 'D3001', 'D3002']);
			expect(result.rows.map((r) => r.words)).toEqual([1, 1, 1]);
		}
	});

	it('i32/u32/f32 は2ワード占有し、後続フィールドは2ワード分ずれる', () => {
		const fields: StructField[] = [
			{ name: 'temp', dataType: 'f32' },
			{ name: 'count', dataType: 'i32' },
			{ name: 'flag', dataType: 'bit' }
		];
		const result = allocateStructFields('D3000', fields);
		expect(result.ok).toBe(true);
		if (result.ok) {
			// temp: D3000-D3001 (2word), count: D3002-D3003 (2word), flag: D3004 (1word)
			expect(result.rows.map((r) => r.address)).toEqual(['D3000', 'D3002', 'D3004']);
			expect(result.rows.map((r) => r.words)).toEqual([2, 2, 1]);
		}
	});

	it('string 型は文字列長ぶんのワードを占有する', () => {
		const fields: StructField[] = [
			{ name: 'name', dataType: 'string', stringLength: 8 },
			{ name: 'next', dataType: 'i16' }
		];
		const result = allocateStructFields('D3000', fields);
		expect(result.ok).toBe(true);
		if (result.ok) {
			expect(result.rows[0]).toMatchObject({ address: 'D3000', words: 8, stringLength: 8 });
			expect(result.rows[1]).toMatchObject({ address: 'D3008', words: 1 });
		}
	});

	it('string 型で文字列長未指定はエラー', () => {
		const fields: StructField[] = [{ name: 'name', dataType: 'string' }];
		const result = allocateStructFields('D3000', fields);
		expect(result.ok).toBe(false);
		if (!result.ok) expect(result.error).toMatch(/文字列長/);
	});

	it('string 型で文字列長が小数（1.5）はエラー', () => {
		const fields: StructField[] = [{ name: 'name', dataType: 'string', stringLength: 1.5 }];
		const result = allocateStructFields('D3000', fields);
		expect(result.ok).toBe(false);
		if (!result.ok) expect(result.error).toMatch(/文字列長/);
	});

	it('bit フィールドは1ワード占有として素直に割り付けられる（重ならない）', () => {
		const fields: StructField[] = [
			{ name: 'flag1', dataType: 'bit' },
			{ name: 'flag2', dataType: 'bit' },
			{ name: 'value', dataType: 'i16' }
		];
		const result = allocateStructFields('M100', fields);
		expect(result.ok).toBe(true);
		if (result.ok) {
			expect(result.rows.map((r) => r.address)).toEqual(['M100', 'M101', 'M102']);
		}
	});

	it('デバイス番号の上限（MAX_DEVICE_NUMBER=0x00FFFFFF）を超えるとエラー', () => {
		const fields: StructField[] = [
			{ name: 'a', dataType: 'i32' },
			{ name: 'b', dataType: 'i16' }
		];
		// D16777215 = MAX_DEVICE_NUMBER。1フィールド目がi32(2word)を占有するため
		// 2フィールド目の開始アドレスが上限を超える。
		const result = allocateStructFields('D16777214', fields);
		expect(result.ok).toBe(false);
		if (!result.ok) expect(result.error).toMatch(/上限/);
	});

	it('ベースアドレスが空ならエラー', () => {
		const result = allocateStructFields('', [{ name: 'a', dataType: 'i16' }]);
		expect(result.ok).toBe(false);
		if (!result.ok) expect(result.error).toMatch(/ベースアドレス/);
	});

	it('ベースアドレスにビットサフィックスがあるとエラー', () => {
		const result = allocateStructFields('D100.5', [{ name: 'a', dataType: 'i16' }]);
		expect(result.ok).toBe(false);
		if (!result.ok) expect(result.error).toMatch(/ビット指定/);
	});

	it('フィールドが0件ならエラー', () => {
		const result = allocateStructFields('D3000', []);
		expect(result.ok).toBe(false);
		if (!result.ok) expect(result.error).toMatch(/1つ以上/);
	});

	it('フィールド名が空ならエラー', () => {
		const result = allocateStructFields('D3000', [{ name: '  ', dataType: 'i16' }]);
		expect(result.ok).toBe(false);
		if (!result.ok) expect(result.error).toMatch(/フィールド名/);
	});
});

describe('manualStructRows: 手動割付', () => {
	it('各フィールドの明示アドレスをそのまま使う', () => {
		const fields: StructField[] = [
			{ name: 'a', dataType: 'i16', address: 'D3000' },
			{ name: 'b', dataType: 'f32', address: 'D3100' },
			{ name: 'c', dataType: 'string', stringLength: 4, address: 'W200' }
		];
		const result = manualStructRows(fields);
		expect(result.ok).toBe(true);
		if (result.ok) {
			expect(result.rows.map((r) => r.address)).toEqual(['D3000', 'D3100', 'W200']);
			expect(result.rows.map((r) => r.words)).toEqual([1, 2, 4]);
		}
	});

	it('アドレス未入力の行があればエラー', () => {
		const result = manualStructRows([{ name: 'a', dataType: 'i16', address: '' }]);
		expect(result.ok).toBe(false);
		if (!result.ok) expect(result.error).toMatch(/アドレス/);
	});

	it('フィールドが0件ならエラー', () => {
		const result = manualStructRows([]);
		expect(result.ok).toBe(false);
	});
});

describe('detectStructAddressCollisions: フィールド間衝突', () => {
	it('自動割付の正常系では衝突なし', () => {
		const alloc = allocateStructFields('D3000', [
			{ name: 'a', dataType: 'f32' },
			{ name: 'b', dataType: 'i16' }
		]);
		expect(alloc.ok).toBe(true);
		if (!alloc.ok) return;
		const collisions = detectStructAddressCollisions(alloc.rows, [], 1);
		expect(collisions).toEqual([]);
	});

	it('手動割付で2フィールドのワード範囲が重なると field-address-overlap を返す', () => {
		const manual = manualStructRows([
			{ name: 'a', dataType: 'f32', address: 'D3000' }, // D3000-D3001
			{ name: 'b', dataType: 'i16', address: 'D3001' } // D3001 と重なる
		]);
		expect(manual.ok).toBe(true);
		if (!manual.ok) return;
		const collisions = detectStructAddressCollisions(manual.rows, [], 1);
		expect(collisions.some((c) => c.kind === 'field-address-overlap' && c.index === 1)).toBe(true);
	});

	it('構造体内でフィールド名が重複すると field-name-duplicate を返す', () => {
		const manual = manualStructRows([
			{ name: 'dup', dataType: 'i16', address: 'D3000' },
			{ name: 'dup', dataType: 'i16', address: 'D3001' }
		]);
		expect(manual.ok).toBe(true);
		if (!manual.ok) return;
		const collisions = detectStructAddressCollisions(manual.rows, [], 1);
		expect(collisions.some((c) => c.kind === 'field-name-duplicate' && c.index === 1)).toBe(true);
	});
});

describe('detectStructAddressCollisions: 既存タグとの衝突', () => {
	const existing: ExistingTagForCollision[] = [
		{ name: 'existingTag', collectionGroupId: 1, address: 'D3001', dataType: 'i16' }
	];

	it('既存タグとアドレス範囲が重なると existing-address-overlap を返す', () => {
		const alloc = allocateStructFields('D3000', [
			{ name: 'a', dataType: 'f32' }, // D3000-D3001, D3001 で既存と重なる
			{ name: 'b', dataType: 'i16' }
		]);
		expect(alloc.ok).toBe(true);
		if (!alloc.ok) return;
		const collisions = detectStructAddressCollisions(alloc.rows, existing, 1);
		expect(collisions.some((c) => c.kind === 'existing-address-overlap' && c.index === 0)).toBe(
			true
		);
	});

	it('既存タグと同名のフィールドは existing-name-duplicate を返す', () => {
		const alloc = allocateStructFields('D4000', [{ name: 'existingTag', dataType: 'i16' }]);
		expect(alloc.ok).toBe(true);
		if (!alloc.ok) return;
		const collisions = detectStructAddressCollisions(alloc.rows, existing, 1);
		expect(collisions.some((c) => c.kind === 'existing-name-duplicate')).toBe(true);
	});

	it('別の収集グループの既存タグとは衝突しない（グループ内一意の設計）', () => {
		const alloc = allocateStructFields('D3000', [{ name: 'a', dataType: 'f32' }]);
		expect(alloc.ok).toBe(true);
		if (!alloc.ok) return;
		// existing は collectionGroupId=1、ここでは別グループ(2)に対して検査する。
		const collisions = detectStructAddressCollisions(alloc.rows, existing, 2);
		expect(collisions).toEqual([]);
	});

	it('アドレス・名前とも衝突がなければ空配列', () => {
		const alloc = allocateStructFields('D5000', [{ name: 'freshTag', dataType: 'i16' }]);
		expect(alloc.ok).toBe(true);
		if (!alloc.ok) return;
		const collisions = detectStructAddressCollisions(alloc.rows, existing, 1);
		expect(collisions).toEqual([]);
	});
});

describe('structRowsToTagInputs', () => {
	it('割付結果を TagInput 配列へ変換する', () => {
		const alloc = allocateStructFields('D3000', [
			{ name: 'a', dataType: 'i16' },
			{ name: 'b', dataType: 'string', stringLength: 4 }
		]);
		expect(alloc.ok).toBe(true);
		if (!alloc.ok) return;
		const inputs = structRowsToTagInputs(alloc.rows, {
			collectionGroupId: 7,
			enabled: true,
			writable: false
		});
		expect(inputs).toEqual([
			{
				name: 'a',
				collectionGroupId: 7,
				address: 'D3000',
				dataType: 'i16',
				stringLength: undefined,
				decimals: 0,
				enabled: true,
				writable: false
			},
			{
				name: 'b',
				collectionGroupId: 7,
				address: 'D3001',
				dataType: 'string',
				stringLength: 4,
				decimals: 0,
				enabled: true,
				writable: false
			}
		]);
	});
});
