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
	import { isAdmin } from '$lib/permissions';
	import { sessionStore } from '$lib/session.svelte';
	import { saveMqttSettings } from '$lib/banto/mqttSettingsAdmin';
	import { saveGrpcSettings } from '$lib/banto/grpcSettingsAdmin';
	import { toastStore } from '$lib/toast.svelte';
	import { errorMessage } from './shared';
	import { mqttSettingsStore } from './mqttSettingsStore.svelte';
	import { grpcSettingsStore } from './grpcSettingsStore.svelte';
	import { hubStatusStore } from './hubStatusStore.svelte';

	const canManageMqtt = $derived(isAdmin(sessionStore.role));
	const canManageGrpc = $derived(isAdmin(sessionStore.role));

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
