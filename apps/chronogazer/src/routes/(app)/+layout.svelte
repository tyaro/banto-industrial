<script lang="ts">
	import { untrack } from 'svelte';
	import { goto, invalidateAll } from '$app/navigation';
	import { getSessionController, notify } from '@banto/admin-core';
	import { isLeavingForLogin, leaveForLogin } from '$lib/banto/logout.svelte';
	import { OWNER_CHANGE_POLICY, watchOwnerChanges } from '$lib/banto/ownerChange';
	import Header from '$lib/components/Header.svelte';
	import Sidebar from '$lib/components/Sidebar.svelte';
	import CommandPalette from '$lib/components/CommandPalette.svelte';
	import { commandPaletteStore } from '$lib/commandPalette.svelte';

	let { children, data } = $props();

	// banto v2.0.0 (Issue #260, design §6.1): the protected layout's three
	// wirings, copied from banto v2.0.0's admin-template
	// `routes/(app)/+layout.svelte` (v2 移行 PR1c). ChronoGazer 固有の差:
	// the notices are Japanese strings (no i18n here), there is no `base`
	// path, and the template's other shell features (unsaved-changes window
	// guard, command history owner, nav badges, sidebar overlay) are not part
	// of this copy.
	//
	// A page belongs to the session it was built for. The page is shown only
	// while the session generation its loads confirmed
	// (`data.sessionGeneration`, from `+layout.ts`) is still the live one, and
	// is rebuilt from scratch (`{#key}`) once the next session's loads
	// complete - so nothing of the old user's page (in-memory state, unsaved
	// input) survives into the next session. A guard re-run that confirms the
	// same session keeps the generation, so an ordinary `invalidateAll()`
	// never rebuilds the page.
	//
	// Wiring ①: whenever the controller's generation differs from the one
	// this page's load confirmed, re-run the loads (`invalidateAll()`), which
	// confirm the session again and send the screen to /login, the retryable
	// error page, or the rebuilt page of the (new) user. This covers every
	// way the generation moves - a background revocation confirmed `none`
	// (banto #241, formerly `onSessionEnded`: LAN/SSE `401`), an ending
	// confirmed before this layout mounted (S-34/S-74), another tab's login
	// that goes unknown -> active without passing `none` (S-79/S-80), and the
	// Tauri desktop session being re-bound by `auth_config_apply` (login-not-
	// required mode ON/OFF/role change, S-94/S-96/S-99). `requestedFor` keeps
	// one invalidation per generation. While this tab is logging out (or
	// leaving for /login), no re-load: that sequence goes to /login itself,
	// and an invalidation started here would win over the navigation
	// (`$lib/banto/logout.svelte.ts`). `isLeavingForLogin()` is reactive, so a
	// generation change skipped meanwhile is handled once it ends.
	const sessionController = getSessionController();
	let requestedFor = -1;
	$effect(() => {
		const generation = sessionController.snapshot.generation;
		if (isLeavingForLogin()) return;
		if (generation !== data.sessionGeneration && requestedFor !== generation) {
			requestedFor = generation;
			void invalidateAll();
		}
	});

	// Wiring ②: another tab logged in as a different user. The controller
	// keeps the change until it is handled (`snapshot.pendingOwnerChange`),
	// so a change confirmed while this layout was not mounted (the 503 page in
	// between, S-81) is reported on mount; a confirmed `none` discards it
	// (S-83). Wiring ① already rebuilds the screen for the new user; this only
	// tells them (`OWNER_CHANGE_POLICY = 'rebuild'`, owner decision
	// 2026-10-02). The shared token is never cleared here (I-17). Wiring ③ -
	// a confirmation that fails after the switch - is the load's 503
	// (`+layout.ts`): not left automatically (S-36/S-60).
	// `untrack`: runs once per mount (the subscription does the rest).
	$effect(() =>
		untrack(() =>
			watchOwnerChanges(sessionController, {
				policy: OWNER_CHANGE_POLICY,
				notify: (policy) =>
					notify(
						'info',
						policy === 'relogin'
							? '別のユーザーでログインされました。もう一度ログインしてください。'
							: '別のユーザーでログインされました。画面をそのユーザーで開き直しました。'
					),
				goToLogin: () => leaveForLogin(() => goto('/login'))
			})
		)
	);

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
			{#if data.sessionGeneration === sessionController.snapshot.generation}
				{#key data.sessionGeneration}
					{@render children()}
				{/key}
			{/if}
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
