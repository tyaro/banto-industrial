<script lang="ts">
	/**
	 * 監査ログ閲覧画面。`admin` のみ到達（+page.ts が非adminをリダイレクト）。
	 * chronogazer の同名ファイルから複製し、以下を削除した:
	 * - デモモード分岐（`isAuditLogAvailable()`/`DEMO_MODE_MESSAGE`） -
	 *   banto-hub にはデモモードが存在しない。
	 * - 保持ポリシー表示（`getAuditConfig()`） - banto-hub バックエンドに
	 *   `/api/audit-log/config` が無い（`auditLogAdmin.ts` の doc comment
	 *   参照）。
	 *
	 * 一覧は BantoGrid の「サーバーモード」: ソート/フィルタ/ページングは
	 * すべて `listAuditLog()`（Rust側 `ListParams` -> SQL）が行い、ブロック
	 * 単位（`BLOCK_SIZE`件）でスクロールに応じて遅延取得する
	 * （`@banto/admin-core` の `createWindowedListResource` は汎用リソース
	 * レジストリ経由を前提にしており、監査ログはその外にあるため、同じ
	 * ブロック読み込みロジックをこのページ内に直接複製している）。
	 */
	import { untrack } from 'svelte';
	import {
		BantoGrid,
		GridState,
		type FilterState,
		type GridColumn,
		type SortState
	} from '@banto/grid-svelte';
	import { isProviderError } from '@banto/admin-core';
	import { toastStore } from '$lib/toast.svelte';
	import { listAuditLog, type AuditLogEntry } from '$lib/banto/auditLogAdmin';

	function errorMessage(err: unknown): string {
		return isProviderError(err) ? err.message : String(err);
	}

	const actionLabels: Record<string, string> = {
		create: '作成',
		update: '更新',
		delete: '削除',
		revoke: '失効',
		login: 'ログイン',
		login_failed: 'ログイン失敗',
		logout: 'ログアウト',
		setup: '初期セットアップ',
		password_reset: 'パスワードリセット',
		settings_change: '設定変更',
		denied: '権限拒否'
	};

	const resultLabels: Record<string, string> = {
		ok: '成功',
		denied: '拒否',
		failed: '失敗'
	};

	const originLabels: Record<string, string> = {
		rest: 'REST'
	};

	function actionLabel(action: string): string {
		return actionLabels[action] ?? action;
	}

	function resultLabel(result: string): string {
		return resultLabels[result] ?? result;
	}

	function originLabel(origin: string): string {
		return originLabels[origin] ?? origin;
	}

	const columns: GridColumn<AuditLogEntry>[] = [
		{ id: 'ts', header: '時刻', accessor: 'ts', width: 175 },
		{
			id: 'actorUsername',
			header: 'ユーザー',
			accessor: (row) => row.actorUsername ?? '-',
			width: 140,
			filterable: true,
			filterType: 'text'
		},
		{
			id: 'actorRole',
			header: 'ロール',
			accessor: (row) => row.actorRole ?? '-',
			width: 90
		},
		{
			id: 'action',
			header: 'アクション',
			accessor: 'action',
			width: 130,
			filterable: true,
			filterType: 'text',
			format: (value) => actionLabel(String(value))
		},
		{
			id: 'resource',
			header: 'リソース',
			accessor: 'resource',
			width: 110,
			filterable: true,
			filterType: 'text'
		},
		{
			id: 'entityId',
			header: '対象ID',
			accessor: (row) => row.entityId ?? '-',
			width: 90,
			align: 'right'
		},
		{
			id: 'origin',
			header: '経路',
			accessor: 'origin',
			width: 90,
			format: (value) => originLabel(String(value))
		},
		{
			id: 'result',
			header: '結果',
			accessor: 'result',
			width: 90,
			format: (value) => resultLabel(String(value))
		}
	];

	const gridState = new GridState<AuditLogEntry>(columns);
	// 既定ソート: 新しい記録が先頭に来るよう ts 降順。
	gridState.sort = [{ field: 'ts', direction: 'desc' }];

	const BLOCK_SIZE = 200;

	/**
	 * `@banto/admin-core`'s `WindowedListResource`（windowed.svelte.ts）の
	 * 縮小コピー: `getDataProvider().getList(resource, params)` の代わりに
	 * `listAuditLog(params)` を直接呼ぶ点のみが異なる。
	 */
	class AuditLogWindow {
		rows: (AuditLogEntry | undefined)[] = $state([]);
		totalCount = $state(0);
		loading = $state(false);
		params: { sort: SortState[]; filters: FilterState[] } = $state({
			sort: [{ field: 'ts', direction: 'desc' }],
			filters: []
		});

		#loadedBlocks = new Set<number>();
		#inFlightBlocks = new Map<number, Promise<void>>();
		#generation = 0;
		#hasTotalCountForGeneration = false;

		#blocksFor(start: number, end: number): number[] {
			if (end <= start) return [];
			const firstBlock = Math.floor(start / BLOCK_SIZE);
			const lastBlock = Math.floor((end - 1) / BLOCK_SIZE);
			const blocks: number[] = [];
			for (let b = firstBlock; b <= lastBlock; b++) blocks.push(b);
			return blocks;
		}

		async ensureRange(start: number, end: number): Promise<void> {
			const generation = this.#generation;
			const blocks = this.#blocksFor(start, end).filter(
				(block) => !this.#loadedBlocks.has(block) && !this.#inFlightBlocks.has(block)
			);
			if (blocks.length === 0) return;

			this.loading = true;
			const fetches = blocks.map((block) => this.#fetchBlock(block, generation));
			blocks.forEach((block, i) => this.#inFlightBlocks.set(block, fetches[i]));
			try {
				await Promise.all(fetches);
			} finally {
				if (generation === this.#generation) {
					blocks.forEach((block) => this.#inFlightBlocks.delete(block));
					this.loading = this.#inFlightBlocks.size > 0;
				}
			}
		}

		async #fetchBlock(block: number, generation: number): Promise<void> {
			const offset = block * BLOCK_SIZE;
			try {
				const result = await listAuditLog({
					pagination: { offset, limit: BLOCK_SIZE },
					sort: this.params.sort,
					filters: this.params.filters
				});
				if (generation !== this.#generation) return;

				if (!this.#hasTotalCountForGeneration) {
					this.#hasTotalCountForGeneration = true;
					this.totalCount = result.totalCount;
					this.rows.length = result.totalCount;
				}
				if (this.rows.length < offset + result.rows.length) {
					this.rows.length = offset + result.rows.length;
				}
				for (let i = 0; i < result.rows.length; i++) {
					this.rows[offset + i] = result.rows[i];
				}
				this.#loadedBlocks.add(block);
			} catch (err) {
				if (generation !== this.#generation) return;
				toastStore.push('error', errorMessage(err));
			}
		}

		#bumpGeneration(): void {
			this.#generation++;
			this.#loadedBlocks.clear();
			this.#inFlightBlocks.clear();
			this.#hasTotalCountForGeneration = false;
		}

		setParams(partial: Partial<{ sort: SortState[]; filters: FilterState[] }>): void {
			this.params = { ...this.params, ...partial };
			this.#bumpGeneration();
			this.rows = new Array(this.totalCount);
		}
	}

	const windowed = new AuditLogWindow();
	windowed.params = { sort: gridState.sort, filters: [] };

	// `untrack`: `ensureRange()` は最初の await の前に `windowed.params` を
	// 同期的に読むので、`untrack` なしだとこの effect が `params` に依存
	// してしまい、`setParams()` の度に再実行され `handleParamsChange` の
	// `ensureRange()` と重複して [0, 100) を取り直してしまう。
	$effect(() => {
		untrack(() => void windowed.ensureRange(0, 100));
	});

	let visibleRange = { start: 0, end: 100 };

	function handleParamsChange(params: { sort: SortState[]; filters: FilterState[] }): void {
		windowed.setParams(params);
		void windowed.ensureRange(visibleRange.start, visibleRange.end);
	}

	function handleVisibleRangeChange(range: { start: number; end: number }): void {
		visibleRange = range;
		void windowed.ensureRange(range.start, range.end);
	}

	function auditRowClass(row: AuditLogEntry): string | undefined {
		return row.result === 'denied' || row.result === 'failed' ? 'audit-row-alert' : undefined;
	}

	let selected: AuditLogEntry | null = $state(null);

	function selectRow(row: AuditLogEntry): void {
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
		<h2>監査ログ</h2>
	</div>

	<p class="note">
		{windowed.totalCount.toLocaleString()}件の記録があります。行をクリックすると下に詳細が表示されます。
	</p>

	<section class="grid-wrap">
		<BantoGrid
			mode="server"
			state={gridState}
			rows={windowed.rows}
			totalRows={windowed.totalCount}
			{columns}
			getRowId={(row) => row.id}
			rowClass={auditRowClass}
			onRowClick={selectRow}
			onParamsChange={handleParamsChange}
			onVisibleRangeChange={handleVisibleRangeChange}
		/>
	</section>

	{#if selected}
		<section class="detail">
			<h3>詳細（ID: {selected.id}）</h3>
			<dl>
				<dt>時刻</dt>
				<dd>{selected.ts}</dd>
				<dt>ユーザー</dt>
				<dd>{selected.actorUsername ?? '-'}</dd>
				<dt>ロール</dt>
				<dd>{selected.actorRole ?? '-'}</dd>
				<dt>アクション</dt>
				<dd>{actionLabel(selected.action)}</dd>
				<dt>リソース</dt>
				<dd>{selected.resource}</dd>
				<dt>対象ID</dt>
				<dd>{selected.entityId ?? '-'}</dd>
				<dt>経路</dt>
				<dd>{originLabel(selected.origin)}</dd>
				<dt>結果</dt>
				<dd class:alert={selected.result === 'denied' || selected.result === 'failed'}>
					{resultLabel(selected.result)}
				</dd>
			</dl>
			{#if selectedDetail}
				<h4>詳細情報（JSON）</h4>
				<pre>{selectedDetail}</pre>
			{/if}
		</section>
	{/if}
</div>

<style>
	.page {
		height: calc(100vh - var(--banto-shell-header-height) - 2.5rem);
		display: flex;
		flex-direction: column;
		min-height: 0;
		gap: 0.5rem;
	}

	.page-header {
		flex: 0 0 auto;
	}

	.page-header h2 {
		margin: 0;
		font-size: 1.1rem;
	}

	.note {
		flex: 0 0 auto;
		margin: 0;
		color: var(--banto-text-muted);
		font-size: 0.8rem;
	}

	.grid-wrap {
		flex: 1;
		min-height: 0;
	}

	:global(.row.audit-row-alert) {
		border-left: 3px solid var(--banto-danger);
	}

	.detail {
		flex: 0 0 auto;
		max-height: 40%;
		overflow-y: auto;
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
</style>
