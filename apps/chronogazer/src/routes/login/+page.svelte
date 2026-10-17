<script lang="ts">
	import { goto } from '$app/navigation';
	import { getAuthProvider, getSessionController, notify } from '@banto/admin-core';
	import { resolveAppPath } from '#lib/navigation.js';
	import { bantoReady, getBantoMode } from '#lib/banto/setup.js';
	import {
		enteredKind,
		enterPublicViewer,
		enterThisPc,
		LOCAL_DESKTOP_GRANT_KIND,
		loginEntries,
		PUBLIC_VIEWER_GRANT_KIND
	} from '#lib/banto/grantPolicy.js';

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

	// #392 B2-2（2026-10-14 オーナー決定 3・7）: サービスの host（`chronogazer-server`）の
	// この PC からの画面だけ（`status().grants.localDesktop`、`loginEntries`）。
	// - 「この PC として続ける」: ログアウトの後はガードの外のこの画面に来るので自動では
	//   入り直さない。押したらガードのかかる画面へ移り、ガードが `localDesktop` に入る。
	// - 初回セットアップの「この PC からはログインなしで使う（管理者）」: 既定でチェック。
	//   選ぶと setup の本文に `localGrant: 'admin'` を載せ、サーバーが設定
	//   `auth.local_grant` を admin にする（設定の既定は off のまま）。
	let showContinueAsThisPc = $state(false);
	let showLocalGrantOptIn = $state(false);
	let localGrantOptIn = $state(true);
	// #612: 「デモは admin / admin」の案内は demo だけ（サービスの画面・LAN・Tauri の窓で
	// 出すと、そのアカウントがあるように読めて誤解を招く。`loginEntries`）。
	let showDemoAccountHint = $state(false);

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
			const entries = loginEntries(getBantoMode(), status);
			showContinueAsViewer = entries.continueAsViewer;
			showContinueAsThisPc = entries.continueAsThisPc;
			showLocalGrantOptIn = entries.localGrantOptIn;
			showDemoAccountHint = entries.demoAccountHint;
		})();
	});

	// #392 B2-2 のレビュー（オーナー P2-1・P2・Copilot）: 2 つのボタンはどちらも、選んだ口の
	// セッションをここで明示的に確定してから移る（`enterPublicViewer`・`enterThisPc`）。
	// ガードに任せると、この PC からの接続も使えるときにガードがそちらを先に発行し、閲覧の
	// 制限が効かなかった。ブラウザの「戻る」でここへ来て別のセッションが残っていれば、
	// 押した = 切り替える操作としてログアウトしてから入る。結果が選んだ kind のときだけ
	// 移り、それ以外（切り替えられない・入れない）は留まって理由を出す。ガードは既にある
	// セッションを上書きしない。
	async function continueWith(
		enter: typeof enterPublicViewer,
		kind: string,
		failure: string
	): Promise<void> {
		error = null;
		submitting = true;
		try {
			const result = await enter(getSessionController(), getAuthProvider());
			if (enteredKind(result, kind)) {
				goto(resolveAppPath('/monitor'));
			} else {
				error = failure;
			}
		} finally {
			submitting = false;
		}
	}

	function continueAsViewer(): Promise<void> {
		return continueWith(
			enterPublicViewer,
			PUBLIC_VIEWER_GRANT_KIND,
			'閲覧のみに切り替えられませんでした。しばらくしてからもう一度試してください'
		);
	}

	function continueAsThisPc(): Promise<void> {
		return continueWith(
			enterThisPc,
			LOCAL_DESKTOP_GRANT_KIND,
			'この PC からの接続に切り替えられませんでした。しばらくしてからもう一度試してください'
		);
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
			const optedIn = showLocalGrantOptIn && localGrantOptIn;
			const params: Record<string, unknown> = { username, password, displayName };
			if (optedIn) params.localGrant = 'admin';
			const result = await setup(params);
			if (result.success) {
				if (optedIn) {
					// setup は作った管理者のトークンを保存して返す。この PC からの接続で
					// 使い続けると選んだので、そのセッションを閉じてから（`enterGrant` は
					// 資格情報が無いときだけ入る）ガードのかかる画面へ移り、ガードが
					// `localDesktop`（サーバーが setup の応答の前に設定を admin にした）に
					// 入る。設定を保存できなかったときは grant が閉じたままなので、ガードは
					// ログイン画面へ戻す（fail closed）。
					try {
						await getAuthProvider().logout();
					} catch {
						// 閉じられなくてもガードが今の資格情報で確定する。
					}
				}
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

			{#if showLocalGrantOptIn}
				<label class="remember">
					<input type="checkbox" bind:checked={localGrantOptIn} />
					この PC からはログインなしで使う（管理者）
				</label>
				<p class="note opt-in-note">
					この PC で動いているプログラムは誰でも管理者として操作できます。共用の PC
					では外してください（あとで設定の「セキュリティ」から変えられます）。
				</p>
			{/if}

			{#if error}
				<p class="error">{error}</p>
			{/if}

			<button type="submit" disabled={submitting}>アカウントを作成</button>
		</form>
	{:else if mode === 'login'}
		<form onsubmit={submitLogin}>
			<h1>🏮 ChronoGazer</h1>
			{#if showDemoAccountHint}
				<p class="note">単体ブラウザ（デモ）モードは admin / admin でログインできます。</p>
			{/if}

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

			{#if showContinueAsThisPc}
				<button
					type="button"
					class="continue-as-viewer"
					onclick={continueAsThisPc}
					disabled={submitting}
				>
					この PC として続ける
				</button>
			{/if}

			{#if showContinueAsViewer}
				<button
					type="button"
					class="continue-as-viewer"
					onclick={continueAsViewer}
					disabled={submitting}
				>
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

	.opt-in-note {
		margin-top: -0.6rem;
		text-align: left;
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
