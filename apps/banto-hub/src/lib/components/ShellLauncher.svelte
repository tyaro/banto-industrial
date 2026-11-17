<script lang="ts">
	/**
	 * シェルのドック（nav-launcher-plan.md §5.1, D2-a。#1159）: `settings.navLayout ===
	 * 'launcher'` のときのサイドバーの代わり。配線だけ - メニューバーと同じ並び
	 * （`visibleNavItems()` → `navBarEntries()`）を、アイコン・未適用の件数・名前と
	 * 一緒に `@banto/ui` の `LauncherBar` のグループに変えて渡す。見た目・拡大・
	 * ツールチップ・≤900px のタブバーは部品が持つ。画面の端は利用者の設定
	 * （`settings.launcherPosition`）。管理グループの前後には区切り線を入れる
	 * （nav-menubar-plan D8-b、`separatorBefore`）。
	 */
	import { page } from '$app/state';
	import {
		isNavItemActive,
		navBarEntries,
		resolveAppPath,
		visibleNavItems,
		type NavItem
	} from '#lib/navigation.js';
	import { sessionStore } from '#lib/session.svelte.js';
	import { settings } from '#lib/settings.svelte.js';
	import { LauncherBar, type LauncherGroup, type LauncherItem } from '@banto/ui';
	import { navIcon } from './navIcons';

	let { pendingCount = 0 }: { pendingCount?: number } = $props();

	/** 未適用の件数のバッジの文字（サイドバーと同じく 99+ で頭打ち）。 */
	function badgeText(count: number): string {
		return count > 99 ? '99+' : String(count);
	}

	function toItem(item: NavItem): LauncherItem {
		const count = item.path === '/status' ? pendingCount : 0;
		return {
			id: item.path,
			href: resolveAppPath(item.path),
			label: item.label,
			icon: navIcon(item.icon),
			active: isNavItemActive(item, page.url.pathname),
			badge:
				count > 0
					? { text: badgeText(count), label: `未適用の変更が${count}件あります` }
					: undefined
		};
	}

	/** 1 つの `<nav>`。グループの先頭と、グループの直後の単独項目の前に区切り線を入れる。 */
	const groups: LauncherGroup[] = $derived.by(() => {
		const items: LauncherItem[] = [];
		let afterGroup = false;
		for (const entry of navBarEntries(visibleNavItems({ role: sessionStore.role }))) {
			const own = entry.kind === 'group' ? entry.items : [entry.item];
			own.forEach((item, index) => {
				const rule = items.length > 0 && (afterGroup || (entry.kind === 'group' && index === 0));
				items.push({ ...toItem(item), ...(rule ? { separatorBefore: true } : {}) });
				afterGroup = false;
			});
			afterGroup = entry.kind === 'group';
		}
		return [{ id: 'main', ariaLabel: '主要ナビゲーション', items }];
	});
</script>

<LauncherBar {groups} position={settings.launcherPosition} />
