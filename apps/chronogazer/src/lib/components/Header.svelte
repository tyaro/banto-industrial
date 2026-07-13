<script lang="ts">
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import { getAuthProvider } from '@banto/admin-core';
	import { pageTitle } from '$lib/navigation';
	import { settings } from '$lib/settings.svelte';
	import { sessionStore } from '$lib/session.svelte';
	import { commandPaletteStore } from '$lib/commandPalette.svelte';

	async function logout() {
		await getAuthProvider().logout();
		goto('/login');
	}
</script>

<header>
	<button
		type="button"
		class="icon-button"
		onclick={() => settings.toggleSidebar()}
		aria-label="サイドバーの切り替え"
	>
		☰
	</button>

	<h1>{pageTitle(page.url.pathname)}</h1>

	<div class="spacer"></div>

	<button
		type="button"
		class="icon-button"
		onclick={() => commandPaletteStore.show()}
		title="Ctrl+K"
		aria-label="コマンドパレットを開く"
	>
		🔍
	</button>

	{#if !sessionStore.authDisabled}
		<button type="button" class="icon-button" onclick={logout}>ログアウト</button>
	{/if}
</header>

<style>
	header {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		height: var(--banto-shell-header-height);
		padding: 0 1rem;
		background: var(--banto-surface);
		border-bottom: 1px solid var(--banto-border);
		/* Glass preset (spec M12): no-op under standard (--banto-backdrop: none). */
		backdrop-filter: var(--banto-backdrop, none);
		-webkit-backdrop-filter: var(--banto-backdrop, none);
	}

	h1 {
		margin: 0;
		font-size: 1rem;
		font-weight: 600;
	}

	.spacer {
		flex: 1;
	}

	.icon-button {
		border: none;
		background: none;
		color: var(--banto-text-muted);
		padding: 0.35rem 0.5rem;
		border-radius: var(--banto-radius);
		cursor: pointer;
		font-size: 0.875rem;
	}

	.icon-button:hover {
		background: color-mix(in srgb, var(--banto-primary) 8%, transparent);
		color: var(--banto-text);
	}
</style>
