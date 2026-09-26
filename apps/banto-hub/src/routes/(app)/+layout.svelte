<script lang="ts">
	// relay-wright の同名ファイルから無改変で複製。
	// T19 S1-d（UX-45、docs/banto-hub-t19-design.md §3.6、2026-09-03）:
	// `CommissioningBanner`（試運転モードの常時表示バナー）を撤去した
	// （2026-09-02 オーナー決定「常時表示しない」）。安全性は損なわれない -
	// 試運転モード中は非 loopback バインドが構造的に拒否される
	// （`enforce_loopback_when_commissioning`）ため、無認証のまま外部
	// ネットワークへ露出することはない。状態を知る手段は
	// `status/+page.svelte` の「サーバー状態」に事実として残した。
	import { afterNavigate, invalidateAll } from '$app/navigation';
	import { onSessionEnded } from '@banto/admin-core';
	import Header from '$lib/components/Header.svelte';
	import Sidebar from '$lib/components/Sidebar.svelte';
	import CommandPalette from '$lib/components/CommandPalette.svelte';
	import {
		hasVisibleLayerAbove,
		hasVisibleMenuLayer,
		LAYER_MARKER_ATTR
	} from '$lib/components/escLayering';
	import { listPendingChanges } from '$lib/banto/pendingChangesAdmin';
	import { countUnappliedPendingChanges } from '$lib/banto/pendingUnappliedCount';
	import { commandPaletteStore } from '$lib/commandPalette.svelte';
	import { mobileNavStore } from '$lib/mobileNav.svelte';
	import { isAdmin } from '$lib/permissions';
	import { sessionStore } from '$lib/session.svelte';

	let { children } = $props();
	let pendingCount = $state(0);

	const POLL_INTERVAL_MS = 3000;
	const hubAdmin = $derived(isAdmin(sessionStore.role));

	function handleKeydown(event: KeyboardEvent): void {
		if (event.key.toLowerCase() === 'k' && (event.ctrlKey || event.metaKey)) {
			event.preventDefault();
			// #381 レビュー対応14回目（層の約束・項目5の補足、`escLayering.ts`）:
			// **コマンドパレットとコンテキストメニューは同じ z（1000）**なので同時に
			// 出さない。同 z は z 順の判定で区別できず、出してしまうと Esc も
			// フォーカスの引き戻しも互いに譲り合って効かなくなる。メニューは
			// 一過性（Esc・外クリック・フォーカスが外れるで閉じる）なので、
			// 開いているあいだは**パレットを開かない**側に倒した - レイアウトから
			// ページのメニューを閉じる口が無いため（閉じてから `Ctrl+K`）。
			// 既に開いているパレットを閉じる方向のトグルは妨げない。
			if (!commandPaletteStore.open && hasVisibleMenuLayer()) return;
			commandPaletteStore.toggle();
		}

		// T19 S3-a（UX-43）: オフキャンバスが開いていれば Escape で閉じる。
		//
		// #381 レビュー対応（層の約束 - **正は `escLayering.ts` の doc**）:
		// サイドバー（z-index 710）は、より手前の層（Drawer/Modal 900、
		// コマンドパレット・コンテキストメニュー 1000）には譲り、自分より下の層
		// （タグ画面の退避ツリー 610、`SplitPane.svelte`）には**閉じたことを
		// `preventDefault` で知らせる**。サイドバーは `role="dialog"` 等を
		// 名乗らない常設ナビで `SplitPane` 側のセレクタからは見えないため
		// （同部品は banto-hub の DOM を知らないアプリ非依存の規約）、この約束を
		// 守るのはこちらの責務。
		if (event.key === 'Escape') {
			// #381 レビュー対応15回目: オフキャンバスが開いているあいだ、サイドバー
			// 自身も層（`data-esc-layer`、z-index 710）として数えられるので、
			// **自分を「上位層」と誤認して永遠に譲らないよう** `except` に渡す
			// （Drawer 900・パレット 1000 には譲り、退避ツリー 610 には譲らない）。
			// 要素は `Sidebar` から受け渡さず、マーカーで引く（`Sidebar` は開いて
			// いるときだけこの属性を出す）。
			const sidebarEl = document.querySelector(`[${LAYER_MARKER_ATTR}='sidebar']`);
			if (event.defaultPrevented || hasVisibleLayerAbove({ except: sidebarEl })) return;
			if (mobileNavStore.open) {
				event.preventDefault();
				mobileNavStore.closeNav();
			}
		}
	}

	// T19 S3-a（UX-43）: ビューポート幅の監視を開始する。$effect のクリーン
	// アップで購読解除する（`viewportWatch.ts` は SSR/matchMedia 未実装
	// 環境でもガードされている）。`mobileNavStore.watchViewport()` 内部で
	// `untrack` している理由は mobileNav.svelte.ts のコメント参照
	// （untrack 無しだと effect_update_depth_exceeded で描画が壊れる罠を
	// 実機で踏んだ）。
	$effect(() => {
		return mobileNavStore.watchViewport();
	});

	// banto v1.7.2（tyaro/banto#241）: 開いている画面のセッションが裏で
	// 失効したら（削除・降格・パスワードの変更/リセット）、ルートガード
	// （`+layout.ts`）を走らせ直してログイン画面へ移る。失効に気づくのは
	// admin-core の SSE（`/api/events`、`setup.ts` の `connectEvents`）で、
	// `check()` で確認できたときだけ知らせる。タグモニタのストリームの
	// 経路（#441 / #445、`sessionRecheck.ts`）とは独立に、どの画面でも効く。
	//
	// `recheckSessionAfterStreamClose`（single-flight）には合流させない:
	// 飛行中の確認はこの知らせより前に始まっており、その答えは失効より前の
	// 判断のことがある（banto の `sessionEnded.ts` の「Ordering」と同じ理由）。
	// SvelteKit の `invalidateAll()` は重なると後の呼び出しが勝つので、
	// ガードは必ずこの知らせの後の状態で判断する。先の呼び出しは結果を捨てて
	// 解決するので、モニタは購読を再開しようとする。この知らせは `check()` が
	// トークンを消した後に届くので、再開は `token_cleared` で止まって確認へ
	// 戻り、最後に走るガードがトークン無しで `/login` へ送る（画面が外れれば
	// `disconnect()` で止まる）。
	$effect(() => onSessionEnded(() => void invalidateAll()));

	// ルート変更時はオフキャンバスを必ず閉じる（設計の「閉じる契機」の1つ）。
	afterNavigate(() => {
		mobileNavStore.closeNav();
	});

	$effect(() => {
		if (!hubAdmin) {
			pendingCount = 0;
			return;
		}

		let cancelled = false;

		async function pollPendingCount(): Promise<void> {
			try {
				const pendingChanges = await listPendingChanges();
				if (!cancelled) {
					// 実機で発見された不具合(2026-08-31、オーナー報告): 全件を
					// 数えると applied/canceled/failed も「未適用」に含めてし
					// まう（4件中3件キャンセル済み・1件適用済みなのに「未適用
					// 4件」と表示された）。未適用として数える state の判断は
					// `pendingUnappliedCount.ts` を参照。
					pendingCount = countUnappliedPendingChanges(pendingChanges);
				}
			} catch {
				// 常時表示用の補助ポーリングなので、失敗時は静かに無視する。
			}
		}

		void pollPendingCount();
		const timer = setInterval(() => void pollPendingCount(), POLL_INTERVAL_MS);

		return () => {
			cancelled = true;
			clearInterval(timer);
		};
	});
</script>

<svelte:window onkeydown={handleKeydown} />

<div class="shell">
	{#if mobileNavStore.isNarrow && mobileNavStore.open}
		<!--
			T19 S3-a（UX-43）: ≤900px オフキャンバスのバックドロップ。z-index は
			Drawer/Modal(900)・CommandPalette/ToastHost/TreeContextMenu(1000)
			より下の 700 に固定する — サイドバーは常設ナビであり、Drawer や
			CommandPalette 等の一時的な最前面 UI の「下」にとどまるべきなので、
			それらが開いていればオフキャンバスより手前に表示され続ける。
		-->
		<button
			type="button"
			class="nav-backdrop"
			onclick={() => mobileNavStore.closeNav()}
			aria-label="背景をクリックしてメニューを閉じる"
		></button>
	{/if}
	<Sidebar {pendingCount} />
	<div class="main">
		<Header {pendingCount} />
		<main>
			{@render children()}
		</main>
	</div>
</div>

{#if commandPaletteStore.open}
	<CommandPalette />
{/if}

<style>
	/*
	 * T19 S3-a（UX-43、docs/banto-hub-t19-design.md §8.2）: 真の固定。
	 * ページ全体をスクロールさせるのではなく、シェルをビューポート全高に
	 * 固定し（`overflow: hidden`）、ヘッダーは縮まない固定領域、本文
	 * （`main`）だけが独自スクロール領域を持つ。position: fixed は使わない
	 * - flex + 専用スクロール領域のほうが破綻しにくい（§8.2）。
	 * `100dvh` はモバイルのアドレスバー増減に追従する動的ビューポート単位。
	 * 未対応ブラウザ向けに `100vh` を先に書いてフォールバックする。
	 */
	.shell {
		display: flex;
		height: 100vh;
		height: 100dvh;
		overflow: hidden;
	}

	.main {
		flex: 1;
		display: flex;
		flex-direction: column;
		min-width: 0;
		height: 100%;
	}

	main {
		flex: 1;
		min-height: 0;
		overflow-y: auto;
		padding: 1.25rem;
	}

	/*
	 * T19 S3-a（UX-43）: ≤900px オフキャンバスのバックドロップ。z-index は
	 * Drawer/Modal(900) より下の 700 に固定する（サイドバー本体は 710、
	 * Sidebar.svelte 参照）。理由: サイドバーは常設ナビであり、ユーザーが
	 * 明示的に開いた Drawer/Modal/CommandPalette 等の一時的な最前面 UI より
	 * 手前に出てそれらを隠してはならない。
	 */
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
