<script lang="ts">
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import { logoutAndLeave } from '#lib/banto/logout.svelte.js';
	import { notifyLogoutOutcome } from '#lib/banto/logoutNotice.js';
	import { pageTitle, resolveAppPath } from '#lib/navigation.js';
	import { settings } from '#lib/settings.svelte.js';
	import { NAV_TOGGLE_ATTR, SIDEBAR_ID, toggleSidebarWithFocus } from '#lib/sidebarToggle.js';
	import { sessionStore } from '#lib/session.svelte.js';
	import { commandPaletteStore } from '#lib/commandPalette.svelte.js';

	async function logout() {
		// banto v2.0.0 (#260, design §6.1): logout() -> confirm the session ->
		// /login only when it is confirmed `none` (another tab's login
		// confirmed meanwhile stays). `'stayed'`/`'unverified'` are told with a
		// toast - see `#lib/banto/logout.svelte.ts` for the full sequence.
		await logoutAndLeave(() => goto(resolveAppPath('/login')), { notify: notifyLogoutOutcome });
	}
</script>

<header>
	<!--
		banto-hub の #714 と同じ: ヘッダーの ☰ は「サイドバーが見えていないとき（折り畳み中）」
		だけ出す。開いている間は `Sidebar.svelte` のロゴ行に折り畳みボタンがあり、
		トグルを 2 つ同時に出さない。
	-->
	{#if settings.sidebarCollapsed}
		<button
			type="button"
			class="icon-button"
			onclick={() => toggleSidebarWithFocus()}
			aria-label="サイドバーを開く"
			aria-expanded={false}
			aria-controls={SIDEBAR_ID}
			{...{ [NAV_TOGGLE_ATTR]: '' }}
		>
			☰
		</button>
	{/if}

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

	{#if sessionStore.publicViewer}
		<!-- I2b（admin-template v3.0.0 の Header.svelte と同じ、ADR-0012）: LAN の
		     閲覧公開のセッションにはアカウントもログアウトも無いので、代わりに
		     本当のログインへ戻る口を出す。 -->
		<button type="button" class="icon-button" onclick={() => goto(resolveAppPath('/login'))}
			>ログイン</button
		>
	{:else if !sessionStore.authDisabled}
		{#if sessionStore.localDesktop}
			<!-- #392 B2-2: サービスへのこの PC からの接続（`localDesktop`）。アカウントは
			     無いが、ログアウトしてアカウントでログインし直せるようボタンは残す。 -->
			<span class="session-label" title="この PC からの接続（ログインなし）">この PC</span>
		{/if}
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

	.session-label {
		color: var(--banto-text-muted);
		font-size: 0.8rem;
		padding: 0.15rem 0.5rem;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
	}

	.icon-button:hover {
		background: color-mix(in srgb, var(--banto-primary) 8%, transparent);
		color: var(--banto-text);
	}
</style>
