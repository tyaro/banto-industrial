/**
 * banto-hub のログアウトの入口（`hubLogout.ts`、banto v2.0.0 #260）のテスト。
 * 既定の SessionController を本物のまま使う。
 *
 * 守りたいこと:
 * - 通常: admin-template の `logoutAndLeave` と同じ（確定した `none` のときだけ /login、
 *   別のセッションが確定していれば留まる）。
 * - 試運転の合成セッションの間: `logoutAndLeave` では /login に行けない（adopt 中の確定は
 *   常に試運転のセッション）ので、v1 と同じくトークンを捨ててログイン画面を見せる。
 *   試運転の合成セッションは終わらない（generation も動かない）。
 */
import { beforeEach, describe, expect, it, vi } from 'vitest';
import {
	getSessionController,
	initBanto,
	resolveSettled,
	type DataProvider
} from '@banto/admin-core';
import { resetDefaultSessionController } from '../../../node_modules/@banto/admin-core/src/sessionController.svelte';

vi.mock('./setup', () => ({ CSRF_HEADER: { 'X-Banto-Client': 'banto' } }));

import { COMMISSIONING_IDENTITY } from './commissioning';
import { COMMISSIONING_KIND, isCommissioningSession } from './commissioningPolicy';
import { hubLogout } from './hubLogout';
import { isLeavingForLogin } from './logout.svelte';
import { accountAnswer, ALICE, fakeAuthProvider, noneAnswer } from './testing/fakeAuthProvider';

let provider: ReturnType<typeof fakeAuthProvider>;
let logout: ReturnType<typeof vi.fn<() => Promise<void>>>;

beforeEach(() => {
	resetDefaultSessionController();
	provider = fakeAuthProvider(accountAnswer(ALICE));
	logout = vi.fn(async () => {
		// ログアウトでトークンが消えた: 次の確認は none、資格情報の変化を通知する。
		provider.setAnswer(noneAnswer);
		provider.change();
	});
	provider.auth.logout = logout;
	initBanto({ dataProvider: {} as DataProvider, authProvider: provider.auth, resources: [] });
});

describe('hubLogout', () => {
	it('通常: logout → 確定した none → /login', async () => {
		const controller = getSessionController();
		await resolveSettled(controller);
		expect(controller.snapshot.identity?.id).toBe('alice');

		const goToLogin = vi.fn(async () => {});
		expect(await hubLogout(goToLogin)).toBe('left');
		expect(logout).toHaveBeenCalledTimes(1);
		expect(goToLogin).toHaveBeenCalledTimes(1);
		expect(controller.snapshot.status).toBe('none');
	});

	it('通常: ログアウトの間に別のセッションが確定していたら留まる（stayed）', async () => {
		const controller = getSessionController();
		await resolveSettled(controller);
		logout.mockImplementation(async () => {
			// 別タブで bob がログインした（このタブのログアウトは追い越された）。
			provider.setAnswer(accountAnswer({ id: 'bob', name: 'Bob', role: 'viewer' }));
			provider.change();
		});
		const goToLogin = vi.fn(async () => {});
		expect(await hubLogout(goToLogin)).toBe('stayed');
		expect(goToLogin).not.toHaveBeenCalled();
		expect(controller.snapshot.identity?.id).toBe('bob');
	});

	it('試運転の合成セッションの間: トークンを捨ててログイン画面を見せる。試運転は終わらない', async () => {
		const controller = getSessionController();
		expect(controller.adopt(COMMISSIONING_IDENTITY, COMMISSIONING_KIND, controller.ticket())).toBe(
			true
		);
		const generation = controller.snapshot.generation;
		let leavingWhileGoing = false;
		const goToLogin = vi.fn(async () => {
			leavingWhileGoing = isLeavingForLogin();
		});

		expect(await hubLogout(goToLogin)).toBe('left');
		expect(logout).toHaveBeenCalledTimes(1);
		expect(goToLogin).toHaveBeenCalledTimes(1);
		expect(leavingWhileGoing).toBe(true);
		expect(isCommissioningSession(controller.snapshot)).toBe(true);
		expect(controller.snapshot.generation).toBe(generation);
	});

	it('試運転の間はトークンの破棄に失敗しても、ログイン画面へは移れる', async () => {
		const controller = getSessionController();
		controller.adopt(COMMISSIONING_IDENTITY, COMMISSIONING_KIND, controller.ticket());
		logout.mockRejectedValue(new Error('network'));
		const goToLogin = vi.fn(async () => {});
		expect(await hubLogout(goToLogin)).toBe('left');
		expect(goToLogin).toHaveBeenCalledTimes(1);
	});
});
