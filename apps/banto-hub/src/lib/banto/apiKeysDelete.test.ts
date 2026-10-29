/**
 * `deleteApiKey`（#713、2026-10-29 オーナー決定）のユニットテスト: 失効済みの
 * キーを `DELETE /api/api-keys/{id}` で消し（204 を JSON として読まない）、
 * 有効なキーの 409 は `ProviderError`（サーバーのメッセージ）になる。
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { ProviderError } from '@banto/admin-core';

// `./setup` は読み込んだだけで接続を始めるので、使う定数だけを差し替える。
vi.mock('./setup', () => ({
	CSRF_HEADER: { 'X-Banto-Client': 'banto' }
}));
vi.mock('@banto/admin-core', async (importOriginal) => ({
	...(await importOriginal<typeof import('@banto/admin-core')>()),
	getAuthProvider: () => ({ getToken: () => null })
}));

import { deleteApiKey } from './apiKeysAdmin';

afterEach(() => {
	vi.unstubAllGlobals();
});

describe('deleteApiKey', () => {
	it('DELETE /api/api-keys/{id} を呼び、204 を JSON として読まない', async () => {
		const fetchMock = vi.fn(async () => new Response(null, { status: 204 }));
		vi.stubGlobal('fetch', fetchMock);

		await expect(deleteApiKey(7)).resolves.toBeUndefined();

		expect(fetchMock).toHaveBeenCalledTimes(1);
		const [path, init] = fetchMock.mock.calls[0] as unknown as [string, RequestInit];
		expect(path).toBe('/api/api-keys/7');
		expect(init.method).toBe('DELETE');
	});

	it('有効なキーの 409 はサーバーのメッセージの ProviderError になる', async () => {
		const message = '有効な API キーは削除できません。先に失効させてください';
		vi.stubGlobal(
			'fetch',
			vi.fn(async () => new Response(JSON.stringify({ kind: 'other', message }), { status: 409 }))
		);

		const error = await deleteApiKey(7).catch((e: unknown) => e);
		expect(error).toBeInstanceOf(ProviderError);
		expect((error as ProviderError).message).toBe(message);
	});
});
