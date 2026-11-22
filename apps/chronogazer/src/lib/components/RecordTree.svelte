<script lang="ts">
	/**
	 * #1179 PR4b（docs/chronogazer/chronogazer-ui-improvement-proposal.md §2.1、2026-11-18）:
	 * 「記録」ページの左ツリー。接続先 → 収集グループの 2 階層を banto の
	 * `@banto/tree-svelte` の `BantoTree` に流し込む、アプリ側コンポーネント。
	 * 手本は HUB の `apps/banto-hub/src/lib/components/ConnectionTree.svelte`
	 * （#1179 PR2a）。ChronoGazer の型に依存する部分はここと `recordTree.ts` に
	 * 閉じ込める。
	 *
	 * - 記録項目（タグ）はツリーに出さない。木の組み立ては `recordTree.ts`
	 *   （純関数・unit test あり）。
	 * - 選択はホスト主導（`selectedId`）。このコンポーネントは選択状態を持たず、
	 *   選んだことを `onselect` で知らせるだけ（未保存の確認でキャンセルされたら、
	 *   ページの `selectedId` が変わらず、BantoTree が元の行へ戻す）。
	 * - 行頭のアイコンで Hub 経由（🔗）か PLC 直結（🔌）かを分け、無効・
	 *   シミュレーションの印を付ける。グループには周期と記録項目の件数。
	 * - グループが 0 件の接続先は `emptyChildren` で「収集グループ未登録」。
	 * - 右クリックメニューはまだ無い（PR4b では作らない）。
	 * - 狭幅（左ツリーがドロワー）では `selectionFollowsFocus=false`（HUB と同じ
	 *   2026-11-18 オーナー決定）。上下キーはフォーカスだけを動かし、
	 *   Enter・Space・クリックで選ぶ。
	 */
	import { BantoTree, type TreeNode } from '@banto/tree-svelte';
	import type { CollectionGroup, PlcConnection, Tag } from '#lib/banto/tagRegistryAdmin.js';
	import {
		buildRecordTreeNodes,
		connectionMarks,
		countRecordsByGroup,
		formatPeriodMs,
		groupMarks,
		isHubConnection,
		isNavigationKey,
		type RecordTreeNodeData
	} from './recordTree.js';

	interface Props {
		connections: readonly PlcConnection[];
		groups: readonly CollectionGroup[];
		records: readonly Tag[];
		selectedId?: string | null;
		/** 上下キーのフォーカス移動に選択を付いてこさせるか。狭幅では `false`。 */
		selectionFollowsFocus?: boolean;
		/**
		 * ノードを選んだ（既に選んでいるノードをもう一度選んだときも通知する）。
		 * `navigation: true` は、上下などのキーでフォーカスが動いたことに選択が
		 * 付いてきた場合（広幅のみ）。ページは履歴を積まず置き換え、狭幅の
		 * ドロワーも閉じない。
		 */
		onselect?: (data: RecordTreeNodeData, info: { navigation: boolean }) => void;
	}

	let {
		connections,
		groups,
		records,
		selectedId = null,
		selectionFollowsFocus = true,
		onselect
	}: Props = $props();

	type NodeData = RecordTreeNodeData;

	const nodes = $derived(buildRecordTreeNodes(connections, groups, records));
	const counts = $derived(countRecordsByGroup(records));

	/** id → data（選択の通知は id で来るので、data を引き直す）。 */
	const dataById = $derived.by((): Map<string, NodeData> => {
		const map = new Map<string, NodeData>();
		for (const node of nodes) {
			if (node.data) map.set(node.id, node.data);
			for (const child of node.children ?? []) {
				if (child.data) map.set(child.id, child.data);
			}
		}
		return map;
	});

	function handleSelectionChange(ids: string[]): void {
		const id = ids[0];
		if (id === undefined) return;
		const data = dataById.get(id);
		if (!data) return;
		const navigation = lastInputWasNavigation;
		lastInputWasNavigation = false;
		onselect?.(data, { navigation });
	}

	/**
	 * 直前の入力がフォーカスを動かすキーだったか。ツリーを包む要素で `keydown`・
	 * `pointerdown` を capture で受けて記録する（BantoTree のハンドラより先に走る）。
	 */
	let lastInputWasNavigation = false;

	/**
	 * 子の無い行の Enter・ダブルクリック。広幅では Enter が選択を通知しないので
	 * ここで通知する。狭幅では BantoTree が先に選択を通知済みなので、二重に
	 * 知らせない（未保存の確認が二重に出る）。HUB の `ConnectionTree` と同じ。
	 */
	function handleActivate(node: TreeNode<NodeData>): void {
		if (!selectionFollowsFocus) return;
		if (node.data) onselect?.(node.data, { navigation: false });
	}
</script>

{#snippet nodeLabel(node: TreeNode<NodeData>)}
	{#if node.data?.kind === 'all'}
		<span class="icon" aria-hidden="true">📁</span>
		<span class="label">すべて</span>
	{:else if node.data?.kind === 'connection'}
		{@const connection = node.data.connection}
		<span
			class="icon"
			aria-hidden="true"
			title={isHubConnection(connection) ? 'Hub 経由' : 'PLC 直結'}
		>
			{isHubConnection(connection) ? '🔗' : '🔌'}
		</span>
		<span class="label">{connection.name}</span>
		{#each connectionMarks(connection).slice(1) as mark (mark)}
			<span class="badge" class:sim={mark === 'シミュレーション'}>{mark}</span>
		{/each}
	{:else if node.data?.kind === 'group'}
		{@const group = node.data.group}
		<span class="icon" aria-hidden="true">⏱</span>
		<span class="label">{group.name}</span>
		<span class="count">({counts.get(group.id) ?? 0})</span>
		<span class="period">{formatPeriodMs(group.periodMs)}</span>
		{#each groupMarks(group) as mark (mark)}
			<span class="badge">{mark}</span>
		{/each}
	{/if}
{/snippet}

{#snippet emptyGroups()}
	<span class="empty-label">収集グループ未登録</span>
{/snippet}

<div
	class="tree-container"
	data-testid="record-tree"
	onkeydowncapture={(event) => (lastInputWasNavigation = isNavigationKey(event.key))}
	onpointerdowncapture={() => (lastInputWasNavigation = false)}
>
	<BantoTree
		{nodes}
		label={nodeLabel}
		autoExpandNewRoots
		{selectionFollowsFocus}
		emptyChildren={emptyGroups}
		selection={selectedId ? [selectedId] : []}
		onSelectionChange={handleSelectionChange}
		onActivate={handleActivate}
	/>
</div>

<style>
	.tree-container {
		height: 100%;
		box-sizing: border-box;
		overflow-y: auto;
		padding: 0.4rem;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
		background: var(--banto-surface);
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

	.badge {
		margin-left: 0.4rem;
		font-size: 0.7rem;
		color: var(--banto-text-muted);
	}

	.badge.sim {
		color: var(--banto-warning);
		font-weight: 700;
	}

	.count,
	.period {
		margin-left: 0.3rem;
		color: var(--banto-text-muted);
		font-size: 0.75rem;
	}

	.empty-label {
		color: var(--banto-text-muted);
		font-style: italic;
	}
</style>
