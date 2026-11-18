<script lang="ts">
	/**
	 * API キー管理画面（#1161 PR4b、docs/chronogazer/chronogazer-agent-api-design.md §3.3・§3.4・§7）。`admin` のみ到達
	 * （+page.ts が非 admin を監視へ）。AI エージェントなどの外部クライアントが `Authorization: Bearer` で
	 * `/api/*` を呼ぶためのキーを発行・失効・削除する。
	 *
	 * 平文のキーは発行の応答にしか載らない（DB にはハッシュだけ）。発行直後にこの画面だけで表示し、
	 * 閉じる・別のキーを発行する・ページを離れると二度と見られない。一覧は行ごとのボタン（失効・削除）が
	 * 要るので `BantoGrid` ではなく素の表にしている。
	 *
	 * §7: LAN に HTTP で出しているとき、キーは平文で流れる。発行の場所に注意を出し、エージェントは
	 * 同じ PC（`127.0.0.1`）から使うよう案内する（`plaintextOverLan`）。
	 */
	import { isProviderError } from '@banto/admin-core';
	import { toastStore } from '#lib/toast.svelte.js';
	import { guardUnsavedChanges, UNSAVED_NOTICE } from '#lib/unsavedChanges.js';
	import { getBantoMode } from '#lib/banto/setup.js';
	import { getServerStatus } from '#lib/banto/serverAdmin.js';
	import {
		apiKeyState,
		createApiKey,
		deleteApiKey,
		listApiKeys,
		listenerBindForWarning,
		plaintextOverLan,
		revokeApiKey,
		type ApiKeyScope,
		type ApiKeySummary,
		type IssuedApiKey
	} from '#lib/banto/apiKeysAdmin.js';

	const available = getBantoMode() === 'server';

	const scopeOptions: { value: ApiKeyScope; label: string; hint: string }[] = [
		{ value: 'read', label: 'read', hint: '設定・状態・現在値・履歴の読み取りだけ' },
		{ value: 'config', label: 'config', hint: 'read に加えて設定の変更と収集の開始・停止' }
	];

	const STATE_LABEL = { active: '有効', revoked: '失効', expired: '期限切れ' } as const;

	function errorMessage(err: unknown): string {
		return isProviderError(err) ? err.message : String(err);
	}

	function formatMs(ms: number | null): string {
		return ms === null ? '—' : new Date(ms).toLocaleString('ja-JP');
	}

	let keys: ApiKeySummary[] = $state([]);
	let loading = $state(false);
	// 状態（期限切れ）の判定に使う「今」。一覧を読み直すたびに更新する。
	let nowMs = $state(Date.now());

	async function reload(): Promise<void> {
		if (!available) return;
		loading = true;
		try {
			keys = await listApiKeys();
			nowMs = Date.now();
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		} finally {
			loading = false;
		}
	}

	// §7: 実際の待受のアドレス。読めない・再起動待ちで不明なら null（常に注意を出す側に倒す）。
	let bind: string | null = $state(null);
	const plaintextWarning = $derived(
		plaintextOverLan(window.location.protocol, window.location.hostname, bind)
	);

	$effect(() => {
		void reload();
		if (!available) return;
		getServerStatus().then(
			(status) => (bind = listenerBindForWarning(status)),
			() => (bind = null)
		);
	});

	let name = $state('');
	let scope: ApiKeyScope = $state('read');
	let expiresLocal = $state('');
	let creating = $state(false);
	let issued: IssuedApiKey | null = $state(null);
	let copied = $state(false);

	const createDirty = $derived(name !== '' || expiresLocal !== '' || scope !== 'read');
	guardUnsavedChanges({
		// 平文のキーを見せている間も、離れると二度と見られないので確認する。
		isDirty: () => createDirty || issued !== null,
		isSaving: () => creating
	});

	async function handleCreate(event: SubmitEvent): Promise<void> {
		event.preventDefault();
		if (name.trim() === '') {
			toastStore.push('error', '名前を入力してください');
			return;
		}
		let expiresAt: number | null = null;
		if (expiresLocal !== '') {
			expiresAt = new Date(expiresLocal).getTime();
			if (Number.isNaN(expiresAt)) {
				toastStore.push('error', '期限の日時が正しくありません');
				return;
			}
		}
		creating = true;
		try {
			issued = await createApiKey({ name: name.trim(), scopes: [scope], expiresAt });
			copied = false;
			name = '';
			scope = 'read';
			expiresLocal = '';
			await reload();
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		} finally {
			creating = false;
		}
	}

	async function copyKey(): Promise<void> {
		if (!issued) return;
		try {
			await navigator.clipboard.writeText(issued.key);
			copied = true;
		} catch {
			toastStore.push('error', 'コピーできませんでした。キーを選択して手でコピーしてください');
		}
	}

	async function handleRevoke(key: ApiKeySummary): Promise<void> {
		if (
			!window.confirm(
				`${key.name}（${key.prefix}）を失効しますか？ 失効したキーは使えなくなります。`
			)
		)
			return;
		try {
			await revokeApiKey(key.id);
			toastStore.push('success', '失効しました');
			await reload();
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		}
	}

	async function handleDelete(key: ApiKeySummary): Promise<void> {
		if (!window.confirm(`${key.name}（${key.prefix}）を削除しますか？ 元に戻せません。`)) return;
		try {
			await deleteApiKey(key.id);
			toastStore.push('success', '削除しました');
			await reload();
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		}
	}
</script>

<div class="page">
	<h2>API キー</h2>

	{#if !available}
		<p class="note">
			API キーは LAN アクセス（組み込みサーバー・サービス）の画面でだけ管理できます。
		</p>
	{:else}
		<section class="create">
			<h3>キーを発行</h3>
			{#if plaintextWarning}
				<p class="warn" role="note" data-testid="plaintext-warning">
					この ChronoGazer は HTTP
					で待ち受けているため、キーは要求のたびに平文でネットワークを流れます。 エージェントは同じ
					PC（<code>127.0.0.1</code>）から使ってください。
				</p>
			{/if}
			{#if createDirty}<p class="note">{UNSAVED_NOTICE}</p>{/if}
			<form onsubmit={handleCreate}>
				<label class="field">
					名前
					<input type="text" bind:value={name} maxlength="64" required />
				</label>
				<fieldset class="field">
					<legend>スコープ</legend>
					{#each scopeOptions as option (option.value)}
						<label class="radio">
							<input type="radio" name="scope" value={option.value} bind:group={scope} />
							<code>{option.label}</code>
							<span class="hint">{option.hint}</span>
						</label>
					{/each}
				</fieldset>
				<label class="field">
					期限（任意。空なら無期限）
					<input type="datetime-local" bind:value={expiresLocal} />
				</label>
				<button type="submit" disabled={creating}>発行</button>
			</form>

			{#if issued}
				<div class="issued" role="status" data-testid="issued-key">
					<p>
						<strong>「{issued.name}」を発行しました。</strong>
						この画面を閉じる・別のキーを発行する・ページを離れると、このキーは二度と表示できません。今コピーして安全な場所に保管してください。
					</p>
					<code class="secret" data-testid="issued-key-value">{issued.key}</code>
					<div class="actions">
						<button type="button" onclick={copyKey}>{copied ? 'コピーしました' : 'コピー'}</button>
						<button type="button" class="danger" onclick={() => (issued = null)}>閉じる</button>
					</div>
				</div>
			{/if}
		</section>

		<section class="list">
			<h3>キーの一覧</h3>
			{#if loading && keys.length === 0}
				<p class="note">読み込み中…</p>
			{:else if keys.length === 0}
				<p class="note">キーはありません。</p>
			{:else}
				<table>
					<thead>
						<tr>
							<th>名前</th><th>prefix</th><th>スコープ</th><th>作成</th><th>最終使用</th>
							<th>期限</th><th>状態</th><th></th>
						</tr>
					</thead>
					<tbody>
						{#each keys as key (key.id)}
							{@const keyState = apiKeyState(key, nowMs)}
							<tr data-testid="api-key-row">
								<td>{key.name}</td>
								<td><code>{key.prefix}</code></td>
								<td>{key.scopes.join(', ')}</td>
								<td>{key.createdAt}</td>
								<td>{formatMs(key.lastUsedAt)}</td>
								<td>{formatMs(key.expiresAt)}</td>
								<td>{STATE_LABEL[keyState]}</td>
								<td>
									{#if key.revokedAt === null}
										<button type="button" class="danger" onclick={() => handleRevoke(key)}
											>失効</button
										>
									{:else}
										<button type="button" class="danger" onclick={() => handleDelete(key)}
											>削除</button
										>
									{/if}
								</td>
							</tr>
						{/each}
					</tbody>
				</table>
			{/if}
		</section>
	{/if}
</div>

<style>
	.page {
		display: flex;
		flex-direction: column;
		gap: 1rem;
		max-width: 960px;
	}
	h2 {
		margin: 0;
		font-size: 1.1rem;
	}
	section {
		background: var(--banto-surface);
		border: 1px solid var(--banto-border);
		border-radius: calc(var(--banto-radius) * 2);
		padding: 1rem 1.25rem;
	}
	h3 {
		margin: 0 0 0.75rem;
		font-size: 0.95rem;
	}
	.note {
		margin: 0 0 0.5rem;
		color: var(--banto-text-muted);
		font-size: 0.8rem;
	}
	.warn {
		margin: 0 0 0.75rem;
		padding: 0.5rem 0.75rem;
		border: 1px solid var(--banto-danger);
		border-radius: var(--banto-radius);
		font-size: 0.8rem;
	}
	.field {
		display: flex;
		flex-direction: column;
		gap: 0.3rem;
		font-size: 0.8rem;
		color: var(--banto-text-muted);
		margin-bottom: 0.75rem;
		border: none;
		padding: 0;
	}
	.field input[type='text'],
	.field input[type='datetime-local'] {
		padding: 0.4rem 0.5rem;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
		background: var(--banto-bg);
		color: var(--banto-text);
	}
	.radio {
		display: flex;
		gap: 0.5rem;
		align-items: center;
		color: var(--banto-text);
	}
	.hint {
		color: var(--banto-text-muted);
	}
	.issued {
		margin-top: 1rem;
		padding: 0.75rem 1rem;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
		font-size: 0.85rem;
	}
	.secret {
		display: block;
		padding: 0.5rem;
		margin: 0.5rem 0;
		background: var(--banto-bg);
		border-radius: var(--banto-radius);
		word-break: break-all;
		user-select: all;
	}
	.actions {
		display: flex;
		gap: 0.75rem;
	}
	table {
		width: 100%;
		border-collapse: collapse;
		font-size: 0.8rem;
	}
	th,
	td {
		text-align: left;
		padding: 0.4rem 0.5rem;
		border-bottom: 1px solid var(--banto-border);
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
	button.danger {
		background: transparent;
		border: 1px solid var(--banto-danger);
		color: var(--banto-danger);
	}
	button.danger:hover {
		background: color-mix(in srgb, var(--banto-danger) 10%, transparent);
	}
</style>
