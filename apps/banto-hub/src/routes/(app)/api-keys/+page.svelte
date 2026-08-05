<script lang="ts">
	/**
	 * APIキー管理画面（`admin` 限定、新規作成）。`/api/v1/*`
	 * を叩く機械クライアント向けキーの発行・一覧・失効
	 * （設計 §5.6・T0-2 `apps/banto-hub/core/src/api_keys.rs`）。
	 *
	 * 発行直後の応答にのみ平文キー全体（`bh_...`）が入っており、二度と
	 * 取得できない（サーバーはハッシュしか保存しない）。実装指示どおり:
	 * 「この画面を閉じると二度と表示されません」という明示的な警告と、
	 * コピーボタンを必須にしている。`issuedKey` は画面遷移/リロードで
	 * 消える一時状態（永続化しない）。
	 *
	 * スコープ入力: `read` は常設チェックボックス、`write:{connection}.
	 * {group}.{tag}` は改行/カンマ区切りのテキストエリアで複数指定できる
	 * ようにし（`write:` プレフィックスは自動付与）、送信直前に配列へ
	 * 組み立てる（`apiKeysAdmin.ts` の `CreateApiKeyInput.scopes` は
	 * `string[]`）。
	 */
	import { isProviderError } from '@banto/admin-core';
	import { toastStore } from '$lib/toast.svelte';
	import {
		listApiKeys,
		createApiKey,
		revokeApiKey,
		type ApiKeySummary,
		type IssuedApiKey
	} from '$lib/banto/apiKeysAdmin';

	function errorMessage(err: unknown): string {
		return isProviderError(err) ? err.message : String(err);
	}

	function applyFieldErrors(err: unknown): Record<string, string> | null {
		if (isProviderError(err) && err.body.kind === 'validation') {
			const map: Record<string, string> = {};
			for (const fe of err.body.field_errors) map[fe.field] = fe.message;
			return map;
		}
		return null;
	}

	let keys: ApiKeySummary[] = $state([]);
	let loading = $state(false);

	async function reload(): Promise<void> {
		loading = true;
		try {
			keys = await listApiKeys();
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		void reload();
	});

	// --- create ---
	let name = $state('');
	let readScope = $state(true);
	let writeScopesText = $state('');
	let createErrors: Record<string, string> = $state({});
	let creating = $state(false);

	/** issuedKey はこの画面を離れる/リロードすると失われる意図的な一時状態。 */
	let issuedKey: IssuedApiKey | null = $state(null);
	let copied = $state(false);

	function parseScopes(): string[] {
		const scopes: string[] = [];
		if (readScope) scopes.push('read');
		for (const line of writeScopesText.split(/[\n,]/)) {
			const trimmed = line.trim();
			if (trimmed === '') continue;
			scopes.push(trimmed.startsWith('write:') ? trimmed : `write:${trimmed}`);
		}
		return scopes;
	}

	async function handleCreate(): Promise<void> {
		creating = true;
		createErrors = {};
		try {
			const scopes = parseScopes();
			if (scopes.length === 0) {
				createErrors = { scopes: '少なくとも1つのスコープを指定してください' };
				return;
			}
			issuedKey = await createApiKey({ name, scopes });
			copied = false;
			toastStore.push('success', '発行しました');
			name = '';
			readScope = true;
			writeScopesText = '';
			await reload();
		} catch (err) {
			const fieldErrors = applyFieldErrors(err);
			if (fieldErrors) createErrors = fieldErrors;
			else toastStore.push('error', errorMessage(err));
		} finally {
			creating = false;
		}
	}

	async function copyIssuedKey(): Promise<void> {
		if (!issuedKey) return;
		try {
			await navigator.clipboard.writeText(issuedKey.key);
			copied = true;
			toastStore.push('success', 'コピーしました');
		} catch {
			toastStore.push('error', 'コピーに失敗しました。手動で選択してコピーしてください。');
		}
	}

	function dismissIssuedKey(): void {
		issuedKey = null;
		copied = false;
	}

	// --- revoke ---
	let revokingId: number | null = $state(null);

	async function handleRevoke(key: ApiKeySummary): Promise<void> {
		if (!window.confirm(`${key.name} を失効させますか？`)) return;
		revokingId = key.id;
		try {
			await revokeApiKey(key.id);
			toastStore.push('success', '失効しました');
			await reload();
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		} finally {
			revokingId = null;
		}
	}

	function formatLastUsed(ms: number | null): string {
		return ms === null ? '未使用' : new Date(ms).toLocaleString('ja-JP');
	}
</script>

<div class="page">
	<h2>APIキー</h2>

	{#if issuedKey}
		<section class="issued">
			<h3>発行しました: {issuedKey.name}</h3>
			<p class="warning">
				この画面を閉じる、またはリロードすると、このキーは二度と表示されません。今すぐコピーして安全な場所に保管してください。
			</p>
			<div class="key-row">
				<code class="key-value">{issuedKey.key}</code>
				<button type="button" onclick={copyIssuedKey}>{copied ? 'コピー済み' : 'コピー'}</button>
			</div>
			<p class="note">
				プレフィックス: {issuedKey.prefix} ・ スコープ: {issuedKey.scopes.join(', ')}
			</p>
			<button type="button" class="secondary" onclick={dismissIssuedKey}>閉じる</button>
		</section>
	{/if}

	<section class="create">
		<h3>新規発行</h3>
		<div class="form-grid">
			<label class="field">
				名前
				<input type="text" bind:value={name} placeholder="MES連携用" />
				{#if createErrors.name}<span class="err">{createErrors.name}</span>{/if}
			</label>
			<label class="field checkbox">
				<input type="checkbox" bind:checked={readScope} />
				read（全タグの現在値・状態の読み取り）
			</label>
			<label class="field wide">
				write
				スコープ（任意、1行または1カンマにつき1つ、"&lbrace;接続&rbrace;.&lbrace;グループ&rbrace;.&lbrace;タグ&rbrace;"
				形式）
				<textarea
					bind:value={writeScopesText}
					rows="3"
					placeholder="例: line1.press.temp, line1.press.pressure"></textarea>
				<span class="hint"
					>ワイルドカードは使えません（タグを明示列挙）。実際の書き込みAPI自体は本スライスの範囲外です。</span
				>
			</label>
			{#if createErrors.scopes}<span class="err">{createErrors.scopes}</span>{/if}
		</div>
		<button type="button" onclick={handleCreate} disabled={creating}>発行</button>
	</section>

	<section class="list">
		<h3>一覧</h3>
		{#if loading && keys.length === 0}
			<p class="loading">読み込み中…</p>
		{:else if keys.length === 0}
			<p class="note">発行済みのAPIキーはありません。</p>
		{:else}
			<table class="key-table">
				<thead>
					<tr>
						<th>名前</th>
						<th>プレフィックス</th>
						<th>スコープ</th>
						<th>作成日時</th>
						<th>最終使用</th>
						<th>状態</th>
						<th></th>
					</tr>
				</thead>
				<tbody>
					{#each keys as key (key.id)}
						<tr class:revoked={key.revokedAt !== null}>
							<td>{key.name}</td>
							<td><code>{key.prefix}</code></td>
							<td>{key.scopes.join(', ')}</td>
							<td>{key.createdAt}</td>
							<td>{formatLastUsed(key.lastUsedAt)}</td>
							<td>{key.revokedAt === null ? '有効' : `失効済み（${key.revokedAt}）`}</td>
							<td>
								{#if key.revokedAt === null}
									<button
										type="button"
										class="danger"
										onclick={() => handleRevoke(key)}
										disabled={revokingId === key.id}
									>
										失効
									</button>
								{/if}
							</td>
						</tr>
					{/each}
				</tbody>
			</table>
		{/if}
	</section>
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

	.issued {
		border-color: var(--banto-primary);
	}

	.warning {
		margin: 0 0 0.75rem;
		padding: 0.6rem 0.8rem;
		border-radius: var(--banto-radius);
		background: color-mix(in srgb, var(--banto-danger) 12%, transparent);
		color: var(--banto-danger);
		font-size: 0.85rem;
		font-weight: 600;
	}

	.key-row {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		margin-bottom: 0.5rem;
	}

	.key-value {
		flex: 1;
		padding: 0.5rem 0.7rem;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
		background: var(--banto-bg);
		font-size: 0.85rem;
		word-break: break-all;
	}

	.note {
		margin: 0 0 0.5rem;
		color: var(--banto-text-muted);
		font-size: 0.8rem;
	}

	.loading {
		color: var(--banto-text-muted);
	}

	.form-grid {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(220px, 1fr));
		gap: 0.75rem;
		margin-bottom: 0.75rem;
	}

	.field {
		display: flex;
		flex-direction: column;
		gap: 0.3rem;
		font-size: 0.8rem;
		color: var(--banto-text-muted);
	}

	.field.wide {
		grid-column: 1 / -1;
	}

	.field.checkbox {
		flex-direction: row;
		align-items: center;
		gap: 0.4rem;
	}

	.field input,
	.field textarea {
		padding: 0.4rem 0.5rem;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
		background: var(--banto-bg);
		color: var(--banto-text);
		font-family: inherit;
	}

	.field.checkbox input {
		width: auto;
	}

	.hint {
		font-size: 0.7rem;
		color: var(--banto-text-muted);
	}

	.err {
		color: var(--banto-danger);
		font-size: 0.75rem;
	}

	table {
		width: 100%;
		border-collapse: collapse;
		font-size: 0.85rem;
	}

	th {
		text-align: left;
		padding: 0.4rem 0.6rem;
		color: var(--banto-text-muted);
		font-weight: 600;
		border-bottom: 1px solid var(--banto-border);
	}

	td {
		padding: 0.4rem 0.6rem;
		border-bottom: 1px solid var(--banto-border);
	}

	tr.revoked td {
		color: var(--banto-text-muted);
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

	button.secondary {
		background: transparent;
		border: 1px solid var(--banto-border);
		color: var(--banto-text);
		font-weight: 400;
	}

	button.danger {
		padding: 0.3rem 0.6rem;
		font-size: 0.8rem;
		background: transparent;
		border: 1px solid var(--banto-danger);
		color: var(--banto-danger);
	}

	button.danger:hover {
		background: color-mix(in srgb, var(--banto-danger) 10%, transparent);
	}
</style>
