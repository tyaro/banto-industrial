/**
 * `sessionRecheck.ts` のユニットテスト（#441・#445、banto v2.0.0 #260 で書き直し）。
 *
 * v2 で確認そのものは SessionController の役目（`GET /api/auth/identity` の
 * 1 往復・期限・同時の確認の合流・開始時の資格情報との照合・背景の確認）。
 * v1 の独自の照合（`/api/auth/check` の直接 `fetch`・トークンの照合・
 * single-flight・期限）は削除したので、ここでは**本物の** `@banto/admin-core`
 * （HTTP 認証プロバイダーと既定の controller）と**本物の** `commissioning.ts` の
 * 上で、ストリームの出来事がどう写るかを確かめる。差し替えるのは `fetch`
 * （偽のサーバー、`testing/hubHttp.ts`）と `invalidateAll()`（SvelteKit の
 * 実行時が無いと動かないので回数を数える）。
 *
 * 守りたいこと:
 * - `1008` で閉じられた（#441）: `controller.signal('app:stream-closed')` +
 *   `invalidateAll()`。走らせ直したガードは signal の**後に**始めた確認で
 *   判断する（失効なら /login）。
 * - 再接続が続けて失敗した（#445）: 画面を動かさず（`invalidateAll()` を
 *   呼ばない）、`session` / `login` / `unverified` を返す。
 *   - 試運転中は policy runner の `recheck`（状態が読めなければ `unverified`、
 *     ネットワーク断でログイン画面へ送らない）。
 *   - 照合できない・返ってこない（10 秒）は `unverified`。トークンは消さない。
 *     その後も controller が背景で確認を続ける（E2E の「切断後ちょうど 1 回」は
 *     成り立たない）。
 *   - 確認の最中に資格情報が変わったら、古い答えは使わない（新しい資格情報で
 *     確かめ直す）。古い `401` が新しいトークンを消さない（compare-and-set）。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { isRedirect } from '@sveltejs/kit';
import { getSessionController } from '@banto/admin-core';

const nav = vi.hoisted(() => ({
	invalidateAll: vi.fn<() => Promise<void>>(async () => {})
}));

vi.mock('$app/navigation', () => ({ invalidateAll: nav.invalidateAll }));
vi.mock('./setup', () => ({ CSRF_HEADER: { 'X-Banto-Client': 'banto' } }));
vi.mock('$lib/banto/setup', () => ({ bantoReady: Promise.resolve() }));
// この最小 vitest 構成には `$lib` の別名が無いので、本物のモジュールへ向ける（差し替えではない）。
vi.mock('$lib/banto/sessionGuard', () => import('./sessionGuard'));
vi.mock('$lib/banto/commissioningPolicy', () => import('./commissioningPolicy'));
vi.mock('$lib/settings.svelte', () => ({ settings: { syncFromProvider: async () => {} } }));

import { load } from '../../routes/(app)/+layout';
import { COMMISSIONING_KIND } from './commissioningPolicy';
import {
	probeSessionAfterReconnectFailures,
	recheckSessionAfterStreamClose
} from './sessionRecheck';
import {
	identityByToken,
	identityOf,
	installHub,
	jsonResponse,
	TOKEN_KEY,
	type SentRequest
} from './testing/hubHttp';

let hub: ReturnType<typeof installHub>;

beforeEach(() => {
	hub = installHub();
	nav.invalidateAll.mockReset();
	nav.invalidateAll.mockImplementation(async () => {});
});

afterEach(() => {
	vi.useRealTimers();
	vi.unstubAllGlobals();
});

async function runGuard(): Promise<unknown> {
	try {
		return await load();
	} catch (thrown) {
		return thrown;
	}
}

const identityRequests = (): SentRequest[] =>
	hub.sent.filter((r) => r.path.endsWith('/api/auth/identity'));

/** alice でログインして、ガードを 1 回通した状態にする。 */
async function signedInAsAlice(): Promise<void> {
	hub.session.setItem(TOKEN_KEY, 'alice-token');
	hub.routes.identity = identityByToken({ 'alice-token': identityOf('alice') });
	await runGuard();
	expect(getSessionController().snapshot.identity?.id).toBe('alice');
}

/** 試運転の合成セッションを確定した状態にする。 */
async function commissioning(): Promise<void> {
	hub.routes.status = async () => jsonResponse(200, { lockedDown: false });
	await runGuard();
	expect(getSessionController().snapshot.kind).toBe(COMMISSIONING_KIND);
}

/** 中断されたら reject する、応答しない要求（本物の `fetch` と同じ）。 */
function hanging(request: SentRequest): Promise<Response> {
	return new Promise<Response>((_resolve, reject) => {
		request.signal?.addEventListener('abort', () =>
			reject(new DOMException('The operation was aborted.', 'AbortError'))
		);
	});
}

describe('recheckSessionAfterStreamClose（#441: 1008 で閉じられた）', () => {
	it('controller に signal を送り、ルートガードを走らせ直す（invalidateAll）', async () => {
		await signedInAsAlice();
		const controller = getSessionController();
		const signal = vi.spyOn(controller, 'signal');

		await recheckSessionAfterStreamClose();

		expect(signal).toHaveBeenCalledWith('app:stream-closed');
		expect(nav.invalidateAll).toHaveBeenCalledTimes(1);
	});

	it('session_revoked: 走らせ直したガードは signal の後の確認で失効を確定し、/login へ', async () => {
		await signedInAsAlice();
		hub.routes.identity = async () => jsonResponse(401, { kind: 'unauthorized' });

		await recheckSessionAfterStreamClose();
		const thrown = await runGuard();

		expect(isRedirect(thrown)).toBe(true);
		if (isRedirect(thrown)) expect(thrown.location).toBe('/login');
		expect(hub.session.getItem(TOKEN_KEY)).toBeNull();
	});

	it('まだ有効なら、ガードはそのまま進む（同じ generation、画面は作り直さない）', async () => {
		await signedInAsAlice();
		const generation = getSessionController().snapshot.generation;

		await recheckSessionAfterStreamClose();
		const data = (await runGuard()) as { sessionGeneration: number };

		expect(data.sessionGeneration).toBe(generation);
	});

	it('S-45: 試運転中の signal は provider に問い合わせない。commissioning_ended はガードの policy runner が end する', async () => {
		await commissioning();
		await recheckSessionAfterStreamClose();
		expect(identityRequests()).toHaveLength(0);
		expect(getSessionController().snapshot.kind).toBe(COMMISSIONING_KIND);

		hub.routes.status = async () => jsonResponse(200, { lockedDown: true });
		const thrown = await runGuard();
		expect(isRedirect(thrown)).toBe(true);
		expect(getSessionController().snapshot.status).toBe('none');
	});
});

describe('probeSessionAfterReconnectFailures（#445: 再接続が続けて失敗したとき）', () => {
	const cases: [string, () => Promise<Response>, 'session' | 'login' | 'unverified'][] = [
		['200 identity', async () => jsonResponse(200, identityOf('alice')), 'session'],
		['200 null（失効したセッション）', async () => jsonResponse(200, null), 'login'],
		['401', async () => jsonResponse(401, { kind: 'unauthorized' }), 'login'],
		[
			'照合の 500',
			async () => jsonResponse(500, { kind: 'storage', message: 'database is locked' }),
			'unverified'
		],
		[
			'到達不能',
			async () => {
				throw new TypeError('Failed to fetch');
			},
			'unverified'
		]
	];
	for (const [label, identity, expected] of cases) {
		it(`ロックダウン済み・identity ${label} → ${expected}。画面は動かさない`, async () => {
			await signedInAsAlice();
			hub.routes.identity = identity;
			expect(await probeSessionAfterReconnectFailures()).toBe(expected);
			expect(nav.invalidateAll).not.toHaveBeenCalled();
		});
	}

	it('確認は identity の 1 往復（/api/auth/check は使わない）', async () => {
		await signedInAsAlice();
		const before = hub.sent.length;
		expect(await probeSessionAfterReconnectFailures()).toBe('session');
		expect(hub.paths().slice(before)).toEqual(['/api/auth/identity']);
	});

	it('照合できないときはトークンを消さない。controller は背景で確認を続ける', async () => {
		vi.useFakeTimers();
		await signedInAsAlice();
		hub.routes.identity = async () => jsonResponse(500, { kind: 'storage', message: 'x' });
		const before = identityRequests().length;

		expect(await probeSessionAfterReconnectFailures()).toBe('unverified');
		expect(hub.session.getItem(TOKEN_KEY)).toBe('alice-token');
		expect(getSessionController().snapshot.identity?.id).toBe('alice'); // 確定状態は変えない

		// 背景の確認（1 秒から倍々）。サーバーが失効を答えたら none が確定する（配線①が /login へ）。
		hub.routes.identity = async () => jsonResponse(401, { kind: 'unauthorized' });
		await vi.advanceTimersByTimeAsync(1_000);
		expect(identityRequests().length).toBeGreaterThan(before + 1);
		expect(getSessionController().snapshot.status).toBe('none');
	});

	it('トークンが無ければ login', async () => {
		await signedInAsAlice();
		hub.session.removeItem(TOKEN_KEY);
		expect(await probeSessionAfterReconnectFailures()).toBe('login');
	});

	it('同時に呼ばれても、生きている要求は 1 本だけ（signal の後の確認だけが答える。古い要求は中断して待ち手を移す）', async () => {
		await signedInAsAlice();
		const releases: Array<() => void> = [];
		hub.routes.identity = () =>
			new Promise<Response>((resolve) => {
				releases.push(() => resolve(jsonResponse(200, identityOf('alice'))));
			});
		const before = identityRequests().length;

		const pending = [
			probeSessionAfterReconnectFailures(),
			probeSessionAfterReconnectFailures(),
			probeSessionAfterReconnectFailures()
		];
		// `cause: 'signal'` はそれぞれ signal の stamp を進める（I-9、S-58）ので、
		// 先に出た要求は合流できず中断され、待ち手は最後の要求へ移る。
		await vi.waitFor(() => expect(identityRequests().length).toBe(before + 3));
		const sent = identityRequests().slice(before);
		expect(sent.slice(0, 2).every((r) => r.signal?.aborted === true)).toBe(true);
		expect(sent[2].signal?.aborted).toBe(false);
		for (const release of releases) release();
		expect(await Promise.all(pending)).toEqual(['session', 'session', 'session']);
	});

	it('返ってこなければ 10 秒で unverified。止めた要求は中断されている', async () => {
		vi.useFakeTimers();
		await signedInAsAlice();
		hub.routes.identity = async (request) => hanging(request);

		const result = probeSessionAfterReconnectFailures();
		await vi.advanceTimersByTimeAsync(10_000);
		expect(await result).toBe('unverified');
		const first = identityRequests().at(-1);
		expect(first?.signal?.aborted).toBe(true);
		expect(hub.session.getItem(TOKEN_KEY)).toBe('alice-token');
	});

	it('S-31: 時間切れの後に遅れて 401 が返ったら、トークンは消え、controller が背景で none を確定する（待ち続けない）', async () => {
		vi.useFakeTimers();
		await signedInAsAlice();
		let lateAnswer: () => void = () => {};
		let calls = 0;
		hub.routes.identity = () => {
			calls += 1;
			if (calls > 1) return Promise.resolve(jsonResponse(401, { kind: 'unauthorized' }));
			// サーバーが中断を無視して遅れて答える、を再現する。
			return new Promise<Response>((resolve) => {
				lateAnswer = () => resolve(jsonResponse(401, { kind: 'unauthorized' }));
			});
		};

		const result = probeSessionAfterReconnectFailures();
		await vi.advanceTimersByTimeAsync(10_000);
		expect(await result).toBe('unverified');

		lateAnswer();
		await vi.advanceTimersByTimeAsync(30_000);
		expect(hub.session.getItem(TOKEN_KEY)).toBeNull();
		expect(getSessionController().snapshot.status).toBe('none');
	});

	it('確認の最中に別タブでログインし直したら、古い答えは使わず新しい資格情報で確かめる。古い 401 は新しいトークンを消さない', async () => {
		hub.local.setItem(TOKEN_KEY, 'old-token');
		hub.routes.identity = identityByToken({ 'old-token': identityOf('alice') });
		await runGuard();

		let answerOld: () => void = () => {};
		hub.routes.identity = (request) => {
			if (request.headers.Authorization === 'Bearer old-token') {
				return new Promise<Response>((resolve) => {
					answerOld = () => resolve(jsonResponse(401, { kind: 'unauthorized' }));
				});
			}
			return Promise.resolve(jsonResponse(200, identityOf('bob')));
		};

		const result = probeSessionAfterReconnectFailures();
		await vi.waitFor(() =>
			expect(identityRequests().at(-1)?.headers.Authorization).toBe('Bearer old-token')
		);
		hub.otherTabWrites('new-token');
		answerOld();

		expect(await result).toBe('session');
		expect(getSessionController().snapshot.identity?.id).toBe('bob');
		expect(hub.local.getItem(TOKEN_KEY)).toBe('new-token');
	});
});

describe('probeSessionAfterReconnectFailures: 試運転中（policy runner の recheck）', () => {
	it('試運転モードのまま → session（identity は呼ばない、generation 据え置き）', async () => {
		await commissioning();
		const generation = getSessionController().snapshot.generation;
		expect(await probeSessionAfterReconnectFailures()).toBe('session');
		expect(identityRequests()).toHaveLength(0);
		expect(getSessionController().snapshot.generation).toBe(generation);
	});

	it('状態が読めなかった（ネットワーク断など）→ unverified。ガードと違い、試運転を終わらせない', async () => {
		await commissioning();
		hub.routes.status = async () => {
			throw new TypeError('Failed to fetch');
		};
		expect(await probeSessionAfterReconnectFailures()).toBe('unverified');
		expect(getSessionController().snapshot.kind).toBe(COMMISSIONING_KIND);
		expect(nav.invalidateAll).not.toHaveBeenCalled();
	});

	it('試運転のつもりで繋いでいたがロックダウンされていた → end → login', async () => {
		await commissioning();
		hub.routes.status = async () => jsonResponse(200, { lockedDown: true });
		expect(await probeSessionAfterReconnectFailures()).toBe('login');
		expect(getSessionController().snapshot.status).toBe('none');
	});

	it('ロックダウン済みで、保存していたトークンが有効なら session（そのアカウントで続ける）', async () => {
		await commissioning();
		hub.session.setItem(TOKEN_KEY, 'alice-token');
		hub.routes.identity = identityByToken({ 'alice-token': identityOf('alice') });
		hub.routes.status = async () => jsonResponse(200, { lockedDown: true });
		expect(await probeSessionAfterReconnectFailures()).toBe('session');
		expect(getSessionController().snapshot.identity?.id).toBe('alice');
	});
});
