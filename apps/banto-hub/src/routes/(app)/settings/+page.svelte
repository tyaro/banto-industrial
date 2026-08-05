<script lang="ts">
	/**
	 * 設定画面。relay-wright の同名ファイル（1126行）から「テーマ」
	 * 「プリセット」「アカウント」の3セクションのみを残した（実装指示:
	 * 「settings は 1126 行版から『テーマ』『プリセット』『アカウント』
	 * だけに削減」）。削除したセクション（ウィンドウ効果/LANアクセス/認証
	 * （ログイン不要モード）/自動ログイン/監査ログ保持ポリシー/バックアップ・
	 * リストア）はいずれも Tauri 専用機能か、実装指示の明示スコープ外
	 * （バックアップ、LAN/認証/自動ログイン設定セクション）に該当する。
	 */
	import type { ThemeMode, ThemePreset } from '@banto/theme';
	import { getAuthProvider, isProviderError } from '@banto/admin-core';
	import { settings } from '$lib/settings.svelte';
	import { toastStore } from '$lib/toast.svelte';

	function errorMessage(err: unknown): string {
		if (isProviderError(err)) {
			if (err.body.kind === 'validation' && err.body.field_errors.length > 0) {
				return err.body.field_errors.map((fe) => fe.message).join(' / ');
			}
			return err.message;
		}
		return String(err);
	}

	const modes: { value: ThemeMode; label: string }[] = [
		{ value: 'light', label: 'ライト' },
		{ value: 'dark', label: 'ダーク' },
		{ value: 'system', label: 'システムに従う' }
	];

	const presets: { value: ThemePreset; label: string }[] = [
		{ value: 'standard', label: 'スタンダード' },
		{ value: 'glass', label: 'ガラス' }
	];

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

<div class="sections">
	<section>
		<h2>テーマ</h2>
		<div class="options" role="radiogroup" aria-label="テーマ">
			{#each modes as mode (mode.value)}
				<label class:selected={settings.themeMode === mode.value}>
					<input
						type="radio"
						name="theme"
						value={mode.value}
						checked={settings.themeMode === mode.value}
						onchange={() => settings.setThemeMode(mode.value)}
					/>
					{mode.label}
				</label>
			{/each}
		</div>

		<h3>プリセット</h3>
		<div class="options" role="radiogroup" aria-label="テーマプリセット">
			{#each presets as preset (preset.value)}
				<label class:selected={settings.themePreset === preset.value}>
					<input
						type="radio"
						name="theme-preset"
						value={preset.value}
						checked={settings.themePreset === preset.value}
						onchange={() => settings.setThemePreset(preset.value)}
					/>
					{preset.label}
				</label>
			{/each}
		</div>
		<p class="note">設定はこの端末に即時保存されます。</p>
	</section>

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
</div>

<style>
	.sections {
		display: flex;
		flex-direction: column;
		gap: 1rem;
		max-width: 560px;
	}

	section {
		background: var(--banto-surface);
		border: 1px solid var(--banto-border);
		border-radius: calc(var(--banto-radius) * 2);
		padding: 1rem 1.25rem;
		backdrop-filter: var(--banto-backdrop, none);
		-webkit-backdrop-filter: var(--banto-backdrop, none);
	}

	h2 {
		margin: 0 0 0.75rem;
		font-size: 1rem;
	}

	h3 {
		margin: 1rem 0 0.5rem;
		font-size: 0.875rem;
		color: var(--banto-text-muted);
	}

	.options {
		display: flex;
		gap: 0.5rem;
	}

	.options label {
		display: flex;
		align-items: center;
		gap: 0.4rem;
		padding: 0.45rem 0.8rem;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
		cursor: pointer;
		font-size: 0.875rem;
	}

	.options label.selected {
		border-color: var(--banto-primary);
		color: var(--banto-primary);
		background: color-mix(in srgb, var(--banto-primary) 10%, transparent);
	}

	.options input {
		position: absolute;
		opacity: 0;
		pointer-events: none;
	}

	section form {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
		max-width: 320px;
	}

	.field {
		display: flex;
		flex-direction: column;
		gap: 0.3rem;
		font-size: 0.8rem;
		color: var(--banto-text-muted);
	}

	.field input {
		padding: 0.4rem 0.5rem;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
		background: var(--banto-bg);
		color: var(--banto-text);
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

	button:hover:not(:disabled) {
		background: var(--banto-primary-hover);
	}

	button:disabled {
		opacity: 0.6;
		cursor: not-allowed;
	}

	.error {
		margin: 0.5rem 0 0;
		color: var(--banto-danger);
		font-size: 0.8rem;
	}

	.note {
		margin: 0.75rem 0 0;
		color: var(--banto-text-muted);
		font-size: 0.8rem;
	}
</style>
