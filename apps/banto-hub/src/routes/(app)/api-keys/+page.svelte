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
	 * スコープ入力: `read`（全タグ）は常設チェックボックス、
	 * `write:{connection}.{group}.{tag}` と `read:{connection}.{group}.{tag}`
	 * / `read:{connection}.{group}.*`（H10 ③、Option B、2026-08-08 オーナー
	 * 決定・docs/h10-3-read-scope-proposal.md）はそれぞれ改行/カンマ区切りの
	 * テキストエリアで複数指定できるようにし（`write:`/`read:` プレフィックス
	 * は自動付与）、送信直前に配列へ組み立てる（`apiKeysAdmin.ts` の
	 * `CreateApiKeyInput.scopes` は `string[]` - サーバー側の文法は
	 * `apps/banto-hub/core/src/api_keys.rs` の `validate_scope` 参照）。
	 * per-tag read スコープは catalog（`GET /api/v1/tags`）を絞らない -
	 * 絞るのは値の読み取り（単一・バルク・ストリーム）のみ（案 B、「発見 ≠
	 * 値アクセス」）。
	 *
	 * H10 ①（docs/improvement-plan.md、2026-08-08 オーナー決定）: 有効期限
	 * は `<input type="date">` で日付のみ受け取り、送信直前にその日の
	 * ローカル終わり（23:59:59）の epoch ミリ秒へ変換する（「その日いっぱい
	 * 有効」という直感に合わせるため - 0時にすると当日選択がほぼ確実に
	 * 「現在時刻より未来」のサーバー側検証に落ちてしまう）。未入力なら
	 * `null`（無期限、既定・動作不変）。一覧の警告バッジは
	 * `apiKeysAdmin.ts` の `apiKeyWarnings`（純関数）で判定する。
	 */
	import { isProviderError } from '@banto/admin-core';
	import { toastStore } from '$lib/toast.svelte';
	import {
		listApiKeys,
		createApiKey,
		revokeApiKey,
		clearTripApiKey,
		apiKeyWarnings,
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

	/** H10 ①: 一覧の警告バッジ（`apiKeyWarnings`）の判定基準時刻。ティッカー
	 *  は持たず、一覧を読み直すたび（`reload()`）に更新する - 「今まさに
	 *  1秒後に切り替わる」精度は不要な表示用途のため。 */
	let nowMs = $state(Date.now());

	async function reload(): Promise<void> {
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

	$effect(() => {
		void reload();
	});

	// --- create ---
	let name = $state('');
	let readScope = $state(true);
	/** H10 ③（Option B）: `read:{connection}.{group}.{tag}` /
	 *  `read:{connection}.{group}.*` の per-tag read スコープ（任意、`read`
	 *  チェックボックスとは独立に併用できる）。 */
	let readScopesText = $state('');
	let writeScopesText = $state('');
	/** H10 ①: `"YYYY-MM-DD"` または空文字（空 = 無期限）。 */
	let expiresAtInput = $state('');
	let createErrors: Record<string, string> = $state({});
	let creating = $state(false);

	/** `expiresAtInput` を「その日のローカル終わり」の epoch ミリ秒へ変換
	 *  する（このファイル冒頭の docblock「H10 ①」参照）。空/不正な日付なら
	 *  `null`（無期限）。 */
	function expiresAtMs(): number | null {
		const trimmed = expiresAtInput.trim();
		if (trimmed === '') return null;
		const ms = new Date(`${trimmed}T23:59:59`).getTime();
		return Number.isNaN(ms) ? null : ms;
	}

	/** issuedKey はこの画面を離れる/リロードすると失われる意図的な一時状態。 */
	let issuedKey: IssuedApiKey | null = $state(null);
	let copied = $state(false);

	function parseScopes(): string[] {
		const scopes: string[] = [];
		if (readScope) scopes.push('read');
		// H10 ③（Option B）: read:{connection}.{group}.{tag} / read:{connection}.
		// {group}.* - 完全一致・グループ・ワイルドカードどちらも許可（サーバー
		// 側の文法検証は api_keys.rs::validate_scope、この入力欄はワイルドカード
		// も禁止しない点が下の write 欄と異なる）。
		for (const line of readScopesText.split(/[\n,]/)) {
			const trimmed = line.trim();
			if (trimmed === '') continue;
			scopes.push(trimmed.startsWith('read:') ? trimmed : `read:${trimmed}`);
		}
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
			issuedKey = await createApiKey({ name, scopes, expiresAt: expiresAtMs() });
			copied = false;
			toastStore.push('success', '発行しました');
			name = '';
			readScope = true;
			readScopesText = '';
			writeScopesText = '';
			expiresAtInput = '';
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

	// --- clear-trip (T2-4、設計 §6-4: レート制限ブレーカのトリップ解除) ---
	let clearingTripId: number | null = $state(null);

	async function handleClearTrip(key: ApiKeySummary): Promise<void> {
		clearingTripId = key.id;
		try {
			await clearTripApiKey(key.id);
			toastStore.push('success', 'トリップを解除しました');
			await reload();
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		} finally {
			clearingTripId = null;
		}
	}

	function formatLastUsed(ms: number | null): string {
		return ms === null ? '未使用' : new Date(ms).toLocaleString('ja-JP');
	}

	/** H10 ①: 有効期限の表示（日付のみ - 発行フォームが日単位でしか
	 *  受け付けないことに合わせる、`formatLastUsed` は日時まで出すが
	 *  こちらは意図的に日付のみ）。 */
	function formatExpiresAt(ms: number | null): string {
		return ms === null ? '無期限' : new Date(ms).toLocaleDateString('ja-JP');
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
				read
				スコープ（任意、1行または1カンマにつき1つ、"&lbrace;接続&rbrace;.&lbrace;グループ&rbrace;.&lbrace;タグ&rbrace;"
				または "&lbrace;接続&rbrace;.&lbrace;グループ&rbrace;.*" 形式）
				<textarea
					bind:value={readScopesText}
					rows="3"
					placeholder="例: line1.fast.temp01, line1.fast.*"></textarea>
				<span class="hint"
					>catalog（タグ一覧・PLCアドレス）は上の read
					と同様に絞られません。ここで指定したタグ以外は値の読み取りのみ403になります。上の read
					にチェックすると、ここでの指定に関わらず全タグを読めます。</span
				>
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
			<label class="field">
				有効期限（任意）
				<input type="date" bind:value={expiresAtInput} />
				{#if createErrors.expiresAt}<span class="err">{createErrors.expiresAt}</span>{/if}
				<span class="hint">未入力なら無期限（既定）。指定した日の終わりまで有効です。</span>
			</label>
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
						<th>有効期限</th>
						<th>状態</th>
						<th>トリップ</th>
						<th></th>
					</tr>
				</thead>
				<tbody>
					{#each keys as key (key.id)}
						<tr class:revoked={key.revokedAt !== null} class:tripped={key.trippedAt !== null}>
							<td>{key.name}</td>
							<td><code>{key.prefix}</code></td>
							<td>{key.scopes.join(', ')}</td>
							<td>{key.createdAt}</td>
							<td>
								{formatLastUsed(key.lastUsedAt)}
								{#if apiKeyWarnings(key, nowMs).longUnused}
									<span class="badge badge-long-unused">長期未使用</span>
								{/if}
							</td>
							<td>
								{formatExpiresAt(key.expiresAt)}
								{#if apiKeyWarnings(key, nowMs).expired}
									<span class="badge badge-expired">期限切れ</span>
								{:else if apiKeyWarnings(key, nowMs).expiringSoon}
									<span class="badge badge-expiring-soon">期限接近</span>
								{/if}
							</td>
							<td>{key.revokedAt === null ? '有効' : `失効済み（${key.revokedAt}）`}</td>
							<td>
								{#if key.trippedAt === null}
									-
								{:else}
									<span class="trip-badge">トリップ中（{key.trippedAt}）</span>
								{/if}
							</td>
							<td class="actions">
								{#if key.trippedAt !== null}
									<button
										type="button"
										class="secondary"
										onclick={() => handleClearTrip(key)}
										disabled={clearingTripId === key.id}
									>
										トリップ解除
									</button>
								{/if}
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

	tr.tripped td {
		color: var(--banto-danger);
	}

	.trip-badge {
		font-weight: 600;
	}

	/* H10 ①: 期限接近/期限切れ/長期未使用の警告バッジ。色分けは既存の
	   `.warning`（危険 = --banto-danger の12%ミックス背景 + 前景色）と
	   同じ手法を踏襲し、新しい色リテラルは増やさない - 深刻度の高い順に
	   danger（期限切れ）> primary（期限接近、まだ切れていない予告）>
	   text-muted（長期未使用、注意喚起のみで即時性はない）。 */
	.badge {
		display: inline-block;
		margin-left: 0.4rem;
		padding: 0.05rem 0.45rem;
		border-radius: var(--banto-radius);
		font-size: 0.72rem;
		font-weight: 600;
		white-space: nowrap;
	}

	.badge-expired {
		color: var(--banto-danger);
		background: color-mix(in srgb, var(--banto-danger) 12%, transparent);
	}

	.badge-expiring-soon {
		color: var(--banto-primary);
		background: color-mix(in srgb, var(--banto-primary) 12%, transparent);
	}

	.badge-long-unused {
		color: var(--banto-text-muted);
		background: color-mix(in srgb, var(--banto-text-muted) 12%, transparent);
	}

	td.actions {
		display: flex;
		gap: 0.4rem;
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
