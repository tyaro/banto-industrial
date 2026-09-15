<script lang="ts">
	/**
	 * アカウントカテゴリ（自動ログイン/アカウント＝パスワード変更）
	 * （#359 relay-wright 分）。元 `+page.svelte` の「自動ログイン」
	 * 「アカウント」セクションから markup・state・関数を無改変で移した。
	 * 常に表示（いずれのセクションもカテゴリ自体へのガードは無く、内部の
	 * `{#if}` だけで出し分ける）。
	 *
	 * `authSettings`（自動ログインの状態表示）は `authSettingsStore`
	 * （Connectivity/Security と共有、初回ロードは `settings/+layout.svelte`
	 * に集約済み）を直接読むだけで、この section 自身はフェッチしない -
	 * `authSettingsStore.svelte.ts` の doc comment参照。有効化/解除の成功後は
	 * 元 `+page.svelte` の `reloadAuthSettings()` と同じタイミングで
	 * `authSettingsStore.load()` を呼び直す。
	 */
	import { getAuthProvider } from '@banto/admin-core';
	import { isTauri } from '$lib/banto/setup';
	import { disableAutologin, enableAutologin } from '$lib/banto/authAdmin';
	import { toastStore } from '$lib/toast.svelte';
	import { sessionStore } from '$lib/session.svelte';
	import { isAdmin } from '$lib/permissions';
	import { authSettingsStore } from './authSettingsStore.svelte';
	import { errorMessage } from './shared';

	const tauri = isTauri();

	// Optional on `AuthProvider` (spec §3.3): older/custom providers may not
	// implement it, in which case the section below shows a note instead of
	// the form (all three built-in providers - demo/Tauri/HTTP - do
	// implement it, demo's just always fails with a fixed message).
	const changePassword = getAuthProvider().changePassword;

	let currentPassword = $state('');
	let newPassword = $state('');
	let newPasswordConfirm = $state('');
	let passwordError: string | null = $state(null);
	let changingPassword = $state(false);

	async function submitChangePassword(event: SubmitEvent): Promise<void> {
		event.preventDefault();
		passwordError = null;

		if (newPassword.length < 8) {
			passwordError = 'パスワードは8文字以上で入力してください';
			return;
		}
		if (newPassword !== newPasswordConfirm) {
			passwordError = 'パスワードが一致しません';
			return;
		}
		if (!changePassword) return;

		changingPassword = true;
		try {
			const result = await changePassword(currentPassword, newPassword);
			if (result.success) {
				currentPassword = '';
				newPassword = '';
				newPasswordConfirm = '';
				toastStore.push('success', 'パスワードを変更しました');
			} else {
				passwordError = result.error ?? 'パスワードの変更に失敗しました';
			}
		} finally {
			changingPassword = false;
		}
	}

	let autologinUsername = $state('');
	let autologinPassword = $state('');
	let enablingAutologin = $state(false);
	let disablingAutologin = $state(false);

	async function submitEnableAutologin(event: SubmitEvent): Promise<void> {
		event.preventDefault();
		enablingAutologin = true;
		try {
			await enableAutologin(autologinUsername, autologinPassword);
			autologinPassword = '';
			toastStore.push('success', '自動ログインを有効にしました');
			await authSettingsStore.load();
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		} finally {
			enablingAutologin = false;
		}
	}

	async function submitDisableAutologin(): Promise<void> {
		disablingAutologin = true;
		try {
			await disableAutologin();
			toastStore.push('success', '自動ログインを解除しました');
			await authSettingsStore.load();
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		} finally {
			disablingAutologin = false;
		}
	}
</script>

{#if tauri && isAdmin(sessionStore.role)}
	<section>
		<h2>自動ログイン</h2>

		{#if sessionStore.authDisabled}
			<p class="note">ログイン不要モードでは自動ログインは不要です。</p>
		{:else}
			<p class="status">
				状態:
				<strong>
					{authSettingsStore.value?.autologinEnabled
						? `有効（${authSettingsStore.value.autologinUsername ?? ''}）`
						: '無効'}
				</strong>
			</p>

			{#if authSettingsStore.value?.autologinEnabled}
				<button type="button" onclick={submitDisableAutologin} disabled={disablingAutologin}>
					自動ログインを解除
				</button>
			{:else}
				<form onsubmit={submitEnableAutologin}>
					<label class="field">
						ユーザー名
						<input type="text" bind:value={autologinUsername} autocomplete="username" />
					</label>
					<label class="field">
						パスワード
						<input type="password" bind:value={autologinPassword} autocomplete="current-password" />
					</label>
					<button type="submit" disabled={enablingAutologin}>自動ログインを有効化</button>
				</form>
			{/if}

			<p class="note">
				資格情報はOSのキーリングに保存されます。起動時にこのアカウントで自動的にログインします。
			</p>
		{/if}
	</section>
{/if}

<section>
	<h2>アカウント</h2>
	{#if sessionStore.authDisabled}
		<p class="note">ログイン不要モードではアカウントがないため、パスワード変更はできません。</p>
	{:else if changePassword}
		<form onsubmit={submitChangePassword}>
			<label class="field">
				現在のパスワード
				<input type="password" bind:value={currentPassword} autocomplete="current-password" />
			</label>
			<label class="field">
				新しいパスワード（8文字以上）
				<input type="password" bind:value={newPassword} autocomplete="new-password" />
			</label>
			<label class="field">
				新しいパスワード（確認）
				<input type="password" bind:value={newPasswordConfirm} autocomplete="new-password" />
			</label>

			{#if passwordError}
				<p class="error">{passwordError}</p>
			{/if}

			<button type="submit" disabled={changingPassword}>パスワードを変更</button>
		</form>
	{:else}
		<p class="note">この環境ではパスワード変更に対応していません。</p>
	{/if}
</section>

<style>
	section form {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
		max-width: 320px;
	}
</style>
