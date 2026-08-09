/**
 * `tagFormNumeric.ts`（TAG-P0-1 拡大分、docs/banto-hub-desktop-plan.md
 * §16.4「`optNum` の null 取りこぼし」）に対するユニットテスト。
 * `tagCsv.test.ts`/`apiKeysAdmin.test.ts` と同じスタイル（describe/it、
 * 依存ゼロの純関数を直接 import）。
 *
 * 回帰本体は「`null` → `undefined`（0 にならない）」— Svelte 5 の
 * `<input type="number">` クリア時の実代入値であり、旧 `optNum` が
 * `Number(null) === 0` に化けさせていたケース。
 */
import { describe, expect, it } from 'vitest';
import { parseOptionalNumber, toOptionalNumberOrNull } from './tagFormNumeric';

describe('parseOptionalNumber', () => {
	it('null は undefined になる（0 に化けない - 回帰本体）', () => {
		expect(parseOptionalNumber(null)).toBeUndefined();
	});

	it('undefined は undefined になる', () => {
		expect(parseOptionalNumber(undefined)).toBeUndefined();
	});

	it('空文字列は undefined になる', () => {
		expect(parseOptionalNumber('')).toBeUndefined();
	});

	it('空白のみの文字列は undefined になる（Number(" ") === 0 に化けさせない）', () => {
		expect(parseOptionalNumber('   ')).toBeUndefined();
	});

	it('数値文字列は number に変換される', () => {
		expect(parseOptionalNumber('12.5')).toBe(12.5);
	});

	it('number はそのまま通る', () => {
		expect(parseOptionalNumber(12.5)).toBe(12.5);
	});

	it('0 という文字列は 0 のまま保持される（未設定と混同しない）', () => {
		expect(parseOptionalNumber('0')).toBe(0);
	});

	// 方針: 変換できない値は「入力エラー」として個別にはじくのではなく、
	// 未設定と同じ扱い（undefined）にする。入力欄の検証・インラインエラー
	// 表示は呼び出し側（+page.svelte の errors）の責務であり、本関数は
	// 送信ペイロードへ意図しない 0 を紛れ込ませないことだけを保証する
	// （旧 optNum の既存挙動を踏襲）。
	it('数値に変換できない文字列は undefined になる', () => {
		expect(parseOptionalNumber('abc')).toBeUndefined();
	});

	it('NaN は undefined になる', () => {
		expect(parseOptionalNumber(NaN)).toBeUndefined();
	});

	it('Infinity は undefined になる（有限数のみ許可）', () => {
		expect(parseOptionalNumber(Infinity)).toBeUndefined();
	});

	it('真偽値は undefined になる（number/string 以外は非対応）', () => {
		expect(parseOptionalNumber(true)).toBeUndefined();
	});
});

describe('toOptionalNumberOrNull', () => {
	it('null は null になる（update 経路の ?? null 相当）', () => {
		expect(toOptionalNumberOrNull(null)).toBeNull();
	});

	it('空文字列は null になる', () => {
		expect(toOptionalNumberOrNull('')).toBeNull();
	});

	it('数値文字列は number に変換される', () => {
		expect(toOptionalNumberOrNull('12.5')).toBe(12.5);
	});

	it('数値に変換できない文字列は null になる', () => {
		expect(toOptionalNumberOrNull('abc')).toBeNull();
	});
});
