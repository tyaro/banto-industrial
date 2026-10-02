<script lang="ts">
	/**
	 * セキュリティカテゴリ（認証＝ログイン不要モード）（#359 chronogazer 分）。
	 * 元 `+page.svelte` の「認証」セクションから markup・state・関数を
	 * 無改変で移した。`tauri && canManageAuthMode()`（`shared.ts` 参照）
	 * のときだけ表示。
	 *
	 * `disabledDraft`/`disabledRoleDraft` は `authSettingsStore.value`
	 * （初回ロードは `settings/+layout.svelte` に集約済み、Account/
	 * Connectivity と共有）が変わるたびに同期する - 元 `+page.svelte` の
	 * `applyAuthSettingsToDrafts` が page-level effect（初回ロード）と
	 * `saveAuthSettings` の両方から呼ばれていたのと同じタイミングを
	 * `$effect` で再現する。
	 *
	 * `authSettingsStore.error`（初回ロード失敗時のメッセージ）をそのまま
	 * 表示する - 元 `+page.svelte` の page-level effect が失敗したときの
	 * `authError` 表示と同じ見た目（`authSettingsStore.svelte.ts` の doc
	 * comment参照）。保存操作自体の失敗は従来どおりトーストのみ。
	 */
	import { invalidateAll } from '$app/navigation';
	import { isTauri } from '$lib/banto/setup';
	import { toastStore } from '$lib/toast.svelte';
	import { applyAuthSettings, type AuthDisabledRole } from '$lib/banto/authAdmin';
	import { authSettingsStore } from './authSettingsStore.svelte';
	import { canManageAuthMode, errorMessage } from './shared';

	const tauri = isTauri();

	const authDisabledRoleOptions: { value: AuthDisabledRole; label: string }[] = [
		{ value: 'admin', label: '管理者' },
		{ value: 'editor', label: '編集者' },
		{ value: 'viewer', label: '閲覧者' }
	];

	let disabledDraft = $state(false);
	let disabledRoleDraft = $state<AuthDisabledRole>('admin');
	let applyingAuth = $state(false);

	$effect(() => {
		const next = authSettingsStore.value;
		if (!next) return;
		disabledDraft = next.disabled;
		disabledRoleDraft = next.disabledRole;
	});

	// ESCAPE HATCH (spec M11, mirrors `auth_config_apply`'s Rust doc comment):
	// while login-not-required mode is CURRENTLY on, any role may still call
	// this - otherwise a synthetic session below `admin` (e.g. a kiosk set to
	// `viewer`) could never turn auth back on.
	const canManage = $derived(canManageAuthMode());

	async function saveAuthSettings(): Promise<void> {
		if (
			disabledDraft &&
			!window.confirm(
				'ログイン不要モードを有効にすると、この端末を開いた人は誰でもログインなしで操作できるようになります。この端末を完全に信頼できる場合のみ続行してください。'
			)
		) {
			return;
		}

		applyingAuth = true;
		try {
			const next = await applyAuthSettings(disabledDraft, disabledRoleDraft);
			authSettingsStore.apply(next);
			// banto v2.0.0（#260、S-94/S-96/S-99）: Rust はこの適用の中でデスクトップの
			// セッションそのものを付け替え、セッションの `seq` を進める（ON にすると
			// Account -> Local(役割)、ON のまま役割を変えると Local の役割変更、OFF に
			// すると Local を終了）。画面の側は何も書かない: `sessionStore` の
			// identity・役割・`authDisabled` は SessionController の確定からの
			// `$derived`（`authDisabled` は `auth_resolve` の `kind === 'local'`）で、
			// この適用の答えから代入すると古い答えが書き戻しうる（S-61）。
			// `invalidateAll()` で load をすべて走らせ直す:
			// - `(app)/+layout.ts` の `resolveSettled` が確定し直す。provider は
			//   この `seq` の前進を観測していないので最初の答えは捨てられ、
			//   追いつき（S-84）の後の答えで確定する。OFF にして Local が終わって
			//   いれば `none` で /login へ移る。
			// - `settings/+layout.ts`（`await parent()` の後に評価）がカテゴリと
			//   ESCAPE HATCH（`canManageAuthMode()`）を確定した役割・`authDisabled`
			//   から計算し直す。この画面が非可視になれば `security/+page.ts` の
			//   `guardCategory` が先頭カテゴリへ送る（PR #372 の空ページ対策）。
			// admin-template の `SecuritySection.svelte` と同じ形（PR1a の移行措置の
			// 説明を v2 の形に戻した）。
			// トーストは load の再実行の前に出す（admin-template と同じ順）。後だと、
			// OFF でログイン画面へ移った後に出る・確認できないとき最大 10 秒遅れる。
			toastStore.push('success', '認証設定を更新しました');
			await invalidateAll();
		} catch (err) {
			// 排他違反（LANアクセス有効中の有効化など）はサーバ側の日本語メッセージ
			// (kind: 'other') をそのままトーストに出す（spec M11）。
			toastStore.push('error', errorMessage(err));
		} finally {
			applyingAuth = false;
		}
	}
</script>

{#if tauri && canManage}
	<section>
		<h2>認証</h2>

		<label class="toggle">
			<input type="checkbox" bind:checked={disabledDraft} />
			ログイン不要モードを有効にする
		</label>

		<div class="server-fields">
			<label class="field">
				起動時のロール
				<select bind:value={disabledRoleDraft} disabled={!disabledDraft}>
					{#each authDisabledRoleOptions as option (option.value)}
						<option value={option.value}>{option.label}</option>
					{/each}
				</select>
			</label>
		</div>

		<button type="button" onclick={saveAuthSettings} disabled={applyingAuth}>保存して適用</button>

		{#if authSettingsStore.error}
			<p class="error">{authSettingsStore.error}</p>
		{/if}

		{#if authSettingsStore.value}
			<p class="status">
				状態: <strong
					>{authSettingsStore.value.disabled
						? '有効（ログイン画面なし）'
						: '無効（通常のログインが必要）'}</strong
				>
			</p>
		{/if}

		<p class="note warning">
			この端末を完全に信頼できる場合のみ有効化してください。LANアクセスとは同時に有効化できません。
		</p>
	</section>
{/if}

<style>
	.note.warning {
		color: var(--banto-danger);
	}
</style>
