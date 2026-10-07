/**
 * #508: 未保存の確認の「強制」判定（ログイン画面への移動は止めない）と、
 * 確認の判定表（`@banto/forms` の `decideLeave`、上流のテストと同じ表）を
 * ChronoGazer の組み合わせで押さえる。ページ側（フォームの dirty 判定）は E2E
 * （`e2e/tests/unsaved-changes.spec.ts`）が見る。
 */
import { describe, expect, it, vi } from 'vitest';
import { decideLeave, runLeaveCheck, type LeaveNavigation } from '@banto/forms';

vi.mock('$app/navigation', () => ({ beforeNavigate: vi.fn() }));
// ChronoGazer は base を使わない（ルート直下）: resolve('login') は '/login'。
vi.mock('$app/paths', () => ({ resolve: (path: string) => `/${path}` }));

import { isForcedNavigation, UNSAVED_CONFIRM_LEAVE } from './unsavedChanges';

function navigation(to: string | null, type = 'link'): LeaveNavigation {
	return {
		type,
		from: { url: new URL('http://localhost/tags') },
		to: to === null ? null : { url: new URL(to, 'http://localhost') },
		cancel: vi.fn()
	};
}

describe('isForcedNavigation', () => {
	it('is true for the login screen (logout, owner change, ended session)', () => {
		expect(isForcedNavigation(navigation('/login'))).toBe(true);
		expect(isForcedNavigation(navigation('/login/'))).toBe(true);
	});

	it('is false for other screens, and when there is no target (reload / tab close)', () => {
		expect(isForcedNavigation(navigation('/monitor'))).toBe(false);
		expect(isForcedNavigation(navigation('/settings/login'))).toBe(false);
		expect(isForcedNavigation(navigation(null, 'leave'))).toBe(false);
	});
});

describe('leave decision with the ChronoGazer wiring', () => {
	const pendingSource = { isPending: () => true, message: () => UNSAVED_CONFIRM_LEAVE };

	it('asks before moving to another screen while something is unsaved', () => {
		const nav = navigation('/monitor');
		const confirm = vi.fn(() => false);
		const result = runLeaveCheck(nav, [pendingSource], { confirm, isForced: isForcedNavigation });
		expect(confirm).toHaveBeenCalledWith(UNSAVED_CONFIRM_LEAVE);
		expect(result).toBe('kept');
		expect(nav.cancel).toHaveBeenCalled();
	});

	it('lets the user leave when they agree', () => {
		const nav = navigation('/monitor');
		const result = runLeaveCheck(nav, [pendingSource], {
			confirm: () => true,
			isForced: isForcedNavigation
		});
		expect(result).toBe('confirmed');
		expect(nav.cancel).not.toHaveBeenCalled();
	});

	it('never asks when the session is leaving for /login', () => {
		const nav = navigation('/login');
		const confirm = vi.fn(() => false);
		const result = runLeaveCheck(nav, [pendingSource], { confirm, isForced: isForcedNavigation });
		expect(confirm).not.toHaveBeenCalled();
		expect(result).toBe('allow');
		expect(nav.cancel).not.toHaveBeenCalled();
	});

	it('never asks when nothing is unsaved', () => {
		const nav = navigation('/monitor');
		const confirm = vi.fn(() => false);
		const clean = { isPending: () => false, message: () => UNSAVED_CONFIRM_LEAVE };
		expect(runLeaveCheck(nav, [clean], { confirm, isForced: isForcedNavigation })).toBe('allow');
		expect(confirm).not.toHaveBeenCalled();
	});

	it('uses the native prompt (block) for a reload / tab close', () => {
		expect(decideLeave({ pending: true, forced: false, samePage: false, type: 'leave' })).toBe(
			'block'
		);
	});
});
