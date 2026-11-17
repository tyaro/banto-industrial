<script lang="ts">
	// #1159（banto v7.2.0 のメニューバー）: メニューバー表示のとき、≤900px、またはバーが
	// 入りきらないときだけ描かれ（描くかは `(app)/+layout.svelte`）、ドロワー（オフキャンバス）に
	// なる条件は `mobileNavStore.drawerMode`（banto-hub の `Sidebar.svelte` と同じ形）。
	// ドロワーは折り畳みを持たない。サイドバー表示は従来どおり（広幅の列・折り畳み）。
	import { tick, untrack } from 'svelte';
	import { page } from '$app/state';
	import {
		isNavItemActive,
		resolveAppPath,
		visibleNavItems,
		type NavItem
	} from '#lib/navigation.js';
	import { settings } from '#lib/settings.svelte.js';
	import { mobileNavStore } from '#lib/mobileNav.svelte.js';
	import {
		NAV_TOGGLE_ATTR,
		SIDEBAR_ID,
		findVisibleNavToggle,
		toggleSidebarWithFocus
	} from '#lib/sidebarToggle.js';
	import { sessionStore } from '#lib/session.svelte.js';

	// #359 chronogazer 分: `item.activeMatch`（無ければ `item.path`）を基準に
	// 前方一致判定する - `navigation.ts` の doc comment参照（`設定` は
	// 遷移先が `/settings/appearance` でも `/settings` 配下ならハイライト
	// させたい）。
	function isActive(item: NavItem): boolean {
		return isNavItemActive(item, page.url.pathname);
	}

	// Spec M10 RBAC: hide admin-only entries (「ユーザー管理」) rather than
	// showing them disabled - navigation-level hiding, same as
	// routes/(app)/users/+page.ts redirecting a non-admin instead of
	// rendering a 403 screen.
	//
	// I2b（admin-template v3.0.0 の `Sidebar.svelte` と同じ、ADR-0012）: LAN の
	// 閲覧公開のセッションは `publicViewer` の許可リストだけを見る。役割に
	// よらない（閲覧公開の役割は常に `viewer` なので admin の項目はどのみち
	// 出ないが、明示しておけば将来それが変わっても正しい）。
	const visibleItems = $derived(
		visibleNavItems({ role: sessionStore.role, publicViewer: sessionStore.publicViewer })
	);

	// ドロワーでは「アイコンのみ折り畳み」に意味が無いので適用しない（常にフルラベル）。
	const collapsed = $derived(!mobileNavStore.drawerMode && settings.sidebarCollapsed);

	let asideEl: HTMLElement | undefined = $state();

	// 開いているドロワーを**層として名乗る**（`@banto/ui` の `layering.ts::LAYER_MARKER_ATTR` =
	// `data-esc-layer`、banto-hub の `Sidebar.svelte` と同じ）。`dialog`/`menu` を名乗らない
	// 常設ナビなので、これが無いと下の層から「上に層がある」と見えず Esc が下から閉じてしまう。
	// 属性名は定数と同じ文字列をマークアップへ直書きしている（変えるときは両方直す）。
	const escLayerMarker = $derived(
		mobileNavStore.drawerMode && mobileNavStore.open ? 'sidebar' : undefined
	);

	// ドロワーが閉じて `inert` になる瞬間、中にフォーカスが残っていたら見えている ☰ へ逃がす。
	// 判定は `$effect.pre`（DOM 更新の前）: `inert` が付いた後だとブラウザが先にフォーカスを外して
	// `<body>` に落とし、「中にあった」ことが分からなくなる。**遷移したときだけ**動かす。
	let navHiddenHandled = false;
	$effect.pre(() => {
		const hidden = mobileNavStore.drawerMode && !mobileNavStore.open;
		untrack(() => {
			if (hidden === navHiddenHandled) return;
			navHiddenHandled = hidden;
			if (!hidden) return;
			const active = document.activeElement;
			if (!(active instanceof HTMLElement) || !asideEl?.contains(active)) return;
			void tick().then(() => findVisibleNavToggle()?.focus());
		});
	});

	// リンクをクリックしたらドロワーを閉じる。ドロワーでなければ no-op。
	function handleNavClick(): void {
		if (mobileNavStore.drawerMode) mobileNavStore.closeNav();
	}
</script>

<!-- 閉じているドロワーは `inert`（画面外のナビ項目がフォーカスを受けないように）。CSS 側でも
     `visibility: hidden` を併用する。 -->
<aside
	id={SIDEBAR_ID}
	bind:this={asideEl}
	class:collapsed
	class:offcanvas={mobileNavStore.drawerMode}
	class:open={mobileNavStore.open}
	inert={mobileNavStore.drawerMode && !mobileNavStore.open}
	data-esc-layer={escLayerMarker}
>
	<div class="brand">
		<span class="brand-icon">🏮</span>
		{#if !collapsed}
			<span class="brand-name">ChronoGazer</span>
			<!-- banto-hub #714: 開いている間の折り畳みボタン（ロゴ行）。折り畳み中はヘッダーの ☰ が開く。ドロワーには出さない。 -->
			{#if !mobileNavStore.drawerMode}
				<button
					type="button"
					class="collapse-toggle"
					onclick={() => toggleSidebarWithFocus()}
					aria-label="サイドバーを折りたたむ"
					aria-expanded={true}
					aria-controls={SIDEBAR_ID}
					{...{ [NAV_TOGGLE_ATTR]: '' }}
				>
					☰
				</button>
			{/if}
		{/if}
	</div>

	<nav>
		{#each visibleItems as item (item.path)}
			<a
				href={resolveAppPath(item.path)}
				class:active={isActive(item)}
				title={collapsed ? item.label : undefined}
				onclick={handleNavClick}
			>
				<span class="icon">{item.icon}</span>
				{#if !collapsed}
					<span>{item.label}</span>
				{/if}
			</a>
		{/each}
	</nav>
</aside>

<style>
	aside {
		width: var(--banto-shell-sidebar-width);
		flex-shrink: 0;
		display: flex;
		flex-direction: column;
		background: var(--banto-surface);
		border-right: 1px solid var(--banto-border);
		transition: width 0.15s ease;
		/* Glass preset (spec M12): no-op under standard (--banto-backdrop: none). */
		backdrop-filter: var(--banto-backdrop, none);
		-webkit-backdrop-filter: var(--banto-backdrop, none);
	}

	aside.collapsed {
		width: var(--banto-shell-sidebar-width-collapsed);
	}

	/* ドロワー（`mobileNavStore.drawerMode`: メニューバーが戻る先、≤900px または入りきらない
	   >900px）。幅のメディアクエリには頼らない（banto-hub の `Sidebar.svelte` と同じ）。
	   z-index は Drawer/Modal(900)・コマンドパレット等(1000) より下、バックドロップ(700)より上。 */
	aside.offcanvas {
		position: fixed;
		inset-block: 0;
		left: 0;
		z-index: 710;
		width: min(var(--banto-shell-sidebar-width), 85vw);
		transform: translateX(-100%);
		/* `visibility` も落とす（transform だけだと退避中もフォーカス・判定に残る）。閉じる
		   アニメーションの間は `visible` のまま最後に切り替わる。 */
		visibility: hidden;
		transition:
			transform 0.2s ease,
			visibility 0.2s ease;
		box-shadow: 12px 0 32px rgba(0, 0, 0, 0.25);
	}

	/* 開くときは `visibility` を遷移させず即座に `visible` にする（banto-hub #399: 遷移させると
	   開いた直後の最初のフレームが `hidden` のままで、層の判定・Esc が噛み合わなくなる）。 */
	aside.offcanvas.open {
		transform: translateX(0);
		visibility: visible;
		transition:
			transform 0.2s ease,
			visibility 0s;
	}

	.brand {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		height: var(--banto-shell-header-height);
		padding: 0 0.9rem;
		border-bottom: 1px solid var(--banto-border);
		font-weight: 700;
	}

	/* banto-hub #714: ロゴ行の右端に寄せる折り畳みボタン（Header の .icon-button と同じ見た目）。 */
	.collapse-toggle {
		margin-left: auto;
		border: none;
		background: none;
		color: var(--banto-text-muted);
		padding: 0.35rem 0.5rem;
		border-radius: var(--banto-radius);
		cursor: pointer;
		font-size: 0.875rem;
		font-weight: 400;
	}

	.collapse-toggle:hover {
		background: color-mix(in srgb, var(--banto-primary) 8%, transparent);
		color: var(--banto-text);
	}

	nav {
		display: flex;
		flex-direction: column;
		padding: 0.5rem;
		gap: 2px;
	}

	nav a {
		display: flex;
		align-items: center;
		gap: 0.6rem;
		padding: 0.5rem 0.6rem;
		border-radius: var(--banto-radius);
		color: var(--banto-text-muted);
		text-decoration: none;
		white-space: nowrap;
	}

	nav a:hover {
		background: color-mix(in srgb, var(--banto-primary) 8%, transparent);
		color: var(--banto-text);
	}

	nav a.active {
		background: color-mix(in srgb, var(--banto-primary) 14%, transparent);
		color: var(--banto-primary);
		font-weight: 600;
	}

	/* Glass preset accent (spec M12): the active nav item gets the accent
	   gradient. Scoped by the preset attribute so standard keeps the flat
	   tint above untouched. */
	:global([data-banto-preset='glass']) nav a.active {
		background: var(--banto-accent-gradient);
		color: var(--banto-text-inverse);
	}

	.icon {
		width: 1.25rem;
		text-align: center;
	}
</style>
