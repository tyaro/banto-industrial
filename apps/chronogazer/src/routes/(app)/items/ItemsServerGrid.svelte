<script lang="ts">
	/**
	 * サーバーモード (spec §4.1, §4.2, §10, M5 Phase A): sort/filter/paging
	 * execute in the DataProvider (InMemory in the browser, Rust+SQLite in
	 * Tauri) via `ListParams`, and BantoGrid only ever holds the rows that
	 * have scrolled into view (+ overscan) - fetched block-by-block through
	 * `createWindowedListResource` as the virtualization window moves.
	 *
	 * Split out from +page.svelte so the mode toggle can cleanly mount/
	 * unmount whichever side is inactive (see ItemsClientGrid.svelte's doc
	 * comment for why: no double `onInvalidate` subscriptions).
	 */
	import {
		BantoGrid,
		GridState,
		type CellEdit,
		type FilterState,
		type GridColumn,
		type SortState
	} from '@banto/grid-svelte';
	import { createWindowedListResource } from '@banto/admin-core';
	import type { Item } from '$lib/banto/sampleData';

	interface Props {
		columns: GridColumn<Item>[];
		/**
		 * Owned by the parent page (+page.svelte, spec M15 Phase C addition) so
		 * its CSV export button can read `state.sort`/`state.filters` directly
		 * and reproduce the exact same ListParams this grid is currently
		 * showing - same wiring pattern ItemsClientGrid.svelte already uses for
		 * its own externally-owned GridState (spec §4.3's group-by <select>).
		 */
		state: GridState<Item>;
		onRowClick: (item: Item) => void;
		onCellEdit: (edit: CellEdit<Item>) => void | Promise<void>;
		onRangePaste: (edits: CellEdit<Item>[], info: { skipped: number }) => void | Promise<void>;
	}

	let { columns, state: gridState, onRowClick, onCellEdit, onRangePaste }: Props = $props();

	const windowed = createWindowedListResource<Item>('items');

	// The most recently requested visible window, so a param change (sort/
	// filter) knows which range to re-fetch under the new params. Updated
	// only by handleVisibleRangeChange below (BantoGrid's own initial
	// onVisibleRangeChange fire is unreliable as a substitute for the first
	// load: `totalRows` is still 0 at that point, so BantoGrid's virtual
	// window computes as empty until the first ensureRange response sets
	// it - hence the hardcoded initial range here rather than reading this
	// variable).
	let visibleRange = { start: 0, end: 100 };

	// Deliberately TWO separate effects rather than one "load + cleanup"
	// effect (a real bug found during manual verification, spec §4.1/§4.2):
	// `ensureRange()` synchronously reads `windowed.params.sort/filters`
	// (as arguments to `getDataProvider().getList()`, before its own first
	// `await`) while still inside this effect's reactive-tracking scope, so
	// a single combined effect ends up depending on `windowed.params` -
	// every `setParams()` call (i.e. every sort/filter change) then reruns
	// it, and Svelte runs the OLD run's cleanup first. If that cleanup were
	// `windowed.dispose()`, the very first sort/filter click would
	// permanently unsubscribe `windowed` from `invalidate('items')` (its
	// constructor-time subscription is never re-established), silently
	// breaking "edit -> invalidate -> refetch" for the rest of the page's
	// life. Keeping disposal in its own effect - one that reads nothing
	// reactive - guarantees it only ever runs once, on unmount.
	$effect(() => {
		void windowed.ensureRange(0, 100); // initial viewport-sized load
	});

	$effect(() => {
		return () => windowed.dispose();
	});

	function handleParamsChange(params: { sort: SortState[]; filters: FilterState[] }): void {
		windowed.setParams(params);
		void windowed.ensureRange(visibleRange.start, visibleRange.end);
	}

	function handleVisibleRangeChange(range: { start: number; end: number }): void {
		visibleRange = range;
		void windowed.ensureRange(range.start, range.end);
	}
</script>

<p class="note">{windowed.totalCount.toLocaleString()}件のデータを表示しています。</p>

<div class="grid-wrap">
	<BantoGrid
		mode="server"
		state={gridState}
		rows={windowed.rows}
		totalRows={windowed.totalCount}
		{columns}
		getRowId={(item) => item.id}
		{onRowClick}
		{onCellEdit}
		{onRangePaste}
		onParamsChange={handleParamsChange}
		onVisibleRangeChange={handleVisibleRangeChange}
	/>
</div>

<style>
	.note {
		flex: 0 0 auto;
		margin: 0 0 0.75rem;
		color: var(--banto-text-muted);
		font-size: 0.8rem;
	}

	.grid-wrap {
		flex: 1;
		min-height: 0;
	}
</style>
