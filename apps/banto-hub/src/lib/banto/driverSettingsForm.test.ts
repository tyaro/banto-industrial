import { describe, expect, it } from 'vitest';
import {
	buildDriverSettings,
	defaultDriverState,
	defaultDriverValues,
	driverStateFromSettings,
	mapSettingsFieldErrors,
	summarizeDriverValues,
	switchDriverState,
	validateDriverValues,
	type DriverFormState,
	type SecretFormValue
} from './driverSettingsForm';
import { FAKE_SECRET_DRIVER, MODBUS_DRIVER, SLMP_DRIVER } from './configPackageTestDrivers';
import type { DriverSettingField } from './tagRegistryAdmin';

const MODBUS = MODBUS_DRIVER.settingsSchema.fields;
const SLMP = SLMP_DRIVER.settingsSchema.fields;
const SECRET = FAKE_SECRET_DRIVER.settingsSchema.fields;

describe('defaultDriverValues', () => {
	it('スキーマの既定で埋める（数値も文字列で持つ）', () => {
		expect(defaultDriverValues(MODBUS)).toEqual({
			host: '',
			port: '502',
			unit_id: '1',
			word_order: 'high_low'
		});
		expect(defaultDriverValues(SLMP)).toEqual({
			host: '',
			port: '5007',
			cpu_type: 'R',
			word_order: 'low_high'
		});
	});
	it('秘密の欄は「変える・空・値なし」', () => {
		expect(defaultDriverValues(SECRET).token).toEqual({ mode: 'change', text: '', state: 'none' });
	});
});

describe('driverStateFromSettings', () => {
	it('保存済みの値を文字列で持ち、無い欄は既定', () => {
		const state = driverStateFromSettings(MODBUS, { host: '10.0.0.1', port: 1502 });
		expect(state.values).toEqual({
			host: '10.0.0.1',
			port: '1502',
			unit_id: '1',
			word_order: 'high_low'
		});
		expect(state.touched).toEqual({});
	});
	it.each([
		['set', 'set'],
		['unreadable', 'unreadable']
	] as const)('$secret=%s は「保つ」・値は持たない', (wire, state) => {
		const result = driverStateFromSettings(SECRET, { host: 'h', token: { $secret: wire } });
		expect(result.values.token).toEqual({ mode: 'keep', text: '', state });
	});
	it('値が載らない秘密の欄は none', () => {
		const result = driverStateFromSettings(SECRET, { host: 'h' });
		expect((result.values.token as SecretFormValue).state).toBe('none');
	});
});

describe('switchDriverState', () => {
	const touched = (
		fields: readonly DriverSettingField[],
		values: Record<string, string>,
		keys: string[]
	) => {
		const state: DriverFormState = defaultDriverState(fields);
		Object.assign(state.values, values);
		for (const k of keys) state.touched[k] = true;
		return { fields, state };
	};
	it('触った欄だけ同じキーで引き継ぎ、他は新しい既定', () => {
		const prev = touched(MODBUS, { host: '10.0.0.9', port: '1502', word_order: 'high_low' }, [
			'host',
			'port'
		]);
		const next = switchDriverState(prev, SLMP);
		expect(next.values).toEqual({
			host: '10.0.0.9',
			port: '1502',
			cpu_type: 'R',
			word_order: 'low_high' // 触っていないので SLMP の既定
		});
		expect(next.touched).toEqual({ host: true, port: true });
	});
	it('触っていない欄は引き継がない', () => {
		const prev = touched(MODBUS, { host: 'x', port: '1' }, []);
		const next = switchDriverState(prev, SLMP);
		expect(next.values.host).toBe('');
		expect(next.values.port).toBe('5007');
	});
	it('触った欄でも新しい選択肢に無い値は引き継がない', () => {
		const prev = touched(SLMP, { word_order: 'low_high' }, ['word_order']);
		const only: DriverSettingField[] = [
			{
				...SLMP[3],
				kind: { type: 'choice', options: [{ value: 'high_low', label: 'h' }] },
				default: 'high_low'
			}
		];
		expect(switchDriverState(prev, only).values.word_order).toBe('high_low');
	});
	it('種類が違う同名の欄は引き継がない', () => {
		const prev = touched(MODBUS, { port: '1502' }, ['port']);
		const asText: DriverSettingField[] = [
			{ ...MODBUS[1], kind: { type: 'text', max_len: null, placeholder: null } }
		];
		expect(switchDriverState(prev, asText).values.port).toBe('502');
	});
	it('秘密の欄は引き継がない', () => {
		const prev = {
			fields: SECRET,
			state: {
				values: { host: 'h', token: { mode: 'change', text: 'pw', state: 'none' }, note: '' },
				touched: { token: true, host: true }
			} as DriverFormState
		};
		const next = switchDriverState(prev, SECRET);
		expect(next.values.token).toEqual({ mode: 'change', text: '', state: 'none' });
		expect(next.values.host).toBe('h');
	});
	it('ホストの接続から替えた（previous=null）ときは全部既定', () => {
		expect(switchDriverState(null, MODBUS)).toEqual(defaultDriverState(MODBUS));
	});
});

describe('validateDriverValues', () => {
	const ok = { host: 'h', port: '502', unit_id: '1', word_order: 'high_low' };
	it('正常', () => {
		expect(validateDriverValues(MODBUS, ok)).toEqual({});
	});
	it.each([
		['host 空', { host: '  ' }, { host: '必須です' }],
		['port 範囲外', { port: '70000' }, { port: '1〜65535 で指定してください' }],
		['port 0', { port: '0' }, { port: '1〜65535 で指定してください' }],
		['port 数字でない', { port: 'abc' }, { port: '整数で指定してください' }],
		['port 小数', { port: '1.5' }, { port: '整数で指定してください' }],
		['unit_id 256', { unit_id: '256' }, { unit_id: '0〜255 で指定してください' }],
		['unit_id 空は既定になるので通る', { unit_id: '' }, {}],
		['port 空でも既定があるので通る', { port: '' }, {}],
		[
			'word_order が選択肢外',
			{ word_order: 'x' },
			{ word_order: 'low_high, high_low のいずれかを指定してください' }
		]
	])('%s', (_name, patch, expected) => {
		expect(validateDriverValues(MODBUS, { ...ok, ...patch })).toEqual(expected);
	});
	it('Text の max_len と、新規の必須の秘密', () => {
		const base = {
			host: 'h',
			token: { mode: 'change', text: 't', state: 'none' } as SecretFormValue,
			note: 'x'.repeat(33)
		};
		expect(validateDriverValues(SECRET, base)).toEqual({ note: '32 文字以下にしてください' });
		expect(
			validateDriverValues(SECRET, {
				...base,
				note: '',
				token: { mode: 'change', text: '', state: 'none' }
			})
		).toEqual({ token: '必須です' });
	});
	it('保存済みの秘密: 保つ=通る、変える+空=断る、消す(必須)=断る', () => {
		const withToken = (token: SecretFormValue) => ({ host: 'h', token, note: '' });
		expect(
			validateDriverValues(SECRET, withToken({ mode: 'keep', text: '', state: 'set' }))
		).toEqual({});
		expect(
			validateDriverValues(SECRET, withToken({ mode: 'change', text: '', state: 'set' }))
		).toEqual({ token: '新しい値を入力してください' });
		expect(
			validateDriverValues(SECRET, withToken({ mode: 'clear', text: '', state: 'unreadable' }))
		).toEqual({ token: '必須です' });
	});
});

describe('buildDriverSettings', () => {
	it('Integer は数値で送り、空欄は送らない（既定になる）', () => {
		expect(
			buildDriverSettings(MODBUS, {
				host: ' h ',
				port: '1502',
				unit_id: '',
				word_order: 'low_high'
			})
		).toEqual({ host: 'h', port: 1502, word_order: 'low_high' });
	});
	it.each([
		['新規: 入力なし → 送らない', { mode: 'change', text: '', state: 'none' }, undefined, false],
		['新規: 入力あり → 文字列', { mode: 'change', text: 'pw', state: 'none' }, 'pw', true],
		['保つ → キーを送らない', { mode: 'keep', text: '', state: 'set' }, undefined, false],
		['消す → null', { mode: 'clear', text: '', state: 'set' }, null, true],
		['変える → 文字列', { mode: 'change', text: 'new', state: 'unreadable' }, 'new', true]
	] as const)('秘密: %s', (_name, token, expected, present) => {
		const settings = buildDriverSettings(SECRET, { host: 'h', token, note: '' });
		expect('token' in settings).toBe(present);
		expect(settings.token).toEqual(expected);
	});
});

describe('mapSettingsFieldErrors', () => {
	const keys = ['host', 'port', 'unit_id', 'word_order'];
	it('settings.<key> だけを欄へ、旧い平らなキーは other へ', () => {
		expect(
			mapSettingsFieldErrors(keys, {
				'settings.host': 'a',
				unitId: 'b',
				'settings.word_order': 'c',
				port: 'd',
				name: 'e',
				protocol: 'f'
			})
		).toEqual({
			fields: { host: 'a', word_order: 'c' },
			other: { unitId: 'b', port: 'd', name: 'e', protocol: 'f' }
		});
	});
	it('postgres の旧い欄名を明示して受ける', () => {
		expect(
			mapSettingsFieldErrors(keys, { 'settings.port': 'a', port: 'b' }, true).fields.port
		).toBe('a / b');
	});
});

describe('summarizeDriverValues', () => {
	it('Choice は表示名、Bool は はい/いいえ、空は（未入力）', () => {
		const rows = summarizeDriverValues(SLMP, {
			host: '10.0.0.1',
			port: '5007',
			cpu_type: 'Q',
			word_order: ''
		});
		expect(rows.map((r) => r.text)).toEqual(['10.0.0.1', '5007', 'Qシリーズ（Q）', '（未入力）']);
	});
	it('Secret は値を出さず状態だけ', () => {
		const row = (token: SecretFormValue) =>
			summarizeDriverValues(SECRET, { host: 'h', token, note: '' })[1].text;
		expect(row({ mode: 'change', text: 'pw', state: 'none' })).toBe('入力あり');
		expect(row({ mode: 'change', text: '', state: 'none' })).toBe('（未入力）');
		expect(row({ mode: 'keep', text: '', state: 'set' })).toBe('設定済み（変えない）');
		expect(row({ mode: 'clear', text: '', state: 'set' })).toBe('消す');
	});
});
