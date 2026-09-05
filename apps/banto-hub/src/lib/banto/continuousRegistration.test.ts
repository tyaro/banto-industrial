/**
 * `continuousRegistration.ts` の `buildContinuousParams`/`generateContinuousTags`
 * に対するユニットテスト（`tagFormNumeric.test.ts` と同じ describe/it スタイル、
 * 依存ゼロの純関数を直接 import）。
 *
 * 回帰本体（TAG-P0-1、docs/banto-hub-desktop-plan.md §9）: Svelte 5 の
 * `<input type="number" bind:value>` は `continuousForm.count` へ
 * `number | null` を代入するが、旧 `continuousParams()` は
 * `form.count.trim()` を呼んでいたため `TypeError: count.trim is not a
 * function` でクラッシュしていた。`buildContinuousParams` はこれを
 * `parseOptionalNumber` 経由のパースに置き換えて修正している。
 */
import { describe, expect, it } from 'vitest';
import {
	buildContinuousParams,
	defaultNamePatternFromAddress,
	defaultStartNumberFromAddress,
	generateContinuousTags,
	incrementAddress,
	MAX_CONTINUOUS_COUNT,
	nextNamePatternOnAddressChange,
	nextStartNumberOnAddressChange,
	type ContinuousFormState
} from './continuousRegistration';

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
		thresholdH: '',
		thresholdHh: '',
		thresholdL: '',
		thresholdLl: '',
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

	it('スケーリング/しきい値が null でも組み立てられ、送信時は未設定（null）になる', () => {
		const params = buildContinuousParams(
			baseForm({ rawLo: null, rawHi: 100, thresholdH: null, thresholdHh: 90 })
		);
		expect(params?.rawLo).toBeNull();
		expect(params?.rawHi).toBe(100);
		expect(params?.thresholdH).toBeNull();
		expect(params?.thresholdHh).toBe(90);
	});
});

describe('buildContinuousParams -> generateContinuousTags（点数と行数の一致 - 回帰本体）', () => {
	it.each([1, 2, 1000])('count=%i で対応件数(%i件)の行が生成される', (count) => {
		const params = buildContinuousParams(baseForm({ count }));
		expect(params).not.toBeNull();
		const result = generateContinuousTags(params!);
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
		const result = generateContinuousTags(params!);
		expect(result.ok).toBe(false);
		if (!result.ok) {
			expect(result.error).toMatch(/1以上/);
		}
	});

	it('負数はエラーになる', () => {
		const params = buildContinuousParams(baseForm({ count: -1 }));
		expect(params).not.toBeNull();
		const result = generateContinuousTags(params!);
		expect(result.ok).toBe(false);
		if (!result.ok) {
			expect(result.error).toMatch(/1以上/);
		}
	});

	it('小数（1.5）はエラーになる', () => {
		const params = buildContinuousParams(baseForm({ count: 1.5 }));
		expect(params).not.toBeNull();
		const result = generateContinuousTags(params!);
		expect(result.ok).toBe(false);
		if (!result.ok) {
			expect(result.error).toMatch(/整数/);
		}
	});

	it(`上限超え（${MAX_CONTINUOUS_COUNT + 1}）はエラーになる`, () => {
		const params = buildContinuousParams(baseForm({ count: MAX_CONTINUOUS_COUNT + 1 }));
		expect(params).not.toBeNull();
		const result = generateContinuousTags(params!);
		expect(result.ok).toBe(false);
		if (!result.ok) {
			expect(result.error).toMatch(new RegExp(`${MAX_CONTINUOUS_COUNT}以下`));
		}
	});
});

// --- T18-3c（docs/banto-hub-t18-design.md「T18-3c 連続登録の基数/bit
// 連番」）: SLMP の16進デバイス繰り上げとワード内 bit 連番 -------------

describe('incrementAddress: 16進デバイスの桁上がり（受け入れケース）', () => {
	it('X1E → X1F → X20（16進の桁境界を正しく繰り上げる）', () => {
		expect(incrementAddress('X1E', 1, 0)).toBe('X1E');
		expect(incrementAddress('X1E', 1, 1)).toBe('X1F');
		expect(incrementAddress('X1E', 1, 2)).toBe('X20');
	});

	it('W1FF → W200', () => {
		expect(incrementAddress('W1FF', 1, 0)).toBe('W1FF');
		expect(incrementAddress('W1FF', 1, 1)).toBe('W200');
	});
});

describe('incrementAddress: ワード内 bit 連番（受け入れケース）', () => {
	// T20-④（2026-09-04）: bit サフィックスの表記が16進になったため、
	// bit 14/15 は D100.E / D100.F と書く（旧テストの D100.14/D100.15 は
	// 10進表記で、今は2桁として拒否される）。
	it('D100.E → D100.F → D101.0（bit15 の次はワード+1・bit0）', () => {
		expect(incrementAddress('D100.E', 1, 0)).toBe('D100.E');
		expect(incrementAddress('D100.E', 1, 1)).toBe('D100.F');
		expect(incrementAddress('D100.E', 1, 2)).toBe('D101.0');
	});
});

describe('incrementAddress: 既存の10進連番は不変', () => {
	it('D100(i16 相当・step1) → D101', () => {
		expect(incrementAddress('D100', 1, 1)).toBe('D101');
	});

	it('D100(i32 相当・step2) → D102', () => {
		expect(incrementAddress('D100', 2, 1)).toBe('D102');
	});

	it('Modbus 参照番号（デバイスニーモニックなし）は素朴な10進増分のまま', () => {
		expect(incrementAddress('40001', 1, 1)).toBe('40002');
	});
});

describe('incrementAddress: 範囲外・不正な形式はエラー（null）', () => {
	it('デバイス番号の上限を超えるとエラー', () => {
		const atMax = 'D16777215'; // MAX_DEVICE_NUMBER = 0x00FFFFFF
		expect(incrementAddress(atMax, 1, 0)).toBe(atMax);
		expect(incrementAddress(atMax, 1, 1)).toBeNull();
	});

	it('bit 軸の上限（number が MAX_DEVICE_NUMBER を超える）を超えるとエラー', () => {
		const nearMax = 'D16777215.0';
		expect(incrementAddress(nearMax, 1, 15)).toBe('D16777215.F');
		expect(incrementAddress(nearMax, 1, 16)).toBeNull();
	});

	it('16進デバイスに10進のみの数字でも不正な文字が混ざると解釈できずエラー', () => {
		expect(incrementAddress('M1A', 1, 0)).toBeNull();
	});

	it('デバイスにも数字にも解釈できない文字列はエラー', () => {
		expect(incrementAddress('ZZZ', 1, 0)).toBeNull();
	});
});

describe('generateContinuousTags: 16進デバイス・bit 連番がプレビューまで通る', () => {
	it('X1E を開始アドレスに、bit 型3点で X1E/X1F/X20 が生成される', () => {
		const params = buildContinuousParams(
			baseForm({ startAddress: 'X1E', dataType: 'bit', count: 3 })
		);
		expect(params).not.toBeNull();
		const result = generateContinuousTags(params!);
		expect(result.ok).toBe(true);
		if (result.ok) {
			expect(result.rows.map((r) => r.address)).toEqual(['X1E', 'X1F', 'X20']);
		}
	});

	it('D100.E を開始アドレスに、bit 型3点で D100.E/D100.F/D101.0 が生成される', () => {
		const params = buildContinuousParams(
			baseForm({ startAddress: 'D100.E', dataType: 'bit', count: 3 })
		);
		expect(params).not.toBeNull();
		const result = generateContinuousTags(params!);
		expect(result.ok).toBe(true);
		if (result.ok) {
			expect(result.rows.map((r) => r.address)).toEqual(['D100.E', 'D100.F', 'D101.0']);
		}
	});

	it('bit サフィックス付きアドレスは data_type が bit 以外だとエラーになる', () => {
		const params = buildContinuousParams(
			baseForm({ startAddress: 'D100.E', dataType: 'i16', count: 3 })
		);
		expect(params).not.toBeNull();
		const result = generateContinuousTags(params!);
		expect(result.ok).toBe(false);
		if (!result.ok) {
			expect(result.error).toMatch(/bit/);
		}
	});
});

// T19 S1-b（UX-35、docs/banto-hub-t19-design.md §2「連続登録の名前パターン
// — デバイス名を既定とし、開始番号はアドレスから導出（入力不要）」）:
// `defaultNamePatternFromAddress`/`defaultStartNumberFromAddress` と、
// それらを touched 追跡でラップする `nextNamePatternOnAddressChange`/
// `nextStartNumberOnAddressChange` のユニットテスト。

describe('defaultNamePatternFromAddress', () => {
	it('SLMP デバイス記法（ワード）はニーモニックをそのまま使う - D3000 → D{n}', () => {
		expect(defaultNamePatternFromAddress('D3000')).toBe('D{n}');
	});

	it('SLMP デバイス記法（ビット）も同様 - M100 → M{n}', () => {
		expect(defaultNamePatternFromAddress('M100')).toBe('M{n}');
	});

	it('16進デバイスもニーモニックだけを使う - W1FF → W{n}', () => {
		expect(defaultNamePatternFromAddress('W1FF')).toBe('W{n}');
	});

	it('bit サフィックス付きアドレスはワード側のニーモニックのみ使う - D100.5 → D{n}', () => {
		expect(defaultNamePatternFromAddress('D100.5')).toBe('D{n}');
	});

	it('小文字入力も大文字ニーモニックへ正規化する - d3000 → D{n}', () => {
		expect(defaultNamePatternFromAddress('d3000')).toBe('D{n}');
	});

	it('デバイスニーモニックを持つ非SLMP形式は先頭の非数字列を使う - AI40001 → AI{n}', () => {
		expect(defaultNamePatternFromAddress('AI40001')).toBe('AI{n}');
	});

	it('Modbus 参照番号のようにデバイス名を持たないアドレスは既定 tag{n} - 40001', () => {
		expect(defaultNamePatternFromAddress('40001')).toBe('tag{n}');
	});

	it('開始アドレスが空文字列なら空文字列を返す（導出しない合図）', () => {
		expect(defaultNamePatternFromAddress('')).toBe('');
		expect(defaultNamePatternFromAddress('   ')).toBe('');
	});
});

describe('defaultStartNumberFromAddress', () => {
	it('SLMP デバイス記法はデバイス番号を返す - D3000 → 3000（設計の例そのまま）', () => {
		expect(defaultStartNumberFromAddress('D3000')).toBe(3000);
	});

	it('16進デバイスも10進のデバイス番号を返す - W1FF → 511', () => {
		expect(defaultStartNumberFromAddress('W1FF')).toBe(0x1ff);
	});

	it('bit サフィックス付きアドレスは bit 位置ではなくデバイス番号を返す - D100.5 → 100', () => {
		expect(defaultStartNumberFromAddress('D100.5')).toBe(100);
	});

	it('デバイス名を持たない Modbus 参照番号は数字列そのものを返す - 40001', () => {
		expect(defaultStartNumberFromAddress('40001')).toBe(40001);
	});

	it('数字を含まないアドレスは null（導出できない合図）', () => {
		expect(defaultStartNumberFromAddress('AI')).toBeNull();
	});

	it('開始アドレスが空文字列なら null', () => {
		expect(defaultStartNumberFromAddress('')).toBeNull();
	});
});

describe('nextNamePatternOnAddressChange', () => {
	it('touched が false なら新アドレスから導出した値を返す', () => {
		expect(nextNamePatternOnAddressChange('D3000', false)).toBe('D{n}');
	});

	it('touched が true なら null（呼び出し側は名前パターン欄に触れない）', () => {
		expect(nextNamePatternOnAddressChange('D3000', true)).toBeNull();
	});
});

describe('nextStartNumberOnAddressChange', () => {
	it('touched が false なら新アドレスから導出した値を文字列で返す', () => {
		expect(nextStartNumberOnAddressChange('D3000', false)).toBe('3000');
	});

	it('touched が false でも導出できないアドレスなら空文字列を返す', () => {
		expect(nextStartNumberOnAddressChange('', false)).toBe('');
	});

	it('touched が true なら null（呼び出し側は開始番号欄に触れない）', () => {
		expect(nextStartNumberOnAddressChange('D3000', true)).toBeNull();
	});
});
