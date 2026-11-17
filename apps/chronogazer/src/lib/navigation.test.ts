import { describe, expect, it } from 'vitest';
import {
	ADMIN_GROUP_KEY,
	isNavItemActive,
	navBarEntries,
	navItems,
	publicNavItems,
	visibleNavItems,
	type NavItem
} from './navigation';

const paths = (items: NavItem[]) => items.map((item) => item.path);

describe('navItems の並び（改善案の順、#1159）', () => {
	it('監視 / ヒストリカル / イベント / タグ設定 / グループ設定 / 管理 / 設定', () => {
		expect(navItems.map((item) => item.label)).toEqual([
			'監視',
			'ヒストリカル',
			'イベント',
			'タグ設定',
			'グループ設定',
			'ユーザー管理',
			'監査ログ',
			'設定'
		]);
	});
});

describe('visibleNavItems', () => {
	it('admin には全項目を navItems の順で見せる', () => {
		expect(paths(visibleNavItems({ role: 'admin' }))).toEqual(paths(navItems));
	});

	it.each(['editor', 'viewer'] as const)('%s には adminOnly の項目を隠す', (role) => {
		const visible = visibleNavItems({ role });
		expect(visible.some((item) => item.adminOnly)).toBe(false);
		expect(paths(visible)).toEqual([
			'/monitor',
			'/historical',
			'/events',
			'/tags',
			'/groups',
			'/settings/appearance'
		]);
	});

	it('閲覧公開は許可リストだけ（役割によらない）', () => {
		for (const role of ['admin', 'editor', 'viewer'] as const) {
			expect(paths(visibleNavItems({ role, publicViewer: true }))).toEqual(paths(publicNavItems()));
		}
		expect(paths(publicNavItems())).toEqual(['/monitor', '/historical', '/events']);
	});
});

describe('navBarEntries', () => {
	it('admin: 監視 / ヒストリカル / イベント / タグ設定 / グループ設定 / 管理(グループ) / 設定 の並び', () => {
		const entries = navBarEntries(visibleNavItems({ role: 'admin' }));
		expect(
			entries.map((entry) => (entry.kind === 'item' ? entry.item.label : `[${entry.key}]`))
		).toEqual([
			'監視',
			'ヒストリカル',
			'イベント',
			'タグ設定',
			'グループ設定',
			`[${ADMIN_GROUP_KEY}]`,
			'設定'
		]);
		const group = entries.find((entry) => entry.kind === 'group');
		expect(group?.kind === 'group' && group.items.map((item) => item.label)).toEqual([
			'ユーザー管理',
			'監査ログ'
		]);
	});

	it('admin 以外にはグループごと出さない', () => {
		const entries = navBarEntries(visibleNavItems({ role: 'viewer' }));
		expect(entries.every((entry) => entry.kind === 'item')).toBe(true);
		expect(entries).toHaveLength(6);
	});

	it('閲覧公開: 3 項目の単独リンクだけ（グループも設定も無い）', () => {
		const entries = navBarEntries(visibleNavItems({ role: 'viewer', publicViewer: true }));
		expect(entries.every((entry) => entry.kind === 'item')).toBe(true);
		expect(entries.map((entry) => entry.kind === 'item' && entry.item.label)).toEqual([
			'監視',
			'ヒストリカル',
			'イベント'
		]);
	});

	it('group を持つ項目は最初のメンバーの位置に集まる（後のメンバーも元の順で）', () => {
		const items: NavItem[] = [
			{ path: '/monitor', label: 'a', icon: '', group: 'g' },
			{ path: '/tags', label: 'b', icon: '' },
			{ path: '/events', label: 'c', icon: '', group: 'g' }
		];
		const entries = navBarEntries(items);
		expect(entries).toHaveLength(2);
		expect(entries[0]).toMatchObject({ kind: 'group', key: 'g' });
		expect(entries[0].kind === 'group' && paths(entries[0].items)).toEqual(['/monitor', '/events']);
		expect(entries[1]).toMatchObject({ kind: 'item' });
	});

	it('adminOnly の項目は group の指定にかかわらず管理グループに入る', () => {
		const items: NavItem[] = [
			{ path: '/users', label: 'u', icon: '', adminOnly: true, group: 'x' }
		];
		expect(navBarEntries(items)).toMatchObject([{ kind: 'group', key: ADMIN_GROUP_KEY }]);
	});
});

describe('isNavItemActive', () => {
	const settings = navItems.find((item) => item.path === '/settings/appearance') as NavItem;

	it('設定は /settings 配下のどのカテゴリでも現在地', () => {
		expect(isNavItemActive(settings, '/settings/data')).toBe(true);
		expect(isNavItemActive(settings, '/settings')).toBe(true);
		expect(isNavItemActive(settings, '/settingsx')).toBe(false);
	});

	it('通常の項目は path の前方一致', () => {
		const tags = navItems.find((item) => item.path === '/tags') as NavItem;
		expect(isNavItemActive(tags, '/tags')).toBe(true);
		expect(isNavItemActive(tags, '/tags/x')).toBe(true);
		expect(isNavItemActive(tags, '/monitor')).toBe(false);
	});
});
