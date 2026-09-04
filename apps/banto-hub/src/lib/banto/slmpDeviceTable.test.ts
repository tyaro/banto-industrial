/**
 * `slmpDeviceTable.ts`（T18-3c、docs/banto-hub-t18-design.md「T18-3c 連続
 * 登録の基数/bit 連番」）に対するユニットテスト。`tagAddressHelp.test.ts`
 * と同じスタイル（describe/it、依存ゼロの純関数を直接 import）。
 *
 * 正本 `crates/banto-plc/src/slmp/address.rs` のテスト（`p()` ヘルパー、
 * `device_table_lists_every_device_exactly_once` 等）と対になる代表ケースを
 * 移植し、「Rust 側と食い違っていないか」をこの vitest だけで確認できる
 * ようにしている。
 */
import { describe, expect, it } from 'vitest';
import {
	formatSlmpAddress,
	MAX_BIT_POSITION,
	MAX_DEVICE_NUMBER,
	parseSlmpAddress,
	SLMP_DEVICE_TABLE
} from './slmpDeviceTable';

describe('SLMP_DEVICE_TABLE', () => {
	it('全28デバイスを重複なく列挙する（Rust DEVICE_TABLE と同数）', () => {
		expect(SLMP_DEVICE_TABLE.length).toBe(28);
		const mnemonics = new Set(SLMP_DEVICE_TABLE.map((d) => d.mnemonic));
		expect(mnemonics.size).toBe(28);
	});

	it('2文字ニーモニックが1文字ニーモニックより先に並ぶ（最長一致の前提）', () => {
		let previousLength = Number.POSITIVE_INFINITY;
		for (const device of SLMP_DEVICE_TABLE) {
			expect(device.mnemonic.length).toBeLessThanOrEqual(previousLength);
			previousLength = device.mnemonic.length;
		}
	});

	it('16進基数の8デバイスは X/Y/B/W/SB/SW/DX/DY のみ', () => {
		const hexMnemonics = SLMP_DEVICE_TABLE.filter((d) => d.radix === 16).map((d) => d.mnemonic);
		expect(new Set(hexMnemonics)).toEqual(new Set(['X', 'Y', 'B', 'W', 'SB', 'SW', 'DX', 'DY']));
	});

	it('タイマ/カウンタの現在値(N)はワード、接点(S)・コイル(C)はビット', () => {
		const byMnemonic = (m: string) => SLMP_DEVICE_TABLE.find((d) => d.mnemonic === m);
		expect(byMnemonic('TN')?.access).toBe('word');
		expect(byMnemonic('SN')?.access).toBe('word');
		expect(byMnemonic('CN')?.access).toBe('word');
		expect(byMnemonic('TS')?.access).toBe('bit');
		expect(byMnemonic('TC')?.access).toBe('bit');
		expect(byMnemonic('CS')?.access).toBe('bit');
		expect(byMnemonic('CC')?.access).toBe('bit');
	});
});

describe('parseSlmpAddress: 往復（parse→format）', () => {
	it('全デバイスのニーモニック+0が自分自身へ往復する', () => {
		for (const device of SLMP_DEVICE_TABLE) {
			const text = `${device.mnemonic}0`;
			const parsed = parseSlmpAddress(text);
			expect(parsed, `${text} should parse`).not.toBeNull();
			expect(parsed?.device).toBe(device);
			expect(parsed?.number).toBe(0);
			expect(formatSlmpAddress(parsed!.mnemonic, parsed!.number)).toBe(text);
		}
	});

	it('代表的な word/hex/bit ケースが両方向で一致する', () => {
		for (const text of ['D100', 'M50', 'X1A', 'W1FF', 'ZR32768', 'SM400', 'DY3F', 'D100.5']) {
			const parsed = parseSlmpAddress(text);
			expect(parsed, `${text} should parse`).not.toBeNull();
			expect(formatSlmpAddress(parsed!.mnemonic, parsed!.number, parsed!.bit)).toBe(text);
		}
	});
});

describe('parseSlmpAddress: 最長一致', () => {
	it('DX10 は D+X10 ではなく DX+10 と解釈する', () => {
		const parsed = parseSlmpAddress('DX10');
		expect(parsed?.mnemonic).toBe('DX');
		expect(parsed?.number).toBe(0x10);
	});

	it('SD100/SW100/SM100/SB100 も2文字ニーモニック側が勝つ', () => {
		expect(parseSlmpAddress('SD100')?.mnemonic).toBe('SD');
		expect(parseSlmpAddress('SW100')?.mnemonic).toBe('SW');
		expect(parseSlmpAddress('SM100')?.mnemonic).toBe('SM');
		expect(parseSlmpAddress('SB100')?.mnemonic).toBe('SB');
	});

	it('S100/D100 は該当する2文字ニーモニックが無いので1文字のまま', () => {
		expect(parseSlmpAddress('S100')).toEqual({
			device: SLMP_DEVICE_TABLE.find((d) => d.mnemonic === 'S'),
			mnemonic: 'S',
			number: 100,
			bit: undefined
		});
		expect(parseSlmpAddress('D100')?.mnemonic).toBe('D');
	});
});

describe('parseSlmpAddress: 基数', () => {
	it('16進デバイスは16進として解釈する', () => {
		expect(parseSlmpAddress('B1F')?.number).toBe(0x1f);
		expect(parseSlmpAddress('X20')?.number).toBe(0x20);
	});

	it('10進デバイスは10進として解釈する（16進に見える文字は拒否）', () => {
		expect(parseSlmpAddress('D100')?.number).toBe(100);
		expect(parseSlmpAddress('D1F')).toBeNull();
		expect(parseSlmpAddress('M1A')).toBeNull();
	});
});

describe('parseSlmpAddress: bit サフィックス（T20-④: 16進1桁）', () => {
	it('ワードデバイスは .0〜.F を受理する', () => {
		expect(parseSlmpAddress('D100.F')?.bit).toBe(15);
		expect(parseSlmpAddress('D100.0')?.bit).toBe(0);
		expect(parseSlmpAddress('D100.A')?.bit).toBe(10);
	});

	it('小文字の16進bit桁も受理する', () => {
		expect(parseSlmpAddress('D100.a')?.bit).toBe(10);
		expect(parseSlmpAddress('D100.f')?.bit).toBe(15);
	});

	it('bit サフィックスは16進基数デバイスでも同じ16進1桁を使う', () => {
		// W はデバイス番号が16進、かつワードデバイス（X はビットデバイスなので
		// bit サフィックス自体を受け付けない - 別の it で確認済み）。
		expect(parseSlmpAddress('W10.A')?.bit).toBe(10);
		expect(parseSlmpAddress('W0.F')?.bit).toBe(15);
	});

	it('2桁の10進表記（旧仕様の.10〜.15）はもう受理しない', () => {
		expect(parseSlmpAddress('D100.10')).toBeNull();
		expect(parseSlmpAddress('D100.15')).toBeNull();
		expect(parseSlmpAddress('D0.99')).toBeNull();
	});

	it('16進として不正な文字は拒否する', () => {
		expect(parseSlmpAddress('D100.G')).toBeNull();
		expect(parseSlmpAddress('D100.Z')).toBeNull();
	});

	it('ビットデバイスへの .N は拒否する（既にビット粒度のため）', () => {
		for (const text of ['M50.0', 'X1A.3', 'Y0.F']) {
			expect(parseSlmpAddress(text)).toBeNull();
		}
	});

	it('不正な bit サフィックスは拒否する', () => {
		for (const text of ['D0.', 'D0..5', 'D0.5.6', 'D0.-1']) {
			expect(parseSlmpAddress(text)).toBeNull();
		}
	});
});

describe('parseSlmpAddress: 境界・その他', () => {
	it('大文字小文字を区別しない', () => {
		expect(parseSlmpAddress('d100')?.mnemonic).toBe('D');
		expect(parseSlmpAddress('x1a')?.number).toBe(0x1a);
	});

	it('前後の空白を無視する', () => {
		expect(parseSlmpAddress('  D100  ')?.number).toBe(100);
	});

	it(`MAX_DEVICE_NUMBER(${MAX_DEVICE_NUMBER}) は受理し、超過は拒否する`, () => {
		expect(parseSlmpAddress(`D${MAX_DEVICE_NUMBER}`)?.number).toBe(MAX_DEVICE_NUMBER);
		expect(parseSlmpAddress(`D${MAX_DEVICE_NUMBER + 1}`)).toBeNull();
	});

	it(`MAX_BIT_POSITION は ${MAX_BIT_POSITION} である（このテストの前提）`, () => {
		expect(MAX_BIT_POSITION).toBe(15);
	});

	it('未知のデバイスニーモニック・空文字は拒否する', () => {
		for (const text of ['T100', 'C100', 'P0', 'K4M0', '', '   ']) {
			expect(parseSlmpAddress(text)).toBeNull();
		}
	});

	it('数字のみ（Modbus 参照番号）はどのデバイスにも一致せず拒否する', () => {
		expect(parseSlmpAddress('40001')).toBeNull();
	});
});

describe('formatSlmpAddress', () => {
	it('未知のニーモニックは例外を投げる', () => {
		expect(() => formatSlmpAddress('Q', 1)).toThrow();
	});

	it('bit は大文字16進1桁で描画する（T20-④）', () => {
		expect(formatSlmpAddress('D', 100, 15)).toBe('D100.F');
		expect(formatSlmpAddress('D', 100, 10)).toBe('D100.A');
	});

	it('小文字入力でも往復すると大文字16進の正規表記になる', () => {
		const parsed = parseSlmpAddress('d100.f');
		expect(formatSlmpAddress(parsed!.mnemonic, parsed!.number, parsed!.bit)).toBe('D100.F');
	});
});
