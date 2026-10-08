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

const gotoMock = vi.hoisted(() => vi.fn());
vi.mock('$app/navigation', () => ({ goto: gotoMock }));
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

import { buildCommands, displayGroupCommands } from './commands';
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

describe('表示グループのコマンド（R1-D）', () => {
	beforeEach(() => {
		state.role = 'viewer';
		state.authDisabled = false;
		state.publicViewer = false;
		gotoMock.mockClear();
	});

	it('グループの数だけ「グループ: ◯◯ を表示」が並び、監視画面の ?group= へ移る', async () => {
		const commands = displayGroupCommands([
			{ id: 3, name: 'ライン1' },
			{ id: 8, name: 'ライン2' }
		]);
		expect(commands.map((c) => [c.id, c.title])).toEqual([
			['monitor.group.3', 'グループ: ライン1 を表示'],
			['monitor.group.8', 'グループ: ライン2 を表示']
		]);
		await commands[1].run();
		expect(gotoMock).toHaveBeenCalledWith('/monitor?group=8');
	});

	it('buildCommands に渡した一覧に追従する（省略なら 0 件）。閲覧公開にも出る', () => {
		expect(buildCommands().some((c) => c.id.startsWith('monitor.group.'))).toBe(false);
		state.publicViewer = true;
		const ids = buildCommands([{ id: 1, name: 'A' }])
			.filter((command) => (command.visible ? command.visible() : true))
			.map((command) => command.id);
		expect(ids).toContain('monitor.group.1');
	});
});
