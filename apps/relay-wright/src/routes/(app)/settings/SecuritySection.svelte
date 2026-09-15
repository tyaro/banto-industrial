<script lang="ts">
	/**
	 * セキュリティカテゴリ（認証＝ログイン不要モード／タグモニタ 手動書き込み／
	 * アーム時限失効 H10）（#359 relay-wright 分）。元 `+page.svelte` の
	 * 「認証」「タグモニタ 手動書き込み」「アーム時限失効（H10）」の3
	 * セクションから markup・state・関数を無改変で移した。
	 *
	 * relay-wright 固有の後半2つ（H2 のタグモニタ手動書き込み・H10 の
	 * アーム時限失効）をテンプレートに無い6番目のカテゴリにせず、この
	 * `security` に同居させた理由（PR 本文にも同じ内容を書く）: どちらも
	 * 「意図しない PLC 書き込みを防ぐ安全装置」で、認証（誰がこのアプリを
	 * 操作できるか）と並べると『安全とアクセス制御』としてまとまる -
	 * banto-hub が試運転モードのロックダウンを `security` に置いた
	 * （#371）のと同じ発想。独自カテゴリを新設する案もあったが、上流
	 * テンプレートとの差分をこれ以上増やさないため採らなかった。
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
	 *
	 * 認証モードの変更成功後は `invalidate('settings:categories')` を呼び、
	 * `+layout.ts` の `load` を再実行させる（chronogazer PR #372 Copilot
	 * レビュー指摘と同型の事前対応 - `+layout.ts` の doc comment参照）。
	 * タグモニタ/アーム時限失効セクションはこのカテゴリ自体の可視性に
	 * 別途 OR で効いているため（admin セッションでは通常どちらかが常に
	 * 見える）、invalidate が効くのは主に non-admin のエスケープハッチ
	 * セッションのケース。
	 */
	import { invalidate } from '$app/navigation';
	import { isTauri } from '$lib/banto/setup';
	import { toastStore } from '$lib/toast.svelte';
	import { sessionStore } from '$lib/session.svelte';
	import { applyAuthSettings, type AuthDisabledRole } from '$lib/banto/authAdmin';
	import {
		getMonitorConfig,
		isMonitorAvailable,
		setMonitorConfig,
		type MonitorConfig
	} from '$lib/banto/monitorAdmin';
	import {
		getArmConfig,
		isEngineAvailable,
		setArmConfig,
		type ArmConfig
	} from '$lib/banto/engineAdmin';
	import { isAdmin } from '$lib/permissions';
	import { authSettingsStore } from './authSettingsStore.svelte';
	import { canManageAuthMode, errorMessage } from './shared';

	const tauri = isTauri();

	// --- M11: login-not-required mode (Tauri only) ---------------------------

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
			sessionStore.authDisabled = next.disabled;
			// `+layout.ts` の `canManageAuthMode()` 判定は load が再実行されるまで
			// 古いまま残る（chronogazer PR #372 Copilot レビュー指摘と同型）。
			// admin 未満のロールがエスケープハッチ（`authDisabled`）で見えていた
			// 認証サブセクションをここで OFF に戻すと `canManageAuthMode()` が
			// false になるので、invalidate してナビを再計算させる - この
			// カテゴリが（タグモニタ/アーム時限失効も含め）完全に非可視になった
			// 場合は `security/+page.ts` の `guardCategory` が再実行されて
			// 先頭カテゴリへ redirect する（空ページに留まらせない）。
			await invalidate('settings:categories');
			toastStore.push('success', '認証設定を更新しました');
		} catch (err) {
			// 排他違反（LANアクセス有効中の有効化など）はサーバ側の日本語メッセージ
			// (kind: 'other') をそのままトーストに出す（spec M11）。
			toastStore.push('error', errorMessage(err));
		} finally {
			applyingAuth = false;
		}
	}

	// --- H2 (2026-08-08 オーナー決定, docs/improvement-plan.md H2 — B案):
	// タグモニタ手動書き込みの有効/無効 -------------------------------------
	// Same "not Tauri-only" shape as the audit-log/backups sections
	// (`monitorAdmin.ts` has a REST fallback, `GET`/`PUT /api/monitor/config`)
	// so a LAN browser admin can also toggle this, not just the desktop app.
	const monitorAvailable = isMonitorAvailable();

	let monitorConfig = $state<MonitorConfig | null>(null);
	let manualWriteDraft = $state(false);
	let applyingMonitor = $state(false);
	let monitorError: string | null = $state(null);

	function applyMonitorConfigToDrafts(config: MonitorConfig): void {
		monitorConfig = config;
		manualWriteDraft = config.manualWriteEnabled;
	}

	$effect(() => {
		if (!monitorAvailable || !isAdmin(sessionStore.role)) return;
		void (async () => {
			try {
				applyMonitorConfigToDrafts(await getMonitorConfig());
			} catch (err) {
				monitorError = errorMessage(err);
			}
		})();
	});

	async function saveMonitorConfig(): Promise<void> {
		applyingMonitor = true;
		monitorError = null;
		try {
			applyMonitorConfigToDrafts(await setMonitorConfig({ manualWriteEnabled: manualWriteDraft }));
			toastStore.push(
				'success',
				manualWriteDraft
					? 'タグモニタの手動書き込みを有効にしました'
					: 'タグモニタの手動書き込みを無効にしました'
			);
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		} finally {
			applyingMonitor = false;
		}
	}

	// --- H10 ②(2026-08-08 オーナー決定, docs/improvement-plan.md H10): arm
	// 時限失効の設定 -----------------------------------------------------------
	// Same "not Tauri-only" shape as the sections above (`engineAdmin.ts` has
	// a REST fallback, `GET`/`PUT /api/engine/config`) so a LAN browser admin
	// can also change this, not just the desktop app.
	const armConfigAvailable = isEngineAvailable();

	let armConfig = $state<ArmConfig | null>(null);
	// 0 は「無効」の wire センチネル（既定 28800 秒 = 8時間 = 1シフト）。
	let autoDisarmSecsDraft = $state(28_800);
	let applyingArmConfig = $state(false);
	let armConfigError: string | null = $state(null);

	function applyArmConfigToDrafts(config: ArmConfig): void {
		armConfig = config;
		autoDisarmSecsDraft = config.autoDisarmSecs;
	}

	$effect(() => {
		if (!armConfigAvailable || !isAdmin(sessionStore.role)) return;
		void (async () => {
			try {
				applyArmConfigToDrafts(await getArmConfig());
			} catch (err) {
				armConfigError = errorMessage(err);
			}
		})();
	});

	async function saveArmConfig(): Promise<void> {
		applyingArmConfig = true;
		armConfigError = null;
		try {
			applyArmConfigToDrafts(
				await setArmConfig({ autoDisarmSecs: Math.max(0, Math.trunc(autoDisarmSecsDraft)) })
			);
			toastStore.push('success', 'arm 自動 disarm の設定を更新しました');
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		} finally {
			applyingArmConfig = false;
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

{#if monitorAvailable && isAdmin(sessionStore.role)}
	<section>
		<h2>タグモニタ 手動書き込み</h2>

		<label class="toggle">
			<input type="checkbox" bind:checked={manualWriteDraft} />
			手動書き込みを有効にする
		</label>

		<button type="button" onclick={saveMonitorConfig} disabled={applyingMonitor}>保存</button>

		{#if monitorError}
			<p class="error">{monitorError}</p>
		{/if}

		{#if monitorConfig}
			<p class="status">
				状態:
				<strong>{monitorConfig.manualWriteEnabled ? '有効' : '無効（既定）'}</strong>
			</p>
		{/if}

		<p class="note warning">
			有効にすると、タグモニタからの手動書き込みは arm / レート制限 / dry-run
			の対象外になります。disarm
			中でも物理書き込みが行われます。無効時はモニタ画面での手動書き込みが拒否されます（既定は無効）。変更は監査ログに記録されます。
		</p>
	</section>
{/if}

{#if armConfigAvailable && isAdmin(sessionStore.role)}
	<section>
		<h2>アーム時限失効（H10）</h2>

		<div class="fields">
			<label class="field">
				自動 disarm までの秒数（0 = 無効）
				<input type="number" min="0" step="1" bind:value={autoDisarmSecsDraft} />
			</label>
		</div>

		<button type="button" onclick={saveArmConfig} disabled={applyingArmConfig}>保存</button>

		{#if armConfigError}
			<p class="error">{armConfigError}</p>
		{/if}

		{#if armConfig}
			<p class="status">
				現在の設定:
				<strong>
					{armConfig.autoDisarmSecs > 0
						? `${(armConfig.autoDisarmSecs / 3600).toLocaleString()}時間（${armConfig.autoDisarmSecs.toLocaleString()}秒）`
						: '無効'}
				</strong>
			</p>
		{/if}

		<p class="note">
			エンジンをアームしてから指定した秒数が経過すると、自動的にディスアームされます（既定 28800秒 =
			8時間 =
			1シフト）。0を入力すると自動失効を無効にします。自動ディスアームは監査ログに記録されます。設定変更は次回のエンジン再構築（リロード）または再起動から反映されます。
		</p>
	</section>
{/if}

<style>
	.note.warning {
		color: var(--banto-danger);
	}
</style>
