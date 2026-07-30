<script lang="ts">
	import '../app.css';
	import { bantoReady } from '$lib/banto/setup'; // initBanto() (+ EventProvider) before any route guard runs (spec §3, §11.1)
	import { settings } from '$lib/settings.svelte';
	import ToastHost from '$lib/components/ToastHost.svelte';

	let { children } = $props();

	// Start theme handling (applies persisted mode, watches OS changes).
	$effect(() => {
		settings.init();
	});
</script>

{#await bantoReady}
	<p class="banto-splash">起動中…</p>
{:then}
	{@render children()}
	<ToastHost />
{/await}

<style>
	.banto-splash {
		min-height: 100vh;
		display: grid;
		place-items: center;
		color: var(--banto-text-muted);
	}
</style>
