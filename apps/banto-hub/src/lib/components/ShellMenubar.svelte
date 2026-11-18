<script lang="ts">
	/**
	 * シェルのメニューバー（nav-menubar-plan.md §5.2, D1。#1159）: `settings.navLayout ===
	 * 'menubar'` のとき、ナビのリンクをヘッダーの行に入れる。配線だけ - 見せる項目の
	 * 絞り込み（RBAC、`visibleNavItems()`）とバーのスロットへの並べ方（`navBarEntries()`、
	 * 管理限定の項目は 1 つの「管理」グループ）を `@banto/ui` の `HeaderNav` に渡し、
	 * 「その他」へのあふれ・ドロップダウン・キーボード操作（矢印・Esc・Home/End、§4.3-4.8）は
	 * `HeaderNav` が持つ。
	 *
	 * admin-template との差: 管理ナビを別の `<nav>` にせず、1 つの `<nav>`（名前は
	 * 「主要ナビゲーション」）に「管理 ▾」を設定の手前に置く（Hub の並び、
	 * `navigation.ts` の doc）。未適用の変更の件数（`pendingCount`）は /status の
	 * 未読の数として渡す（サイドバーの `pending-badge` と同じ数、99+ で頭打ちは部品側）。
	 * Header.svelte が >900px のときだけマウントする。`onInsufficient` は「その他」にも
	 * 入らないほど狭いことを知らせ、シェルがドロワーへ戻す（§4.9）。
	 */
	import { page } from '$app/state';
	import {
		isNavItemActive,
		navBarEntries,
		NAV_GROUP_LABELS,
		navItemHref,
		visibleNavItems,
		type NavItem
	} from '#lib/navigation.js';
	import { sessionStore } from '#lib/session.svelte.js';
	import { sharedTreeTarget } from '#lib/treeSelection.svelte.js';
	import { HeaderNav, type HeaderNavEntry, type HeaderNavLink } from '@banto/ui';
	import { navIcon } from './navIcons';

	interface Props {
		/** 未適用の変更の件数（/status の未読として出す）。 */
		pendingCount?: number;
		/** メインのナビが「その他」にも入らないほど狭くなった（`true`）、または戻った（`false`）。 */
		onInsufficient?: (insufficient: boolean) => void;
	}

	let { pendingCount = 0, onInsufficient }: Props = $props();

	function toLink(item: NavItem): HeaderNavLink {
		return {
			id: item.path,
			href: navItemHref(item.path, sharedTreeTarget.current),
			label: item.label,
			icon: navIcon(item.icon),
			active: isNavItemActive(item, page.url.pathname),
			count: item.path === '/status' ? pendingCount : 0
		};
	}

	const entries: HeaderNavEntry[] = $derived(
		navBarEntries(visibleNavItems({ role: sessionStore.role })).map((entry) =>
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

	const updatesLabel = (count: number): string => `未適用の変更が${count}件あります`;
</script>

<HeaderNav
	{entries}
	ariaLabel="主要ナビゲーション"
	moreLabel="その他"
	{updatesLabel}
	{onInsufficient}
/>
