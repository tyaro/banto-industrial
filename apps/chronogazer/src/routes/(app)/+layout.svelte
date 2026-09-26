<script lang="ts">
	import { invalidateAll } from '$app/navigation';
	import { onSessionEnded } from '@banto/admin-core';
	import Header from '$lib/components/Header.svelte';
	import Sidebar from '$lib/components/Sidebar.svelte';
	import CommandPalette from '$lib/components/CommandPalette.svelte';
	import { commandPaletteStore } from '$lib/commandPalette.svelte';

	let { children } = $props();

	// banto v1.7.2 (tyaro/banto#241): when the session behind an open screen
	// is revoked in the background (account deleted/demoted, password
	// changed/reset), re-run the route guard (`+layout.ts`) so the screen
	// moves to the login page. The signal comes from admin-core's SSE
	// (`/api/events`, `connectEvents` in `setup.ts`, LAN/server mode only) and
	// is only sent once `check()` confirmed the session ended. Inert in the
	// Tauri shell (`createTauriEventProvider` has no `401` to report) and in
	// the plain `vite dev` demo mode (no EventProvider).
	$effect(() => onSessionEnded(() => void invalidateAll()));

	// Ctrl+K / Cmd+K (spec M16): a global toggle registered here (the app
	// shell), not inside CommandPalette itself - it must keep working to
	// CLOSE the palette while focus is inside its own search input (or any
	// other input/textarea on the page), which a listener scoped to just the
	// palette component couldn't do once it's unmounted.
	function handleKeydown(event: KeyboardEvent): void {
		if (event.key.toLowerCase() === 'k' && (event.ctrlKey || event.metaKey)) {
			event.preventDefault();
			commandPaletteStore.toggle();
		}
	}
</script>

<svelte:window onkeydown={handleKeydown} />

<div class="shell">
	<Sidebar />
	<div class="main">
		<Header />
		<main>
			{@render children()}
		</main>
	</div>
</div>

{#if commandPaletteStore.open}
	<CommandPalette />
{/if}

<style>
	.shell {
		display: flex;
		min-height: 100vh;
	}

	.main {
		flex: 1;
		display: flex;
		flex-direction: column;
		min-width: 0;
	}

	main {
		flex: 1;
		padding: 1.25rem;
	}
</style>
