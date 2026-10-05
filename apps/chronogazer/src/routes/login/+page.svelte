<script lang="ts">
	import { goto } from '$app/navigation';
	import { getAuthProvider, notify } from '@banto/admin-core';
	import { resolveAppPath } from '#lib/navigation.js';
	import { bantoReady, getBantoMode } from '#lib/banto/setup.js';

	// Undecided until `status()` resolves (or is absent, treated as
	// "already initialized" - see below): render nothing rather than
	// flashing one form then the other.
	let mode: 'loading' | 'setup' | 'login' = $state('loading');

	let username = $state('');
	let password = $state('');
	let displayName = $state('');
	let passwordConfirm = $state('');
	let error: string | null = $state(null);
	let submitting = $state(false);

	// "ログイン状態を保持する" (spec M11 "LAN Remember me"): only meaningful
	// for a LAN browser client (`createHttpAuthProvider` switches its token's
	// storage between sessionStorage/localStorage based on this flag). Inside
	// the Tauri webview a session already lives exactly as long as the window
	// does, and the plain-browser demo provider has no persistence story at
	// all - showing the checkbox there would offer a choice that does
	// nothing, so it's gated on `getBantoMode() === 'server'`. Set once
	// `bantoReady` resolves (below) - `getBantoMode()` reads the real
	// environment only after that probe finishes, same as `mode`.
	let showRemember = $state(false);
	let remember = $state(false);

	// banto v2.0.0（#260、design §6.1）: ログイン・初回セットアップが
	// `superseded`（別のログイン・ログアウトが先に確定し、このログインは何も
	// 保存しなかった）を返したら、エラーではなく通知して /monitor へ移る。保護
	// ガード（`(app)/+layout.ts`）が今保存されている資格情報で確定する。
	// admin-template の `routes/login/+page.svelte` と同じ扱い（文言も同じ）。
	// 「閲覧のみで続ける」（I2b、admin-template v3.0.0 の login と同じ、
	// ADR-0012）: LAN のブラウザ（`server` モード、`showRemember` と同じ条件）
	// だけ - Tauri の窓には LAN 向けの閲覧公開の面は無く（この窓の「ログイン
	// 不要」は M11 が受け持つ）、単体ブラウザのデモにはトークンを発行する
	// バックエンドが無い。`status()` が返ってから決める。
	let showContinueAsViewer = $state(false);

	const LOGIN_SUPERSEDED_MESSAGE = '別のセッションが先に確定しました。現在のセッションで開きます。';

	$effect(() => {
		void (async () => {
			await bantoReady; // provider selection (spec §11.1's three-way probe) must finish first
			showRemember = getBantoMode() === 'server';
			const status = await getAuthProvider().status?.();
			// No `status()` on this provider (an older/custom AuthProvider,
			// spec §3.3's members are optional for backward compatibility):
			// behave as if an account already exists, i.e. the normal login
			// form.
			mode = status && !status.initialized ? 'setup' : 'login';
			showContinueAsViewer = getBantoMode() === 'server' && status?.grants?.publicViewer === true;
		})();
	});

	// (app) のガードが閲覧者のセッションを自分で発行する（`+layout.ts` の
	// `grantFallback` → `enterGrant('publicViewer')`）ので、ここはガードの
	// かかる画面へ移るだけ。
	function continueAsViewer(): void {
		goto(resolveAppPath('/monitor'));
	}

	async function submitLogin(event: SubmitEvent) {
		event.preventDefault();
		error = null;
		submitting = true;
		try {
			const params: Record<string, unknown> = { username, password };
			// Only sent when the checkbox is actually shown (LAN browser mode -
			// see `showRemember` above); omitting it elsewhere keeps the wire
			// body identical to the pre-M11 shape.
			if (showRemember && remember) params.remember = true;
			const result = await getAuthProvider().login(params);
			if (result.success) {
				goto(resolveAppPath('/monitor'));
			} else if (result.superseded) {
				notify('info', LOGIN_SUPERSEDED_MESSAGE);
				goto(resolveAppPath('/monitor'));
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
				goto(resolveAppPath('/monitor'));
			} else if (result.superseded) {
				notify('info', LOGIN_SUPERSEDED_MESSAGE);
				goto(resolveAppPath('/monitor'));
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
			<h1>🏮 ChronoGazer</h1>
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
			<h1>🏮 ChronoGazer</h1>
			<p class="note">
				単体ブラウザ（デモ）モードは admin / admin
				でログインできます。Tauri/LANモードでは初回起動時に
				作成したアカウントでログインしてください。
			</p>

			<label>
				ユーザー名
				<input type="text" bind:value={username} autocomplete="username" />
			</label>

			<label>
				パスワード
				<input type="password" bind:value={password} autocomplete="current-password" />
			</label>

			{#if showRemember}
				<label class="remember">
					<input type="checkbox" bind:checked={remember} />
					ログイン状態を保持する（30日間）
				</label>
			{/if}

			{#if error}
				<p class="error">{error}</p>
			{/if}

			<button type="submit" disabled={submitting}>ログイン</button>

			{#if showContinueAsViewer}
				<button type="button" class="continue-as-viewer" onclick={continueAsViewer}>
					閲覧のみで続ける
				</button>
			{/if}
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
		/* Glass preset (spec M12): no-op under standard (--banto-backdrop: none). */
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

	/* Glass preset accent (spec M12): the login button gets the accent
	   gradient; hover brightens it instead of swapping to the flat hover
	   color (a gradient has no single hover counterpart). */
	:global([data-banto-preset='glass']) button {
		background: var(--banto-accent-gradient);
	}

	:global([data-banto-preset='glass']) button:hover:not(:disabled) {
		background: var(--banto-accent-gradient);
		filter: brightness(1.08);
	}

	/* 「閲覧のみで続ける」は主操作（ログイン）と並ぶ副操作なので、塗らずに
	   枠だけにする（admin-template では ghost ボタン）。ガラスのプリセットの
	   グラデーションより後に置いて、そちらでも枠だけにする。 */
	button.continue-as-viewer,
	:global([data-banto-preset='glass']) button.continue-as-viewer {
		background: none;
		border: 1px solid var(--banto-border);
		color: var(--banto-text);
		font-weight: 400;
	}

	button.continue-as-viewer:hover:not(:disabled),
	:global([data-banto-preset='glass']) button.continue-as-viewer:hover:not(:disabled) {
		background: color-mix(in srgb, var(--banto-primary) 8%, transparent);
	}
</style>
