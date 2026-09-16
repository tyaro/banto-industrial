<script lang="ts">
	// relay-wright の同名コンポーネントから無改変で複製。
	import { tick, untrack } from 'svelte';
	import { page } from '$app/state';
	import { navItems, type NavItem } from '$lib/navigation';
	import { settings } from '$lib/settings.svelte';
	import { mobileNavStore } from '$lib/mobileNav.svelte';
	import { sessionStore } from '$lib/session.svelte';
	import { isAdmin } from '$lib/permissions';
	import { APP_NAME } from '$lib/appName';

	let { pendingCount = 0 }: { pendingCount?: number } = $props();

	let asideEl: HTMLElement | undefined = $state();

	// #359 段階2: `item.activeMatch`（無ければ `item.path`）を基準に前方一致
	// 判定する - `navigation.ts` の doc comment参照（`設定` は遷移先が
	// `/settings/appearance` でも `/settings` 配下ならハイライトさせたい）。
	function isActive(item: NavItem): boolean {
		const match = item.activeMatch ?? item.path;
		return page.url.pathname === match || page.url.pathname.startsWith(match + '/');
	}

	// RBAC: admin-only 項目は表示自体を隠す（無効化して見せない）。
	const visibleItems = $derived(
		navItems.filter((item) => !item.adminOnly || isAdmin(sessionStore.role))
	);

	// T19 S3-a（UX-43）: ≤900px では「アイコンのみ折り畳み」に意味が無いので
	// 適用しない - 狭幅では常にフルラベル表示にする。
	const collapsed = $derived(!mobileNavStore.isNarrow && settings.sidebarCollapsed);

	/**
	 * #381 レビュー対応12回目（層の約束・項目6、`escLayering.ts`）: **退避した
	 * 瞬間、中にフォーカスが残っていたらヘッダーの ☰ へ逃がす**（`SplitPane` の
	 * `focusFallback` と同じ役割）。オフキャンバスは閉じると `inert` になるので、
	 * 中の項目にフォーカスが残っていると `<body>` へ落ちる - 実際に踏むのは
	 * 「サイドバーの項目にフォーカス → コマンドパレットのナビ系コマンド →
	 * `goto()` は待たれないのでパレットは先に閉じてその項目へ戻し、あとから
	 * `afterNavigate` がサイドバーを畳む」という順序（パレット側の戻しだけでは
	 * 拾えない）。**遷移したときだけ**動かす（`mobileNavStore` は状態を
	 * オブジェクトごと差し替えるので、値が同じでも通知が飛ぶ）。
	 */
	/**
	 * #381 レビュー対応15回目: 開いているオフキャンバスを**層として名乗る**
	 * （`escLayering.ts::LAYER_MARKER_ATTR` = `data-esc-layer`）。`dialog`/`menu` を
	 * 名乗らない常設ナビなので、これが無いと下の層（退避ツリー 610）から
	 * 「上に層がある」と見えず、Esc が下から閉じてしまう。z-index は属性ではなく
	 * CSS の 710 を `effectiveZIndex` が読む。**属性名は定数と同じ文字列を
	 * マークアップへ直書きしている**（Svelte の属性名に定数を展開できないため -
	 * 変えるときは両方直す）。広幅・閉じている間は付けない。
	 */
	const escLayerMarker = $derived(
		mobileNavStore.isNarrow && mobileNavStore.open ? 'sidebar' : undefined
	);

	let navHiddenHandled = false;
	$effect.pre(() => {
		const hidden = mobileNavStore.isNarrow && !mobileNavStore.open;
		untrack(() => {
			if (hidden === navHiddenHandled) return;
			navHiddenHandled = hidden;
			if (!hidden) return;
			// **判定は `$effect.pre`（DOM 更新の前）で**: `inert` が付いた後だと
			// ブラウザが先にフォーカスを外して `<body>` に落としてしまい、「中に
			// フォーカスがあった」ことがもう分からない。移すのは更新後（`tick()`）。
			const active = document.activeElement;
			if (!(active instanceof HTMLElement) || !asideEl?.contains(active)) return;
			void tick().then(() => document.querySelector<HTMLElement>('header button')?.focus());
		});
	});

	// リンクをクリックしたらオフキャンバスを閉じる（設計の「閉じる契機」の
	// 1つ）。デスクトップ幅では isNarrow が false なので no-op。
	function handleNavClick(): void {
		if (mobileNavStore.isNarrow) mobileNavStore.closeNav();
	}
</script>

<!--
	#381 レビュー対応12回目（層の約束・項目6、`escLayering.ts`）: 狭幅で閉じている
	オフキャンバスは `inert`。`transform: translateX(-100%)` だけだと矩形も
	`visibility` も残り、**画面外のナビ項目がフォーカスを受けられてしまう**
	（`focusRestore.ts::canRestoreFocusTo` も「生きている」と判定する）。CSS 側でも
	`visibility: hidden` を併用する（`SplitPane` の退避ペインと同じ形）。
-->
<aside
	bind:this={asideEl}
	class:collapsed
	class:offcanvas={mobileNavStore.isNarrow}
	class:open={mobileNavStore.open}
	inert={mobileNavStore.isNarrow && !mobileNavStore.open}
	data-esc-layer={escLayerMarker}
>
	<div class="brand">
		<span class="brand-icon">🏮</span>
		{#if !collapsed}
			<span class="brand-name">{APP_NAME}</span>
		{/if}
	</div>

	<nav>
		{#each visibleItems as item (item.path)}
			<a
				href={item.path}
				class:active={isActive(item)}
				title={collapsed ? item.label : undefined}
				onclick={handleNavClick}
			>
				<span class="icon">{item.icon}</span>
				{#if !collapsed}
					<span class="nav-label">
						<span>{item.label}</span>
						{#if item.path === '/status' && pendingCount > 0}
							<span class="pending-badge">{pendingCount}</span>
						{/if}
					</span>
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
		/* T19 S3-a（UX-43）: ナビが長くなった場合に備え、サイドバー自身が
		   独自スクロール領域を持つ（本文と別にスクロールできる）。 */
		overflow-y: auto;
		background: var(--banto-surface);
		border-right: 1px solid var(--banto-border);
		transition: width 0.15s ease;
		backdrop-filter: var(--banto-backdrop, none);
		-webkit-backdrop-filter: var(--banto-backdrop, none);
	}

	aside.collapsed {
		width: var(--banto-shell-sidebar-width-collapsed);
	}

	/*
	 * T19 S3-a（UX-43、docs/banto-hub-t19-design.md §8.2）: ≤900px の
	 * オフキャンバス化。admin-template のパターン（オフキャンバス +
	 * オーバーレイ）に倣うが、上流コンポーネントの移植ではなく自前実装
	 * （§7.7 の通り上流にレイアウトコンポーネントは存在しない）。
	 *
	 * z-index=710 は Drawer/Modal(900)・CommandPalette/ToastHost/
	 * TreeContextMenu(1000) より下に固定する - サイドバーは常設ナビであり、
	 * ユーザーが明示的に開いた一時的な最前面 UI の手前に出て隠してはなら
	 * ない（バックドロップ=700 は `(app)/+layout.svelte` 側）。
	 */
	@media (max-width: 900px) {
		aside.offcanvas {
			position: fixed;
			inset-block: 0;
			left: 0;
			z-index: 710;
			width: min(var(--banto-shell-sidebar-width), 85vw);
			transform: translateX(-100%);
			/* #381 レビュー対応12回目: `visibility` も落とす（上の markup の
			   コメント参照 - transform だけだと退避中もフォーカス・判定に残る）。
			   `visibility` は離散なので、閉じるアニメーションの間は `visible` の
			   まま最後に切り替わる。 */
			visibility: hidden;
			transition:
				transform 0.2s ease,
				visibility 0.2s ease;
			box-shadow: 12px 0 32px rgba(0, 0, 0, 0.25);
		}

		aside.offcanvas.open {
			transform: translateX(0);
			visibility: visible;
		}
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

	:global([data-banto-preset='glass']) nav a.active {
		background: var(--banto-accent-gradient);
		color: var(--banto-text-inverse);
	}

	.icon {
		width: 1.25rem;
		text-align: center;
	}

	.nav-label {
		display: inline-flex;
		align-items: center;
		gap: 0.45rem;
		min-width: 0;
	}

	.pending-badge {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		min-width: 1.35rem;
		height: 1.35rem;
		padding: 0 0.4rem;
		border-radius: 999px;
		background: color-mix(in srgb, var(--banto-danger) 12%, transparent);
		color: var(--banto-danger);
		font-size: 0.72rem;
		font-weight: 700;
		line-height: 1;
	}
</style>
