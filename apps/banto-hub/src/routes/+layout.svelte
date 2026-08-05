<script lang="ts">
	// relay-wright の同名ファイルから複製。
	import '../app.css';
	import { bantoReady } from '$lib/banto/setup'; // initBanto() (+ EventProvider) をどのルートガードより先に完了させる
	import { settings } from '$lib/settings.svelte';
	import ToastHost from '$lib/components/ToastHost.svelte';

	let { children } = $props();

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
