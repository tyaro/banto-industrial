<script lang="ts">
	/**
	 * 監査ログ閲覧画面（spec M14）。`admin` のみ到達（+page.ts が非adminを
	 * ダッシュボードへリダイレクト）。
	 *
	 * 一覧は BantoGrid の「サーバーモード」（items 一覧ページの
	 * ItemsServerGrid.svelte と同じ発想）: ソート/フィルタ/ページングは
	 * すべて `listAuditLog()`（Rust側 `ListParams` -> SQL）が行い、
	 * ブロック単位（`BLOCK_SIZE`件）でスクロールに応じて遅延取得する。
	 * ただし `@banto/admin-core` の `createWindowedListResource` は
	 * `getDataProvider()` の汎用レジストリ経由の資源を前提にしており、
	 * 監査ログは usersAdmin.ts と同じ理由（専用のワイヤ形状・Tauriコマンド名）
	 * でその外にあるため、同じブロック読み込みロジックをこのページ内に
	 * 直接複製している（`packages/admin-core/src/windowed.svelte.ts` 参照）。
	 *
	 * デモモード（プレーンな vite dev/preview、バックエンドなし）では
	 * 監査ログDBそのものが存在しないため、案内文のみ表示する
	 * （isAuditLogAvailable()、usersAdmin.ts と同じ流儀）。
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
	import {
		DEMO_MODE_MESSAGE,
		getAuditConfig,
		isAuditLogAvailable,
		listAuditLog,
		type AuditLogEntry
	} from '$lib/banto/auditLogAdmin';

	function errorMessage(err: unknown): string {
		return isProviderError(err) ? err.message : String(err);
	}

	const available = isAuditLogAvailable();

	const actionLabels: Record<string, string> = {
		create: '作成',
		update: '更新',
		delete: '削除',
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
		tauri: 'デスクトップ',
		rest: 'LAN/ブラウザ'
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
			width: 110,
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
	// 既定ソート: 新しい記録が先頭に来るよう ts 降順（spec M14）。
	gridState.sort = [{ field: 'ts', direction: 'desc' }];

	const BLOCK_SIZE = 200;

	/**
	 * `@banto/admin-core`'s `WindowedListResource`（windowed.svelte.ts）の
	 * 縮小コピー: `getDataProvider().getList(resource, params)` の代わりに
	 * `listAuditLog(params)` を直接呼ぶ点のみが異なる。ブロック単位フェッチ・
	 * 世代カウンタによる競合防止の設計はそちらのコメントを参照。
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

	// `untrack` (spec M14, mirrors ItemsServerGrid.svelte's split-effects
	// comment): `ensureRange()` synchronously reads `windowed.params` (inside
	// `#fetchBlock`, before its own first `await`) while still inside this
	// effect's reactive-tracking scope. Without `untrack`, this effect would
	// end up depending on `windowed.params` and rerun on every
	// `setParams()` call, redundantly re-fetching [0, 100) on top of
	// `handleParamsChange`'s own `ensureRange()` call for the currently
	// visible range. `untrack` keeps this effect a true "run once on mount"
	// initial load, same intent as `onMount` but effect-based so it still
	// only runs client-side.
	$effect(() => {
		if (!available) return;
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

	// spec M14: result='denied'/'failed' の行を控えめな左ボーダーで視覚的に
	// 区別する（生色禁止・--banto-danger を使用）。BantoGrid の rowClass
	// prop が返すクラスに対して、下のスタイル内で :global() セレクタを
	// 当てている。
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

	// --- 保持ポリシー（表示のみ・設定変更は「設定」画面で行う） -----------
	let retentionNote: string | null = $state(null);

	$effect(() => {
		if (!available) return;
		void (async () => {
			try {
				const config = await getAuditConfig();
				const days = config.retentionDays !== null ? `${config.retentionDays}日` : '無期限';
				const rows =
					config.retentionRows !== null ? `${config.retentionRows.toLocaleString()}件` : '無制限';
				retentionNote = `保持ポリシー: 最大${days} / 最大${rows}（「設定」画面で変更できます）`;
			} catch {
				// 表示専用の補足情報なので、取得に失敗しても画面は壊さない。
				retentionNote = null;
			}
		})();
	});
</script>

<div class="page">
	<div class="page-header">
		<h2>監査ログ</h2>
	</div>

	{#if !available}
		<p class="note">
			{DEMO_MODE_MESSAGE}。単体ブラウザのデモモードには監査ログDBがないため、この機能はTauriアプリまたはLANアクセス（組み込みサーバー）でのみ利用できます。
		</p>
	{:else}
		{#if retentionNote}
			<p class="note">{retentionNote}</p>
		{/if}

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

	/* spec M14: BantoGrid's `rowClass` prop adds this class to a row's outer
	   `.row` element (packages/grid-svelte/src/BantoGrid.svelte); `:global()`
	   is required here since that element is rendered by a different
	   component (Svelte scopes styles per-component by default). Subdued
	   left border only, theme-variable based - no raw colors (spec M14). */
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
