import { describe, expect, it } from 'vitest';
import { isLanExposure, validateOpcUaSettings } from './opcuaSettingsForm';
import type { OpcUaSettings } from './opcuaSettingsAdmin';

const defaults: OpcUaSettings = {
	enabled: false,
	bind: '127.0.0.1',
	port: 4840,
	endpointPath: '/',
	namespaceUri: 'urn:banto:opcua:tags',
	advertisedHost: null
};

describe('OPC UA 設定フォーム', () => {
	it('既定値とローカル IPv6 を受け付ける', () => {
		expect(validateOpcUaSettings(defaults)).toBeNull();
		expect(validateOpcUaSettings({ ...defaults, bind: '::1' })).toBeNull();
	});

	it('IP とポートとパスと Namespace URI を検証する', () => {
		for (const overrides of [
			{ bind: 'example.local' },
			{ bind: '999.0.0.1' },
			{ port: 0 },
			{ port: 65536 },
			{ port: 4840.5 },
			{ endpointPath: 'opcua' },
			{ endpointPath: '/tags?x=1' },
			{ namespaceUri: '  ' }
		]) {
			expect(validateOpcUaSettings({ ...defaults, ...overrides })).not.toBeNull();
		}
	});

	it('全インターフェースへの bind には公開ホストが必要', () => {
		for (const bind of ['0.0.0.0', '::', '0:0:0:0:0:0:0:0']) {
			expect(validateOpcUaSettings({ ...defaults, bind })).toContain('公開ホスト');
			expect(
				validateOpcUaSettings({ ...defaults, bind, advertisedHost: 'hub.example.local' })
			).toBeNull();
		}
	});

	it('公開ホストには URL、ポート、パスを受け付けない', () => {
		for (const advertisedHost of ['https://hub.local', 'hub.local:4840', 'hub.local/path']) {
			expect(validateOpcUaSettings({ ...defaults, advertisedHost })).toContain('公開ホスト');
		}
		expect(validateOpcUaSettings({ ...defaults, advertisedHost: '2001:db8::1' })).toBeNull();
		expect(validateOpcUaSettings({ ...defaults, advertisedHost: '[::1]' })).toBeNull();
		expect(validateOpcUaSettings({ ...defaults, advertisedHost: 'hub.example.local.' })).toBeNull();
		expect(validateOpcUaSettings({ ...defaults, advertisedHost: '::' })).not.toBeNull();
	});

	it('予約済み Namespace URI を拒否する', () => {
		for (const namespaceUri of ['http://opcfoundation.org/UA/', 'urn:banto:opcua:server']) {
			expect(validateOpcUaSettings({ ...defaults, namespaceUri })).toContain('Namespace URI');
		}
	});

	it('非ループバック bind または公開ホストなら LAN 警告を出す', () => {
		expect(isLanExposure('127.0.0.1', '')).toBe(false);
		expect(isLanExposure('::1', 'localhost')).toBe(false);
		expect(isLanExposure('0.0.0.0', 'hub.local')).toBe(true);
		expect(isLanExposure('127.0.0.1', 'hub.local')).toBe(true);
	});
});
