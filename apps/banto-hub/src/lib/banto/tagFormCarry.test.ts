/**
 * `tagFormCarry.ts`（T18-2c、docs/banto-hub-desktop-plan.md §9.4 TAG-UX-2）
 * に対するユニットテスト。`tagFormLayout.test.ts` と同じスタイル
 * （describe/it、依存ゼロの純関数を直接 import）。
 */
import { describe, expect, it } from 'vitest';
import { carryFormForNext } from './tagFormCarry';

describe('carryFormForNext', () => {
	it('name/address だけを空文字列にし、他フィールドは引き継ぐ', () => {
		const previous = {
			name: 'temp1',
			collectionGroupId: '3',
			address: 'D100',
			dataType: 'f32',
			unit: '℃',
			decimals: '1',
			enabled: true,
			writable: true,
			tagKind: 'plc',
			retain: false
		};

		expect(carryFormForNext(previous)).toEqual({
			...previous,
			name: '',
			address: ''
		});
	});

	it('親設定（タグ種別・収集グループ）を保持する', () => {
		const previous = { name: 'a', address: 'D1', tagKind: 'internal', collectionGroupId: '9' };
		const next = carryFormForNext(previous);
		expect(next.tagKind).toBe('internal');
		expect(next.collectionGroupId).toBe('9');
	});

	it('name/address が既に空でも安全（二重クリアで変化なし）', () => {
		const previous = { name: '', address: '', unit: 'kg' };
		expect(carryFormForNext(previous)).toEqual({ name: '', address: '', unit: 'kg' });
	});

	it('元のオブジェクトを変更しない（新しいオブジェクトを返す）', () => {
		const previous = { name: 'temp1', address: 'D100', unit: '℃' };
		const next = carryFormForNext(previous);
		expect(next).not.toBe(previous);
		expect(previous.name).toBe('temp1');
		expect(previous.address).toBe('D100');
	});
});
