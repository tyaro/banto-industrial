<script lang="ts">
	/**
	 * アプリ全体のエラー画面。banto v1.7.0 #204: `(app)/+layout.ts` のルート
	 * ガードが、サーバーがセッションを**照合できなかった**とき（照合の 500・
	 * 到達不能）にここへ来る。保存しているトークン（Remember me を含む）は
	 * 消していないので、「再試行」はガードをもう一度走らせるだけ - サーバーが
	 * 答えられるようになれば、そのままセッションが続く。ログイン画面へは
	 * 送らない（admin-template の `routes/+error.svelte` と同じ考え方）。
	 *
	 * banto v2.0.0（#260 実装-3、design §6.1、S-81、I-24）: 「再試行」は
	 * `location.reload()` ではなく、この文書の中で load を走らせ直す
	 * `refreshAll()`（v4.0.0 まで `invalidateAll()`）。SessionController を保ったまま再試行するので、この
	 * 画面の間に確定したユーザーの変更（`pendingOwnerChange`）は、保護
	 * レイアウトが再び mount したときに通知される。ページ全体の再読み込み
	 * （ブラウザの再読み込み）では controller が作り直され、その記録は残らない
	 * （admin-template の同名ファイルと同じ）。
	 */
	import { refreshAll } from '$app/navigation';
	import { page } from '$app/state';
	import { resolveAppPath } from '#lib/navigation.js';

	// 再試行の間も無効にしない: 返ってこない再試行が出口まで塞がないように
	// （もう一度押せば新しい再試行が始まる。ブラウザの再読み込みも使える）。
	function retry(): void {
		void refreshAll();
	}
</script>

<div class="error-page" role="alert">
	<div class="card">
		<h1>エラーが発生しました</h1>
		<p class="status">{page.status}</p>
		<p class="message">{page.error?.message ?? ''}</p>
		<div class="actions">
			<button type="button" onclick={retry}>再試行</button>
			<a href={resolveAppPath('/')}>トップへ戻る</a>
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
