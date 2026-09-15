<script lang="ts">
	/**
	 * 設定画面のシェル（#359 段階2）。上流テンプレート v1.6.0 の
	 * settings/+layout.svelte にならい、カテゴリナビ + 各カテゴリの
	 * ルート（`{@render children()}`）をここに集約する。
	 *
	 * banto-hub には template の `PageHeader` コンポーネントが無く、
	 * ページ見出しは既存どおり `Header.svelte` の `<h1>`（`pageTitle()`、
	 * `navigation.ts` 参照）が担っている（他の画面もページ内に別途
	 * タイトルを描画していない）ため、ここでは独自の見出しを新設せず
	 * カテゴリナビだけを追加する。
	 *
	 * `data.categories` は `+layout.ts` が現在のセッションで見える subset に
	 * 絞り込み済みなので、ナビには常にアクセス可能なカテゴリしか出ない
	 * （段階1の `{#if}` ガードと同じ「見せない」方針を実ルートに適用した
	 * もの）。
	 *
	 * PR #371 の Copilot レビュー是正: `hubStatusStore`（収集状態・MQTT
	 * 接続状態の共有ストア）の5秒ポーリングをここへ引き上げる。段階1では
	 * ConnectivitySection の admin 限定 `$effect` が担っていたが、
	 * カテゴリを実ルートに分割した結果 `/settings/data` へ直接遷移すると
	 * ConnectivitySection がマウントされずポーリングが始まらず、
	 * `hubStatusStore.collectionState` が `null` のまま = DataSection の
	 * 構成パッケージ import ガードが効かなくなっていた（詳細は
	 * `hubStatusStore.svelte.ts` の doc comment、上流 PR #198 の Copilot
	 * レビューにならった対処）。admin 限定という既存条件は
	 * ConnectivitySection の元の `$effect` から変えずに移す。
	 */
	import { page } from '$app/state';
	import { isAdmin } from '$lib/permissions';
	import { sessionStore } from '$lib/session.svelte';
	import { hubStatusStore } from './hubStatusStore.svelte';
	import type { SettingsCategory } from './categories';
	import type { LayoutProps } from './$types';
	import './settings.css';

	let { data, children }: LayoutProps = $props();

	/** Sidebar.svelte の `isActive` と同じ「自分自身 or 配下のパスなら active」判定。 */
	function isActive(category: SettingsCategory): boolean {
		return page.url.pathname === category.path || page.url.pathname.startsWith(category.path + '/');
	}

	// hubStatusStore の5秒ポーリング（admin 限定 - 旧 ConnectivitySection の
	// `$effect` と同じガード条件）。設定画面のどのカテゴリを開いていても
	// （connectivity/data 以外でも）常にマウントされている +layout.svelte
	// 側で行うことで、直接遷移でも `collectionState`/`mqttConnected` が
	// 必ず埋まるようにする。
	const canViewHubStatus = $derived(isAdmin(sessionStore.role));

	$effect(() => {
		if (!canViewHubStatus) return;
		const refresh = () => {
			void hubStatusStore.load().catch(() => {
				// 状態表示・import ガードの補助情報 - 取得失敗はエラー表示せず黙って保持する
				// （元 ConnectivitySection の `loadMqttStatus` と同じ方針）。
			});
		};
		refresh();
		const interval = setInterval(refresh, 5000);
		return () => clearInterval(interval);
	});
</script>

<div class="settings-layout">
	<nav class="section-nav" aria-label="設定のカテゴリ">
		{#each data.categories as category (category.id)}
			<a
				href={category.path}
				class:active={isActive(category)}
				aria-current={isActive(category) ? 'page' : undefined}
			>
				{category.label}
			</a>
		{/each}
	</nav>

	<div class="settings-content">
		{@render children()}
	</div>
</div>

<style>
	.settings-layout {
		display: flex;
		flex-direction: column;
		gap: 1rem;
	}

	/* 1024px 未満: コンテンツの上に横タブとして並べる（実装指示のとおり）。 */
	.section-nav {
		display: flex;
		flex-wrap: wrap;
		gap: 0.4rem;
	}

	.section-nav a {
		padding: 0.3rem 0.7rem;
		border: 1px solid var(--banto-border);
		border-radius: 999px;
		color: var(--banto-text-muted);
		font-size: 0.8rem;
		font-weight: 600;
		text-decoration: none;
	}

	.section-nav a:hover {
		background: color-mix(in srgb, var(--banto-primary) 8%, transparent);
		color: var(--banto-text);
	}

	.section-nav a.active {
		border-color: var(--banto-primary);
		color: var(--banto-primary);
		background: color-mix(in srgb, var(--banto-primary) 10%, transparent);
	}

	:global([data-banto-preset='glass']) .section-nav a.active {
		background: var(--banto-accent-gradient);
		color: var(--banto-text-inverse);
	}

	.settings-content {
		min-width: 0;
	}

	/* 1024px 以上: 左レール sticky、右にコンテンツ（実装指示のとおり）。
	   sticky の offset は Header.svelte の高さトークンに合わせる - レールは
	   固定ヘッダーの下をクリアしつつ本文だけがスクロールする
	   （Sidebar.svelte の `aside` と同じ考え方）。 */
	@media (min-width: 1024px) {
		.settings-layout {
			flex-direction: row;
			align-items: flex-start;
			gap: 1.5rem;
		}

		.section-nav {
			flex-direction: column;
			flex: 0 0 200px;
			gap: 0.25rem;
			position: sticky;
			top: calc(var(--banto-shell-header-height) + 1rem);
		}

		.settings-content {
			flex: 1;
		}
	}
</style>
