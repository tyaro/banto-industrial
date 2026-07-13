<script lang="ts">
	/**
	 * クライアントモード (spec §4.1, M2-M4): unchanged from before M5 - fetch
	 * the whole demo dataset once (limit large enough to cover it) and let
	 * BantoGrid do client-side sort/filter/paging under virtualization.
	 *
	 * Split out from +page.svelte (M5 Phase A) so the mode toggle can fully
	 * mount/unmount whichever side is inactive: `createListResource`
	 * subscribes to `onInvalidate('items')` in its constructor, so keeping
	 * both this and ItemsServerGrid always alive at the parent level would
	 * leave two live subscriptions (and two in-flight resources) at once.
	 * A plain `{#if}` in the parent already destroys this component (running
	 * the `$effect` cleanup below) when the toggle flips to サーバー.
	 */
	import { BantoGrid, GridState, type CellEdit, type GridColumn } from '@banto/grid-svelte';
	import { createListResource } from '@banto/admin-core';
	import type { Item } from '$lib/banto/sampleData';

	/**
	 * M5 Phase B (spec §4.3) grouping demo: a client-only derived row shape
	 * with a `category` field (the item name's first whitespace token, e.g.
	 * "緑茶 500ml" -> "緑茶"), so the shared items header's group-by select has
	 * a column worth grouping by. `Item` itself (and the server-mode grid)
	 * stay untouched - this is purely a client-mode presentation concern.
	 */
	export type ItemRow = Item & { category: string };

	interface Props {
		columns: GridColumn<ItemRow>[];
		/**
		 * Owned by the parent page (+page.svelte) so its shared group-by
		 * <select> can call `state.setGroupBy(...)` directly (spec §4.3
		 * wiring pattern, same as ItemsServerGrid's own externally-owned
		 * GridState for reading sort/filters).
		 */
		state: GridState<ItemRow>;
		onRowClick: (item: Item) => void;
		onCellEdit: (edit: CellEdit<Item>) => void | Promise<void>;
		onRangePaste: (edits: CellEdit<Item>[], info: { skipped: number }) => void | Promise<void>;
	}

	let { columns, state: gridState, onRowClick, onCellEdit, onRangePaste }: Props = $props();

	const list = createListResource<Item>('items', {
		initialParams: { pagination: { offset: 0, limit: 20_000 } }
	});

	$effect(() => {
		void list.load();
		return () => list.dispose();
	});

	/** Derive the grouping demo's `category` field (spec §4.3): name's first whitespace token. */
	function toItemRow(item: Item): ItemRow {
		return { ...item, category: item.name.split(/\s+/)[0] ?? item.name };
	}

	const rows = $derived(list.rows.map(toItemRow));
</script>

<p class="note">{list.totalCount.toLocaleString()}件のデータを表示しています。</p>

{#if list.loading && list.rows.length === 0}
	<p class="loading">読み込み中…</p>
{:else}
	<div class="grid-wrap">
		<BantoGrid
			{rows}
			{columns}
			state={gridState}
			getRowId={(item) => item.id}
			onRowClick={(row: ItemRow) => onRowClick(row)}
			onCellEdit={(edit: CellEdit<ItemRow>) => onCellEdit(edit)}
			onRangePaste={(edits: CellEdit<ItemRow>[], info) => onRangePaste(edits, info)}
		/>
	</div>
{/if}

<style>
	.note {
		flex: 0 0 auto;
		margin: 0 0 0.75rem;
		color: var(--banto-text-muted);
		font-size: 0.8rem;
	}

	.loading {
		flex: 1;
		display: grid;
		place-items: center;
		color: var(--banto-text-muted);
	}

	.grid-wrap {
		flex: 1;
		min-height: 0;
	}
</style>
