<script lang="ts">
	/**
	 * デモ（GitHub Pages、docs/demo-pages-design.md §3.4・2026-11-23 決定）で、対象外の画面へ
	 * アプリの中から着いたとき（コマンドパレット・ブックマークの履歴・画面内のリンク）の案内。
	 * その画面の中身（children）は描画しない - 描けば、その画面が模擬に無い `/api` を呼ぶ。
	 * 対象外の画面を直に開く・読み直すのは、静的な配信の 404.html が受ける。
	 */
	import { DEMO_NAV_PATHS, navItems, resolveAppPath } from '#lib/navigation.js';

	const targets = DEMO_NAV_PATHS.map((path) => ({
		path,
		label: navItems.find((item) => item.path === path)?.label ?? path
	}));
</script>

<section class="demo-out-of-scope" aria-labelledby="demo-out-of-scope-title">
	<h2 id="demo-out-of-scope-title">この画面はデモでは使えません</h2>
	<p>デモで触れるのは次の画面です。</p>
	<ul>
		{#each targets as target (target.path)}
			<li><a href={resolveAppPath(target.path)}>{target.label}（{target.path}）</a></li>
		{/each}
	</ul>
</section>

<style>
	.demo-out-of-scope {
		max-width: 36rem;
		margin: 2rem auto;
		padding: 1.5rem;
		border: 1px solid var(--banto-border);
		border-radius: 0.5rem;
		background: var(--banto-surface);
	}

	h2 {
		margin: 0 0 0.75rem;
		font-size: 1.1rem;
	}

	p {
		margin: 0 0 0.5rem;
	}

	ul {
		margin: 0;
		padding-left: 1.25rem;
	}
</style>
