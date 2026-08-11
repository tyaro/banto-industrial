<script lang="ts">
	// relay-wright の同名コンポーネントから無改変で複製。
	import { page } from '$app/state';
	import { navItems } from '$lib/navigation';
	import { settings } from '$lib/settings.svelte';
	import { sessionStore } from '$lib/session.svelte';
	import { isAdmin } from '$lib/permissions';
	import { APP_NAME } from '$lib/appName';

	let { pendingCount = 0 }: { pendingCount?: number } = $props();

	function isActive(path: string): boolean {
		return page.url.pathname === path || page.url.pathname.startsWith(path + '/');
	}

	// RBAC: admin-only 項目は表示自体を隠す（無効化して見せない）。
	const visibleItems = $derived(
		navItems.filter((item) => !item.adminOnly || isAdmin(sessionStore.role))
	);
</script>

<aside class:collapsed={settings.sidebarCollapsed}>
	<div class="brand">
		<span class="brand-icon">🏮</span>
		{#if !settings.sidebarCollapsed}
			<span class="brand-name">{APP_NAME}</span>
		{/if}
	</div>

	<nav>
		{#each visibleItems as item (item.path)}
			<a
				href={item.path}
				class:active={isActive(item.path)}
				title={settings.sidebarCollapsed ? item.label : undefined}
			>
				<span class="icon">{item.icon}</span>
				{#if !settings.sidebarCollapsed}
					<span class="nav-label">
						<span>{item.label}</span>
						{#if item.path === '/status' && pendingCount > 0}
							<span class="pending-badge">{pendingCount}</span>
						{/if}
					</span>
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

	:global([data-banto-preset='glass']) nav a.active {
		background: var(--banto-accent-gradient);
		color: var(--banto-text-inverse);
	}

	.icon {
		width: 1.25rem;
		text-align: center;
	}

	.nav-label {
		display: inline-flex;
		align-items: center;
		gap: 0.45rem;
		min-width: 0;
	}

	.pending-badge {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		min-width: 1.35rem;
		height: 1.35rem;
		padding: 0 0.4rem;
		border-radius: 999px;
		background: color-mix(in srgb, var(--banto-danger) 12%, transparent);
		color: var(--banto-danger);
		font-size: 0.72rem;
		font-weight: 700;
		line-height: 1;
	}
</style>
