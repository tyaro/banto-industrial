<script lang="ts">
	import { page } from '$app/state';
	import { navItems, type NavItem } from '$lib/navigation';
	import { settings } from '$lib/settings.svelte';
	import { sessionStore } from '$lib/session.svelte';
	import { isAdmin } from '$lib/permissions';

	// #359 chronogazer 分: `item.activeMatch`（無ければ `item.path`）を基準に
	// 前方一致判定する - `navigation.ts` の doc comment参照（`設定` は
	// 遷移先が `/settings/appearance` でも `/settings` 配下ならハイライト
	// させたい）。
	function isActive(item: NavItem): boolean {
		const match = item.activeMatch ?? item.path;
		return page.url.pathname === match || page.url.pathname.startsWith(match + '/');
	}

	// Spec M10 RBAC: hide admin-only entries (「ユーザー管理」) rather than
	// showing them disabled - navigation-level hiding, same as
	// routes/(app)/users/+page.ts redirecting a non-admin instead of
	// rendering a 403 screen.
	const visibleItems = $derived(
		navItems.filter((item) => !item.adminOnly || isAdmin(sessionStore.role))
	);
</script>

<aside class:collapsed={settings.sidebarCollapsed}>
	<div class="brand">
		<span class="brand-icon">🏮</span>
		{#if !settings.sidebarCollapsed}
			<span class="brand-name">ChronoGazer</span>
		{/if}
	</div>

	<nav>
		{#each visibleItems as item (item.path)}
			<a
				href={item.path}
				class:active={isActive(item)}
				title={settings.sidebarCollapsed ? item.label : undefined}
			>
				<span class="icon">{item.icon}</span>
				{#if !settings.sidebarCollapsed}
					<span>{item.label}</span>
				{/if}
			</a>
		{/each}
	</nav>
</aside>

<style>
	aside {
		width: var(--banto-shell-sidebar-width);
		flex-shrink: 0;
		display: flex;
		flex-direction: column;
		background: var(--banto-surface);
		border-right: 1px solid var(--banto-border);
		transition: width 0.15s ease;
		/* Glass preset (spec M12): no-op under standard (--banto-backdrop: none). */
		backdrop-filter: var(--banto-backdrop, none);
		-webkit-backdrop-filter: var(--banto-backdrop, none);
	}

	aside.collapsed {
		width: var(--banto-shell-sidebar-width-collapsed);
	}

	.brand {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		height: var(--banto-shell-header-height);
		padding: 0 0.9rem;
		border-bottom: 1px solid var(--banto-border);
		font-weight: 700;
	}

	nav {
		display: flex;
		flex-direction: column;
		padding: 0.5rem;
		gap: 2px;
	}

	nav a {
		display: flex;
		align-items: center;
		gap: 0.6rem;
		padding: 0.5rem 0.6rem;
		border-radius: var(--banto-radius);
		color: var(--banto-text-muted);
		text-decoration: none;
		white-space: nowrap;
	}

	nav a:hover {
		background: color-mix(in srgb, var(--banto-primary) 8%, transparent);
		color: var(--banto-text);
	}

	nav a.active {
		background: color-mix(in srgb, var(--banto-primary) 14%, transparent);
		color: var(--banto-primary);
		font-weight: 600;
	}

	/* Glass preset accent (spec M12): the active nav item gets the accent
	   gradient. Scoped by the preset attribute so standard keeps the flat
	   tint above untouched. */
	:global([data-banto-preset='glass']) nav a.active {
		background: var(--banto-accent-gradient);
		color: var(--banto-text-inverse);
	}

	.icon {
		width: 1.25rem;
		text-align: center;
	}
</style>
