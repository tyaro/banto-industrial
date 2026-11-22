<script lang="ts">
	// relay-wright の同名コンポーネントから複製。#1159（banto v7.2.0 のメニューバー、
	// nav-menubar-plan.md §4.1/§4.6/§4.9）: メニューバー表示では、>900px のあいだ
	// ブランド + `ShellMenubar` が行の先頭に入り（ページ名は
	// `--banto-shell-menubar-title-max` で省略記号、全文は `title`）、≤900px、または
	// バーが「その他」にも入らない（`menubarInsufficient`）ときはドロワーのボタンが
	// 戻ってサイドバーがドロワーになる（insufficient の間もバーは隠してマウントしたまま）。
	// ドックでは ☰ を出さず（ドロワーが無い）ブランドを左端に置く。
	import { tick } from 'svelte';
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import { hubLogout } from '#lib/banto/hubLogout.js';
	import { DEMO_NAV_PATHS, pageTitle, resolveAppPath } from '#lib/navigation.js';
	import { mobileNavStore } from '#lib/mobileNav.svelte.js';
	import { NAV_TOGGLE_ATTR, SIDEBAR_ID } from '#lib/mobileNav.js';
	import { settings } from '#lib/settings.svelte.js';
	import { sessionStore } from '#lib/session.svelte.js';
	import { commandPaletteStore } from '#lib/commandPalette.svelte.js';
	import { APP_NAME } from '#lib/appName.js';
	import ShellMenubar from './ShellMenubar.svelte';

	let { pendingCount = 0 }: { pendingCount?: number } = $props();

	// T19 S3-a（UX-43）+ #714: ☰ の振り分けと置き場所 - 狭幅(≤900px)ではオフキャンバスの
	// 開閉、デスクトップでは従来どおりサイドバーの折り畳み
	// （`mobileNavStore.toggleHamburger` に判断を集約、mobileNav.ts 参照）。
	// ヘッダー側に出すのは「サイドバーが見えていないとき」（狭幅 / 折り畳み中）だけ。
	// 広幅で開いている間は `Sidebar.svelte` のロゴ行に折り畳みボタンがあり、
	// ここには出さない（トグルを 2 つ同時に出さない）。
	const view = $derived(mobileNavStore.view);
	// サイドバー表示: 狭幅（ドロワー）か折り畳み中だけ ☰ を出す。メニューバー表示: サイドバーが
	// ドロワーのときだけ（>900px のバーがあるあいだは出さない）。ドックは出さない。
	const showToggle = $derived(
		settings.navLayout === 'sidebar'
			? mobileNavStore.drawerMode || settings.sidebarCollapsed
			: view.showDrawerButton
	);
	const showBrand = $derived(settings.navLayout === 'launcher' || view.showBar);
	const hamburgerLabel = $derived(
		mobileNavStore.drawerMode
			? mobileNavStore.open
				? 'メニューを閉じる'
				: 'メニューを開く'
			: 'サイドバーを開く'
	);
	const hamburgerExpanded = $derived(mobileNavStore.drawerMode ? mobileNavStore.open : false);

	let barHost: HTMLElement | undefined = $state();
	let drawerButtonHost: HTMLElement | undefined = $state();

	// `HeaderNav` は入りきらなくなったとき自分のドロップダウンを閉じるだけで、フォーカスの
	// 行き先はシェルが決める（§4.9）: バーの中にあったフォーカスは、バーに替わる ☰ へ。
	// ☰・ドロワーの中にあったフォーカスは、バーが戻ったとき（☰ とドロワーが消える）バーの
	// 先頭の項目へ。
	async function handleMenubarInsufficient(insufficient: boolean): Promise<void> {
		const active = document.activeElement;
		const focusToButton = insufficient && barHost?.contains(active) === true;
		const focusToBar =
			!insufficient &&
			(drawerButtonHost?.contains(active) === true ||
				document.getElementById(SIDEBAR_ID)?.contains(active) === true);
		mobileNavStore.setMenubarInsufficient(insufficient);
		if (!focusToButton && !focusToBar) return;
		await tick();
		const target = focusToButton
			? drawerButtonHost?.querySelector<HTMLElement>('button')
			: barHost?.querySelector<HTMLElement>('[data-header-nav-top]');
		target?.focus();
	}

	// 900px の境目（とナビの置き場所の切り替え）はバーとドロワーを `onInsufficient` を経ずに
	// 入れ替えるので、フォーカスのあった要素が外れる。DOM が変わる前に控え、入れ替わる側へ
	// 渡す - バーが消えるなら ☰、ドロワーが消えるならバーの先頭の項目。まだ測定前のバーは
	// `visibility: hidden` でフォーカスを断るので、数フレームやり直す。
	let lastShowBar: boolean | undefined;
	$effect.pre(() => {
		const next = view.showBar;
		const previous = lastShowBar;
		lastShowBar = next;
		if (previous === undefined || next === previous) return;
		const active = document.activeElement;
		const fromBar = !next && barHost?.contains(active) === true;
		const fromDrawer =
			next &&
			(drawerButtonHost?.contains(active) === true ||
				document.getElementById(SIDEBAR_ID)?.contains(active) === true);
		if (!fromBar && !fromDrawer) return;
		let frames = 10;
		const handOver = (): void => {
			// 別の置き場所へ切り替わった（設定、別タブからの同期を含む）ときは >900px に ☰ が無く、
			// 替わりはサイドバーまたはドックの先頭のリンク。
			const target = fromBar
				? (drawerButtonHost?.querySelector<HTMLElement>('button') ??
					document.querySelector<HTMLElement>(`#${SIDEBAR_ID} a[href], .launcher a[href]`))
				: barHost?.querySelector<HTMLElement>('[data-header-nav-top]');
			if (!target?.isConnected || target.closest('[inert]')) return;
			target.focus();
			const lost = document.activeElement === null || document.activeElement === document.body;
			if (lost && --frames > 0) requestAnimationFrame(handOver);
		};
		void tick().then(handOver);
	});

	const pageTitleText = $derived(pageTitle(page.url.pathname));

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
		<span class="drawer-button" bind:this={drawerButtonHost}>
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
		</span>
	{/if}

	{#if showBrand}
		<div class="brand">
			<span class="brand-icon" aria-hidden="true">🏮</span>
			<span class="brand-name">{APP_NAME}</span>
		</div>
	{/if}

	{#if view.showBar}
		<!-- メニューバー（§4.1）: 行の残りを取る（spacer は無し）。入りきらない間（§4.9）は
		     隠して inert - 同じナビはドロワーが持つ - が、幅は保って HeaderNav が戻りを測れる。 -->
		<div
			class="menubar"
			bind:this={barHost}
			style:visibility={mobileNavStore.menubarInsufficient ? 'hidden' : undefined}
			inert={mobileNavStore.menubarInsufficient}
		>
			<ShellMenubar {pendingCount} onInsufficient={handleMenubarInsufficient} />
		</div>
	{/if}

	<h1 class:menubar-title={view.showBar} title={view.showBar ? pageTitleText : undefined}>
		{pageTitleText}
	</h1>

	{#if !view.showBar}
		<div class="spacer"></div>
	{/if}

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

	{#if import.meta.env.VITE_BANTO_DEMO === '1'}
		<!-- デモ（docs/demo-pages-design.md §3.4）: ログアウトは無い（ログイン画面にデモの模擬が無く、
		     戻れなくなる）。状態は全部メモリなので、入口を読み込み直せば最初に戻る。 -->
		<button
			type="button"
			class="icon-button"
			onclick={() => location.assign(resolveAppPath(DEMO_NAV_PATHS[0]))}
		>
			デモを最初から
		</button>
	{:else if !sessionStore.authDisabled}
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

	/* ページ名は残し、メニューバーに幅を譲って省略記号で切る（D6）。 */
	h1.menubar-title {
		flex: 0 1 auto;
		max-width: var(--banto-shell-menubar-title-max);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.brand {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		flex-shrink: 0;
		font-weight: 700;
	}

	.menubar {
		display: flex;
		align-items: center;
		gap: 0.25rem;
		flex: 1 1 0;
		min-width: 0;
		height: 100%;
	}

	.drawer-button {
		display: contents;
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
