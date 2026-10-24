/**
 * `tagDataTypeOptions.ts`（R2b-6d-2b、docs/banto-hub-driver-ui-design.md §7）のユニットテスト。
 * ホストの表の各場合と、偽のドライバ（SLMP 風・Modbus 風）の `supportedKinds` を固定する。
 */
import { describe, expect, it } from 'vitest';
import { dataTypeOptionsFor } from './tagDataTypeOptions';
import { MODBUS_DRIVER, SLMP_DRIVER } from './configPackageTestDrivers';
import type { DriverInfo } from './tagRegistryAdmin';

const ALL = ['bit', 'i16', 'u16', 'i32', 'u32', 'f32', 'i64', 'u64', 'f64', 'string'];
const NO_64 = ['bit', 'i16', 'u16', 'i32', 'u32', 'f32', 'string'];
const values = (opts: { value: string }[]) => opts.map((o) => o.value);

// SLMP 風: string あり・64bit なし。Modbus 風: 64bit あり・string なし。
const SLMP_LIKE: DriverInfo = {
	...SLMP_DRIVER,
	supportedKinds: ['bit', 'i16', 'u16', 'i32', 'u32', 'f32', 'string']
};
const MODBUS_LIKE: DriverInfo = {
	...MODBUS_DRIVER,
	supportedKinds: ['bit', 'i16', 'u16', 'i32', 'u32', 'f32', 'i64', 'u64', 'f64']
};

describe('dataTypeOptionsFor: ドライバの接続', () => {
	it('SLMP 風: string を選べ、64bit は選べない', () => {
		expect(values(dataTypeOptionsFor('slmp', SLMP_LIKE))).toEqual(NO_64);
	});

	it('Modbus 風: 64bit を選べ、string は選べない', () => {
		expect(values(dataTypeOptionsFor('modbus-tcp', MODBUS_LIKE))).toEqual(
			ALL.filter((v) => v !== 'string')
		);
	});

	it('順序は supportedKinds の並びに依らず固定の表の順', () => {
		const shuffled: DriverInfo = { ...SLMP_LIKE, supportedKinds: ['string', 'u16', 'bit'] };
		expect(values(dataTypeOptionsFor('slmp', shuffled))).toEqual(['bit', 'u16', 'string']);
	});

	it('ドライバがあればホストの表（push など）より優先する', () => {
		expect(values(dataTypeOptionsFor('push', SLMP_LIKE))).toEqual(NO_64);
	});
});

describe('dataTypeOptionsFor: ホストの表', () => {
	it('push: string を除く全部（64bit を含む）', () => {
		expect(values(dataTypeOptionsFor('push'))).toEqual(ALL.filter((v) => v !== 'string'));
	});

	it('postgres・virtual・未選択: 64bit を除く', () => {
		for (const protocol of ['postgres', 'virtual', undefined]) {
			expect(values(dataTypeOptionsFor(protocol))).toEqual(NO_64);
		}
	});

	it('ドライバの id だが一覧に無い・未着: 64bit を除く安全側', () => {
		for (const protocol of ['slmp', 'modbus-tcp', 'unknown-driver']) {
			expect(values(dataTypeOptionsFor(protocol))).toEqual(NO_64);
		}
	});
});

describe('dataTypeOptionsFor: ドライバの一覧が未着', () => {
	it('ドライバの id は全部の型を出す（既存タグの i64 などが消えない）', () => {
		for (const protocol of ['slmp', 'modbus-tcp', 'unknown-driver']) {
			expect(values(dataTypeOptionsFor(protocol, undefined, false))).toEqual(ALL);
		}
	});

	it('届いたが載っていないドライバは 64bit 抜き', () => {
		expect(values(dataTypeOptionsFor('unknown-driver', undefined, true))).toEqual(NO_64);
	});

	it('ホストの接続・未選択は未着でも変わらない', () => {
		for (const protocol of ['push', 'postgres', 'virtual', undefined]) {
			expect(values(dataTypeOptionsFor(protocol, undefined, false))).toEqual(
				values(dataTypeOptionsFor(protocol, undefined, true))
			);
		}
	});

	it('記述子があれば未着フラグに関わらず supportedKinds', () => {
		expect(values(dataTypeOptionsFor('slmp', SLMP_LIKE, false))).toEqual(NO_64);
	});
});
