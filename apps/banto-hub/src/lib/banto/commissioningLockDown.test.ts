/**
 * 試運転モードのロックダウンの後の順序（`commissioningLockDown.ts`、banto
 * v2.0.0 #260、設計 §6.2・S-45・S-62）のテスト。本物の SessionController の上で
 * 走らせる。
 *
 * 守りたいこと:
 * - ロックダウンの**前に**取った ticket で `end` → 確定 → `none` なら /login
 * - `end` から /login への遷移が終わるまで、保護レイアウトの配線①を止める
 *   （`isLeavingForLogin()` が真。`end` で generation が動いた瞬間も真）
 * - ロックダウンの間に別の判定が adopt し直した（ticket が失効）なら、まだ試運転の
 *   間だけ新しい ticket でやり直す。すでに終わっていれば何もしない
 * - ロックダウンが失敗したら何も終わらせない（投げ直す）
 * - admin-template 流のログアウト（`logoutAndLeave`）では試運転を終わらせられない
 *   （adopt 中の確定は常に試運転のセッション）ことの対照
 */
import { describe, expect, it, vi } from 'vitest';
import { createSessionController, type SessionController } from '@banto/admin-core';

vi.mock('./setup', () => ({ CSRF_HEADER: { 'X-Banto-Client': 'banto' } }));

import { COMMISSIONING_IDENTITY } from './commissioning';
import { endCommissioningSession, lockDownAndLeave } from './commissioningLockDown';
import { COMMISSIONING_KIND, isCommissioningSession } from './commissioningPolicy';
import { isLeavingForLogin, logoutAndLeave } from './logout.svelte';
import { accountAnswer, ALICE, fakeAuthProvider, noneAnswer } from './testing/fakeAuthProvider';

function adoptedController(answer = noneAnswer) {
	const provider = fakeAuthProvider(answer);
	const controller = createSessionController(provider.auth);
	expect(controller.adopt(COMMISSIONING_IDENTITY, COMMISSIONING_KIND, controller.ticket())).toBe(
		true
	);
	expect(isCommissioningSession(controller.snapshot)).toBe(true);
	return { provider, controller };
}

/** 世代が動いた瞬間の `isLeavingForLogin()` を記録する。 */
function recordLeavingOnGenerationChange(controller: SessionController): boolean[] {
	const seen: boolean[] = [];
	controller.subscribe((next, prev) => {
		if (next.generation !== prev.generation) seen.push(isLeavingForLogin());
	});
	return seen;
}

describe('lockDownAndLeave', () => {
	it('ロックダウン → end → 確定した none → /login。end から遷移の終わりまで配線①を止める', async () => {
		const { provider, controller } = adoptedController(noneAnswer);
		const seenAtGenerationChange = recordLeavingOnGenerationChange(controller);
		const lockDown = vi.fn(async () => {
			// ロックダウンの要求の間は、まだ遷移の手順に入っていない。
			expect(isLeavingForLogin()).toBe(false);
		});
		let leavingWhileGoing = false;
		const goToLogin = vi.fn(async () => {
			leavingWhileGoing = isLeavingForLogin();
		});

		const outcome = await lockDownAndLeave({ lockDown, goToLogin, controller });

		expect(outcome).toBe('left');
		expect(lockDown).toHaveBeenCalledTimes(1);
		expect(goToLogin).toHaveBeenCalledTimes(1);
		expect(controller.snapshot.status).toBe('none');
		expect(seenAtGenerationChange).toEqual([true]); // end の commit の時点で止めていた
		expect(leavingWhileGoing).toBe(true);
		expect(isLeavingForLogin()).toBe(false); // 終われば配線①は戻る
		expect(provider.resolve).toHaveBeenCalled(); // end の後は provider で確定する
	});

	it('ロックダウンが失敗したら、何も終わらせずに投げ直す', async () => {
		const { controller } = adoptedController();
		const goToLogin = vi.fn(async () => {});
		await expect(
			lockDownAndLeave({
				lockDown: async () => {
					throw new Error('validation');
				},
				goToLogin,
				controller
			})
		).rejects.toThrow('validation');
		expect(isCommissioningSession(controller.snapshot)).toBe(true);
		expect(goToLogin).not.toHaveBeenCalled();
		expect(isLeavingForLogin()).toBe(false);
	});

	it('S-62 系: ロックダウンの間に別の判定が adopt し直した（ticket が失効）→ 新しい ticket で end する', async () => {
		const { controller } = adoptedController();
		const goToLogin = vi.fn(async () => {});
		const outcome = await lockDownAndLeave({
			lockDown: async () => {
				// ガードの再実行（別の navigation）が同じ C を adopt し直した: epoch が進む
				expect(
					controller.adopt(COMMISSIONING_IDENTITY, COMMISSIONING_KIND, controller.ticket())
				).toBe(true);
			},
			goToLogin,
			controller
		});
		expect(outcome).toBe('left');
		expect(controller.snapshot.status).toBe('none');
		expect(goToLogin).toHaveBeenCalledTimes(1);
	});

	it('ロックダウンの間にガードが先に end していたら、終わらせるものは無い（そのまま確定へ）', async () => {
		const { controller } = adoptedController();
		const goToLogin = vi.fn(async () => {});
		const outcome = await lockDownAndLeave({
			lockDown: async () => {
				expect(controller.end('commissioning-locked', controller.ticket())).toBe(true);
			},
			goToLogin,
			controller
		});
		expect(outcome).toBe('left');
		expect(goToLogin).toHaveBeenCalledTimes(1);
	});

	it('保存していたトークンが有効なら、そのアカウントで続ける（stayed、/login へは行かない）', async () => {
		const { controller } = adoptedController(accountAnswer(ALICE));
		const goToLogin = vi.fn(async () => {});
		const outcome = await lockDownAndLeave({ lockDown: async () => {}, goToLogin, controller });
		expect(outcome).toBe('stayed');
		expect(controller.snapshot.identity?.id).toBe('alice');
		expect(goToLogin).not.toHaveBeenCalled();
		expect(isLeavingForLogin()).toBe(false);
	});

	it('確認できなければ unverified（/login へは行かない）', async () => {
		const { controller } = adoptedController(async () => {
			throw new TypeError('Failed to fetch');
		});
		const goToLogin = vi.fn(async () => {});
		const outcome = await lockDownAndLeave({ lockDown: async () => {}, goToLogin, controller });
		expect(outcome).toBe('unverified');
		expect(goToLogin).not.toHaveBeenCalled();
		expect(controller.snapshot.status).toBe('none'); // end は済んでいる（試運転には戻らない）
	});
});

describe('endCommissioningSession', () => {
	it('ticket が current なら end する', () => {
		const { controller } = adoptedController();
		expect(endCommissioningSession(controller, controller.ticket())).toBe(true);
		expect(controller.snapshot.status).toBe('none');
	});

	it('試運転でなければ何もしない（true）', () => {
		const provider = fakeAuthProvider();
		const controller = createSessionController(provider.auth);
		const before = controller.snapshot;
		expect(endCommissioningSession(controller, controller.ticket())).toBe(true);
		expect(controller.snapshot).toBe(before);
	});
});

describe('対照: admin-template 流のログアウトでは試運転を終わらせられない（S-45）', () => {
	it('adopt 中の logoutAndLeave は stayed（/login へ行けない）', async () => {
		const { controller, provider } = adoptedController(noneAnswer);
		const goToLogin = vi.fn(async () => {});
		const outcome = await logoutAndLeave(goToLogin, { controller, provider: provider.auth });
		expect(outcome).toBe('stayed');
		expect(goToLogin).not.toHaveBeenCalled();
		expect(isCommissioningSession(controller.snapshot)).toBe(true);
	});
});
