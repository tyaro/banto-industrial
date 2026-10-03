/**
 * 試運転モードのロックダウンの後の順序（`commissioningLockDown.ts`、banto
 * v3.0.0、ADR-0017）のテスト。本物の `@banto/admin-core`（HTTP 認証プロバイダーと
 * 既定の controller）の上で、偽のサーバー（`testing/hubHttp.ts`）に答えさせる。
 *
 * 守りたいこと:
 * - 試運転の grant のトークンで確定している状態から、ロックダウンの後にサーバーが
 *   そのトークンを失効させる（identity が 401）→ 確定した `none` → /login（`'left'`）。
 *   遷移の間は保護レイアウトの配線①を止める（`isLeavingForLogin()` が真）
 * - 保存していたアカウントのトークンが有効なら、そのアカウントで続ける（`'stayed'`、
 *   /login へは行かない）
 * - 確認できなければ `'unverified'`（/login へは行かない）
 * - ロックダウンが失敗したら何も変えずに投げ直す（確認も /login も無い）
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { getSessionController, resolveSettled } from '@banto/admin-core';

vi.mock('./setup', () => ({ CSRF_HEADER: { 'X-Banto-Client': 'banto' } }));

import { isCommissioningSession } from './commissioning';
import { lockDownAndLeave } from './commissioningLockDown';
import { isLeavingForLogin } from './logout.svelte';
import {
	commissioningHub,
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
	vi.unstubAllGlobals();
});

/** 試運転の grant のトークンを保存して確定させる（ガードの `grantFallback` の結果と同じ状態）。 */
async function enterCommissioning(): Promise<void> {
	commissioningHub(hub, 'grant-token');
	hub.session.setItem(TOKEN_KEY, 'grant-token');
	await resolveSettled(getSessionController());
	expect(isCommissioningSession(getSessionController().snapshot)).toBe(true);
}

describe('lockDownAndLeave', () => {
	it('ロックダウン → grant のトークンが失効 → 確定した none → /login。遷移の間は配線①を止める', async () => {
		await enterCommissioning();
		const controller = getSessionController();
		const generation = controller.snapshot.generation;
		const lockDown = vi.fn(async () => {
			// ロックダウンの要求の間は、まだ遷移の手順に入っていない。
			expect(isLeavingForLogin()).toBe(false);
			// サーバーは保存の直後に commissioning の grant を全部失効させる。
			hub.routes.identity = async () => jsonResponse(401, { kind: 'unauthorized' });
		});
		let leavingWhileGoing = false;
		const goToLogin = vi.fn(async () => {
			leavingWhileGoing = isLeavingForLogin();
		});

		const outcome = await lockDownAndLeave({ lockDown, goToLogin });

		expect(outcome).toBe('left');
		expect(lockDown).toHaveBeenCalledTimes(1);
		expect(goToLogin).toHaveBeenCalledTimes(1);
		expect(leavingWhileGoing).toBe(true);
		expect(isLeavingForLogin()).toBe(false); // 終われば配線①は戻る
		expect(controller.snapshot.status).toBe('none');
		expect(controller.snapshot.generation).toBeGreaterThan(generation);
		expect(hub.session.getItem(TOKEN_KEY)).toBeNull(); // 401 でトークンが消えた
	});

	it('保存していたアカウントのトークンが有効なら、そのアカウントで続ける（stayed、/login へは行かない）', async () => {
		hub.session.setItem(TOKEN_KEY, 'alice-token');
		hub.routes.identity = identityByToken({
			'alice-token': identityOf('alice', 'admin', 'account')
		});
		await resolveSettled(getSessionController());
		const goToLogin = vi.fn(async () => {});

		const outcome = await lockDownAndLeave({ lockDown: async () => {}, goToLogin });

		expect(outcome).toBe('stayed');
		expect(getSessionController().snapshot.identity?.id).toBe('alice');
		expect(getSessionController().snapshot.kind).toBe('account');
		expect(goToLogin).not.toHaveBeenCalled();
		expect(isLeavingForLogin()).toBe(false);
	});

	it('確認できなければ unverified（/login へは行かず、トークンは残る）', async () => {
		await enterCommissioning();
		const goToLogin = vi.fn(async () => {});

		const outcome = await lockDownAndLeave({
			lockDown: async () => {
				hub.routes.identity = async () =>
					jsonResponse(500, { kind: 'storage', message: 'database is locked' });
			},
			goToLogin
		});

		expect(outcome).toBe('unverified');
		expect(goToLogin).not.toHaveBeenCalled();
		expect(hub.session.getItem(TOKEN_KEY)).toBe('grant-token');
		expect(isLeavingForLogin()).toBe(false);
	});

	it('ロックダウンが失敗したら、何も変えずに投げ直す（確認も /login も無い）', async () => {
		await enterCommissioning();
		const goToLogin = vi.fn(async () => {});
		const before = hub.paths().length;

		await expect(
			lockDownAndLeave({
				lockDown: async () => {
					throw new Error('validation');
				},
				goToLogin
			})
		).rejects.toThrow('validation');

		expect(isCommissioningSession(getSessionController().snapshot)).toBe(true);
		expect(hub.paths().length).toBe(before);
		expect(goToLogin).not.toHaveBeenCalled();
		expect(isLeavingForLogin()).toBe(false);
	});
});
