/**
 * PR #499 レビュー P3: コマンドパレットも閲覧公開（`publicViewer`）の表示規則に
 * 合わせる。サイドバー（`publicNavItems`）・ヘッダー（「ログイン」）と同じ。
 */
import { beforeEach, describe, expect, it, vi } from 'vitest';

const state = vi.hoisted(() => ({
	role: 'admin' as string | null,
	authDisabled: false,
	publicViewer: false
}));

vi.mock('$app/navigation', () => ({ goto: vi.fn() }));
vi.mock('./banto/logout.svelte', () => ({ logoutAndLeave: vi.fn() }));
vi.mock('./banto/logoutNotice', () => ({ notifyLogoutOutcome: vi.fn() }));
vi.mock('./settings.svelte', () => ({ settings: {} }));
vi.mock('./session.svelte', () => ({
	sessionStore: {
		get role() {
			return state.role;
		},
		get authDisabled() {
			return state.authDisabled;
		},
		get publicViewer() {
			return state.publicViewer;
		}
	}
}));

import { buildCommands } from './commands';
import { publicNavItems } from './navigation';

function visibleIds(): string[] {
	return buildCommands()
		.filter((command) => (command.visible ? command.visible() : true))
		.map((command) => command.id);
}

describe('buildCommands の表示規則', () => {
	beforeEach(() => {
		state.role = 'admin';
		state.authDisabled = false;
		state.publicViewer = false;
	});

	it('管理者: 全ナビゲーションとログアウトが出て、ログインは出ない', () => {
		const ids = visibleIds();
		expect(ids).toContain('nav./settings/appearance');
		expect(ids).toContain('nav./users');
		expect(ids).toContain('session.logout');
		expect(ids).not.toContain('session.login');
	});

	it('閲覧者（アカウント）: adminOnly が隠れる', () => {
		state.role = 'viewer';
		const ids = visibleIds();
		expect(ids).not.toContain('nav./users');
		expect(ids).toContain('session.logout');
	});

	it('ログイン不要モード: ログアウトが出ない', () => {
		state.authDisabled = true;
		expect(visibleIds()).not.toContain('session.logout');
	});

	it('閲覧公開: 許可リストのナビゲーションだけ。設定・ログアウトは出ず、ログインが出る', () => {
		state.role = 'viewer';
		state.publicViewer = true;
		const ids = visibleIds();
		const navIds = ids.filter((id) => id.startsWith('nav.'));
		expect(navIds.sort()).toEqual(
			publicNavItems()
				.map((item) => `nav.${item.path}`)
				.sort()
		);
		expect(ids).not.toContain('nav./settings/appearance');
		expect(ids).not.toContain('nav./tags');
		expect(ids).not.toContain('session.logout');
		expect(ids).toContain('session.login');
	});
});
