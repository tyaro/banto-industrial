<script lang="ts">
	import { untrack } from 'svelte';
	import { afterNavigate, goto, refreshAll } from '$app/navigation';
	import { getSessionController, notify } from '@banto/admin-core';
	import { isLeavingForLogin, leaveForLogin } from '#lib/banto/logout.svelte.js';
	import { OWNER_CHANGE_POLICY, watchOwnerChanges } from '#lib/banto/ownerChange.js';
	import { isNavigationSettled } from '#lib/banto/navigationSettled.svelte.js';
	import { resolveAppPath } from '#lib/navigation.js';
	import Header from '#lib/components/Header.svelte';
	import SetupBanner from '#lib/components/SetupBanner.svelte';
	import Sidebar from '#lib/components/Sidebar.svelte';
	import ShellLauncher from '#lib/components/ShellLauncher.svelte';
	import { settings } from '#lib/settings.svelte.js';
	import { mobileNavStore } from '#lib/mobileNav.svelte.js';
	import { hasVisibleLayerAbove, LAYER_MARKER_ATTR } from '@banto/ui';
	import CommandPalette from '#lib/components/CommandPalette.svelte';
	import { commandPaletteStore } from '#lib/commandPalette.svelte.js';

	let { children, data } = $props();

	// banto v2.0.0 (Issue #260, design §6.1): the protected layout's three
	// wirings, copied from banto v2.0.0's admin-template
	// `routes/(app)/+layout.svelte` (v2 移行 PR1c). ChronoGazer 固有の差:
	// the notices are Japanese strings (no i18n here), there is no `base`
	// path, and the template's other shell features (unsaved-changes window
	// guard, command history owner, nav badges) are not part of this copy.
	// The narrow-width sidebar drawer was added with the menubar (#1159) and is
	// used only as the menubar's fallback (see `resolveShellNav`).
	//
	// A page belongs to the session it was built for. The page is shown only
	// while the session generation its loads confirmed
	// (`data.sessionGeneration`, from `+layout.ts`) is still the live one, and
	// is rebuilt from scratch (`{#key}`) once the next session's loads
	// complete - so nothing of the old user's page (in-memory state, unsaved
	// input) survives into the next session. A guard re-run that confirms the
	// same session keeps the generation, so an ordinary `refreshAll()`
	// never rebuilds the page.
	//
	// Wiring ①: whenever the controller's generation differs from the one
	// this page's load confirmed, re-run the loads (`refreshAll()`), which
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
	// (`#lib/banto/logout.svelte.ts`). `isLeavingForLogin()` is reactive, so a
	// generation change skipped meanwhile is handled once it ends.
	// banto #326（v4.0.0）: nor while a navigation is in flight (another tab's
	// login or a background revocation can land in the middle of one, and
	// this layout is mounted at the end of one). A `refreshAll()` started then
	// makes SvelteKit abort the navigation - the user's move is lost - and
	// skip `beforeNavigate` afterwards (`#lib/banto/navigationSettled.svelte.ts`).
	// `isNavigationSettled()` is reactive too: once the navigation completes,
	// this compares the generation ITS load confirmed and re-runs only if that
	// one is stale.
	const sessionController = getSessionController();
	let requestedFor = -1;
	$effect(() => {
		const generation = sessionController.snapshot.generation;
		if (isLeavingForLogin()) return;
		if (!isNavigationSettled()) return;
		if (generation !== data.sessionGeneration && requestedFor !== generation) {
			requestedFor = generation;
			void refreshAll();
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
				goToLogin: () => leaveForLogin(() => goto(resolveAppPath('/login')))
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

		// #1159: ドロワー（メニューバーの戻り先）が開いていれば Escape で閉じる（banto-hub の
		// `(app)/+layout.svelte` と同じ、`@banto/ui` の `layering.ts` の層の約束）。開いている
		// ドロワー自身も層（`data-esc-layer`、z-index 710）として数えられるので、**自分を
		// 「上位層」と誤認して永遠に譲らないよう** `except` に渡す（Drawer 900・パレット 1000 には
		// 譲り、それより下の層には譲らない）。要素は `Sidebar` から受け渡さず、マーカーで引く。
		if (event.key === 'Escape') {
			const sidebarEl = document.querySelector(`[${LAYER_MARKER_ATTR}='sidebar']`);
			if (event.defaultPrevented || hasVisibleLayerAbove({ except: sidebarEl })) return;
			if (mobileNavStore.open) {
				event.preventDefault();
				mobileNavStore.closeNav();
			}
		}
	}

	// ビューポート幅の監視を開始する（`watchViewport` の中の `untrack` が必須な理由は
	// `mobileNav.svelte.ts` のコメント参照）。
	$effect(() => {
		return mobileNavStore.watchViewport();
	});

	// #1159（banto v7.2.0 のメニューバー、nav-menubar-plan.md §4.9・D10-a）: ナビの置き場所
	// ごとにシェルが出すものは `mobileNavStore.view`（`resolveShellNav`）が決める。
	// メニューバーでは >900px にサイドバーは無く（ヘッダーがナビを持つ）、≤900px、または
	// ヘッダーのナビが「その他」にも入らないときだけサイドバーがドロワーになる
	// （`menubarInsufficient` は `Header` が `HeaderNav` から受けて store に置く）。ヘッダーが
	// バーを外す ≤900px・置き場所の切り替えではフラグを落とす（新しいバーが測り直して報告する）。
	const view = $derived(mobileNavStore.view);
	const launcher = $derived(settings.navLayout === 'launcher');
	const launcherPosition = $derived(settings.launcherPosition);
	$effect(() => {
		if (settings.navLayout !== 'menubar' || mobileNavStore.isNarrow) {
			mobileNavStore.setMenubarInsufficient(false);
		}
	});
	// ドロワーでなくなった（バーが戻った・ドックにした・広幅になった）ら、開いたままにしない。
	$effect(() => {
		if (!view.drawerMode) mobileNavStore.closeNav();
	});

	// トーストの積み位置はこのシェルの外（ルートの兄弟）にあるので、ドックのあいだだけルートに
	// 印を付け、banto.css がトーストをバーから避ける（nav-launcher-plan.md §4.1）。外れたら消す。
	$effect(() => {
		if (!launcher) return;
		const root = document.documentElement;
		root.setAttribute('data-banto-shell-nav', 'launcher');
		root.setAttribute('data-banto-shell-launcher-position', launcherPosition);
		return () => {
			root.removeAttribute('data-banto-shell-nav');
			root.removeAttribute('data-banto-shell-launcher-position');
		};
	});

	// ルート変更時はドロワーを必ず閉じる。
	afterNavigate(({ shallow }) => {
		if (shallow) return;

		mobileNavStore.closeNav();
	});
</script>

<svelte:window onkeydown={handleKeydown} />

<div
	class="shell"
	class:with-launcher={launcher}
	class:launcher-top={launcher && launcherPosition === 'top'}
	class:launcher-left={launcher && launcherPosition === 'left'}
	class:launcher-right={launcher && launcherPosition === 'right'}
>
	{#if mobileNavStore.drawerMode && mobileNavStore.open}
		<!--
			ドロワー（メニューバーの戻り先）のバックドロップ。z-index は Drawer/Modal(900)・
			CommandPalette/ToastHost 等(1000) より下の 700 に固定する - サイドバーは常設ナビであり、
			それらの一時的な最前面 UI の「下」にとどまるべきなので（banto-hub と同じ）。
		-->
		<button
			type="button"
			class="nav-backdrop"
			onclick={() => mobileNavStore.closeNavFromBackdrop()}
			aria-label="背景をクリックしてメニューを閉じる"
		></button>
	{/if}
	{#if launcher}
		<ShellLauncher />
	{:else if view.showSidebar}
		<Sidebar />
	{/if}
	<div class="main">
		<Header />
		<SetupBanner />
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

	/* ドック（nav-launcher-plan.md §4.1）: 固定のバーが本文の端を覆わないよう、置いた辺に
	   バーの分の余白を取る。上のバーはヘッダーの下に置く（バーをヘッダーの高さだけ下げる）。 */
	.shell.with-launcher main {
		padding-bottom: calc(1.25rem + var(--banto-shell-launcher-reserve));
	}

	.shell.launcher-top main,
	.shell.launcher-left main,
	.shell.launcher-right main {
		padding-bottom: 1.25rem;
	}

	.shell.launcher-top {
		--banto-shell-launcher-inset-top: var(--banto-shell-header-height);
	}

	.shell.launcher-top main {
		padding-top: calc(1.25rem + var(--banto-shell-launcher-reserve));
	}

	.shell.launcher-left main {
		padding-left: calc(1.25rem + var(--banto-shell-launcher-reserve));
	}

	.shell.launcher-right main {
		padding-right: calc(1.25rem + var(--banto-shell-launcher-reserve));
	}

	/* ≤900px はどの位置でも下の全幅タブバー（§4.6, D5）。 */
	@media (max-width: 900px) {
		.shell.with-launcher main,
		.shell.launcher-top main,
		.shell.launcher-left main,
		.shell.launcher-right main {
			padding: 1.25rem 1.25rem calc(1.25rem + var(--banto-shell-launcher-tabbar-reserve));
		}
	}

	/* ドロワーのバックドロップ。z-index は Drawer/Modal(900) より下の 700（サイドバー本体は 710）。 */
	.nav-backdrop {
		position: fixed;
		inset: 0;
		z-index: 700;
		display: block;
		width: 100%;
		border: none;
		margin: 0;
		padding: 0;
		background: rgba(0, 0, 0, 0.35);
		cursor: pointer;
	}
</style>
