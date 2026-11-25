<script lang="ts">
	// relay-wright の同名ファイルから複製。
	//
	// 差分: banto-hub は常に「LANブラウザ+組み込みサーバー」環境固定
	// （setup.ts の getBantoMode() は 'server' 固定）なので、relay-wright に
	// あった `showRemember = getBantoMode() === 'server'` という環境分岐は
	// 不要 - 「ログイン状態を保持」チェックは実装指示どおり常時表示する。
	import { goto } from '$app/navigation';
	import { getAuthProvider, notify } from '@banto/admin-core';
	import { bantoReady } from '#lib/banto/setup.js';
	import { isLoopbackOrigin } from '#lib/banto/loopbackOrigin.js';
	import { APP_NAME } from '#lib/appName.js';
	import { resolveAppPath } from '#lib/navigation.js';

	// status() が解決するまでは未確定: 一瞬でも片方のフォームを描画して
	// 出し直す「フラッシュ」を避けるため何も出さない。
	let mode: 'loading' | 'login' = $state('loading');

	let username = $state('');
	let password = $state('');
	let error: string | null = $state(null);
	let submitting = $state(false);
	let remember = $state(false);
	// #644 PR5: 同じ PC（loopback）の origin では Remember me を使わない（先取りした別プロセスが
	// localStorage を読めるため）。
	const loopback = isLoopbackOrigin();

	// banto v2.0.0（#260、design §6.1）: ログインが
	// `superseded`（別のログイン・ログアウトが先に確定し、このログインは何も
	// 保存しなかった）を返したら、エラーではなく通知して /status へ移る。保護
	// ガード（`(app)/+layout.ts`）が今保存されている資格情報で確定する。
	// admin-template の `routes/login/+page.svelte` と同じ扱い（文言も同じ）。
	const LOGIN_SUPERSEDED_MESSAGE = '別のセッションが先に確定しました。現在のセッションで開きます。';

	$effect(() => {
		void (async () => {
			await bantoReady;
			// banto v8: 初回セットアップの画面は無い（最初の管理者は `banto-hub create-admin`）。
			mode = 'login';
		})();
	});

	async function submitLogin(event: SubmitEvent) {
		event.preventDefault();
		error = null;
		submitting = true;
		try {
			const params: Record<string, unknown> = { username, password };
			if (remember && !loopback) params.remember = true;
			const result = await getAuthProvider().login(params);
			if (result.success) {
				goto(resolveAppPath('/status'));
			} else if (result.superseded) {
				notify('info', LOGIN_SUPERSEDED_MESSAGE);
				goto(resolveAppPath('/status'));
			} else {
				error = result.error ?? 'ログインに失敗しました';
			}
		} finally {
			submitting = false;
		}
	}
</script>

<div class="page">
	{#if mode === 'login'}
		<form onsubmit={submitLogin}>
			<h1>🏮 {APP_NAME}</h1>

			<label>
				ユーザー名
				<input type="text" bind:value={username} autocomplete="username" />
			</label>

			<label>
				パスワード
				<input type="password" bind:value={password} autocomplete="current-password" />
			</label>

			{#if loopback}
				<p class="remember-note">
					同じ PC
					からの接続では、ログイン状態を保持しません（#644）。タブを閉じると再ログインが必要です。
				</p>
			{:else}
				<label class="remember">
					<input type="checkbox" bind:checked={remember} />
					ログイン状態を保持する（30日間）
				</label>
			{/if}

			{#if error}
				<p class="error">{error}</p>
			{/if}

			<button type="submit" disabled={submitting}>ログイン</button>
		</form>
	{/if}
</div>

<style>
	.page {
		min-height: 100vh;
		display: grid;
		place-items: center;
	}

	form {
		display: flex;
		flex-direction: column;
		gap: 1rem;
		width: 320px;
		padding: 2rem;
		background: var(--banto-surface);
		border: 1px solid var(--banto-border);
		border-radius: calc(var(--banto-radius) * 2);
		backdrop-filter: var(--banto-backdrop, none);
		-webkit-backdrop-filter: var(--banto-backdrop, none);
	}

	h1 {
		margin: 0;
		text-align: center;
		font-size: 1.5rem;
	}

	label {
		display: flex;
		flex-direction: column;
		gap: 0.3rem;
		font-size: 0.875rem;
		color: var(--banto-text-muted);
	}

	input {
		padding: 0.5rem 0.6rem;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
		background: var(--banto-bg);
		color: var(--banto-text);
	}

	input:focus-visible {
		outline: none;
		box-shadow: var(--banto-focus-ring);
	}

	.remember-note {
		margin: 0;
		font-size: 0.85rem;
		opacity: 0.75;
	}

	.remember {
		flex-direction: row;
		align-items: center;
		gap: 0.4rem;
		cursor: pointer;
	}

	.remember input {
		padding: 0;
		width: auto;
	}

	.error {
		margin: 0;
		text-align: center;
		color: var(--banto-danger);
		font-size: 0.8rem;
	}

	button {
		padding: 0.55rem;
		border: none;
		border-radius: var(--banto-radius);
		background: var(--banto-primary);
		color: var(--banto-text-inverse);
		font-weight: 600;
		cursor: pointer;
	}

	button:hover:not(:disabled) {
		background: var(--banto-primary-hover);
	}

	button:disabled {
		opacity: 0.6;
		cursor: not-allowed;
	}

	:global([data-banto-preset='glass']) button {
		background: var(--banto-accent-gradient);
	}

	:global([data-banto-preset='glass']) button:hover:not(:disabled) {
		background: var(--banto-accent-gradient);
		filter: brightness(1.08);
	}
</style>
