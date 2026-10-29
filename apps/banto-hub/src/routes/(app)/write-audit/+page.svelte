<script lang="ts">
	/**
	 * 書き込み監査閲覧画面（T2-4、`admin` 限定、新規作成）。
	 * `apps/banto-hub/core/src/write_audit.rs`（`hub_write_audit` テーブル、
	 * log-before-write の記録先）を一覧する - `audit-log/+page.svelte` ほど
	 * 高度な仮想スクロール(BantoGrid)は使わず、`ts` 降順の最新
	 * `BLOCK_SIZE` 件をまとめて取得し「もっと読み込む」で追加取得する
	 * シンプルな実装にしている（書き込み監査は監査ログほど件数の伸びが
	 * 速くない想定 - 収集ではなく能動的な書き込み操作のみが対象）。
	 */
	import { onMount } from 'svelte';
	import { isProviderError } from '@banto/admin-core';
	import { toastStore } from '#lib/toast.svelte.js';
	import { formatSqliteUtc } from '#lib/banto/sqliteTime.js';
	import { listWriteAudit, type WriteAuditEntry } from '#lib/banto/writeAuditAdmin.js';

	function errorMessage(err: unknown): string {
		return isProviderError(err) ? err.message : String(err);
	}

	const actionLabels: Record<string, string> = {
		write: '書き込み',
		rate_limit_tripped: 'レート制限トリップ'
	};

	const resultLabels: Record<string, string> = {
		ok: '成功',
		failed: '失敗',
		uncertain: '結果不明',
		suppressed_disabled: '抑制（受付off）',
		suppressed_rate_limited: '抑制（レート制限）'
	};

	function actionLabel(action: string): string {
		return actionLabels[action] ?? action;
	}

	function resultLabel(result: string): string {
		return resultLabels[result] ?? result;
	}

	function isAlertResult(result: string): boolean {
		return result !== 'ok';
	}

	const BLOCK_SIZE = 100;

	let rows: WriteAuditEntry[] = $state([]);
	let totalCount = $state(0);
	let loading = $state(false);
	let loadedOffset = $state(0);
	/**
	 * 結果での絞り込み（#898）。空文字は「すべて」。`uncertain`（結果不明 =
	 * 送ったあとで結果が分からなくなった書込み）を `failed` と分けて見るため。
	 */
	let resultFilter = $state('');

	/**
	 * 絞り込みを変えるたびに進める世代。`loadMore` は await の前に控え、戻った
	 * ときに世代が変わっていたら（= 前の絞り込みの応答）捨てる。
	 */
	let generation = 0;

	async function loadMore(): Promise<void> {
		const gen = generation;
		loading = true;
		try {
			const result = await listWriteAudit({
				pagination: { offset: loadedOffset, limit: BLOCK_SIZE },
				sort: [{ field: 'ts', direction: 'desc' }],
				filters: resultFilter ? [{ field: 'result', op: 'eq', value: resultFilter }] : []
			});
			if (gen !== generation) return;
			rows = [...rows, ...result.rows];
			totalCount = result.totalCount;
			loadedOffset += result.rows.length;
		} catch (err) {
			if (gen !== generation) return;
			toastStore.push('error', errorMessage(err));
		} finally {
			// 古い要求が、新しい要求の loading を落とさない。
			if (gen === generation) loading = false;
		}
	}

	// 最初のページは mount で 1 回だけ。以後の絞り込み変更は changeResultFilter が起点。
	onMount(() => {
		void loadMore();
	});

	function changeResultFilter(event: Event): void {
		generation += 1;
		resultFilter = (event.currentTarget as HTMLSelectElement).value;
		rows = [];
		totalCount = 0;
		loadedOffset = 0;
		selected = null;
		void loadMore();
	}

	let selected: WriteAuditEntry | null = $state(null);

	function selectRow(row: WriteAuditEntry): void {
		selected = row;
	}

	const selectedDetail = $derived.by((): string | null => {
		if (!selected?.detail) return null;
		try {
			return JSON.stringify(JSON.parse(selected.detail), null, 2);
		} catch {
			return selected.detail;
		}
	});
</script>

<div class="page">
	<div class="page-header">
		<h2>書き込み監査</h2>
	</div>

	<label class="filter">
		結果
		<select value={resultFilter} onchange={changeResultFilter}>
			<option value="">すべて</option>
			{#each Object.entries(resultLabels) as [value, label] (value)}
				<option {value}>{label}</option>
			{/each}
		</select>
	</label>

	<p class="note">
		{totalCount.toLocaleString()}件の記録があります（{rows.length}件読み込み済み）。行をクリックすると下に詳細が表示されます。
	</p>

	<section class="table-wrap">
		{#if loading && rows.length === 0}
			<p class="loading">読み込み中…</p>
		{:else if rows.length === 0}
			<p class="note">書き込み監査の記録はありません。</p>
		{:else}
			<table>
				<thead>
					<tr>
						<th>時刻</th>
						<th>APIキー</th>
						<th>タグ</th>
						<th>要求値</th>
						<th>アクション</th>
						<th>結果</th>
					</tr>
				</thead>
				<tbody>
					{#each rows as row (row.id)}
						<tr class:alert-row={isAlertResult(row.result)} onclick={() => selectRow(row)}>
							<td>{formatSqliteUtc(row.ts)}</td>
							<td
								>{row.apiKeyNameSnapshot}{#if row.apiKeyPrefixSnapshot}
									<small class="key-prefix">{row.apiKeyPrefixSnapshot}</small>{/if}</td
							>
							<td class="tag-name">{row.externalNameSnapshot}</td>
							<td class="value">{row.valueRequested ?? row.valueRequestedText ?? '-'}</td>
							<td>{actionLabel(row.action)}</td>
							<td class:alert={isAlertResult(row.result)}>{resultLabel(row.result)}</td>
						</tr>
					{/each}
				</tbody>
			</table>
			{#if rows.length < totalCount}
				<button type="button" class="secondary" onclick={loadMore} disabled={loading}>
					{loading ? '読み込み中…' : 'もっと読み込む'}
				</button>
			{/if}
		{/if}
	</section>

	{#if selected}
		<section class="detail">
			<h3>詳細（ID: {selected.id}）</h3>
			<dl>
				<dt>時刻</dt>
				<dd>{formatSqliteUtc(selected.ts)}</dd>
				<dt>APIキー</dt>
				<dd>
					{selected.apiKeyNameSnapshot}{#if selected.apiKeyPrefixSnapshot}
						<small class="key-prefix">{selected.apiKeyPrefixSnapshot}</small>{/if}（ID: {selected.apiKeyId}）
				</dd>
				<dt>タグ</dt>
				<dd>{selected.externalNameSnapshot}（ID: {selected.tagId}）</dd>
				<dt>要求値</dt>
				<dd>{selected.valueRequested ?? selected.valueRequestedText ?? '-'}</dd>
				<dt>アクション</dt>
				<dd>{actionLabel(selected.action)}</dd>
				<dt>結果</dt>
				<dd class:alert={isAlertResult(selected.result)}>{resultLabel(selected.result)}</dd>
			</dl>
			{#if selectedDetail}
				<h4>詳細情報（JSON）</h4>
				<pre>{selectedDetail}</pre>
			{/if}
		</section>
	{/if}
</div>

<style>
	.key-prefix {
		color: var(--banto-text-muted, #6b7280);
		font-family: monospace;
		margin-left: 0.25rem;
	}

	.page {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
	}

	.page-header h2 {
		margin: 0;
		font-size: 1.1rem;
	}

	.note {
		margin: 0;
		color: var(--banto-text-muted);
		font-size: 0.8rem;
	}

	.loading {
		color: var(--banto-text-muted);
	}

	.filter {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		font-size: 0.85rem;
	}

	.table-wrap {
		background: var(--banto-surface);
		border: 1px solid var(--banto-border);
		border-radius: calc(var(--banto-radius) * 2);
		padding: 1rem 1.25rem;
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
		max-height: 65vh;
		overflow-y: auto;
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
		position: sticky;
		top: 0;
		background: var(--banto-surface);
	}

	td {
		padding: 0.4rem 0.6rem;
		border-bottom: 1px solid var(--banto-border);
	}

	tr {
		cursor: pointer;
	}

	tr:hover td {
		background: color-mix(in srgb, var(--banto-primary) 6%, transparent);
	}

	tr.alert-row td {
		color: var(--banto-danger);
	}

	.tag-name {
		font-family: var(--banto-font-mono, monospace);
	}

	.value {
		font-variant-numeric: tabular-nums;
	}

	.detail {
		background: var(--banto-surface);
		border: 1px solid var(--banto-border);
		border-radius: calc(var(--banto-radius) * 2);
		padding: 1rem 1.25rem;
	}

	.detail h3 {
		margin: 0 0 0.75rem;
		font-size: 0.95rem;
	}

	.detail h4 {
		margin: 0.75rem 0 0.5rem;
		font-size: 0.85rem;
		color: var(--banto-text-muted);
	}

	dl {
		display: grid;
		grid-template-columns: max-content 1fr;
		gap: 0.35rem 1rem;
		margin: 0;
		font-size: 0.85rem;
	}

	dt {
		color: var(--banto-text-muted);
	}

	dd {
		margin: 0;
	}

	dd.alert {
		color: var(--banto-danger);
		font-weight: 600;
	}

	pre {
		margin: 0;
		padding: 0.75rem;
		background: var(--banto-bg);
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
		font-size: 0.8rem;
		white-space: pre-wrap;
		word-break: break-word;
	}

	button {
		align-self: flex-start;
		padding: 0.5rem 1rem;
		border: none;
		border-radius: var(--banto-radius);
		background: var(--banto-primary);
		color: var(--banto-text-inverse);
		font-weight: 600;
		cursor: pointer;
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
</style>
