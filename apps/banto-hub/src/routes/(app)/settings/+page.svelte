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
	import { sessionStore } from '$lib/session.svelte';
	import { isAdmin } from '$lib/permissions';
	import { getHubStatus } from '$lib/banto/hubStatus';
	import {
		getMqttSettings,
		saveMqttSettings,
		type MqttSettings
	} from '$lib/banto/mqttSettingsAdmin';
	import {
		getGrpcSettings,
		saveGrpcSettings,
		type GrpcSettings
	} from '$lib/banto/grpcSettingsAdmin';

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

	// --- MQTT 発行（T3、設計 §5.3、admin 限定） -------------------------------
	//
	// 保存は `PUT /api/mqtt-settings` を叩くだけで即時適用される(実装指示
	// 「保存で即時適用」- サーバー側が保存直後に `MqttPublisher::apply` を
	// 呼ぶので、ここでは保存 API を呼んで結果を反映するだけでよい)。
	// `password` は空欄のまま保存すると「変更なし」として扱われる
	// (`mqttSettingsAdmin.ts` の doc comment参照) - フォームには常に空欄で
	// 表示し、入力があった場合だけ送る。

	const canManageMqtt = $derived(isAdmin(sessionStore.role));

	const mqttQosOptions: { value: 0 | 1; label: string }[] = [
		{ value: 0, label: '0（At most once）' },
		{ value: 1, label: '1（At least once）' }
	];

	let mqttEnabled = $state(false);
	let mqttHost = $state('');
	let mqttPort = $state(1883);
	let mqttClientId = $state('banto-hub');
	let mqttUsername = $state('');
	let mqttPassword = $state('');
	let mqttPrefix = $state('banto');
	let mqttQos: 0 | 1 = $state(1);
	let mqttMinIntervalMs = $state(1000);

	let mqttLoaded = $state(false);
	let mqttSaving = $state(false);
	let mqttError: string | null = $state(null);
	let mqttConnected = $state(false);

	function applyMqttSettings(loaded: MqttSettings): void {
		mqttEnabled = loaded.enabled;
		mqttHost = loaded.host;
		mqttPort = loaded.port;
		mqttClientId = loaded.clientId;
		mqttUsername = loaded.username ?? '';
		mqttPrefix = loaded.prefix;
		mqttQos = loaded.qos === 0 ? 0 : 1;
		mqttMinIntervalMs = loaded.minIntervalMs;
		// password は常に空欄のまま(サーバーは返さない - このセクションの
		// doc comment参照)。
	}

	async function loadMqttStatus(): Promise<void> {
		try {
			const status = await getHubStatus();
			mqttConnected = status.mqtt.connected;
		} catch {
			// 状態表示だけの補助情報 - 取得失敗はエラー表示せず黙って保持する。
		}
	}

	$effect(() => {
		if (!canManageMqtt) return;
		let cancelled = false;
		(async () => {
			try {
				const loaded = await getMqttSettings();
				if (!cancelled) applyMqttSettings(loaded);
			} catch (err) {
				if (!cancelled) mqttError = errorMessage(err);
			} finally {
				if (!cancelled) mqttLoaded = true;
			}
			await loadMqttStatus();
		})();
		const interval = setInterval(() => void loadMqttStatus(), 5000);
		return () => {
			cancelled = true;
			clearInterval(interval);
		};
	});

	async function submitMqttSettings(event: SubmitEvent): Promise<void> {
		event.preventDefault();
		mqttError = null;
		mqttSaving = true;
		try {
			const saved = await saveMqttSettings({
				enabled: mqttEnabled,
				host: mqttHost,
				port: mqttPort,
				clientId: mqttClientId,
				username: mqttUsername.trim() === '' ? null : mqttUsername,
				password: mqttPassword,
				prefix: mqttPrefix,
				qos: mqttQos,
				minIntervalMs: mqttMinIntervalMs
			});
			applyMqttSettings(saved);
			mqttPassword = '';
			toastStore.push('success', 'MQTT 設定を保存しました(即時適用されます)');
			await loadMqttStatus();
		} catch (err) {
			mqttError = errorMessage(err);
		} finally {
			mqttSaving = false;
		}
	}

	// --- gRPC（T4、設計 §5.4、admin 限定） -----------------------------------
	//
	// 保存は `PUT /api/grpc-settings` を叩くだけで即時適用される(実装指示
	// 「保存で即時適用」- サーバー側が保存直後に `GrpcServer::apply` を
	// 呼ぶので、ここでは保存 API を呼んで結果を反映するだけでよい)。MQTT と
	// 違いパスワード等の秘匿情報を持たないため、フォームは常に現在値を
	// そのまま表示する。

	const canManageGrpc = $derived(isAdmin(sessionStore.role));

	let grpcEnabled = $state(false);
	let grpcPort = $state(50051);

	let grpcLoaded = $state(false);
	let grpcSaving = $state(false);
	let grpcError: string | null = $state(null);

	function applyGrpcSettings(loaded: GrpcSettings): void {
		grpcEnabled = loaded.enabled;
		grpcPort = loaded.port;
	}

	$effect(() => {
		if (!canManageGrpc) return;
		let cancelled = false;
		(async () => {
			try {
				const loaded = await getGrpcSettings();
				if (!cancelled) applyGrpcSettings(loaded);
			} catch (err) {
				if (!cancelled) grpcError = errorMessage(err);
			} finally {
				if (!cancelled) grpcLoaded = true;
			}
		})();
		return () => {
			cancelled = true;
		};
	});

	async function submitGrpcSettings(event: SubmitEvent): Promise<void> {
		event.preventDefault();
		grpcError = null;
		grpcSaving = true;
		try {
			const saved = await saveGrpcSettings({ enabled: grpcEnabled, port: grpcPort });
			applyGrpcSettings(saved);
			toastStore.push('success', 'gRPC 設定を保存しました(即時適用されます)');
		} catch (err) {
			grpcError = errorMessage(err);
		} finally {
			grpcSaving = false;
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

	{#if canManageMqtt}
		<section>
			<h2>
				MQTT 発行
				<span class="status-pill" class:ok={mqttConnected} class:bad={!mqttConnected}>
					{mqttConnected ? '接続中' : '未接続'}
				</span>
			</h2>
			{#if mqttLoaded}
				<form onsubmit={submitMqttSettings}>
					<label class="field checkbox">
						<input type="checkbox" bind:checked={mqttEnabled} />
						MQTT 発行を有効にする
					</label>

					<label class="field">
						ブローカーホスト
						<input type="text" bind:value={mqttHost} placeholder="例: 192.168.1.10" />
					</label>
					<label class="field">
						ポート
						<input type="number" min="1" max="65535" bind:value={mqttPort} />
					</label>
					<label class="field">
						クライアント ID
						<input type="text" bind:value={mqttClientId} />
					</label>
					<label class="field">
						ユーザー名（任意）
						<input type="text" bind:value={mqttUsername} autocomplete="off" />
					</label>
					<label class="field">
						パスワード（変更する場合のみ入力。空欄なら現在の値を維持）
						<input
							type="password"
							bind:value={mqttPassword}
							autocomplete="new-password"
							placeholder="変更しない場合は空欄のまま"
						/>
					</label>
					<label class="field">
						トピック prefix
						<input type="text" bind:value={mqttPrefix} />
					</label>

					<h3>QoS</h3>
					<div class="options" role="radiogroup" aria-label="MQTT QoS">
						{#each mqttQosOptions as option (option.value)}
							<label class:selected={mqttQos === option.value}>
								<input
									type="radio"
									name="mqtt-qos"
									checked={mqttQos === option.value}
									onchange={() => (mqttQos = option.value)}
								/>
								{option.label}
							</label>
						{/each}
					</div>

					<label class="field">
						最短発行間隔（ミリ秒）
						<input type="number" min="0" bind:value={mqttMinIntervalMs} />
					</label>

					{#if mqttError}
						<p class="error">{mqttError}</p>
					{/if}

					<button type="submit" disabled={mqttSaving}>保存(即時適用)</button>
					<p class="note">
						トピックは <code>{'{prefix}/{connection}/{group}/{tag}'}</code> の形式で発行されます。パスワードは
						サーバーに平文で保存されます(閉域 LAN 前提)。
					</p>
				</form>
			{:else}
				<p class="note">読み込み中...</p>
			{/if}
		</section>
	{/if}

	{#if canManageGrpc}
		<section>
			<h2>
				gRPC
				<span class="status-pill" class:ok={grpcEnabled} class:bad={!grpcEnabled}>
					{grpcEnabled ? '有効' : '無効'}
				</span>
			</h2>
			{#if grpcLoaded}
				<form onsubmit={submitGrpcSettings}>
					<label class="field checkbox">
						<input type="checkbox" bind:checked={grpcEnabled} />
						gRPC サーバーを有効にする
					</label>

					<label class="field">
						ポート
						<input type="number" min="1" max="65535" bind:value={grpcPort} />
					</label>

					{#if grpcError}
						<p class="error">{grpcError}</p>
					{/if}

					<button type="submit" disabled={grpcSaving}>保存(即時適用)</button>
					<p class="note">
						REST/WebSocket とは別ポートで listen します(既定 50051)。無効化中はこのポートで一切
						listen しません。
					</p>
				</form>
			{:else}
				<p class="note">読み込み中...</p>
			{/if}
		</section>
	{/if}
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
		display: flex;
		align-items: center;
		gap: 0.5rem;
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

	.note code {
		background: var(--banto-bg);
		border: 1px solid var(--banto-border);
		border-radius: 4px;
		padding: 0.05rem 0.3rem;
		font-size: 0.75rem;
	}

	.status-pill {
		font-size: 0.7rem;
		font-weight: 600;
		padding: 0.15rem 0.55rem;
		border-radius: 999px;
	}

	.status-pill.ok {
		color: var(--banto-success, #1a7f37);
		background: color-mix(in srgb, var(--banto-success, #1a7f37) 15%, transparent);
	}

	.status-pill.bad {
		color: var(--banto-text-muted);
		background: color-mix(in srgb, var(--banto-text-muted) 15%, transparent);
	}

	.field.checkbox {
		flex-direction: row;
		align-items: center;
		gap: 0.5rem;
	}

	.field.checkbox input {
		width: auto;
	}
</style>
