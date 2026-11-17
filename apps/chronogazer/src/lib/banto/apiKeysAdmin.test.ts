import { describe, expect, it } from 'vitest';
import { apiKeyState, isLoopbackHost, plaintextOverLan, type ApiKeySummary } from './apiKeysAdmin';

const base: ApiKeySummary = {
	id: 1,
	name: 'k',
	prefix: 'p',
	scopes: ['read'],
	createdAt: '2026-01-01',
	lastUsedAt: null,
	revokedAt: null,
	expiresAt: null
};

describe('apiKeyState', () => {
	it('無期限で未失効は有効', () => expect(apiKeyState(base, 1000)).toBe('active'));
	it('期限が過ぎたら期限切れ', () =>
		expect(apiKeyState({ ...base, expiresAt: 500 }, 1000)).toBe('expired'));
	it('期限前は有効', () => expect(apiKeyState({ ...base, expiresAt: 2000 }, 1000)).toBe('active'));
	it('失効は期限切れより優先', () =>
		expect(apiKeyState({ ...base, revokedAt: 'x', expiresAt: 500 }, 1000)).toBe('revoked'));
});

describe('isLoopbackHost', () => {
	it.each(['127.0.0.1', 'localhost', '::1', '[::1]', '127.1.2.3'])('%s は loopback', (h) =>
		expect(isLoopbackHost(h)).toBe(true)
	);
	it.each(['0.0.0.0', '192.168.1.5', '::', 'example.com'])('%s は loopback でない', (h) =>
		expect(isLoopbackHost(h)).toBe(false)
	);
});

describe('plaintextOverLan', () => {
	it('HTTPS なら注意しない', () =>
		expect(plaintextOverLan('https:', '192.168.1.5', '0.0.0.0')).toBe(false));
	it('HTTP で loopback だけなら注意しない', () =>
		expect(plaintextOverLan('http:', '127.0.0.1', '127.0.0.1')).toBe(false));
	it('HTTP で 0.0.0.0 待受は注意', () =>
		expect(plaintextOverLan('http:', '127.0.0.1', '0.0.0.0')).toBe(true));
	it('HTTP で LAN のアドレスから開いていれば注意', () =>
		expect(plaintextOverLan('http:', '192.168.1.5', '127.0.0.1')).toBe(true));
	it('待受が不明なら安全側で注意', () =>
		expect(plaintextOverLan('http:', '127.0.0.1', null)).toBe(true));
});
