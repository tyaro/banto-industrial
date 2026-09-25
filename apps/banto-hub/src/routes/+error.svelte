<script lang="ts">
	/**
	 * アプリ全体のエラー画面。banto v1.7.0 #204: `(app)/+layout.ts` のルート
	 * ガードが、サーバーがセッションを**照合できなかった**とき（照合の 500・
	 * 到達不能）にここへ来る。保存しているトークン（Remember me を含む）は
	 * 消していないので、「再試行」はガードをもう一度走らせるだけ - サーバーが
	 * 答えられるようになれば、そのままセッションが続く。ログイン画面へは
	 * 送らない（admin-template の `routes/+error.svelte` と同じ考え方）。
	 */
	import { page } from '$app/state';

	function retry() {
		// 全体を読み直すと、ガードを含むすべての load が最初から走り直す。
		location.reload();
	}
</script>

<div class="error-page" role="alert">
	<div class="card">
		<h1>エラーが発生しました</h1>
		<p class="status">{page.status}</p>
		<p class="message">{page.error?.message ?? ''}</p>
		<div class="actions">
			<button type="button" onclick={retry}>再試行</button>
			<a href="/">トップへ戻る</a>
		</div>
	</div>
</div>

<style>
	.error-page {
		min-height: 100vh;
		display: grid;
		place-items: center;
		padding: 1.5rem;
	}

	.card {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
		max-width: 28rem;
		padding: 2rem;
		background: var(--banto-surface);
		border: 1px solid var(--banto-border);
		border-radius: calc(var(--banto-radius) * 2);
	}

	h1 {
		margin: 0;
		font-size: 1.25rem;
	}

	.status {
		margin: 0;
		color: var(--banto-text-muted);
		font-size: 0.8rem;
	}

	.message {
		margin: 0;
		color: var(--banto-text-muted);
	}

	.actions {
		display: flex;
		align-items: center;
		gap: 1rem;
	}

	button {
		padding: 0.5rem 1rem;
		border: none;
		border-radius: var(--banto-radius);
		background: var(--banto-primary);
		color: var(--banto-text-inverse);
		font-weight: 600;
		cursor: pointer;
	}

	button:hover {
		background: var(--banto-primary-hover);
	}

	a {
		color: var(--banto-text-muted);
	}
</style>
