<script lang="ts">
	/**
	 * T13-1（docs/ux-plan.md §4b）: 接続（plc_connections）→ 収集グループの
	 * 2階層を汎用部品 TreeView.svelte に流し込む、アプリ側コンポーネント。
	 * banto-hub の型（PlcConnection/CollectionGroup/Tag）に依存する部分は
	 * すべてここに閉じ込め、TreeView 自体はジェネリックなまま保つ
	 * （汎用部品を `@banto/*` へ昇格する際、このファイルは banto-hub 側に
	 * 残る想定 — docs/ux-plan.md §4b「実装の置き場所」参照）。
	 *
	 * 選択状態はこのコンポーネントが所有しない（tags ページ側が
	 * `selectedId` を渡し、選択イベントで通知を受けるだけ）。
	 */
	import TreeView from './TreeView.svelte';
	import type { TreeNode } from './treeTypes';
	import type { ConnectionTreeNodeData } from './connectionTreeTypes';
	import { buildTagCountsByGroup, buildGroupsByConnection } from './connectionTreeBuild';
	import {
		isVirtualConnection,
		CALC_CONNECTION_NAME,
		MEM_CONNECTION_NAME,
		type PlcConnection,
		type CollectionGroup,
		type Tag
	} from '$lib/banto/tagRegistryAdmin';

	interface Props {
		connections: PlcConnection[];
		groups: CollectionGroup[];
		tags: Tag[];
		selectedId?: string | null;
		onselect?: (data: ConnectionTreeNodeData) => void;
		/**
		 * T13-3（2026-08-08 オーナー決定、拡張点のみここで仕込む）:
		 * TreeView の `oncontextmenu` をそのまま再公開する。tags ページは
		 * まだ配線しない — 「すべて」ノードで新規接続、接続ノードで新規
		 * グループ、グループノードで新規タグ、というコンテキストメニューは
		 * T13-3 の作業。
		 */
		oncontextmenu?: (
			node: TreeNode<ConnectionTreeNodeData>,
			position: { x: number; y: number }
		) => void;
	}

	let { connections, groups, tags, selectedId = null, onselect, oncontextmenu }: Props = $props();

	// T18-5a（docs/banto-hub-t18-design.md「T18-5a 大量タグ性能」第1段）:
	// tags/groups をそれぞれ1回だけ Map に集計する（O(T)/O(G)）。connections/
	// groups/tags が変わらない限り再計算されないので、ラベル描画のたびに
	// 全走査していた旧実装（tagCountForGroup の O(グループ数×タグ数)、
	// groups.filter の O(接続数×グループ数)）を避けられる。詳細は
	// connectionTreeBuild.ts のコメント参照。
	const tagCountsByGroup = $derived(buildTagCountsByGroup(tags));
	const groupsByConnection = $derived(buildGroupsByConnection(groups));

	function tagCountForGroup(groupId: number): number {
		return tagCountsByGroup.get(groupId) ?? 0;
	}

	const nodes = $derived.by((): TreeNode<ConnectionTreeNodeData>[] => {
		const allNode: TreeNode<ConnectionTreeNodeData> = { id: 'all', data: { kind: 'all' } };
		const connectionNodes = connections.map((connection): TreeNode<ConnectionTreeNodeData> => {
			const childGroups = groupsByConnection.get(connection.id) ?? [];
			return {
				id: `conn:${connection.id}`,
				data: { kind: 'connection', connection },
				children: childGroups.map((group): TreeNode<ConnectionTreeNodeData> => {
					return { id: `group:${group.id}`, data: { kind: 'group', group, connection } };
				})
			};
		});
		return [allNode, ...connectionNodes];
	});

	function connectionBadge(connection: PlcConnection): string | null {
		if (isVirtualConnection(connection)) {
			if (connection.name === CALC_CONNECTION_NAME) return '🧮 calc';
			if (connection.name === MEM_CONNECTION_NAME) return '💾 mem';
			return null;
		}
		return connection.simulation ? '⚠ SIM' : null;
	}

	// TreeView は選択イベントをノード全体（TreeNode<T>）で通知するが、
	// ConnectionTree の `onselect` prop は呼び出し側（tags ページ）の
	// 使い勝手を優先してノードの中身（data）だけを渡す — ここで剥がす。
	function handleSelect(node: TreeNode<ConnectionTreeNodeData>): void {
		onselect?.(node.data);
	}
</script>

<TreeView {nodes} {selectedId} onselect={handleSelect} {oncontextmenu}>
	{#snippet label(node)}
		{#if node.data.kind === 'all'}
			<span class="label">すべて</span>
		{:else if node.data.kind === 'connection'}
			<span class="label">{node.data.connection.name}</span>
			{#if connectionBadge(node.data.connection)}
				<span
					class="badge"
					class:sim={node.data.connection.simulation && !isVirtualConnection(node.data.connection)}
					title={node.data.connection.simulation && !isVirtualConnection(node.data.connection)
						? 'シミュレーション接続（実機ではありません）'
						: undefined}>{connectionBadge(node.data.connection)}</span
				>
			{/if}
		{:else if node.data.kind === 'group'}
			<span class="label">{node.data.group.name}</span>
			<span class="count">({tagCountForGroup(node.data.group.id)})</span>
		{/if}
	{/snippet}
</TreeView>

<style>
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

	.count {
		margin-left: 0.3rem;
		color: var(--banto-text-muted);
		font-size: 0.75rem;
	}
</style>
