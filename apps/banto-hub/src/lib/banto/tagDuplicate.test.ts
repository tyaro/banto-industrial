/**
 * `tagDuplicate.ts`（T18-3a、docs/banto-hub-desktop-plan.md §9.4 TAG-UX-D
 * 前半）に対するユニットテスト。`tagFormCarry.test.ts`/`tagConflictDiff.test.ts`
 * と同じスタイル（describe/it、依存ゼロの純関数を直接 import）。
 */
import { describe, expect, it } from 'vitest';
import { buildDuplicateName, buildDuplicateFormValues } from './tagDuplicate';

describe('buildDuplicateName', () => {
	it('衝突しなければ `_copy` を付ける', () => {
		expect(buildDuplicateName('temp1', [])).toBe('temp1_copy');
		expect(buildDuplicateName('temp1', ['temp2', 'other'])).toBe('temp1_copy');
	});

	it('`_copy` が既に使われていれば `_copy2` にする', () => {
		expect(buildDuplicateName('temp1', ['temp1', 'temp1_copy'])).toBe('temp1_copy2');
	});

	it('`_copy`/`_copy2` が両方使われていれば `_copy3` を選ぶ', () => {
		expect(buildDuplicateName('temp1', ['temp1', 'temp1_copy', 'temp1_copy2'])).toBe('temp1_copy3');
	});

	it('番号に歯抜けがあれば、その空き（最小の未使用番号）を埋める', () => {
		// _copy2 が無く _copy/_copy3 だけ使われているケース: 昇順に最初に
		// 見つかる空き（_copy2）を選ぶ＝歯抜けの穴を優先して埋める。
		expect(buildDuplicateName('temp1', ['temp1_copy', 'temp1_copy3'])).toBe('temp1_copy2');
	});

	it('無関係な名前とは衝突しない', () => {
		expect(buildDuplicateName('temp1', ['temp1_copyX', 'other_copy'])).toBe('temp1_copy');
	});

	it('空文字列の baseName でも安全（`_copy` を返す）', () => {
		expect(buildDuplicateName('', [])).toBe('_copy');
		expect(buildDuplicateName('', ['_copy'])).toBe('_copy2');
	});
});

describe('buildDuplicateFormValues', () => {
	const source = {
		name: 'temp1',
		collectionGroupId: '3',
		address: 'D100',
		dataType: 'f32',
		stringLength: '',
		rawLo: '0',
		rawHi: '100',
		engLo: '0',
		engHi: '100',
		unit: '℃',
		decimals: '1',
		thresholdH: '90',
		thresholdHh: '95',
		thresholdL: '10',
		thresholdLl: '5',
		enabled: true,
		writable: true,
		tagKind: 'plc',
		expression: '',
		retain: false
	};

	it('address を空文字列にクリアする', () => {
		expect(buildDuplicateFormValues(source, []).address).toBe('');
	});

	it('name 以外の属性をすべて引き継ぐ（型/単位/スケーリング/しきい値/有効/書き込み許可/種別等）', () => {
		const result = buildDuplicateFormValues(source, []);
		expect(result).toEqual({
			...source,
			name: 'temp1_copy',
			address: ''
		});
	});

	it('既存名と衝突する場合は `_copy2` 以降を選ぶ', () => {
		expect(buildDuplicateFormValues(source, ['temp1', 'temp1_copy']).name).toBe('temp1_copy2');
		expect(buildDuplicateFormValues(source, ['temp1', 'temp1_copy', 'temp1_copy2']).name).toBe(
			'temp1_copy3'
		);
	});

	it('id/revision に相当するフィールドを持つ入力を渡しても、それらは戻り値にそのまま素通りする（本関数は name/address 以外を関与しない）', () => {
		// buildDuplicateFormValues 自体は id/revision の有無を判定しない -
		// 呼び出し側（+page.svelte）が formFromTag 等で id/revision を含まない
		// フォーム形へ変換してから渡す契約になっている。ここでは万一 id 相当の
		// フィールドが混ざっていても壊れず、他フィールドと同様に素通しされる
		// ことだけを確認する。
		const withExtra = { ...source, id: 42, revision: 7 };
		const result = buildDuplicateFormValues(withExtra, []);
		expect(result.id).toBe(42);
		expect(result.revision).toBe(7);
	});

	it('空/欠損フィールド（空文字列の unit・stringLength 等）はそのまま引き継ぐ', () => {
		const blankish = { ...source, unit: '', stringLength: '', expression: '' };
		const result = buildDuplicateFormValues(blankish, []);
		expect(result.unit).toBe('');
		expect(result.stringLength).toBe('');
		expect(result.expression).toBe('');
	});

	it('name/address 以外を変更しても元の source オブジェクトは変更しない', () => {
		const result = buildDuplicateFormValues(source, []);
		expect(result).not.toBe(source);
		expect(source.name).toBe('temp1');
		expect(source.address).toBe('D100');
	});
});
