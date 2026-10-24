/**
 * `tagAddressHelp.ts`（T18-2b、R2b-6d-2b、docs/banto-hub-driver-ui-design.md §7）に対する
 * ユニットテスト。`tagFormLayout.test.ts` と同じスタイル（describe/it、依存ゼロの純関数を
 * 直接 import）。ドライバは `configPackageTestDrivers.ts` の偽の記述子に `addressHelp` を足す。
 */
import { describe, expect, it } from 'vitest';
import { addressHelpFor } from './tagAddressHelp';
import { MODBUS_DRIVER, SLMP_DRIVER } from './configPackageTestDrivers';
import type { DriverInfo, TagDataType } from './tagRegistryAdmin';

const SLMP_WITH_HELP: DriverInfo = {
	...SLMP_DRIVER,
	addressHelp: 'MELSEC のデバイス記法です。\n例: D100 / M50 / D100.F'
};
const MODBUS_WITH_HELP: DriverInfo = {
	...MODBUS_DRIVER,
	addressHelp: '5 桁の参照番号です。\n例: 40001'
};

describe('addressHelpFor: ドライバの接続', () => {
	it('文法の説明はドライバの addressHelp をそのまま（改行も保って）返す', () => {
		for (const d of [SLMP_WITH_HELP, MODBUS_WITH_HELP]) {
			const help = addressHelpFor(d.id, 'f32', d);
			expect(help.driverHelp).toBe(d.addressHelp);
		}
	});

	it('ホストの表（例・デバイス・bit の説明・placeholder）は混ぜない', () => {
		const help = addressHelpFor('slmp', 'u16', SLMP_WITH_HELP);
		expect(help.examples).toEqual([]);
		expect(help.deviceHint).toBe('');
		expect(help.bitHint).toBe('');
		expect(help.placeholder).toBe('');
	});

	it('ドライバがあればプロトコルが何でもドライバを優先する', () => {
		expect(addressHelpFor('push', 'u16', SLMP_WITH_HELP).driverHelp).toBe(
			SLMP_WITH_HELP.addressHelp
		);
	});

	it('占有の注意はデータ型だけから作り、ドライバに依らない', () => {
		const expectations: [TagDataType, string][] = [
			['bit', '1 点'],
			['i16', '1 点'],
			['u16', '1 点'],
			['i32', '2 点'],
			['u32', '2 点'],
			['f32', '2 点'],
			['i64', '4 点'],
			['u64', '4 点'],
			['f64', '4 点'],
			['string', '文字列長']
		];
		for (const [dt, word] of expectations) {
			const a = addressHelpFor('slmp', dt, SLMP_WITH_HELP).occupancyHint;
			const b = addressHelpFor('modbus-tcp', dt, MODBUS_WITH_HELP).occupancyHint;
			expect(a).toContain(word);
			expect(a).toBe(b);
		}
	});

	it('bit 型は 1 点（ビットデバイスかワード内ビット）と言う', () => {
		const hint = addressHelpFor('slmp', 'bit', SLMP_WITH_HELP).occupancyHint;
		expect(hint).toContain('ワード内の 1 ビット');
	});
});

describe('addressHelpFor: ドライバの id なのに記述子が無い（一覧が未着・一覧に無い）', () => {
	it('未選択と同じ説明にする', () => {
		for (const protocol of ['slmp', 'modbus-tcp', 'unknown-driver']) {
			expect(addressHelpFor(protocol, 'f32')).toEqual(addressHelpFor(undefined, 'f32'));
		}
	});
});

describe('addressHelpFor: virtual（calc/mem）', () => {
	it('例は空、PLC アドレスを持たない旨のヒントを返す', () => {
		const help = addressHelpFor('virtual', 'f32');
		expect(help.driverHelp).toBe('');
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
		expect(help.driverHelp).toBe('');
		expect(help.examples).toEqual([]);
		expect(help.deviceHint).toContain('選択');
	});
});

describe('addressHelpFor: push（R2a-2e、送り手が決めるキー）', () => {
	it('PLC アドレスではなくキーの説明を出し、PLC の例を混ぜない', () => {
		const help = addressHelpFor('push', 'f32');
		expect(help.placeholder).toContain('line1.temp');
		expect(help.deviceHint).toContain('送り手');
		expect(help.deviceHint).toContain('大文字小文字を区別');
		expect(help.examples.map((e) => e.address)).toEqual(['line1.temp', 'Line1/Temp']);
		expect(help.examples.some((e) => e.address === 'D100' || e.address === '40001')).toBe(false);
		expect(help.occupancyHint).toContain('スケーリング');
	});

	it('string 型は使えないことを出す', () => {
		expect(addressHelpFor('push', 'string').occupancyHint).toContain('string 型は使えません');
	});
});
