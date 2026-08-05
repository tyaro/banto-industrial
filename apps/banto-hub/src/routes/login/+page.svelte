<script lang="ts">
	// relay-wright の同名ファイルから複製。
	//
	// 差分: banto-hub は常に「LANブラウザ+組み込みサーバー」環境固定
	// （setup.ts の getBantoMode() は 'server' 固定）なので、relay-wright に
	// あった `showRemember = getBantoMode() === 'server'` という環境分岐は
	// 不要 - 「ログイン状態を保持」チェックは実装指示どおり常時表示する。
	import { goto } from '$app/navigation';
	import { getAuthProvider } from '@banto/admin-core';
	import { bantoReady } from '$lib/banto/setup';
	import { APP_NAME } from '$lib/appName';

	// status() が解決するまでは未確定: 一瞬でも片方のフォームを描画して
	// 出し直す「フラッシュ」を避けるため何も出さない。
	let mode: 'loading' | 'setup' | 'login' = $state('loading');

	let username = $state('');
	let password = $state('');
	let displayName = $state('');
	let passwordConfirm = $state('');
	let error: string | null = $state(null);
	let submitting = $state(false);
	let remember = $state(false);

	$effect(() => {
		void (async () => {
			await bantoReady;
			const status = await getAuthProvider().status?.();
			// status() が無い AuthProvider（古い/独自実装）: 通常のログイン
			// フォームとして振る舞う。
			mode = status && !status.initialized ? 'setup' : 'login';
		})();
	});

	async function submitLogin(event: SubmitEvent) {
		event.preventDefault();
		error = null;
		submitting = true;
		try {
			const params: Record<string, unknown> = { username, password };
			if (remember) params.remember = true;
			const result = await getAuthProvider().login(params);
			if (result.success) {
				goto('/status');
			} else {
				error = result.error ?? 'ログインに失敗しました';
			}
		} finally {
			submitting = false;
		}
	}

	async function submitSetup(event: SubmitEvent) {
		event.preventDefault();
		error = null;

		if (password.length < 8) {
			error = 'パスワードは8文字以上で入力してください';
			return;
		}
		if (password !== passwordConfirm) {
			error = 'パスワードが一致しません';
			return;
		}

		submitting = true;
		try {
			const setup = getAuthProvider().setup;
			if (!setup) {
				error = 'この環境では初期セットアップに対応していません';
				return;
			}
			const result = await setup({ username, password, displayName });
			if (result.success) {
				goto('/status');
			} else {
				error = result.error ?? 'セットアップに失敗しました';
			}
		} finally {
			submitting = false;
		}
	}
</script>

<div class="page">
	{#if mode === 'setup'}
		<form onsubmit={submitSetup}>
			<h1>🏮 {APP_NAME}</h1>
			<p class="note">初回起動です。管理者アカウントを作成してください。</p>

			<label>
				表示名
				<input type="text" bind:value={displayName} autocomplete="name" />
			</label>

			<label>
				ユーザー名
				<input type="text" bind:value={username} autocomplete="username" />
			</label>

			<label>
				パスワード（8文字以上）
				<input type="password" bind:value={password} autocomplete="new-password" />
			</label>

			<label>
				パスワード（確認）
				<input type="password" bind:value={passwordConfirm} autocomplete="new-password" />
			</label>

			{#if error}
				<p class="error">{error}</p>
			{/if}

			<button type="submit" disabled={submitting}>アカウントを作成</button>
		</form>
	{:else if mode === 'login'}
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

			<label class="remember">
				<input type="checkbox" bind:checked={remember} />
				ログイン状態を保持する（30日間）
			</label>

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

	.note {
		margin: 0;
		text-align: center;
		color: var(--banto-text-muted);
		font-size: 0.8rem;
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
