/**
 * `tagAddressHelp.ts`（T18-2b、docs/banto-hub-desktop-plan.md §9.4 TAG-UX-6 /
 * TAG-UX-B）に対するユニットテスト。`tagFormLayout.test.ts` と同じスタイル
 * （describe/it、依存ゼロの純関数を直接 import）。
 */
import { describe, expect, it } from 'vitest';
import { addressHelpFor } from './tagAddressHelp';

describe('addressHelpFor: slmp', () => {
	it('SLMP デバイス表記の例を返す（D100 を含む）', () => {
		const help = addressHelpFor('slmp', 'f32');
		expect(help.examples.some((e) => e.address === 'D100')).toBe(true);
		expect(help.examples.length).toBeGreaterThan(0);
	});

	it('プレースホルダは D100 系のビット指定込み', () => {
		expect(addressHelpFor('slmp', 'f32').placeholder).toContain('D100');
	});

	it('bit 型: ビットデバイス1点指定のヒント', () => {
		expect(addressHelpFor('slmp', 'bit').occupancyHint).toContain('ビットデバイス');
	});

	it('i16/u16: ワード1点分のヒント', () => {
		expect(addressHelpFor('slmp', 'i16').occupancyHint).toContain('1点');
		expect(addressHelpFor('slmp', 'u16').occupancyHint).toContain('1点');
	});

	it('i32/u32/f32: ワード2点連続のヒント', () => {
		for (const dt of ['i32', 'u32', 'f32'] as const) {
			expect(addressHelpFor('slmp', dt).occupancyHint).toContain('2点');
		}
	});

	it('string: 文字列長に応じた占有のヒント', () => {
		expect(addressHelpFor('slmp', 'string').occupancyHint).toContain('文字列長');
	});

	it('bit 指定（.N）の書式ヒントを含む', () => {
		expect(addressHelpFor('slmp', 'f32').bitHint).toContain('.');
		expect(addressHelpFor('slmp', 'f32').bitHint).toContain('0〜15');
	});

	it('対応デバイスのヒントに主要なビット/ワードデバイスを含む', () => {
		const hint = addressHelpFor('slmp', 'f32').deviceHint;
		expect(hint).toContain('D');
		expect(hint).toContain('M');
		expect(hint).toContain('X');
	});
});

describe('addressHelpFor: modbus-tcp（TAG-UX-B 受け入れ条件）', () => {
	it('D100 を推奨例として表示しない', () => {
		const help = addressHelpFor('modbus-tcp', 'f32');
		expect(help.examples.some((e) => e.address.includes('D100'))).toBe(false);
		expect(help.placeholder).not.toContain('D100');
	});

	it('Modbus 参照番号の例を返す', () => {
		const help = addressHelpFor('modbus-tcp', 'f32');
		expect(help.examples.some((e) => e.address === '40001')).toBe(true);
		expect(help.examples.some((e) => e.address === '30001')).toBe(true);
		expect(help.examples.some((e) => e.address === '00001')).toBe(true);
	});

	it('bit 型: コイル/ディスクリート入力1点指定のヒント', () => {
		expect(addressHelpFor('modbus-tcp', 'bit').occupancyHint).toContain('コイル');
	});

	it('i16/u16: レジスタ1点分のヒント', () => {
		expect(addressHelpFor('modbus-tcp', 'i16').occupancyHint).toContain('1点');
	});

	it('i32/u32/f32: レジスタ2点連続のヒント', () => {
		for (const dt of ['i32', 'u32', 'f32'] as const) {
			expect(addressHelpFor('modbus-tcp', dt).occupancyHint).toContain('2点');
		}
	});

	it('string: 文字列長に応じた占有のヒント', () => {
		expect(addressHelpFor('modbus-tcp', 'string').occupancyHint).toContain('文字列長');
	});

	it('bit 指定（.N）はレジスタのみ・コイル系は不可という書式ヒントを含む', () => {
		const hint = addressHelpFor('modbus-tcp', 'f32').bitHint;
		expect(hint).toContain('.');
		expect(hint).toContain('コイル');
	});

	it('対応エリア（0xxxx/1xxxx/3xxxx/4xxxx）のヒントを含む', () => {
		const hint = addressHelpFor('modbus-tcp', 'f32').deviceHint;
		expect(hint).toContain('0xxxx');
		expect(hint).toContain('4xxxx');
	});
});

describe('addressHelpFor: virtual（calc/mem）', () => {
	it('例は空、PLC アドレスを持たない旨のヒントを返す', () => {
		const help = addressHelpFor('virtual', 'f32');
		expect(help.examples).toEqual([]);
		expect(help.deviceHint).toContain('内部');
		expect(help.deviceHint).toContain('演算');
	});

	it('bit 指定ヒントは「該当なし」', () => {
		expect(addressHelpFor('virtual', 'f32').bitHint).toContain('該当なし');
	});
});

describe('addressHelpFor: protocol 未選択（グループ未選択）', () => {
	it('例は空で、接続選択を促すヒントを返す', () => {
		const help = addressHelpFor(undefined, 'f32');
		expect(help.examples).toEqual([]);
		expect(help.deviceHint).toContain('選択');
	});

	it('D100 を断定的な推奨例として examples には含めない', () => {
		expect(addressHelpFor(undefined, 'f32').examples).toEqual([]);
	});
});
