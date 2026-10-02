/**
 * (app) ルートガード（`routes/(app)/+layout.ts`）のユニットテスト（banto
 * v1.7.0 #204、v2.0.0 #260 で書き直し）。
 *
 * v1 は `resolveProtectedSession` を包んだ `decideProtectedRoute` を、その
 * モックの上でテストしていた。v2 で確定は SessionController の役目になり、
 * ガードは試運転の policy runner（`mode: 'guard'`）を通るので、**本物の**
 * `@banto/admin-core`（HTTP 認証プロバイダーと既定の controller、`initBanto`）
 * と**本物の** `commissioning.ts` の HTTP ヘルパーの上で、ガードの `load()`
 * そのものを呼ぶ。差し替えるのは `fetch`（偽のサーバー、`testing/hubHttp.ts`）・
 * `bantoReady`・テーマ設定の同期だけ。
 *
 * 守りたいこと:
 * - サーバーがセッションを**照合できなかった**とき（照合の `500`・到達不能・
 *   期限切れ）は 503（エラー画面と再試行）にし、/login へ送らない。保存して
 *   いるトークン（通常の `sessionStorage` も Remember me の `localStorage` も）を
 *   消さない。閲覧者への切り替え（`/api/auth/status`）もしない。
 * - 確認は `GET /api/auth/identity` の 1 往復。確定した `none`（`401`・トークン
 *   無し）だけ /login。
 * - 試運転モード: 状態の取得 → `adopt`（provider に問い合わせない、generation は
 *   据え置き）。ロックダウン済み・取得の失敗は迂回しない（v1 と同じ安全側）。
 * - 返す generation は、この load が確認したもの。
 * - Remember me で別タブがユーザーを切り替えたら、このタブも確定し直して
 *   未処理のユーザーの変更を記録する（v2 移行の完了条件 8、#257）。試運転中は
 *   対象外（S-46）。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { isHttpError, isRedirect } from '@sveltejs/kit';
import { getSessionController } from '@banto/admin-core';

vi.mock('./setup', () => ({ CSRF_HEADER: { 'X-Banto-Client': 'banto' } }));
vi.mock('$lib/banto/setup', () => ({ bantoReady: Promise.resolve() }));
// この最小 vitest 構成には `$lib` の別名が無いので、本物のモジュールへ向ける（差し替えではない）。
vi.mock('$lib/banto/sessionGuard', () => import('./sessionGuard'));
vi.mock('$lib/banto/commissioningPolicy', () => import('./commissioningPolicy'));
vi.mock('$lib/settings.svelte', () => ({ settings: { syncFromProvider: async () => {} } }));

import { load } from '../../routes/(app)/+layout';
import { SESSION_CHECK_FAILED_MESSAGE } from './sessionGuard';
import { COMMISSIONING_KIND } from './commissioningPolicy';
import {
	identityByToken,
	identityOf,
	installHub,
	jsonResponse,
	TOKEN_KEY
} from './testing/hubHttp';

let hub: ReturnType<typeof installHub>;

beforeEach(() => {
	hub = installHub();
});

afterEach(() => {
	vi.useRealTimers();
	vi.unstubAllGlobals();
});

/** ガードを走らせ、投げたもの（redirect / error）か load の戻り値を返す。 */
async function runGuard(): Promise<unknown> {
	try {
		return await load();
	} catch (thrown) {
		return thrown;
	}
}

function expect503(thrown: unknown): void {
	expect(isHttpError(thrown)).toBe(true);
	if (isHttpError(thrown)) {
		expect(thrown.status).toBe(503);
		expect(thrown.body.message).toBe(SESSION_CHECK_FAILED_MESSAGE);
	}
}

function expectLogin(thrown: unknown): void {
	expect(isRedirect(thrown)).toBe(true);
	if (isRedirect(thrown)) expect(thrown.location).toBe('/login');
}

const unverifiedCases: [string, () => Promise<Response>][] = [
	[
		'照合の 500（DB が答えない）',
		async () => jsonResponse(500, { kind: 'storage', message: 'database is locked' })
	],
	[
		'到達不能（fetch が失敗）',
		async () => {
			throw new TypeError('Failed to fetch');
		}
	]
];

describe('ガード: 照合できないときはトークンを残してエラー画面（503）', () => {
	for (const [label, identity] of unverifiedCases) {
		it(`${label}: 通常のセッション`, async () => {
			hub.session.setItem(TOKEN_KEY, 'normal-token');
			hub.routes.identity = identity;

			expect503(await runGuard());
			expect(hub.session.getItem(TOKEN_KEY)).toBe('normal-token');
			// 確認は identity の 1 往復だけ。閲覧者への切り替え（/api/auth/status）もしない
			expect(hub.paths()).toEqual(['/api/commissioning/status', '/api/auth/identity']);
		});

		it(`${label}: Remember me のセッション`, async () => {
			hub.local.setItem(TOKEN_KEY, 'remembered-token');
			hub.routes.identity = identity;

			expect503(await runGuard());
			expect(hub.local.getItem(TOKEN_KEY)).toBe('remembered-token');
		});
	}

	it('返ってこなければ 10 秒で 503。トークンは残る', async () => {
		vi.useFakeTimers();
		hub.session.setItem(TOKEN_KEY, 'tok');
		hub.routes.identity = (request) =>
			new Promise<Response>((_resolve, reject) => {
				request.signal?.addEventListener('abort', () =>
					reject(new DOMException('The operation was aborted.', 'AbortError'))
				);
			});
		const pending = runGuard();
		await vi.advanceTimersByTimeAsync(10_000);
		expect503(await pending);
		expect(hub.session.getItem(TOKEN_KEY)).toBe('tok');
	});
});

describe('ガード: 確定したときだけ判断する', () => {
	it('401（失効）はログイン画面へ。送ったトークンは消える', async () => {
		hub.local.setItem(TOKEN_KEY, 'revoked-token');
		hub.routes.identity = async () => jsonResponse(401, { kind: 'unauthorized' });

		expectLogin(await runGuard());
		expect(hub.local.getItem(TOKEN_KEY)).toBeNull();
	});

	it('トークンが無ければログイン画面へ（identity を呼ばない）', async () => {
		expectLogin(await runGuard());
		expect(hub.paths()).toEqual(['/api/commissioning/status']);
	});

	it('有効なら進み、この load が確認した generation を返す', async () => {
		hub.session.setItem(TOKEN_KEY, 'live-token');
		hub.routes.identity = identityByToken({ 'live-token': identityOf('alice') });

		const data = (await runGuard()) as { sessionGeneration: number };
		const snapshot = getSessionController().snapshot;
		expect(snapshot.status).toBe('active');
		expect(snapshot.identity?.id).toBe('alice');
		expect(data.sessionGeneration).toBe(snapshot.generation);
		expect(hub.session.getItem(TOKEN_KEY)).toBe('live-token');

		// 同じセッションの再 load は generation を変えない（世代ゲートが画面を作り直さない）。
		const again = (await runGuard()) as { sessionGeneration: number };
		expect(again.sessionGeneration).toBe(data.sessionGeneration);
	});
});

describe('ガード: 試運転モード（policy runner の guard）', () => {
	it('試運転モードなら adopt で確定して進む。identity には問い合わせない', async () => {
		hub.routes.status = async () => jsonResponse(200, { lockedDown: false });

		const data = (await runGuard()) as { sessionGeneration: number };
		const snapshot = getSessionController().snapshot;
		expect(snapshot.kind).toBe(COMMISSIONING_KIND);
		expect(snapshot.identity?.role).toBe('admin');
		expect(data.sessionGeneration).toBe(snapshot.generation);
		expect(hub.paths()).toEqual(['/api/commissioning/status']);

		// 毎回の load で adopt し直しても generation は据え置き（S-44）。
		const again = (await runGuard()) as { sessionGeneration: number };
		expect(again.sessionGeneration).toBe(data.sessionGeneration);
	});

	it('commissioning_ended の後: ロックダウン済みでトークンが無ければ end → /login', async () => {
		hub.routes.status = async () => jsonResponse(200, { lockedDown: false });
		await runGuard();
		const generation = getSessionController().snapshot.generation;

		hub.routes.status = async () => jsonResponse(200, { lockedDown: true });
		expectLogin(await runGuard());
		expect(getSessionController().snapshot.status).toBe('none');
		expect(getSessionController().snapshot.generation).toBeGreaterThan(generation);
	});

	const statusFailures: [string, () => Promise<Response>][] = [
		['500', async () => jsonResponse(500, { kind: 'storage', message: 'x' })],
		[
			'到達不能',
			async () => {
				throw new TypeError('Failed to fetch');
			}
		]
	];
	for (const [label, status] of statusFailures) {
		it(`状態を読めなかった（${label}）ときは安全側（迂回しない）。トークンが無ければ /login`, async () => {
			hub.routes.status = status;
			expectLogin(await runGuard());
			expect(getSessionController().snapshot.kind).not.toBe(COMMISSIONING_KIND);
		});
	}

	it('状態を読めなかったときは、試運転の合成セッションも end してから確認する（v1 と同じ安全側）', async () => {
		hub.routes.status = async () => jsonResponse(200, { lockedDown: false });
		await runGuard();
		hub.routes.status = async () => {
			throw new TypeError('Failed to fetch');
		};
		expectLogin(await runGuard());
		expect(getSessionController().snapshot.status).toBe('none');
	});
});

describe('Remember me: 別タブのユーザーの切り替え（完了条件 8、#257）', () => {
	it('別タブが別のユーザーでログインしたら、保留 → 再 load で新しいユーザーに確定し、変更を記録する', async () => {
		hub.local.setItem(TOKEN_KEY, 'alice-token');
		hub.routes.identity = identityByToken({
			'alice-token': identityOf('alice'),
			'bob-token': identityOf('bob', 'viewer')
		});
		const first = (await runGuard()) as { sessionGeneration: number };
		const controller = getSessionController();
		expect(controller.snapshot.identity?.id).toBe('alice');

		hub.otherTabWrites('bob-token');
		// このタブは保留（unknown）に移り、generation が動く = 配線①が再 load を出す。
		expect(controller.snapshot.status).toBe('unknown');
		expect(controller.snapshot.generation).not.toBe(first.sessionGeneration);

		const second = (await runGuard()) as { sessionGeneration: number };
		expect(controller.snapshot.identity?.id).toBe('bob');
		expect(second.sessionGeneration).toBe(controller.snapshot.generation);
		expect(second.sessionGeneration).not.toBe(first.sessionGeneration);
		// 配線②（ownerChange.ts、OWNER_CHANGE_POLICY = 'rebuild'）が通知する変更。
		expect(controller.snapshot.pendingOwnerChange).toEqual({
			from: 'account:alice',
			to: 'account:bob'
		});
	});

	it('別タブのログアウト（トークンの消去）なら、再 load で none → /login', async () => {
		hub.local.setItem(TOKEN_KEY, 'alice-token');
		hub.routes.identity = identityByToken({ 'alice-token': identityOf('alice') });
		await runGuard();

		hub.otherTabWrites(null);
		expectLogin(await runGuard());
		expect(getSessionController().snapshot.status).toBe('none');
	});

	it('S-46: 試運転中は別タブのログインで保留にしない（generation 据え置き、試運転のまま）', async () => {
		hub.routes.status = async () => jsonResponse(200, { lockedDown: false });
		hub.routes.identity = identityByToken({ 'bob-token': identityOf('bob') });
		const first = (await runGuard()) as { sessionGeneration: number };

		hub.otherTabWrites('bob-token');
		const controller = getSessionController();
		expect(controller.snapshot.kind).toBe(COMMISSIONING_KIND);
		expect(controller.snapshot.generation).toBe(first.sessionGeneration);

		const second = (await runGuard()) as { sessionGeneration: number };
		expect(second.sessionGeneration).toBe(first.sessionGeneration);
		expect(controller.snapshot.kind).toBe(COMMISSIONING_KIND);
		expect(hub.paths()).not.toContain('/api/auth/identity');
	});
});
