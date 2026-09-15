<script lang="ts">
	/**
	 * アカウントカテゴリ（パスワード変更）（#359 段階1）。元 `+page.svelte` の
	 * 「アカウント」セクションから markup・state・関数を無改変で移した
	 * だけで、挙動は変えない。常に表示（権限ガード無し）。
	 */
	import { getAuthProvider } from '@banto/admin-core';
	import { toastStore } from '$lib/toast.svelte';
	import { errorMessage } from './shared';

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
		} catch (err) {
			passwordError = errorMessage(err);
		} finally {
			changingPassword = false;
		}
	}
</script>

<section>
	<h2>アカウント</h2>
	{#if changePassword}
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
