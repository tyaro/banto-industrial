<script lang="ts">
	import '../app.css';
	import { bantoReady } from '$lib/banto/setup'; // initBanto() (+ EventProvider) before any route guard runs (spec §3, §11.1)
	import { settings } from '$lib/settings.svelte';
	import ToastHost from '$lib/components/ToastHost.svelte';
	// banto v3.0.0（#286）: 起動待ちは「起動中…」と「サーバーに接続できません
	// ＋再接続」の 2 状態（`startup.ts` / `startupState.svelte.ts`）。
	import StartupSplash from '$lib/components/StartupSplash.svelte';

	let { children } = $props();

	// Start theme handling (applies persisted mode, watches OS changes).
	$effect(() => {
		settings.init();
	});
</script>

{#await bantoReady}
	<StartupSplash />
{:then}
	{@render children()}
	<ToastHost />
{/await}
