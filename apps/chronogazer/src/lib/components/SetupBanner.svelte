<script lang="ts">
	/**
	 * 最初の管理者がまだいないことの案内（#392 B2-2、2026-10-14 オーナー決定 1）。
	 *
	 * サービスの host（`chronogazer-server`）は、未初期化の間この PC からの接続
	 * （`localDesktop`）に admin を出すので、ログインなしで監視画面まで入れる。その間は
	 * 画面の上に「管理者のアカウントがまだありません」と出し、初回セットアップ
	 * （`/login` の作成フォーム。ガードの外なので grant のセッションのまま開ける）へ
	 * 誘う。`localDesktop` のセッションのときだけ `status()` を 1 回読み（`(app)` の
	 * レイアウトはログイン画面から戻るたびに作り直されるので、作成の後は出ない）、
	 * 読めなければ出さない。
	 */
	import { getAuthProvider, type GrantStatus } from '@banto/admin-core';
	import { needsFirstAdmin } from '#lib/banto/grantPolicy.js';
	import { resolveAppPath } from '#lib/navigation.js';
	import { sessionStore } from '#lib/session.svelte.js';

	let status: GrantStatus | undefined = $state(undefined);

	$effect(() => {
		if (!sessionStore.localDesktop) return;
		let cancelled = false;
		void (async () => {
			try {
				const read = await getAuthProvider().status?.();
				if (!cancelled) status = read;
			} catch {
				// 読めなければ案内を出さない（案内は補助で、境界はサーバー）。
			}
		})();
		return () => {
			cancelled = true;
		};
	});

	const shown = $derived(needsFirstAdmin(sessionStore.localDesktop, status));
</script>

{#if shown}
	<div class="setup-banner" role="status">
		<span>
			管理者のアカウントがまだありません。この PC
			からはログインなしで使えていますが、本番で使う前に管理者を作成してください。
		</span>
		<a href={resolveAppPath('/login')}>管理者を作成する</a>
	</div>
{/if}

<style>
	.setup-banner {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.5rem 1rem;
		padding: 0.5rem 1rem;
		background: var(--banto-warning-tint);
		border-bottom: 1px solid var(--banto-border);
		color: var(--banto-warning-tint-text);
		font-size: 0.85rem;
	}

	.setup-banner a {
		color: inherit;
		font-weight: 600;
	}
</style>
