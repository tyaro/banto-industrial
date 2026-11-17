<script lang="ts">
	/**
	 * シェルのドック（nav-launcher-plan.md §5.1, D2-a。#1159）: `settings.navLayout ===
	 * 'launcher'` のときのサイドバーの代わり。配線だけ - メニューバーと同じ並び
	 * （`visibleNavItems()` → `navBarEntries()`）を、アイコンと名前と一緒に
	 * `@banto/ui` の `LauncherBar` のグループに変えて渡す。見た目・拡大・ツールチップ・
	 * ≤900px のタブバーは部品が持つ。画面の端は利用者の設定（`settings.launcherPosition`）。
	 * 管理グループの前後には区切り線を入れる（nav-menubar-plan D8-b、`separatorBefore`）。
	 * ChronoGazer のナビには未読のバッジが無いので、バッジは渡さない（Hub との差）。
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

	function toItem(item: NavItem): LauncherItem {
		return {
			id: item.path,
			href: resolveAppPath(item.path),
			label: item.label,
			icon: navIcon(item.icon),
			active: isNavItemActive(item, page.url.pathname)
		};
	}

	/** 1 つの `<nav>`。グループの先頭と、グループの直後の単独項目の前に区切り線を入れる。 */
	const groups: LauncherGroup[] = $derived.by(() => {
		const items: LauncherItem[] = [];
		let afterGroup = false;
		const visible = visibleNavItems({
			role: sessionStore.role,
			publicViewer: sessionStore.publicViewer
		});
		for (const entry of navBarEntries(visible)) {
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
