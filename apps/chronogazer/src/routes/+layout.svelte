<script lang="ts">
	import '../app.css';
	import { afterNavigate, invalidateAll } from '$app/navigation';
	import { navigating, page } from '$app/state';
	import { bantoReady } from '$lib/banto/setup'; // initBanto() (+ EventProvider) before any route guard runs (spec §3, §11.1)
	import { settings } from '$lib/settings.svelte';
	import ToastHost from '$lib/components/ToastHost.svelte';
	// banto v3.0.0（#286）: 起動待ちは「起動中…」と「サーバーに接続できません
	// ＋再接続」の 2 状態（`startup.ts` / `startupState.svelte.ts`）。
	import StartupSplash from '$lib/components/StartupSplash.svelte';
	import { isStartupDeferral } from '$lib/banto/startupGate';

	let { children } = $props();

	// Start theme handling (applies persisted mode, watches OS changes).
	$effect(() => {
		settings.init();
	});

	// banto v3.0.1（#321）: a protected route opened before startup finished was
	// deferred by its guard (`$lib/banto/startupGate.ts`) - nothing of it has
	// run. Keep the splash up instead of the error page, and re-run the loads
	// once startup has finished, so the same URL opens (or goes where the
	// guard sends it). A deferral can only come from a load started before
	// `bantoReady` resolved, so the re-run (which starts after it) clears it.
	//
	// The re-run waits until the navigation that produced the deferral has
	// COMPLETED (`afterNavigate`, and no other navigation in flight): an
	// `invalidateAll()` that starts while SvelteKit is still finishing a
	// navigation makes that navigation abort without clearing its internal
	// "navigating" flag (@sveltejs/kit 2.70 `client.js`), after which
	// `beforeNavigate` - the unsaved-changes guard - never runs again. A fast
	// startup (Tauri, a local server) resolves `bantoReady` exactly then.
	const startupDeferred = $derived(isStartupDeferral(page.error));
	let started = $state(false);
	let navigationDone = $state(false);
	void bantoReady.then(() => {
		started = true;
	});
	afterNavigate(() => {
		navigationDone = true;
	});
	$effect(() => {
		if (startupDeferred && started && navigationDone && navigating.to === null) {
			void invalidateAll();
		}
	});
</script>

{#await bantoReady}
	<StartupSplash />
{:then}
	{#if startupDeferred}
		<StartupSplash />
	{:else}
		{@render children()}
		<ToastHost />
	{/if}
{/await}
