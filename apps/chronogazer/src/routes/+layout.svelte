<script lang="ts">
	import '../app.css';
	import { refreshAll } from '$app/navigation';
	import { page } from '$app/state';
	import { bantoReady } from '#lib/banto/setup.js'; // initBanto() (+ EventProvider) before any route guard runs (spec §3, §11.1)
	import { settings } from '#lib/settings.svelte.js';
	// Toast stack: `@banto/ui`'s ToastHost over the app's single store (banto #220 phase 3).
	import { ToastHost } from '@banto/ui';
	import { toastStore } from '#lib/toast.svelte.js';
	// banto v3.0.0（#286）: 起動待ちは「起動中…」と「サーバーに接続できません
	// ＋再接続」の 2 状態（`startup.ts` / `startupState.svelte.ts`）。
	import StartupSplash from '#lib/components/StartupSplash.svelte';
	import { isStartupDeferral } from '#lib/banto/startupGate.js';
	import {
		isNavigationSettled,
		trackFirstNavigation
	} from '#lib/banto/navigationSettled.svelte.js';

	let { children } = $props();

	// Start theme handling (applies persisted mode, watches OS changes).
	$effect(() => {
		settings.init();
	});

	// banto v3.0.1（#321）、v4.0.0（#326・#325）: a protected route opened before startup finished was
	// deferred by its guard (`#lib/banto/startupGate.ts`) - nothing of it has
	// run. Keep the splash up instead of the error page, and re-run the loads
	// once startup has finished, so the same URL opens (or goes where the
	// guard sends it). A deferral can only come from a load started before
	// `bantoReady` resolved, so the re-run (which starts after it) clears it.
	//
	// The re-run waits until the navigation that produced the deferral has
	// COMPLETED and no other one is in flight (`isNavigationSettled()`): a
	// `refreshAll()` that starts while SvelteKit is still finishing a
	// navigation aborts it and leaves `beforeNavigate` - the unsaved-changes
	// guard - skipped (see `#lib/banto/navigationSettled.svelte.ts`). A fast
	// startup (Tauri, a local server) resolves `bantoReady` exactly then.
	// This layout is mounted by the first navigation, so it is also the one
	// that records its end for the whole app (`trackFirstNavigation`, which
	// wiring ① of `(app)/+layout.svelte` relies on too, banto #326).
	trackFirstNavigation();
	const startupDeferred = $derived(isStartupDeferral(page.error));
	let started = $state(false);
	void bantoReady.then(() => {
		started = true;
	});
	$effect(() => {
		if (startupDeferred && started && isNavigationSettled()) {
			void refreshAll();
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
		<ToastHost store={toastStore} />
	{/if}
{/await}
