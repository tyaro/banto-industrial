/**
 * banto-hub のログアウトの入口（`hubLogout.ts`、banto v3.0.0）のテスト。
 * 既定の SessionController を本物のまま使う。
 *
 * 守りたいこと:
 * - 通常: admin-template の `logoutAndLeave` と同じ（確定した `none` のときだけ /login、
 *   別のセッションが確定していれば留まる）。
 * - 試運転の grant のセッション（kind `commissioning`）の間も同じ: `logout()` が
 *   トークンを捨て、確定した `none` → /login。試運転のセッションは残らない
 *   （次の保護画面への遷移でガードの `grantFallback` が無言で再発行する）。
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

import { isCommissioningSession } from './commissioning';
import { hubLogout } from './hubLogout';
import { isLeavingForLogin } from './logout.svelte';
import {
	accountAnswer,
	ALICE,
	fakeAuthProvider,
	grantAnswer,
	noneAnswer
} from './testing/fakeAuthProvider';

let provider: ReturnType<typeof fakeAuthProvider>;
let logout: ReturnType<typeof vi.fn<() => Promise<void>>>;

function setup(initial = accountAnswer(ALICE)): void {
	resetDefaultSessionController();
	provider = fakeAuthProvider(initial);
	logout = vi.fn(async () => {
		// ログアウトでトークンが消えた: 次の確認は none、資格情報の変化を通知する。
		provider.setAnswer(noneAnswer);
		provider.change();
	});
	provider.auth.logout = logout;
	initBanto({ dataProvider: {} as DataProvider, authProvider: provider.auth, resources: [] });
}

beforeEach(() => setup());

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

	it('試運転の grant のセッションの間: logout → none → /login。試運転のセッションは残らない', async () => {
		setup(grantAnswer());
		const controller = getSessionController();
		await resolveSettled(controller);
		expect(isCommissioningSession(controller.snapshot)).toBe(true);
		let leavingWhileGoing = false;
		const goToLogin = vi.fn(async () => {
			leavingWhileGoing = isLeavingForLogin();
		});

		expect(await hubLogout(goToLogin)).toBe('left');
		expect(logout).toHaveBeenCalledTimes(1);
		expect(goToLogin).toHaveBeenCalledTimes(1);
		expect(leavingWhileGoing).toBe(true);
		expect(isCommissioningSession(controller.snapshot)).toBe(false);
		expect(controller.snapshot.status).toBe('none');
	});

	it('試運転の間はトークンの破棄（logout）が失敗しても、確認が none なら /login へ移る', async () => {
		setup(grantAnswer());
		const controller = getSessionController();
		await resolveSettled(controller);
		logout.mockImplementation(async () => {
			provider.setAnswer(noneAnswer);
			provider.change();
			throw new Error('network');
		});
		const goToLogin = vi.fn(async () => {});
		expect(await hubLogout(goToLogin)).toBe('left');
		expect(goToLogin).toHaveBeenCalledTimes(1);
	});
});
