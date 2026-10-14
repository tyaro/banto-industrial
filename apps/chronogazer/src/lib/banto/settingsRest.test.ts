/**
 * 設定の REST の経路（#392 B2-2）: `serverAdmin.ts`・`authAdmin.ts` が `getBantoMode()`
 * で Tauri のコマンドと REST（`/api/settings/*`）を選び分けること、REST の要求の形
 * （CSRF のヘッダー・Bearer・本文）、失敗の `ProviderError` への正規化、`hostInfo.ts`
 * の「この画面から変えられるか」の表。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { isProviderError } from '@banto/admin-core';

const mocks = vi.hoisted(() => ({
	mode: 'server' as 'tauri' | 'server' | 'demo',
	invoke: vi.fn(),
	token: 'tok' as string | null
}));

vi.mock('./setup', () => ({ getBantoMode: () => mocks.mode }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: mocks.invoke }));
vi.mock('@banto/admin-core', async (importOriginal) => ({
	...(await importOriginal<typeof import('@banto/admin-core')>()),
	getAuthProvider: () => ({ getToken: () => mocks.token })
}));

import { applyServerSettings, getServerStatus } from './serverAdmin';
import {
	applyAuthSettings,
	applyLocalGrant,
	DESKTOP_ONLY_MESSAGE,
	disableAutologin,
	enableAutologin,
	getAuthSettings,
	SERVICE_ONLY_MESSAGE
} from './authAdmin';
import {
	getHostInfo,
	isLoopbackHostname,
	resetHostInfoCache,
	settingsEditableHere
} from './hostInfo';

const json = (status: number, body: unknown) =>
	new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } });

const serverView = {
	enabled: false,
	running: false,
	bind: '127.0.0.1',
	port: 8730,
	viewerPublic: false,
	urls: [],
	qrSvgs: [],
	restartRequired: true
};

const authView = {
	disabled: false,
	disabledRole: 'admin',
	localGrant: 'admin',
	localGrantSupported: true,
	localGrantRestartRequired: false
};

let fetchMock: ReturnType<typeof vi.fn>;

beforeEach(() => {
	mocks.mode = 'server';
	mocks.token = 'tok';
	mocks.invoke.mockReset();
	fetchMock = vi.fn();
	vi.stubGlobal('fetch', fetchMock);
	resetHostInfoCache();
});

afterEach(() => {
	vi.unstubAllGlobals();
});

function lastRequest(): { url: string; init: RequestInit & { headers: Record<string, string> } } {
	const [url, init] = fetchMock.mock.calls.at(-1)!;
	return { url, init };
}

describe('serverAdmin（LAN の公開設定）', () => {
	it('server モードは GET /api/settings/server（CSRF と Bearer）', async () => {
		fetchMock.mockResolvedValueOnce(json(200, serverView));
		await expect(getServerStatus()).resolves.toEqual(serverView);
		const { url, init } = lastRequest();
		expect(url).toBe('/api/settings/server');
		expect(init.method).toBe('GET');
		expect(init.headers['X-Banto-Client']).toBe('banto');
		expect(init.headers.Authorization).toBe('Bearer tok');
		expect(mocks.invoke).not.toHaveBeenCalled();
	});

	it('server モードの適用は PUT で 4 つの欄を送る', async () => {
		fetchMock.mockResolvedValueOnce(json(200, serverView));
		const status = await applyServerSettings(true, '0.0.0.0', 8730, true);
		expect(status.restartRequired).toBe(true);
		const { url, init } = lastRequest();
		expect(url).toBe('/api/settings/server');
		expect(init.method).toBe('PUT');
		expect(init.headers['Content-Type']).toBe('application/json');
		expect(JSON.parse(String(init.body))).toEqual({
			enabled: true,
			bind: '0.0.0.0',
			port: 8730,
			viewerPublic: true
		});
	});

	it('403 の本文（この PC からだけ）は ProviderError の message になる', async () => {
		fetchMock.mockResolvedValueOnce(
			json(403, { kind: 'other', message: 'この PC からの接続（127.0.0.1）でだけ変更できます' })
		);
		const err = await applyServerSettings(false, '127.0.0.1', 8721, false).catch((e) => e);
		expect(isProviderError(err)).toBe(true);
		expect(err.message).toContain('この PC から');
	});

	it('Banto の本文でない失敗は「<status> <text>」、届かなければ接続できません', async () => {
		fetchMock.mockResolvedValueOnce(new Response('<html>', { status: 502, statusText: 'Bad' }));
		await expect(getServerStatus()).rejects.toMatchObject({ message: '502 Bad' });
		fetchMock.mockRejectedValueOnce(new TypeError('fetch failed'));
		await expect(getServerStatus()).rejects.toMatchObject({ message: 'サーバーに接続できません' });
	});

	it('tauri モードはコマンド、demo は reject（fetch しない）', async () => {
		mocks.mode = 'tauri';
		mocks.invoke.mockResolvedValueOnce(serverView);
		await getServerStatus();
		expect(mocks.invoke).toHaveBeenCalledWith('server_status', undefined);
		mocks.invoke.mockRejectedValueOnce({ kind: 'other', message: '使用中のポート' });
		await expect(applyServerSettings(true, '0.0.0.0', 1, false)).rejects.toMatchObject({
			message: '使用中のポート'
		});
		expect(mocks.invoke).toHaveBeenLastCalledWith('server_apply', {
			enabled: true,
			bind: '0.0.0.0',
			port: 1,
			viewerPublic: false
		});
		mocks.mode = 'demo';
		await expect(getServerStatus()).rejects.toSatisfy(isProviderError);
		await expect(applyServerSettings(true, '0.0.0.0', 1, false)).rejects.toSatisfy(isProviderError);
		expect(fetchMock).not.toHaveBeenCalled();
	});
});

describe('authAdmin（認証の設定）', () => {
	it('server モードは GET /api/settings/auth を AuthSettings に写す（自動ログインは無い）', async () => {
		fetchMock.mockResolvedValueOnce(json(200, authView));
		await expect(getAuthSettings()).resolves.toEqual({
			disabled: false,
			disabledRole: 'admin',
			autologinEnabled: false,
			autologinUsername: null,
			localGrant: 'admin',
			localGrantSupported: true,
			localGrantRestartRequired: false
		});
		expect(lastRequest().url).toBe('/api/settings/auth');
		expect(mocks.invoke).not.toHaveBeenCalled();
	});

	it('applyLocalGrant は PUT {localGrant} だけを送る', async () => {
		fetchMock.mockResolvedValueOnce(
			json(200, { ...authView, localGrant: 'viewer', localGrantRestartRequired: true })
		);
		const next = await applyLocalGrant('viewer');
		expect(next.localGrant).toBe('viewer');
		expect(next.localGrantRestartRequired).toBe(true);
		const { url, init } = lastRequest();
		expect(url).toBe('/api/settings/auth');
		expect(init.method).toBe('PUT');
		expect(JSON.parse(String(init.body))).toEqual({ localGrant: 'viewer' });
	});

	it('server モードではログイン不要モード・自動ログインを reject（要求しない）', async () => {
		for (const call of [
			() => applyAuthSettings(true, 'viewer'),
			() => enableAutologin('a', 'b'),
			() => disableAutologin()
		]) {
			await expect(call()).rejects.toMatchObject({ message: DESKTOP_ONLY_MESSAGE });
		}
		expect(fetchMock).not.toHaveBeenCalled();
		expect(mocks.invoke).not.toHaveBeenCalled();
	});

	it('tauri モードはコマンドのまま、applyLocalGrant は reject', async () => {
		mocks.mode = 'tauri';
		mocks.invoke.mockResolvedValue({
			disabled: false,
			disabledRole: 'admin',
			autologinEnabled: false,
			autologinUsername: null
		});
		await getAuthSettings();
		expect(mocks.invoke).toHaveBeenLastCalledWith('auth_config_get', undefined);
		await applyAuthSettings(true, 'viewer');
		expect(mocks.invoke).toHaveBeenLastCalledWith('auth_config_apply', {
			disabled: true,
			disabledRole: 'viewer'
		});
		await expect(applyLocalGrant('admin')).rejects.toMatchObject({
			message: SERVICE_ONLY_MESSAGE
		});
		expect(fetchMock).not.toHaveBeenCalled();
	});
});

describe('hostInfo（この画面から変えられるか）', () => {
	const cases: [string | undefined, string, boolean][] = [
		['server', '127.0.0.1', true],
		['service', '127.0.0.1', true],
		['server', 'localhost', true],
		['server', 'LOCALHOST', true],
		['server', '[::1]', true],
		// LAN の IP で開いた画面は LAN の listener 経由。
		['server', '192.0.2.10', false],
		['server', '127.0.0.2', false],
		['server', 'chronogazer.example', false],
		// デスクトップ・banto-serve は REST の変更を受けない。
		['desktop', '127.0.0.1', false],
		['serve', '127.0.0.1', false],
		// 読めていなければ変えられない側。
		[undefined, '127.0.0.1', false]
	];
	for (const [hostKind, hostname, expected] of cases) {
		it(`${hostKind ?? '（不明）'} @ ${hostname} → ${expected}`, () => {
			expect(settingsEditableHere(hostKind, hostname)).toBe(expected);
		});
	}

	it('isLoopbackHostname', () => {
		expect(isLoopbackHostname('::1')).toBe(true);
		expect(isLoopbackHostname('0.0.0.0')).toBe(false);
	});

	it('GET /api/host は認証なし（Bearer なし）で 1 回だけ読み、失敗は覚えない', async () => {
		const info = { product: 'chronogazer', hostKind: 'server', version: '0.1.0', scope: 'x' };
		fetchMock.mockRejectedValueOnce(new TypeError('fetch failed'));
		await expect(getHostInfo()).rejects.toSatisfy(isProviderError);
		fetchMock.mockResolvedValueOnce(json(200, info));
		await expect(getHostInfo()).resolves.toEqual(info);
		await expect(getHostInfo()).resolves.toEqual(info);
		expect(fetchMock).toHaveBeenCalledTimes(2);
		const { url, init } = lastRequest();
		expect(url).toBe('/api/host');
		expect(init.headers['X-Banto-Client']).toBe('banto');
		expect(init.headers.Authorization).toBeUndefined();
	});
});
