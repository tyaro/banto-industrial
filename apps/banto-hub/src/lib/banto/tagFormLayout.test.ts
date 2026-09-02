/**
 * `tagFormLayout.ts`（T18-2a、docs/banto-hub-desktop-plan.md §9.4 TAG-UX-B）
 * に対するユニットテスト。`tagFormNumeric.test.ts`/`tagDeleteImpact.test.ts`
 * と同じスタイル（describe/it、依存ゼロの純関数を直接 import）。
 */
import { describe, expect, it } from 'vitest';
import {
	DISPLAY_SCALING_FIELDS,
	DISPLAY_SCALING_VALUE_FIELDS,
	THRESHOLD_FIELDS,
	WRITE_SAFETY_FIELDS,
	hasFieldError,
	hasAnyFieldValue,
	buildConfirmExternalName,
	environmentLabel,
	writePermissionLabel,
	fieldErrorsFromList
} from './tagFormLayout';

describe('hasFieldError', () => {
	it('DISPLAY_SCALING_FIELDS のいずれかにエラーがあれば true', () => {
		expect(hasFieldError({ rawLo: 'invalid' }, DISPLAY_SCALING_FIELDS)).toBe(true);
		expect(hasFieldError({ engHi: 'invalid' }, DISPLAY_SCALING_FIELDS)).toBe(true);
	});

	it('DISPLAY_SCALING_FIELDS に無関係なエラーだけなら false', () => {
		expect(hasFieldError({ name: 'required' }, DISPLAY_SCALING_FIELDS)).toBe(false);
	});

	it('THRESHOLD_FIELDS のいずれかにエラーがあれば true', () => {
		expect(hasFieldError({ thresholdHh: 'invalid' }, THRESHOLD_FIELDS)).toBe(true);
	});

	it('THRESHOLD_FIELDS に無関係なエラーだけなら false', () => {
		expect(hasFieldError({ unit: 'invalid' }, THRESHOLD_FIELDS)).toBe(false);
	});

	it('WRITE_SAFETY_FIELDS（writable）にエラーがあれば true', () => {
		expect(hasFieldError({ writable: 'invalid' }, WRITE_SAFETY_FIELDS)).toBe(true);
	});

	it('WRITE_SAFETY_FIELDS に無関係なエラーだけなら false', () => {
		expect(hasFieldError({ address: 'invalid' }, WRITE_SAFETY_FIELDS)).toBe(false);
	});

	it('errors が空なら常に false', () => {
		expect(hasFieldError({}, DISPLAY_SCALING_FIELDS)).toBe(false);
		expect(hasFieldError({}, THRESHOLD_FIELDS)).toBe(false);
		expect(hasFieldError({}, WRITE_SAFETY_FIELDS)).toBe(false);
	});

	it('値が空文字列（falsy）のエラーキーは無視する', () => {
		expect(hasFieldError({ rawLo: '' }, DISPLAY_SCALING_FIELDS)).toBe(false);
	});
});

describe('hasAnyFieldValue', () => {
	it('DISPLAY_SCALING_VALUE_FIELDS のいずれかに非空文字列があれば true', () => {
		expect(hasAnyFieldValue({ rawLo: '0' }, DISPLAY_SCALING_VALUE_FIELDS)).toBe(true);
		expect(hasAnyFieldValue({ engHi: '100' }, DISPLAY_SCALING_VALUE_FIELDS)).toBe(true);
	});

	it('decimals は DISPLAY_SCALING_VALUE_FIELDS に含まれない（常に既定値 0 を持つため対象外）', () => {
		expect(hasAnyFieldValue({ decimals: '2' }, DISPLAY_SCALING_VALUE_FIELDS)).toBe(false);
	});

	it('THRESHOLD_FIELDS のいずれかに非空文字列があれば true', () => {
		expect(hasAnyFieldValue({ thresholdHh: '80' }, THRESHOLD_FIELDS)).toBe(true);
	});

	it('すべて空文字列なら false', () => {
		expect(
			hasAnyFieldValue({ rawLo: '', rawHi: '', engLo: '', engHi: '' }, DISPLAY_SCALING_VALUE_FIELDS)
		).toBe(false);
	});

	it('前後の空白だけの値は未設定として扱う', () => {
		expect(hasAnyFieldValue({ rawLo: '   ' }, DISPLAY_SCALING_VALUE_FIELDS)).toBe(false);
	});

	it('対象キー自体が values に無くても false のまま（例外にしない）', () => {
		expect(hasAnyFieldValue({}, DISPLAY_SCALING_VALUE_FIELDS)).toBe(false);
	});

	it('数値の "0" は非空文字列として設定済み扱いにする', () => {
		expect(hasAnyFieldValue({ thresholdL: '0' }, THRESHOLD_FIELDS)).toBe(true);
	});
});

describe('buildConfirmExternalName', () => {
	it('全フィールドがあれば {connection}.{group}.{tag} を組み立てる', () => {
		expect(
			buildConfirmExternalName({ connectionName: 'line1', groupName: 'fast', tagName: 'temp1' })
		).toBe('line1.fast.temp1');
	});

	it('connectionName が未指定なら (未選択) で埋める', () => {
		expect(buildConfirmExternalName({ groupName: 'fast', tagName: 'temp1' })).toBe(
			'(未選択).fast.temp1'
		);
	});

	it('connectionName が空白のみなら (未選択) で埋める', () => {
		expect(
			buildConfirmExternalName({ connectionName: '  ', groupName: 'fast', tagName: 'temp1' })
		).toBe('(未選択).fast.temp1');
	});

	it('groupName が未指定なら (未選択) で埋める', () => {
		expect(buildConfirmExternalName({ connectionName: 'line1', tagName: 'temp1' })).toBe(
			'line1.(未選択).temp1'
		);
	});

	it('groupName が空白のみなら (未選択) で埋める', () => {
		expect(
			buildConfirmExternalName({ connectionName: 'line1', groupName: '   ', tagName: 'temp1' })
		).toBe('line1.(未選択).temp1');
	});

	it('tagName が空文字列なら (未入力) で埋める', () => {
		expect(
			buildConfirmExternalName({ connectionName: 'line1', groupName: 'fast', tagName: '' })
		).toBe('line1.fast.(未入力)');
	});

	it('tagName が空白のみなら (未入力) で埋める', () => {
		expect(
			buildConfirmExternalName({ connectionName: 'line1', groupName: 'fast', tagName: '   ' })
		).toBe('line1.fast.(未入力)');
	});

	it('tagName の前後の空白は trim して表示する', () => {
		expect(
			buildConfirmExternalName({ connectionName: 'line1', groupName: 'fast', tagName: '  temp1  ' })
		).toBe('line1.fast.temp1');
	});

	it('何も無ければ全セグメントがプレースホルダになる', () => {
		expect(buildConfirmExternalName({ tagName: '' })).toBe('(未選択).(未選択).(未入力)');
	});
});

describe('environmentLabel', () => {
	it('undefined は "-"', () => {
		expect(environmentLabel(undefined)).toBe('-');
	});

	it('true はシミュレーション（SIM）', () => {
		expect(environmentLabel(true)).toBe('シミュレーション（SIM）');
	});

	it('false は実機', () => {
		expect(environmentLabel(false)).toBe('実機');
	});
});

describe('writePermissionLabel', () => {
	it('computed は writable=true でも常に不許可（演算タグは書き込み不可）', () => {
		expect(writePermissionLabel('computed', true)).toBe('不許可（演算タグは書き込み不可）');
	});

	it('computed は writable=false でも不許可（演算タグは書き込み不可）', () => {
		expect(writePermissionLabel('computed', false)).toBe('不許可（演算タグは書き込み不可）');
	});

	it('plc かつ writable=true は許可', () => {
		expect(writePermissionLabel('plc', true)).toBe('許可');
	});

	it('plc かつ writable=false は不許可', () => {
		expect(writePermissionLabel('plc', false)).toBe('不許可');
	});

	it('internal かつ writable=true は許可', () => {
		expect(writePermissionLabel('internal', true)).toBe('許可');
	});

	it('internal かつ writable=false は不許可', () => {
		expect(writePermissionLabel('internal', false)).toBe('不許可');
	});
});

describe('fieldErrorsFromList', () => {
	it('field/message のペアをフィールド名でマップ化する', () => {
		expect(
			fieldErrorsFromList([
				{ field: 'name', message: '必須です' },
				{ field: 'address', message: '不正なアドレスです' }
			])
		).toEqual({ name: '必須です', address: '不正なアドレスです' });
	});

	it('空配列なら空オブジェクト', () => {
		expect(fieldErrorsFromList([])).toEqual({});
	});

	it('configuration にアドレスを含むメッセージがあり address 未設定なら address にもコピーする', () => {
		expect(
			fieldErrorsFromList([{ field: 'configuration', message: 'アドレスの形式が不正です' }])
		).toEqual({
			configuration: 'アドレスの形式が不正です',
			address: 'アドレスの形式が不正です'
		});
	});

	it('address が既に設定されていれば configuration の内容で上書きしない', () => {
		expect(
			fieldErrorsFromList([
				{ field: 'address', message: '個別のアドレスエラー' },
				{ field: 'configuration', message: 'アドレスの形式が不正です' }
			])
		).toEqual({
			address: '個別のアドレスエラー',
			configuration: 'アドレスの形式が不正です'
		});
	});

	it('configuration にアドレスを含まないメッセージなら address へコピーしない', () => {
		expect(
			fieldErrorsFromList([{ field: 'configuration', message: '式の評価に失敗しました' }])
		).toEqual({ configuration: '式の評価に失敗しました' });
	});
});
