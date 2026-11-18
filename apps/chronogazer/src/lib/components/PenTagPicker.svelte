<script lang="ts">
	/**
	 * #1179 PR6b（docs/chronogazer-ui-improvement-proposal.md §4、2026-11-18）:
	 * 「表示グループ」ページの右ペイン「タグ選択」。ペンに割り当てるタグを、
	 * 接続先 → 収集グループのツリーか検索から選ぶ。判断（木の組み立て・行の状態・
	 * 検索と打ち切り・上限）は `penTagPicker.ts` の純関数（unit test あり）で、
	 * このコンポーネントは表示と操作だけを持つ。
	 *
	 * - 出どころは 2 つ: ChronoGazer の Hub 以外の接続先の記録項目と、Hub の
	 *   カタログ（未登録のタグは保存で記録項目に登録される）。詳細は `penTagPicker.ts`。
	 * - **タグをツリーに常時展開しない**。左上の木は「接続先 → 収集グループ」までで、
	 *   描くのは選んだ収集グループのタグか、検索の結果（200 件まで）だけ。
	 * - 2 つのモード。`add` = ペンを足す（チェックで複数選べる。追加後もこの画面に留まり、
	 *   `aria-live` で知らせる）、`replace` = 1 本のペンのタグを選び直す（選ぶと呼び出し側が
	 *   ペンの設定へ戻す）。
	 * - 追加の操作: 行のダブルクリック、行の「追加」ボタン、チェックボックスで複数選んで
	 *   「選んだ N 件を追加」、行にフォーカスして Enter（追加）・Space（チェック）。
	 *   ペンの上限を超える追加は**止めて理由を出す**（黙って一部だけ足さない）。
	 * - 選択状態（ツリーの選択・検索語・チェック）はこのコンポーネントが持つ。ペンの
	 *   下書きは持たず、`onpick` で選んだ行を呼び出し側に渡すだけ。
	 */
	import { BantoTree, type TreeNode } from '@banto/tree-svelte';
	import type { CollectionGroup, PlcConnection, Tag } from '#lib/banto/tagRegistryAdmin.js';
	import {
		addBlocker,
		addableSelection,
		buildPickerCatalog,
		hiddenRowsMessage,
		hubNoteOf,
		limitRows,
		rowMark,
		rowState,
		searchRows,
		type PickerHubCatalog,
		type PickerNodeData,
		type PickerRow
	} from './penTagPicker.js';
	import { hubStatusLabel } from '#lib/banto/hubAdmin.js';

	interface Props {
		mode: 'add' | 'replace';
		/** 固定の見出し（例: 「タグ追加中・追加先: ライン1」）。 */
		title: string;
		backLabel?: string;
		connections: readonly PlcConnection[];
		collectionGroups: readonly CollectionGroup[];
		tags: readonly Tag[];
		hub: PickerHubCatalog | null;
		hubLoading: boolean;
		hubError: string | null;
		/** いま下書きのペンが使っているキー（置き換えのときは本人のペンを除く）。 */
		used: ReadonlySet<string>;
		/** あと何本足せるか（`add` のみ）。 */
		remaining: number;
		/** ペンの上限（理由の文言用）。 */
		max: number;
		disabled?: boolean;
		onpick: (rows: PickerRow[]) => void;
		onback: () => void;
		onrefresh: () => void;
	}

	let {
		mode,
		title,
		backLabel = '戻る',
		connections,
		collectionGroups,
		tags,
		hub,
		hubLoading,
		hubError,
		used,
		remaining,
		max,
		disabled = false,
		onpick,
		onback,
		onrefresh
	}: Props = $props();

	type NodeData = PickerNodeData;

	const hubNote = $derived(hubNoteOf(hub, hubLoading, hubError, hubStatusLabel));
	const catalog = $derived(
		buildPickerCatalog({ connections, collectionGroups, tags, hub, hubNote })
	);

	/** id → data（選択の通知は id で来るので引き直す。3 階層なので再帰）。 */
	const dataById = $derived.by((): Map<string, NodeData> => {
		const map = new Map<string, NodeData>();
		const walk = (list: readonly TreeNode<NodeData>[]): void => {
			for (const node of list) {
				if (node.data) map.set(node.id, node.data);
				if (node.children) walk(node.children);
			}
		};
		walk(catalog.nodes);
		return map;
	});

	let query = $state('');
	let selectedNode = $state<string | null>(null);
	let checked = $state<Set<string>>(new Set());
	let announcement = $state('');

	const searching = $derived(query.trim() !== '');
	const activeNode = $derived(
		selectedNode !== null && dataById.get(selectedNode)?.rowsKey ? selectedNode : null
	);
	const sourceRows = $derived.by((): PickerRow[] => {
		if (searching) return searchRows(catalog.allRows, query);
		return activeNode ? (catalog.rowsByNode.get(activeNode) ?? []) : [];
	});
	const limited = $derived(limitRows(sourceRows));
	const effectiveRemaining = $derived(mode === 'add' ? remaining : 1);

	/** チェックのうち、いま追加できるもの。 */
	const checkedAddable = $derived(addableSelection(catalog.allRows, checked, used));
	const bulkBlocker = $derived(
		checkedAddable.length > 0 ? addBlocker(checkedAddable.length, effectiveRemaining, max) : null
	);
	/** 1 件ずつの追加を止める理由（ペンが満杯のときだけ）。 */
	const fullBlocker = $derived(mode === 'add' ? addBlocker(1, remaining, max) : null);

	function handleSelectionChange(ids: string[]): void {
		const id = ids[0];
		if (id !== undefined && dataById.get(id)?.rowsKey) selectedNode = id;
	}

	function toggle(row: PickerRow): void {
		if (mode !== 'add' || rowState(row, used).kind !== 'addable') return;
		const next = new Set(checked);
		if (next.has(row.key)) next.delete(row.key);
		else next.add(row.key);
		checked = next;
	}

	function commit(rows: PickerRow[]): void {
		if (disabled || rows.length === 0) return;
		const blocker = addBlocker(rows.length, effectiveRemaining, max);
		if (blocker !== null) {
			announcement = blocker;
			return;
		}
		onpick(rows);
		const picked = new Set(rows.map((row) => row.key));
		checked = new Set([...checked].filter((key) => !picked.has(key)));
		if (mode === 'add') {
			announcement =
				rows.length === 1 ? `「${rows[0].name}」を追加しました` : `${rows.length} 件を追加しました`;
		}
	}

	function addRow(row: PickerRow): void {
		if (rowState(row, used).kind !== 'addable') return;
		commit([row]);
	}

	function handleRowKeydown(event: KeyboardEvent, row: PickerRow): void {
		// 行そのものにフォーカスがあるときだけ（中のボタン・チェックボックスの操作は奪わない）。
		if (event.target !== event.currentTarget) return;
		if (event.key === 'Enter') {
			event.preventDefault();
			addRow(row);
		} else if (event.key === ' ') {
			event.preventDefault();
			toggle(row);
		} else if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
			const sibling =
				event.key === 'ArrowDown'
					? (event.currentTarget as HTMLElement).nextElementSibling
					: (event.currentTarget as HTMLElement).previousElementSibling;
			if (sibling instanceof HTMLElement) {
				event.preventDefault();
				sibling.focus();
			}
		}
	}

	function rowAddLabel(row: PickerRow): string {
		return mode === 'add' ? `「${row.name}」を追加` : `「${row.name}」を選ぶ`;
	}
</script>

{#snippet nodeLabel(node: TreeNode<NodeData>)}
	{#if node.data?.kind === 'connection' || node.data?.kind === 'hub-connection'}
		<span class="icon" aria-hidden="true">🔌</span>
		<span class="label">{node.label}</span>
	{:else if node.data?.kind === 'hub-root'}
		<span class="icon" aria-hidden="true">🔗</span>
		<span class="label">Hub</span>
		{#if node.data.note}<span class="note-inline">{node.data.note}</span>{/if}
	{:else}
		<span class="icon" aria-hidden="true">⏱</span>
		<span class="label">{node.label}</span>
	{/if}
{/snippet}

<div class="picker" data-testid="pen-tag-picker">
	<div class="picker-head">
		<h4 data-testid="pen-tag-picker-title">{title}</h4>
		<button type="button" class="secondary" onclick={onback} {disabled}>{backLabel}</button>
	</div>

	<div class="tools">
		<input
			type="search"
			class="search"
			placeholder="タグを検索（名前・接続先・単位など）"
			aria-label="タグを検索"
			data-testid="pen-tag-picker-search"
			bind:value={query}
		/>
		<button
			type="button"
			class="secondary"
			onclick={onrefresh}
			disabled={disabled || hubLoading}
			title="Hub のカタログと記録項目を読み直します"
		>
			一覧を更新
		</button>
	</div>
	{#if mode === 'add'}
		<p class="remaining" data-testid="pen-tag-picker-remaining">
			あと {remaining} 本追加できます（上限 {max} 本）
		</p>
		{#if fullBlocker !== null}
			<p class="field-error" role="alert">{fullBlocker}</p>
		{/if}
	{/if}

	{#if !searching}
		<div class="tree-box" data-testid="pen-tag-picker-tree">
			<BantoTree
				nodes={catalog.nodes}
				label={nodeLabel}
				autoExpandNewRoots
				selection={activeNode ? [activeNode] : []}
				onSelectionChange={handleSelectionChange}
			/>
		</div>
	{/if}

	<div class="results" data-testid="pen-tag-picker-results">
		{#if !searching && activeNode === null}
			<p class="note">収集グループを選ぶと、そのタグの一覧を出します。検索は全体から探します。</p>
		{:else if sourceRows.length === 0}
			<p class="note">
				{searching ? '一致するタグがありません。' : 'この収集グループにタグがありません。'}
			</p>
		{:else}
			<div class="table-scroll">
				<table>
					<thead>
						<tr>
							{#if mode === 'add'}<th class="check-col"><span class="sr-only">選択</span></th>{/if}
							<th>名前</th>
							{#if searching}<th>接続先</th><th>収集グループ</th>{/if}
							<th>単位</th>
							<th>状態</th>
							<th class="act-col"><span class="sr-only">操作</span></th>
						</tr>
					</thead>
					<tbody>
						{#each limited.shown as row (row.key + '|' + row.nodeId)}
							{@const state = rowState(row, used)}
							{@const mark = rowMark(row, state)}
							<tr
								class:added={state.kind === 'added'}
								class:blocked={state.kind === 'blocked'}
								tabindex="0"
								data-testid="pen-tag-picker-row"
								data-key={row.key}
								ondblclick={() => addRow(row)}
								onkeydown={(event) => handleRowKeydown(event, row)}
							>
								{#if mode === 'add'}
									<td class="check-col">
										<input
											type="checkbox"
											aria-label={`「${row.name}」を選ぶ`}
											checked={checked.has(row.key)}
											disabled={disabled || state.kind !== 'addable'}
											onchange={() => toggle(row)}
										/>
									</td>
								{/if}
								<td class="name-col" title={row.address}>{row.name}</td>
								{#if searching}<td>{row.connection}</td><td>{row.group}</td>{/if}
								<td>{row.unit ?? ''}</td>
								<td
									class="mark"
									class:unregistered={row.kind === 'unregistered' && state.kind === 'addable'}
									>{mark}</td
								>
								<td class="act-col">
									<button
										type="button"
										class="secondary"
										aria-label={rowAddLabel(row)}
										disabled={disabled ||
											state.kind !== 'addable' ||
											(mode === 'add' && fullBlocker !== null)}
										onclick={() => addRow(row)}
									>
										{mode === 'add' ? '追加' : '選ぶ'}
									</button>
								</td>
							</tr>
						{/each}
					</tbody>
				</table>
			</div>
			{#if limited.hidden > 0}
				<p class="note" data-testid="pen-tag-picker-hidden">{hiddenRowsMessage(limited.hidden)}</p>
			{/if}
		{/if}
	</div>

	{#if mode === 'add'}
		<div class="bulk">
			<button
				type="button"
				data-testid="pen-tag-picker-bulk"
				disabled={disabled || checkedAddable.length === 0 || bulkBlocker !== null}
				onclick={() => commit(checkedAddable)}
			>
				選んだ {checkedAddable.length} 件を追加
			</button>
			{#if bulkBlocker !== null}
				<p class="field-error" role="alert">{bulkBlocker}</p>
			{/if}
		</div>
	{/if}

	<p class="sr-only" aria-live="polite" data-testid="pen-tag-picker-live">{announcement}</p>
</div>

<style>
	.picker {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
		min-width: 0;
	}

	.picker-head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.5rem;
	}

	.picker-head h4 {
		margin: 0;
		font-size: 0.95rem;
	}

	.tools {
		display: flex;
		gap: 0.4rem;
	}

	.search {
		flex: 1;
		min-width: 0;
	}

	.remaining {
		margin: 0;
		font-size: 0.85rem;
		color: var(--banto-text-muted);
	}

	.tree-box {
		max-height: 14rem;
		overflow-y: auto;
		padding: 0.3rem;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
		background: var(--banto-surface);
	}

	.results {
		min-width: 0;
	}

	.table-scroll {
		overflow-x: auto;
	}

	table {
		width: 100%;
		border-collapse: collapse;
		font-size: 0.85rem;
	}

	th,
	td {
		padding: 0.25rem 0.4rem;
		text-align: left;
		border-bottom: 1px solid var(--banto-border);
		vertical-align: middle;
	}

	tbody tr:focus-visible {
		outline: 2px solid var(--banto-focus-ring, currentColor);
		outline-offset: -2px;
	}

	tr.added,
	tr.blocked {
		color: var(--banto-text-muted);
	}

	.check-col,
	.act-col {
		width: 1%;
		white-space: nowrap;
	}

	.name-col {
		max-width: 12rem;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.mark.unregistered {
		color: var(--banto-warning);
	}

	.bulk {
		display: flex;
		flex-direction: column;
		gap: 0.3rem;
	}

	.note {
		margin: 0;
		font-size: 0.85rem;
		color: var(--banto-text-muted);
	}

	.field-error {
		margin: 0;
		font-size: 0.85rem;
		color: var(--banto-danger);
	}

	.icon {
		display: inline-block;
		margin-right: 0.3rem;
	}

	.label {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.note-inline {
		margin-left: 0.4rem;
		font-size: 0.75rem;
		color: var(--banto-text-muted);
	}

	.sr-only {
		position: absolute;
		width: 1px;
		height: 1px;
		margin: -1px;
		padding: 0;
		overflow: hidden;
		clip: rect(0, 0, 0, 0);
		white-space: nowrap;
		border: 0;
	}
</style>
