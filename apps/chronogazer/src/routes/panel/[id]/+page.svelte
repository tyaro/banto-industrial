<script lang="ts">
	/**
	 * Standalone panel window (spec §5.3 v2 pop-out). Deliberately OUTSIDE the
	 * `(app)` route group: it gets none of that group's shell (no
	 * Sidebar/Header, `routes/(app)/+layout.svelte`) AND none of its auth
	 * GUARD (`routes/(app)/+layout.ts`'s `redirect(307, '/login')`) - a
	 * popped-out panel is a chrome-less native window whose ONLY content is
	 * this one panel, and it must never redirect: the main window (and ONLY
	 * the main window) owns the login flow, so an unauthenticated visit here
	 * just shows a message instead.
	 *
	 * Works in two contexts with identical code:
	 *  - as this route rendered in a plain browser tab (Playwright-verifiable
	 *    in this dev environment, which has no webkit2gtk to run the real
	 *    Tauri multiwindow path);
	 *  - as the actual content of the native `WebviewWindow` src-tauri's
	 *    `panel_open` command opens (`WebviewUrl::App("panel/{id}")`) - not
	 *    verifiable here, verify on a machine with webkit2gtk.
	 */
	import { page } from '$app/state';
	import { getAuthProvider } from '@banto/admin-core';
	import { bantoReady } from '$lib/banto/setup';
	import { findPanelDef } from '$lib/banto/panels';
	import DashboardPanel from '$lib/components/DashboardPanel.svelte';

	const id = $derived(page.params.id ?? '');
	const def = $derived(findPanelDef(id));

	let authState: 'checking' | 'ok' | 'unauthenticated' = $state('checking');

	$effect(() => {
		void (async () => {
			await bantoReady; // provider selection (spec §11.1's three-way probe) must finish first
			authState = (await getAuthProvider().check()) ? 'ok' : 'unauthenticated';
		})();
	});

	// The route's own viewport is the chart's height budget (no dashboard
	// grid/card chrome around it here) - measured the same
	// bind:clientHeight pattern as the dashboard's dock panes use.
	let bodyHeight: number = $state(0);
	const chartHeight = $derived(Math.max(140, bodyHeight - 16));
</script>

<svelte:head>
	<title>{def?.title ?? id} - Banto</title>
</svelte:head>

{#if authState === 'checking'}
	<p class="status">確認中…</p>
{:else if authState === 'unauthenticated'}
	<p class="status">ログインが必要です。メインウィンドウでログインしてください。</p>
{:else}
	<div class="panel-window">
		<header class="panel-header">
			{#if def?.icon}<span class="icon" aria-hidden="true">{def.icon}</span>{/if}
			<span class="title">{def?.title ?? id}</span>
		</header>
		<div class="panel-body" bind:clientHeight={bodyHeight}>
			<DashboardPanel {id} height={chartHeight} />
		</div>
	</div>
{/if}

<style>
	.status {
		min-height: 100vh;
		display: grid;
		place-items: center;
		margin: 0;
		padding: 1rem;
		text-align: center;
		color: var(--banto-text-muted);
	}

	.panel-window {
		height: 100vh;
		display: flex;
		flex-direction: column;
	}

	.panel-header {
		flex: 0 0 auto;
		display: flex;
		align-items: center;
		gap: 0.4rem;
		height: var(--banto-dock-titlebar-height, 2rem);
		padding: 0 0.75rem;
		background: var(--banto-surface-raised);
		border-bottom: 1px solid var(--banto-border);
		font-size: 0.85rem;
		font-weight: 600;
		color: var(--banto-text);
	}

	.panel-body {
		flex: 1;
		min-height: 0;
		overflow: auto;
		box-sizing: border-box;
		padding: 0.75rem;
		background: var(--banto-surface);
	}
</style>
