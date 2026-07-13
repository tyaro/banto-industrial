<script lang="ts">
	import {
		GridState,
		convertCsvRow,
		csvFilename,
		csvForExcel,
		mapCsvHeader,
		parseCsv,
		toCsv,
		type CellEdit,
		type GridColumn
	} from '@banto/grid-svelte';
	import {
		getDataProvider,
		getResource,
		invalidate,
		isProviderError,
		notify
	} from '@banto/admin-core';
	import { goto } from '$app/navigation';
	import type { Item } from '$lib/banto/sampleData';
	import { sessionStore } from '$lib/session.svelte';
	import { canWriteResources } from '$lib/permissions';
	import {
		DEMO_MODE_MESSAGE as ITEMS_IMPORT_DEMO_MESSAGE,
		importItems,
		isItemsImportAvailable,
		type ItemImportRow
	} from '$lib/banto/itemsAdmin';
	import ItemsClientGrid, { type ItemRow } from './ItemsClientGrid.svelte';
	import ItemsServerGrid from './ItemsServerGrid.svelte';

	const resource = getResource('items');

	// Spec M10 RBAC: `viewer` gets a read-only items page (no 新規作成 button,
	// no inline cell editing below); `editor`/`admin` are unchanged from
	// before M10.
	const canWrite = $derived(canWriteResources(sessionStore.role));

	// M5 Phase A (spec §4.1, §10): the items page demonstrates both grid data
	// modes side by side via a toggle. Plain $state, not persisted - the
	// default is サーバー so a fresh visit shows the real client->
	// DataProvider->(Rust+SQLite in Tauri) path this milestone adds.
	let mode: 'client' | 'server' = $state('server');

	const baseColumns: GridColumn<Item>[] = [
		{
			id: 'open',
			header: '',
			accessor: () => '',
			width: 70,
			resizable: false,
			sortable: false,
			cell: (row) => ({ text: '開く', href: `/items/${row.id}` })
		},
		{
			id: 'id',
			header: 'ID',
			accessor: 'id',
			width: 80,
			align: 'right',
			filterable: true,
			filterType: 'number'
		},
		{
			id: 'name',
			header: '商品名',
			accessor: 'name',
			width: 260,
			filterable: true,
			filterType: 'text',
			editable: true,
			editor: 'text',
			// Same rules/messages as itemsSchema.name (src/lib/banto/setup.ts).
			// Trim before checking emptiness/length, matching Rust's
			// `input.name.trim()` in validate_item_input (items.rs) exactly -
			// otherwise a whitespace-only name passes here and only fails
			// after a round trip to the real Tauri backend.
			validate: (value) => {
				const str = String(value ?? '').trim();
				if (str.length === 0) return '必須項目です';
				if (str.length > 40) return '40文字以内で入力してください';
				return null;
			}
		},
		{
			id: 'price',
			header: '価格',
			accessor: 'price',
			width: 120,
			align: 'right',
			filterable: true,
			filterType: 'number',
			format: (value) => `¥${(value as number).toLocaleString()}`,
			editable: true,
			editor: 'number',
			// Same rules/messages as itemsSchema.price (src/lib/banto/setup.ts).
			// Rust's ItemInput.price is `i64` (items.rs), so a fractional value
			// (e.g. "10.5") must be rejected here too, not just bounds-checked.
			validate: (value) => {
				const num = Number(value);
				if (num < 0) return '0以上で入力してください';
				if (num > 99999) return '99999以下で入力してください';
				if (!Number.isInteger(num)) return '整数で入力してください';
				return null;
			}
		},
		{
			id: 'stock',
			header: '在庫',
			accessor: 'stock',
			width: 100,
			align: 'right',
			filterable: true,
			filterType: 'number',
			editable: true,
			editor: 'number',
			// Same rule/message as itemsSchema.stock (src/lib/banto/setup.ts).
			// Rust's ItemInput.stock is `i64` (items.rs), so a fractional value
			// must be rejected here too, not just bounds-checked.
			validate: (value) => {
				const num = Number(value);
				if (num < 0) return '0以上で入力してください';
				if (!Number.isInteger(num)) return '整数で入力してください';
				return null;
			}
		},
		{
			id: 'updatedAt',
			header: '更新日',
			accessor: 'updatedAt',
			width: 140
		}
	];

	/**
	 * Force every column's `editable` off for `viewer` (spec M10 RBAC):
	 * `editable: false` disables BantoGrid's inline cell editor for that
	 * column entirely, same mechanism already used for naturally read-only
	 * columns like `updatedAt`/`open`. `editor`/`admin` get `baseColumns`
	 * unchanged.
	 */
	function withWritePermission(cols: GridColumn<Item>[]): GridColumn<Item>[] {
		if (canWrite) return cols;
		return cols.map((column) => (column.editable ? { ...column, editable: false } : column));
	}

	const columns = $derived(withWritePermission(baseColumns));

	function columnById(id: string): GridColumn<Item> {
		return columns.find((column) => column.id === id)!;
	}

	// M5 Phase B (spec §4.3) grouping demo: the CLIENT grid only gets an extra
	// 「カテゴリ」 column (ItemsClientGrid derives it from `name`) plus
	// per-column aggregates, so its own column array is built separately from
	// the shared `columns` above (which stays exactly as-is for サーバー mode -
	// grouping has no server-mode equivalent yet, spec §4.3). `$derived.by`
	// (rather than a plain `const`, pre-M10) since it now reads `columns`,
	// itself derived from `canWrite`/`sessionStore.role`.
	const clientColumns = $derived.by((): GridColumn<ItemRow>[] => [
		columnById('open'),
		{ ...columnById('id'), aggregate: 'count' },
		columnById('name'),
		{
			id: 'category',
			header: 'カテゴリ',
			accessor: 'category',
			width: 140,
			filterable: true,
			filterType: 'text',
			groupable: true
		},
		{ ...columnById('price'), aggregate: 'avg' },
		{ ...columnById('stock'), aggregate: 'sum' },
		columnById('updatedAt')
	]);

	// Owned here (not inside ItemsClientGrid) so the shared header's group-by
	// <select> below can call `.setGroupBy(...)` directly - same wiring
	// pattern ItemsServerGrid already uses for its own externally-owned
	// GridState (spec §4.1/§4.3).
	// svelte-ignore state_referenced_locally
	const clientGridState = new GridState<ItemRow>(clientColumns);

	// M15 Phase C: owned here (not inside ItemsServerGrid) so the CSV export
	// button below can read `.sort`/`.filters` directly and reproduce the
	// exact same ListParams the server-mode grid is currently showing - same
	// externally-owned-GridState pattern as clientGridState above.
	// svelte-ignore state_referenced_locally
	const serverGridState = new GridState<Item>(columns);

	type GroupByOption = '' | 'category' | 'updatedAt';

	function handleGroupByChange(event: Event) {
		const value = (event.currentTarget as HTMLSelectElement).value as GroupByOption;
		clientGridState.setGroupBy(value === '' ? null : value);
	}

	function handleRowClick(item: Item) {
		goto(`/items/${item.id}`);
	}

	/** Merge one edited field onto the row's other current values (DataProvider.update expects the full editable value set). */
	function mergedValues(row: Item, field: string, value: unknown): Record<string, unknown> {
		return { name: row.name, price: row.price, stock: row.stock, [field]: value };
	}

	// M3 (spec §4.5): commit a single inline cell edit. A validation error
	// from the provider is re-thrown as a plain Error so BantoGrid re-enters
	// edit mode on that cell and shows the message inline; any other
	// provider error is unexpected, so it's also toasted before rethrowing.
	//
	// BantoGrid's onCellEdit contract only understands `Error.message` - a
	// cell can display exactly one message, no structured shape - so when the
	// provider returns several field_errors (mergedValues always sends the
	// full row, so an edit to one field can surface a violation on another),
	// we must pick just one. Priority: the entry for the field the user
	// actually edited wins if present (that's the one they can see and fix
	// inline); only fall back to the first entry when the edited field itself
	// has no violation (rare - e.g. some other field was already invalid).
	// This is a known limitation of the current onCellEdit contract, pending
	// a richer (multi-field) error shape in a later milestone.
	//
	// Works unchanged in both grid modes: on success it calls invalidate(),
	// which client mode picks up via ListResource's onInvalidate-triggered
	// reload and server mode via WindowedListResource's onInvalidate-
	// triggered refresh() (re-fetching just the currently visible blocks).
	async function handleCellEdit(edit: CellEdit<Item>) {
		try {
			await getDataProvider().update(
				'items',
				edit.rowId,
				mergedValues(edit.row, edit.field, edit.value)
			);
			invalidate('items');
		} catch (err) {
			if (isProviderError(err) && err.body.kind === 'validation') {
				const fieldError =
					err.body.field_errors.find((fe) => fe.field === edit.field) ?? err.body.field_errors[0];
				throw new Error(fieldError?.message ?? err.message, { cause: err });
			}
			notify('error', isProviderError(err) ? err.message : String(err));
			throw err;
		}
	}

	// M3 (spec §4.5): a pasted TSV range can touch several rows/columns at
	// once. Group by row so multi-column pastes on one row become a single
	// `update()` call with all of that row's edited fields merged.
	async function handleRangePaste(edits: CellEdit<Item>[], info: { skipped: number }) {
		const byRow = new Map<string | number, { row: Item; values: Record<string, unknown> }>();
		for (const edit of edits) {
			const entry = byRow.get(edit.rowId) ?? {
				row: edit.row,
				values: { name: edit.row.name, price: edit.row.price, stock: edit.row.stock }
			};
			entry.values[edit.field] = edit.value;
			byRow.set(edit.rowId, entry);
		}

		let updated = 0;
		for (const [rowId, entry] of byRow) {
			try {
				await getDataProvider().update('items', rowId, entry.values);
				updated++;
			} catch (err) {
				notify('error', isProviderError(err) ? err.message : String(err));
			}
		}

		if (updated > 0) {
			invalidate('items');
			notify('success', `${updated}件更新しました`);
		}
		if (info.skipped > 0) {
			notify('info', `${info.skipped}セルはスキップされました`);
		}
	}

	// --- M15 Phase C: CSV export/import ------------------------------------

	/** CSV columns: every real `items` column except the synthetic 「開く」 link column (its accessor is `() => ''` - a blank, useless CSV cell/header). Shared by export (toCsv) and import (mapCsvHeader) so a round-tripped export re-imports cleanly. */
	const csvColumns = $derived(columns.filter((column) => column.id !== 'open'));

	/** Trigger a browser download of `content` named `filename` via a temporary Blob object URL - same pattern as `packages/charts/src/core/export.ts`'s `downloadSvg`. */
	function downloadTextFile(content: string, filename: string, mimeType: string): void {
		const blob = new Blob([content], { type: mimeType });
		const url = URL.createObjectURL(blob);
		try {
			const a = document.createElement('a');
			a.href = url;
			a.download = filename;
			document.body.appendChild(a);
			a.click();
			document.body.removeChild(a);
		} finally {
			URL.revokeObjectURL(url);
		}
	}

	let exporting = $state(false);

	// Export always reflects whichever grid mode is currently on screen: the
	// server-mode grid's sort/filters are sent straight through to
	// getDataProvider().getList() (they ARE ListParams already); the
	// client-mode grid instead applies sort/filters itself inside BantoGrid,
	// so its GridState is read the same way and forwarded through the same
	// getList() call - the DataProvider (InMemory/Tauri/REST) reproduces
	// identical filtering to what BantoGrid shows client-side (spec §4.1/§4.2
	// keep both implementations in lockstep). One exception: filter out any
	// sort/filter entry whose field isn't a real `items` column - the client
	// grouping demo's synthetic 'category' column (ItemsClientGrid.svelte)
	// has no server-side equivalent, and forwarding it would silently match
	// zero rows instead of the rows actually shown.
	async function handleExport(): Promise<void> {
		exporting = true;
		try {
			const active = mode === 'client' ? clientGridState : serverGridState;
			const validFields = new Set(csvColumns.map((column) => column.id));
			const sort = active.sort.filter((entry) => validFields.has(entry.field));
			const filters = active.filters.filter((entry) => validFields.has(entry.field));

			const result = await getDataProvider().getList<Item>('items', {
				pagination: { offset: 0, limit: 20_000 },
				sort,
				filters
			});
			const csv = csvForExcel(toCsv(csvColumns, result.rows));
			downloadTextFile(csv, csvFilename('items'), 'text/csv;charset=utf-8');
			notify('success', `${result.rows.length}件をエクスポートしました`);
		} catch (err) {
			notify('error', isProviderError(err) ? err.message : String(err));
		} finally {
			exporting = false;
		}
	}

	interface ImportRowPreview {
		/** 1-based CSV line number, header counted as line 1 (so the first data row is line 2). */
		csvLine: number;
		id?: number;
		name?: string;
		price?: number;
		stock?: number;
		errors: { columnId: string; message: string }[];
	}

	interface ImportPreviewState {
		fileName: string;
		/** Header cells that matched no known column - shown as "無視される列". */
		ignoredHeaders: string[];
		/** Required columns (name/price/stock) missing from the header entirely - fatal, `rows` is left empty. */
		missingRequired: string[];
		rows: ImportRowPreview[];
		/** Populated after a submitted import comes back with row errors (all-or-nothing rollback, spec M15) - null before the first submit attempt. */
		serverErrors: { row: number; message: string }[] | null;
	}

	let importPreview: ImportPreviewState | null = $state(null);
	let importSubmitting = $state(false);
	let importFileInput: HTMLInputElement | undefined = $state();

	const REQUIRED_IMPORT_COLUMN_IDS = ['name', 'price', 'stock'] as const;

	/** Column header label for an error's `columnId`, falling back to the raw id if unrecognized (e.g. a synthetic 'id' entry - see parseIdCell below). */
	function columnLabel(columnId: string): string {
		return columns.find((column) => column.id === columnId)?.header || columnId;
	}

	/**
	 * `convertCsvRow`'s per-cell errors come in two shapes: a parse failure
	 * already embeds `"${column.header}: "` (core/csv.ts), a `column.validate`
	 * failure does not. Normalize both to the same `"label: message"` shape
	 * for display, without doubling up the label when it's already there.
	 */
	function formatCsvError(columnId: string, message: string): string {
		const label = columnLabel(columnId);
		const prefix = `${label}: `;
		return message.startsWith(prefix) ? message : `${prefix}${message}`;
	}

	/**
	 * The `id` column isn't run through `convertCsvRow` (see buildImportPreview
	 * below) - its editor defaults to 'text' (baseColumns sets no `editor` on
	 * it), which would pass an empty cell through as `ok:false` under 'number'
	 * semantics or as a literal string under 'text' semantics, neither of
	 * which is "no id -> INSERT" (spec M15: "id あり→UPDATE / なし→INSERT").
	 * Parsed by hand instead: blank means "no id", anything else must be an
	 * integer.
	 */
	function parseIdCell(raw: string): { id?: number; error?: string } {
		const trimmed = raw.trim();
		if (trimmed === '') return {};
		const num = Number(trimmed);
		if (!Number.isFinite(num) || !Number.isInteger(num)) {
			return { error: '整数のIDを指定してください（新規作成する場合は空欄にしてください）' };
		}
		return { id: num };
	}

	/**
	 * Parse `text` (a selected CSV file's contents) into a preview the user
	 * confirms before anything is sent to the server. `id`/`updatedAt` are
	 * deliberately pulled out of the mapping passed to `convertCsvRow`: `id`
	 * needs its own optional-integer handling (parseIdCell, above) and
	 * `updatedAt` is a read-only column that must never be written back - both
	 * still count as recognized columns though (mapCsvHeader sees the FULL
	 * `csvColumns` set), so a header that names them is never misreported as
	 * an unrecognized/"無視される列" column.
	 */
	function buildImportPreview(fileName: string, text: string): ImportPreviewState | null {
		const parsed = parseCsv(text);
		if (parsed.length === 0) {
			notify('error', 'CSVにデータがありません');
			return null;
		}
		const [header, ...dataRows] = parsed;
		const { mapped, unknown } = mapCsvHeader<Item>(header, csvColumns);

		const missingRequired = REQUIRED_IMPORT_COLUMN_IDS.filter(
			(id) => !mapped.some((entry) => entry.column.id === id)
		);

		const idMapping = mapped.find((entry) => entry.column.id === 'id');
		const valueMapping = mapped.filter(
			(entry) => entry.column.id !== 'id' && entry.column.id !== 'updatedAt'
		);

		const rows: ImportRowPreview[] =
			missingRequired.length > 0
				? []
				: dataRows.map((cells, index) => {
						const csvLine = index + 2;
						const { values, errors } = convertCsvRow<Item>(cells, valueMapping);
						const rowErrors = errors.map((e) => ({ columnId: e.columnId, message: e.message }));

						let id: number | undefined;
						if (idMapping) {
							const idResult = parseIdCell(cells[idMapping.index] ?? '');
							if (idResult.error) rowErrors.push({ columnId: 'id', message: idResult.error });
							id = idResult.id;
						}

						return {
							csvLine,
							id,
							name: values.name,
							price: values.price,
							stock: values.stock,
							errors: rowErrors
						};
					});

		return { fileName, ignoredHeaders: unknown, missingRequired, rows, serverErrors: null };
	}

	function handleImportButtonClick(): void {
		if (!isItemsImportAvailable()) {
			notify('info', ITEMS_IMPORT_DEMO_MESSAGE);
			return;
		}
		importFileInput?.click();
	}

	async function handleImportFileChange(event: Event): Promise<void> {
		const input = event.currentTarget as HTMLInputElement;
		const file = input.files?.[0];
		input.value = ''; // allow re-selecting the same file (e.g. after fixing it) later
		if (!file) return;
		const text = await file.text();
		importPreview = buildImportPreview(file.name, text);
	}

	function cancelImport(): void {
		importPreview = null;
	}

	// Guarded a second time here (not just via the button's `disabled`, spec
	// M15: 実行ボタンを無効化) - the server runs the whole batch
	// all-or-nothing, so sending it with known-bad rows would just get every
	// row rejected together.
	async function executeImport(): Promise<void> {
		if (!importPreview) return;
		if (importPreview.missingRequired.length > 0) return;
		if (importPreview.rows.some((row) => row.errors.length > 0)) return;

		const payload: ItemImportRow[] = importPreview.rows.map((row) => ({
			id: row.id,
			name: row.name ?? '',
			price: row.price ?? 0,
			stock: row.stock ?? 0
		}));

		importSubmitting = true;
		try {
			const result = await importItems(payload);
			if (result.errors.length > 0) {
				// Rolled back server-side (all-or-nothing) - keep the preview open
				// so the user can see exactly what to fix and retry.
				importPreview = { ...importPreview, serverErrors: result.errors };
				notify('error', `インポートに失敗しました（${result.errors.length}件のエラー）`);
			} else {
				notify('success', `インポートしました（新規${result.created}件・更新${result.updated}件）`);
				invalidate('items');
				importPreview = null;
			}
		} catch (err) {
			notify('error', isProviderError(err) ? err.message : String(err));
		} finally {
			importSubmitting = false;
		}
	}
</script>

<div class="page">
	<div class="page-header">
		<h2>{resource.label}</h2>
		<div class="page-header-actions">
			<div class="mode-toggle" role="group" aria-label="表示モード切り替え">
				<button type="button" class:active={mode === 'client'} onclick={() => (mode = 'client')}>
					クライアント
				</button>
				<button type="button" class:active={mode === 'server'} onclick={() => (mode = 'server')}>
					サーバー
				</button>
			</div>
			<label class="group-by">
				グループ化:
				<select
					disabled={mode !== 'client'}
					title={mode !== 'client' ? 'グループ化はクライアントモードのみ' : undefined}
					onchange={handleGroupByChange}
				>
					<option value="">グループなし</option>
					<option value="category">カテゴリ</option>
					<option value="updatedAt">更新日</option>
				</select>
			</label>
			<button type="button" onclick={handleExport} disabled={exporting}>
				{exporting ? 'エクスポート中…' : 'CSVエクスポート'}
			</button>
			{#if canWrite}
				<button type="button" onclick={handleImportButtonClick}>CSVインポート</button>
				<input
					class="file-input"
					type="file"
					accept=".csv,.txt"
					bind:this={importFileInput}
					onchange={handleImportFileChange}
				/>
				<button type="button" onclick={() => goto('/items/new')}>新規作成</button>
			{/if}
		</div>
	</div>

	<p class="note">
		セル編集（ダブルクリック/Enter）・範囲選択・コピー&ペースト対応（M3）。「クライアント」は全件を一度に取得し、ソート/フィルタ/ページングをブラウザ側（BantoGrid）で行います。「サーバー」ではソート/フィルタ/ページングをDataProvider（単体ブラウザ=InMemory、Tauri/LANブラウザ=Rust+SQLite、REST/SSE経由）が実行し、行はスクロールに応じてブロック単位で遅延取得します（M5）。他クライアントの変更はSSE/Tauriイベント経由で自動反映されます（M6）。M5:
		クライアントモードでグループ化・集計に対応（グループ化はクライアントモードのみ。サーバーモードでのグループ化は今後の対応予定です）。CSVエクスポート/インポート（M15）:
		エクスポートは現在の表示モードのソート/フィルタを反映した全件をダウンロードします。インポートは編集者以上のみ利用でき、id列ありは更新・なしは新規作成として扱われます（1件でもエラーがあると全体がロールバックされます）。
	</p>

	{#if importPreview}
		<section class="import-preview">
			<h3>CSVインポート確認 — {importPreview.fileName}</h3>
			{#if importPreview.missingRequired.length > 0}
				<p class="error">
					CSVヘッダーに必須列がありません: {importPreview.missingRequired
						.map(columnLabel)
						.join('、')}
				</p>
			{:else}
				{@const errorRows = importPreview.rows.filter((row) => row.errors.length > 0)}
				{@const createCount = importPreview.rows.filter((row) => row.id === undefined).length}
				{@const updateCount = importPreview.rows.filter((row) => row.id !== undefined).length}
				<p class="summary">
					新規 {createCount}件 / 更新 {updateCount}件 / エラー {errorRows.length}件（全{importPreview
						.rows.length}行）
				</p>
				{#if importPreview.ignoredHeaders.length > 0}
					<p class="note">無視される列: {importPreview.ignoredHeaders.join('、')}</p>
				{/if}
				{#if errorRows.length > 0}
					<ul class="error-list">
						{#each errorRows.slice(0, 20) as row (row.csvLine)}
							<li>
								{row.csvLine}行目: {row.errors
									.map((e) => formatCsvError(e.columnId, e.message))
									.join(' / ')}
							</li>
						{/each}
					</ul>
					{#if errorRows.length > 20}
						<p class="note">他{errorRows.length - 20}件</p>
					{/if}
				{/if}
				{#if importPreview.serverErrors}
					<p class="error">サーバーでの処理結果（すべてロールバックされました）:</p>
					<ul class="error-list">
						{#each importPreview.serverErrors.slice(0, 20) as serverError, i (i)}
							<li>
								{importPreview.rows[serverError.row]?.csvLine ?? serverError.row + 2}行目: {serverError.message}
							</li>
						{/each}
					</ul>
					{#if importPreview.serverErrors.length > 20}
						<p class="note">他{importPreview.serverErrors.length - 20}件</p>
					{/if}
				{/if}
			{/if}
			<div class="actions">
				<button
					type="button"
					onclick={executeImport}
					disabled={importSubmitting ||
						importPreview.missingRequired.length > 0 ||
						importPreview.rows.some((row) => row.errors.length > 0)}
				>
					{importSubmitting ? '実行中…' : 'インポート実行'}
				</button>
				<button type="button" class="secondary" onclick={cancelImport} disabled={importSubmitting}
					>キャンセル</button
				>
			</div>
		</section>
	{/if}

	{#if mode === 'client'}
		<ItemsClientGrid
			columns={clientColumns}
			state={clientGridState}
			onRowClick={handleRowClick}
			onCellEdit={handleCellEdit}
			onRangePaste={handleRangePaste}
		/>
	{:else}
		<ItemsServerGrid
			{columns}
			state={serverGridState}
			onRowClick={handleRowClick}
			onCellEdit={handleCellEdit}
			onRangePaste={handleRangePaste}
		/>
	{/if}
</div>

<style>
	.page {
		height: calc(100vh - var(--banto-shell-header-height) - 2.5rem);
		display: flex;
		flex-direction: column;
		min-height: 0;
	}

	.page-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		flex: 0 0 auto;
		margin-bottom: 0.5rem;
		gap: 1rem;
	}

	.page-header h2 {
		margin: 0;
		font-size: 1.1rem;
	}

	.page-header-actions {
		display: flex;
		align-items: center;
		gap: 0.75rem;
	}

	.page-header button {
		padding: 0.5rem 1rem;
		border: none;
		border-radius: var(--banto-radius);
		background: var(--banto-primary);
		color: var(--banto-text-inverse);
		font-weight: 600;
		cursor: pointer;
	}

	.page-header button:hover:not(:disabled) {
		background: var(--banto-primary-hover);
	}

	.page-header button:disabled {
		opacity: 0.6;
		cursor: not-allowed;
	}

	.mode-toggle {
		display: inline-flex;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
		overflow: hidden;
	}

	.mode-toggle button {
		padding: 0.4rem 0.8rem;
		border: none;
		background: var(--banto-surface);
		color: var(--banto-text-muted);
		font-size: 0.8rem;
		font-weight: 600;
		cursor: pointer;
	}

	.mode-toggle button:hover {
		background: color-mix(in srgb, var(--banto-primary) 8%, transparent);
	}

	.mode-toggle button.active {
		background: var(--banto-primary);
		color: var(--banto-text-inverse);
	}

	.group-by {
		display: inline-flex;
		align-items: center;
		gap: 0.4rem;
		font-size: 0.8rem;
		color: var(--banto-text-muted);
	}

	.group-by select {
		padding: 0.35rem 0.5rem;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
		background: var(--banto-surface);
		color: var(--banto-text);
		font-size: 0.8rem;
	}

	.group-by select:disabled {
		cursor: not-allowed;
		opacity: 0.6;
	}

	.note {
		flex: 0 0 auto;
		margin: 0 0 0.75rem;
		color: var(--banto-text-muted);
		font-size: 0.8rem;
	}

	/* Visually hidden but still focusable/clickable via the CSVインポート
	   button's importFileInput?.click() - same "real file input, no fake
	   input" approach as a plain native file picker, just not shown itself. */
	.file-input {
		position: absolute;
		width: 1px;
		height: 1px;
		padding: 0;
		margin: -1px;
		overflow: hidden;
		clip: rect(0, 0, 0, 0);
		white-space: nowrap;
		border: 0;
	}

	.import-preview {
		flex: 0 0 auto;
		margin: 0 0 0.75rem;
		padding: 0.75rem 1rem;
		border: 1px solid var(--banto-border);
		border-radius: calc(var(--banto-radius) * 2);
		background: var(--banto-surface);
	}

	.import-preview h3 {
		margin: 0 0 0.5rem;
		font-size: 0.95rem;
	}

	.import-preview .summary {
		margin: 0 0 0.5rem;
		font-weight: 600;
	}

	.import-preview .error {
		margin: 0 0 0.5rem;
		color: var(--banto-danger);
		font-weight: 600;
	}

	.error-list {
		margin: 0 0 0.5rem;
		padding-left: 1.25rem;
		max-height: 220px;
		overflow-y: auto;
		font-size: 0.8rem;
		color: var(--banto-text);
	}

	.error-list li {
		margin-bottom: 0.25rem;
	}

	.import-preview .actions {
		display: flex;
		gap: 0.75rem;
	}

	.import-preview button {
		padding: 0.5rem 1rem;
		border: none;
		border-radius: var(--banto-radius);
		background: var(--banto-primary);
		color: var(--banto-text-inverse);
		font-weight: 600;
		cursor: pointer;
	}

	.import-preview button:hover:not(:disabled) {
		background: var(--banto-primary-hover);
	}

	.import-preview button:disabled {
		opacity: 0.6;
		cursor: not-allowed;
	}

	.import-preview button.secondary {
		background: transparent;
		border: 1px solid var(--banto-border);
		color: var(--banto-text);
	}

	.import-preview button.secondary:hover:not(:disabled) {
		background: color-mix(in srgb, var(--banto-text) 8%, transparent);
	}
</style>
