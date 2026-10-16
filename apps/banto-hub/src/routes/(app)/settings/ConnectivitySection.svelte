<script lang="ts">
	/**
	 * 接続カテゴリ（MQTT 発行・gRPC）（#359 段階1、PR #371 の Copilot
	 * レビュー是正で段階2改修）。元 `+page.svelte` の「MQTT 発行」「gRPC」
	 * セクションから markup・state・関数を無改変で移した。いずれも admin
	 * 限定（T3/T4、設計 §5.3/§5.4）。
	 *
	 * MQTT/gRPC の設定値そのものは `mqttSettingsStore`/`grpcSettingsStore`
	 * （2つ以上の section から参照される state）に出した - DataSection の
	 * 構成パッケージ import 成功後、この section を経由せず直接
	 * `mqttSettingsStore.load()`/`grpcSettingsStore.load()` を呼んで最新値を
	 * 反映する（元 `+page.svelte` の `applyMqttSettings`/`applyGrpcSettings`
	 * 直接呼び出しと同じ挙動）。フォーム送信中フラグ・エラー文言・
	 * `password` 入力欄はこの section にしか要らないのでローカルのまま残す。
	 *
	 * 接続状態表示（MQTT の「接続中/未接続」ピル）は、この section が
	 * ローカルに5秒ポーリングしていた `mqttConnected` を廃止し、
	 * `hubStatusStore.mqttConnected` を直接読むように変えた
	 * （`hubStatusStore.svelte.ts` の doc comment参照）。ポーリング自体は
	 * `/settings/data` へ直接遷移してもこの section を経由せず動くよう
	 * `settings/+layout.svelte`（admin 限定という既存条件は同じ）へ引き上げ
	 * 済みなので、この section 側は保存直後の即時反映のためだけに
	 * `hubStatusStore.load()` を呼ぶ。
	 */
	import { isAdmin } from '#lib/permissions.js';
	import { sessionStore } from '#lib/session.svelte.js';
	import { saveMqttSettings } from '#lib/banto/mqttSettingsAdmin.js';
	import { saveGrpcSettings } from '#lib/banto/grpcSettingsAdmin.js';
	import { getOpcUaSettings, saveOpcUaSettings } from '#lib/banto/opcuaSettingsAdmin.js';
	import { isLanExposure, validateOpcUaSettings } from '#lib/banto/opcuaSettingsForm.js';
	import { toastStore } from '#lib/toast.svelte.js';
	import { errorMessage } from './shared';
	import { mqttSettingsStore } from './mqttSettingsStore.svelte';
	import { grpcSettingsStore } from './grpcSettingsStore.svelte';
	import { hubStatusStore } from './hubStatusStore.svelte';

	const canManageMqtt = $derived(isAdmin(sessionStore.role));
	const canManageGrpc = $derived(isAdmin(sessionStore.role));
	const canManageOpcUa = $derived(isAdmin(sessionStore.role));

	const mqttQosOptions: { value: 0 | 1; label: string }[] = [
		{ value: 0, label: '0（At most once）' },
		{ value: 1, label: '1（At least once）' }
	];

	let mqttPassword = $state('');

	let mqttLoaded = $state(false);
	let mqttSaving = $state(false);
	let mqttError: string | null = $state(null);

	/** 保存直後の即時反映用（5秒ポーリング自体は `settings/+layout.svelte` が担う）。 */
	async function refreshHubStatus(): Promise<void> {
		try {
			await hubStatusStore.load();
		} catch {
			// 状態表示だけの補助情報 - 取得失敗はエラー表示せず黙って保持する。
		}
	}

	$effect(() => {
		if (!canManageMqtt) return;
		let cancelled = false;
		(async () => {
			try {
				await mqttSettingsStore.load();
			} catch (err) {
				if (!cancelled) mqttError = errorMessage(err);
			} finally {
				if (!cancelled) mqttLoaded = true;
			}
		})();
		return () => {
			cancelled = true;
		};
	});

	async function submitMqttSettings(event: SubmitEvent): Promise<void> {
		event.preventDefault();
		mqttError = null;
		mqttSaving = true;
		try {
			const saved = await saveMqttSettings({
				enabled: mqttSettingsStore.enabled,
				host: mqttSettingsStore.host,
				port: mqttSettingsStore.port,
				clientId: mqttSettingsStore.clientId,
				username: mqttSettingsStore.username.trim() === '' ? null : mqttSettingsStore.username,
				password: mqttPassword,
				prefix: mqttSettingsStore.prefix,
				qos: mqttSettingsStore.qos,
				minIntervalMs: mqttSettingsStore.minIntervalMs
			});
			mqttSettingsStore.applyLoaded(saved);
			mqttPassword = '';
			toastStore.push('success', 'MQTT 設定を保存しました(即時適用されます)');
			await refreshHubStatus();
		} catch (err) {
			mqttError = errorMessage(err);
		} finally {
			mqttSaving = false;
		}
	}

	let grpcLoaded = $state(false);
	let grpcSaving = $state(false);
	let grpcError: string | null = $state(null);

	$effect(() => {
		if (!canManageGrpc) return;
		let cancelled = false;
		(async () => {
			try {
				await grpcSettingsStore.load();
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
			const saved = await saveGrpcSettings({
				enabled: grpcSettingsStore.enabled,
				bind: grpcSettingsStore.bind,
				port: grpcSettingsStore.port
			});
			grpcSettingsStore.applyLoaded(saved);
			toastStore.push('success', 'gRPC 設定を保存しました(即時適用されます)');
		} catch (err) {
			grpcError = errorMessage(err);
		} finally {
			grpcSaving = false;
		}
	}

	let opcUaLoaded = $state(false);
	let opcUaLoadFailed = $state(false);
	let opcUaSaving = $state(false);
	let opcUaError: string | null = $state(null);
	let opcUaEnabled = $state(false);
	let opcUaSavedEnabled = $state(false);
	let opcUaBind = $state('127.0.0.1');
	let opcUaPort = $state(4840);
	let opcUaEndpointPath = $state('/');
	let opcUaNamespaceUri = $state('urn:banto:opcua:tags');
	let opcUaAdvertisedHost = $state('');
	const opcUaLanExposure = $derived(isLanExposure(opcUaBind, opcUaAdvertisedHost));

	$effect(() => {
		if (!canManageOpcUa) return;
		let cancelled = false;
		(async () => {
			try {
				const loaded = await getOpcUaSettings();
				if (cancelled) return;
				applyOpcUaSettings(loaded);
			} catch (err) {
				if (!cancelled) {
					opcUaLoadFailed = true;
					opcUaError = errorMessage(err);
				}
			} finally {
				if (!cancelled) opcUaLoaded = true;
			}
		})();
		return () => {
			cancelled = true;
		};
	});

	async function retryLoadOpcUaSettings(): Promise<void> {
		opcUaError = null;
		opcUaLoaded = false;
		try {
			applyOpcUaSettings(await getOpcUaSettings());
			opcUaLoadFailed = false;
		} catch (err) {
			opcUaError = errorMessage(err);
		} finally {
			opcUaLoaded = true;
		}
	}

	function applyOpcUaSettings(settings: {
		enabled: boolean;
		bind: string;
		port: number;
		endpointPath: string;
		namespaceUri: string;
		advertisedHost: string | null;
	}): void {
		opcUaEnabled = settings.enabled;
		opcUaSavedEnabled = settings.enabled;
		opcUaBind = settings.bind;
		opcUaPort = settings.port;
		opcUaEndpointPath = settings.endpointPath;
		opcUaNamespaceUri = settings.namespaceUri;
		opcUaAdvertisedHost = settings.advertisedHost ?? '';
	}

	async function submitOpcUaSettings(event: SubmitEvent): Promise<void> {
		event.preventDefault();
		opcUaError = null;
		const input = {
			enabled: opcUaEnabled,
			bind: opcUaBind.trim(),
			port: opcUaPort,
			endpointPath: opcUaEndpointPath.trim(),
			namespaceUri: opcUaNamespaceUri.trim(),
			advertisedHost: opcUaAdvertisedHost.trim() || null
		};
		const validationError = validateOpcUaSettings(input);
		if (validationError) {
			opcUaError = validationError;
			return;
		}
		opcUaSaving = true;
		try {
			applyOpcUaSettings(await saveOpcUaSettings(input));
			toastStore.push('success', 'OPC UA 設定を保存しました(即時適用されます)');
		} catch (err) {
			opcUaError = errorMessage(err);
		} finally {
			opcUaSaving = false;
		}
	}
</script>

{#if canManageMqtt}
	<section>
		<h2>
			MQTT 発行
			<span
				class="status-pill"
				class:ok={hubStatusStore.mqttConnected}
				class:bad={!hubStatusStore.mqttConnected}
			>
				{hubStatusStore.mqttConnected ? '接続中' : '未接続'}
			</span>
		</h2>
		{#if mqttLoaded}
			<form onsubmit={submitMqttSettings}>
				<label class="field checkbox">
					<input type="checkbox" bind:checked={mqttSettingsStore.enabled} />
					MQTT 発行を有効にする
				</label>

				<label class="field">
					ブローカーホスト
					<input type="text" bind:value={mqttSettingsStore.host} placeholder="例: 192.168.1.10" />
				</label>
				<label class="field">
					ポート
					<input type="number" min="1" max="65535" bind:value={mqttSettingsStore.port} />
				</label>
				<label class="field">
					クライアント ID
					<input type="text" bind:value={mqttSettingsStore.clientId} />
				</label>
				<label class="field">
					ユーザー名（任意）
					<input type="text" bind:value={mqttSettingsStore.username} autocomplete="off" />
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
					<input type="text" bind:value={mqttSettingsStore.prefix} />
				</label>

				<h3>QoS</h3>
				<div class="options" role="radiogroup" aria-label="MQTT QoS">
					{#each mqttQosOptions as option (option.value)}
						<label class:selected={mqttSettingsStore.qos === option.value}>
							<input
								type="radio"
								name="mqtt-qos"
								checked={mqttSettingsStore.qos === option.value}
								onchange={() => (mqttSettingsStore.qos = option.value)}
							/>
							{option.label}
						</label>
					{/each}
				</div>

				<label class="field">
					最短発行間隔（ミリ秒）
					<input type="number" min="0" bind:value={mqttSettingsStore.minIntervalMs} />
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
			<span
				class="status-pill"
				class:ok={grpcSettingsStore.enabled}
				class:bad={!grpcSettingsStore.enabled}
			>
				{grpcSettingsStore.enabled ? '有効' : '無効'}
			</span>
		</h2>
		{#if grpcLoaded}
			<form onsubmit={submitGrpcSettings}>
				<label class="field checkbox">
					<input type="checkbox" bind:checked={grpcSettingsStore.enabled} />
					gRPC サーバーを有効にする
				</label>

				<label class="field">
					Bind アドレス
					<input type="text" bind:value={grpcSettingsStore.bind} placeholder="127.0.0.1" />
				</label>
				<p class="note">
					127.0.0.1 = このPCのみ(既定・推奨) / 0.0.0.0 = 全インターフェース(非推奨: TLS が 無いため
					API キーが平文で LAN に流れます)。
				</p>

				<label class="field">
					ポート
					<input type="number" min="1" max="65535" bind:value={grpcSettingsStore.port} />
				</label>

				{#if grpcError}
					<p class="error">{grpcError}</p>
				{/if}

				<button type="submit" disabled={grpcSaving}>保存(即時適用)</button>
				<p class="note">
					REST/WebSocket とは別ポートで listen します(既定 50051)。無効化中はこのポートで一切 listen
					しません。
				</p>
			</form>
		{:else}
			<p class="note">読み込み中...</p>
		{/if}
	</section>
{/if}

{#if canManageOpcUa}
	<section>
		<h2>
			OPC UA
			<span
				class="status-pill"
				class:ok={opcUaLoaded && !opcUaLoadFailed && opcUaSavedEnabled}
				class:bad={!opcUaLoaded || opcUaLoadFailed || !opcUaSavedEnabled}
			>
				{opcUaLoaded && !opcUaLoadFailed
					? `設定: ${opcUaSavedEnabled ? '有効' : '無効'}`
					: '設定: 未確認'}
			</span>
		</h2>
		{#if opcUaLoaded && opcUaLoadFailed}
			<p class="error" role="alert">{opcUaError}</p>
			<button type="button" onclick={retryLoadOpcUaSettings}>再読み込み</button>
		{:else if opcUaLoaded}
			<form onsubmit={submitOpcUaSettings}>
				<p class="note">
					上の表示は保存済みの設定です。実際の待受状態や起動失敗は Hub
					の診断ログで確認してください。
				</p>
				<label class="field checkbox">
					<input type="checkbox" bind:checked={opcUaEnabled} />
					OPC UA サーバーを有効にする
				</label>
				<label class="field">
					Bind アドレス
					<input type="text" bind:value={opcUaBind} placeholder="127.0.0.1" required />
				</label>
				<label class="field">
					ポート
					<input type="number" min="1" max="65535" step="1" bind:value={opcUaPort} required />
				</label>
				<label class="field">
					エンドポイントパス
					<input type="text" bind:value={opcUaEndpointPath} placeholder="/" required />
				</label>
				<label class="field">
					Namespace URI
					<input
						type="text"
						bind:value={opcUaNamespaceUri}
						placeholder="urn:banto:opcua:tags"
						required
					/>
				</label>
				<label class="field">
					公開ホスト（任意）
					<input type="text" bind:value={opcUaAdvertisedHost} placeholder="例: hub.example.local" />
				</label>
				<p class="note">
					0.0.0.0 または :: で待ち受ける場合は、クライアントが接続するホスト名または IP
					アドレスを入力してください。ポートや URL は不要です。
				</p>
				{#if opcUaLanExposure}
					<p class="error" role="alert">
						LAN への公開に注意: この OPC UA サーバーは SecurityPolicy None / Anonymous
						のみです。認証・暗号化なしでタグ値が読めるため、信頼できる閉域網でのみ有効にしてください。
					</p>
				{:else}
					<p class="note">
						SecurityPolicy None / Anonymous のみ対応します。既定ではこの PC からのみ接続できます。
					</p>
				{/if}
				{#if opcUaError}
					<p class="error" role="alert">{opcUaError}</p>
				{/if}
				<button type="submit" disabled={opcUaSaving}>保存(即時適用)</button>
			</form>
		{:else}
			<p class="note">読み込み中...</p>
		{/if}
	</section>
{/if}

<style>
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

	.note code {
		background: var(--banto-bg);
		border: 1px solid var(--banto-border);
		border-radius: 4px;
		padding: 0.05rem 0.3rem;
		font-size: 0.75rem;
	}
</style>
