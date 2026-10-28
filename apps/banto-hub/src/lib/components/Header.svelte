<script lang="ts">
	// relay-wright の同名コンポーネントから無改変で複製。
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import { hubLogout } from '#lib/banto/hubLogout.js';
	import { pageTitle, resolveAppPath } from '#lib/navigation.js';
	import { mobileNavStore } from '#lib/mobileNav.svelte.js';
	import { NAV_TOGGLE_ATTR, SIDEBAR_ID } from '#lib/mobileNav.js';
	import { settings } from '#lib/settings.svelte.js';
	import { sessionStore } from '#lib/session.svelte.js';
	import { commandPaletteStore } from '#lib/commandPalette.svelte.js';

	let { pendingCount = 0 }: { pendingCount?: number } = $props();

	// T19 S3-a（UX-43）+ #714: ☰ の振り分けと置き場所 - 狭幅(≤900px)ではオフキャンバスの
	// 開閉、デスクトップでは従来どおりサイドバーの折り畳み
	// （`mobileNavStore.toggleHamburger` に判断を集約、mobileNav.ts 参照）。
	// ヘッダー側に出すのは「サイドバーが見えていないとき」（狭幅 / 折り畳み中）だけ。
	// 広幅で開いている間は `Sidebar.svelte` のロゴ行に折り畳みボタンがあり、
	// ここには出さない（トグルを 2 つ同時に出さない）。
	const showToggle = $derived(mobileNavStore.isNarrow || settings.sidebarCollapsed);
	const hamburgerLabel = $derived(
		mobileNavStore.isNarrow
			? mobileNavStore.open
				? 'メニューを閉じる'
				: 'メニューを開く'
			: 'サイドバーを開く'
	);
	const hamburgerExpanded = $derived(mobileNavStore.isNarrow ? mobileNavStore.open : false);

	async function logout() {
		// banto v2.0.0 (#260, design §6.1): logout() -> confirm the session ->
		// /login only when it is confirmed `none` (another tab's login
		// confirmed meanwhile stays). `'stayed'`/`'unverified'` are told with a
		// toast - see `#lib/banto/logout.svelte.ts` for the full sequence.
		// 試運転の grant のセッションも同じ流れ（`#lib/banto/hubLogout.ts`）。
		await hubLogout(() => goto(resolveAppPath('/login')));
	}
</script>

<header>
	{#if showToggle}
		<button
			type="button"
			class="icon-button"
			onclick={() => mobileNavStore.toggleHamburger()}
			aria-label={hamburgerLabel}
			aria-expanded={hamburgerExpanded}
			aria-controls={SIDEBAR_ID}
			{...{ [NAV_TOGGLE_ATTR]: '' }}
		>
			☰
		</button>
	{/if}

	<h1>{pageTitle(page.url.pathname)}</h1>

	<div class="spacer"></div>

	{#if pendingCount > 0}
		<div class="pending-pill" aria-label={`未適用の変更が${pendingCount}件あります`}>
			未適用 {pendingCount}件
		</div>
	{/if}

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
		/* T19 S3-a（UX-43）: 真の固定 - ヘッダーは本文のスクロールに関わらず
		   縮まない固定領域にする（(app)/+layout.svelte の `main` だけが
		   専用スクロール領域を持つ）。 */
		flex-shrink: 0;
		padding: 0 1rem;
		background: var(--banto-surface);
		border-bottom: 1px solid var(--banto-border);
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

	.pending-pill {
		display: inline-flex;
		align-items: center;
		padding: 0.2rem 0.55rem;
		border-radius: 999px;
		border: 1px solid color-mix(in srgb, var(--banto-danger) 24%, var(--banto-border));
		background: color-mix(in srgb, var(--banto-danger) 10%, var(--banto-surface));
		color: var(--banto-danger);
		font-size: 0.75rem;
		font-weight: 600;
		white-space: nowrap;
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
