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
	generateContinuousTags,
	MAX_CONTINUOUS_COUNT,
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
