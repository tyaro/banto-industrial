import { describe, expect, it } from 'vitest';
import {
	DEMO_NAV_PATHS,
	isDemoTargetPath,
	ADMIN_GROUP_KEY,
	isNavItemActive,
	navBarEntries,
	navItemHref,
	navItems,
	visibleNavItems,
	type NavItem
} from './navigation';

const paths = (items: NavItem[]) => items.map((item) => item.path);

describe('visibleNavItems', () => {
	it('admin には全項目を navItems の順で見せる', () => {
		expect(paths(visibleNavItems({ role: 'admin' }))).toEqual(paths(navItems));
	});

	it.each(['editor', 'viewer'] as const)('%s には adminOnly の項目を隠す', (role) => {
		const visible = visibleNavItems({ role });
		expect(visible.some((item) => item.adminOnly)).toBe(false);
		expect(paths(visible)).toEqual(['/status', '/tags', '/sink', '/settings/appearance']);
	});
});

describe('navBarEntries', () => {
	it('admin: 状態 / タグ / DB Sink / 管理(グループ) / 設定 の並び', () => {
		const entries = navBarEntries(visibleNavItems({ role: 'admin' }));
		expect(
			entries.map((entry) => (entry.kind === 'item' ? entry.item.label : `[${entry.key}]`))
		).toEqual(['状態', 'タグ', 'DB Sink', `[${ADMIN_GROUP_KEY}]`, '設定']);
		const group = entries.find((entry) => entry.kind === 'group');
		expect(group?.kind === 'group' && group.items.map((item) => item.label)).toEqual([
			'APIキー',
			'ユーザー管理',
			'監査ログ',
			'書き込み監査'
		]);
	});

	it('admin 以外にはグループごと出さない', () => {
		const entries = navBarEntries(visibleNavItems({ role: 'viewer' }));
		expect(entries.every((entry) => entry.kind === 'item')).toBe(true);
		expect(entries).toHaveLength(4);
	});

	it('group を持つ項目は最初のメンバーの位置に集まる（後のメンバーも元の順で）', () => {
		const items: NavItem[] = [
			{ path: '/status', label: 'a', icon: '', group: 'g' },
			{ path: '/tags', label: 'b', icon: '' },
			{ path: '/sink', label: 'c', icon: '', group: 'g' }
		];
		const entries = navBarEntries(items);
		expect(entries).toHaveLength(2);
		expect(entries[0]).toMatchObject({ kind: 'group', key: 'g' });
		expect(entries[0].kind === 'group' && paths(entries[0].items)).toEqual(['/status', '/sink']);
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
		expect(isNavItemActive(tags, '/status')).toBe(false);
	});
});

describe('navItemHref（#1179 PR3b）', () => {
	const group = { type: 'group', id: 4 } as const;

	it('タグへは共有している対象を ?target= で付ける', () => {
		expect(navItemHref('/tags', group)).toBe('/tags?target=group:4');
		expect(navItemHref('/tags', { type: 'connection', id: 2 })).toBe('/tags?target=connection:2');
	});

	it('「すべて」なら何も付けない', () => {
		expect(navItemHref('/tags', { type: 'all' })).toBe('/tags');
	});

	it('ほかのページへは付けない', () => {
		expect(navItemHref('/status', group)).toBe('/status');
	});
});

describe('デモのビルド（docs/demo-pages-design.md §3.4）', () => {
	it('visibleNavItems は役割によらず対象画面だけを見せる', () => {
		expect(paths(visibleNavItems({ role: 'admin' }, true))).toEqual(['/tags']);
		expect(paths(visibleNavItems({ role: 'viewer' }, true))).toEqual(['/tags']);
	});

	it('製品のビルド（demo=false）の見え方は変えない', () => {
		expect(paths(visibleNavItems({ role: 'admin' }, false))).toEqual(paths(navItems));
	});

	it('DEMO_NAV_PATHS は navItems の項目だけ', () => {
		for (const path of DEMO_NAV_PATHS) expect(paths(navItems)).toContain(path);
	});

	it('isDemoTargetPath は対象画面（末尾の / と配下を含む）だけ true', () => {
		const target = DEMO_NAV_PATHS[0];
		expect(isDemoTargetPath(target)).toBe(true);
		expect(isDemoTargetPath(`${target}/`)).toBe(true);
		expect(isDemoTargetPath(`${target}/x`)).toBe(true);
		expect(isDemoTargetPath(`${target}s`)).toBe(false);
		expect(isDemoTargetPath('/users')).toBe(false);
		expect(isDemoTargetPath('/settings/appearance')).toBe(false);
	});
});
