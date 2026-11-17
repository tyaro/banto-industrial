<script lang="ts">
	/**
	 * シェルのメニューバー（nav-menubar-plan.md §5.2, D1。#1159）: `settings.navLayout ===
	 * 'menubar'` のとき、ナビのリンクをヘッダーの行に入れる。配線だけ - 見せる項目の
	 * 絞り込み（RBAC と閲覧公開の許可リスト、`visibleNavItems()`）とバーのスロットへの
	 * 並べ方（`navBarEntries()`、管理限定の項目は 1 つの「管理」グループ）を `@banto/ui` の
	 * `HeaderNav` に渡し、「その他」へのあふれ・ドロップダウン・キーボード操作
	 * （矢印・Esc・Home/End、§4.3-4.8）は `HeaderNav` が持つ。
	 *
	 * admin-template との差: 管理ナビを別の `<nav>` にせず、1 つの `<nav>`（名前は
	 * 「主要ナビゲーション」）に「管理 ▾」を設定の手前に置く（`navigation.ts` の doc）。
	 * ChronoGazer のナビには未読のバッジが無いので件数は渡さない。Header.svelte が
	 * >900px のときだけマウントする。`onInsufficient` は「その他」にも入らないほど狭い
	 * ことを知らせ、シェルがドロワーへ戻す（§4.9）。
	 */
	import { page } from '$app/state';
	import {
		isNavItemActive,
		navBarEntries,
		NAV_GROUP_LABELS,
		resolveAppPath,
		visibleNavItems,
		type NavItem
	} from '#lib/navigation.js';
	import { sessionStore } from '#lib/session.svelte.js';
	import { HeaderNav, type HeaderNavEntry, type HeaderNavLink } from '@banto/ui';
	import { navIcon } from './navIcons';

	interface Props {
		/** メインのナビが「その他」にも入らないほど狭くなった（`true`）、または戻った（`false`）。 */
		onInsufficient?: (insufficient: boolean) => void;
	}

	let { onInsufficient }: Props = $props();

	function toLink(item: NavItem): HeaderNavLink {
		return {
			id: item.path,
			href: resolveAppPath(item.path),
			label: item.label,
			icon: navIcon(item.icon),
			active: isNavItemActive(item, page.url.pathname)
		};
	}

	const entries: HeaderNavEntry[] = $derived(
		navBarEntries(
			visibleNavItems({ role: sessionStore.role, publicViewer: sessionStore.publicViewer })
		).map((entry) =>
			entry.kind === 'item'
				? { kind: 'link', ...toLink(entry.item) }
				: {
						kind: 'group',
						id: entry.key,
						label: NAV_GROUP_LABELS[entry.key] ?? entry.key,
						items: entry.items.map(toLink)
					}
		)
	);

	const updatesLabel = (count: number): string => `未読が${count}件あります`;
</script>

<HeaderNav
	{entries}
	ariaLabel="主要ナビゲーション"
	moreLabel="その他"
	{updatesLabel}
	{onInsufficient}
/>
