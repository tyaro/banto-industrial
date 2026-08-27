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
	 *
	 * T18-6c（2026-08-27 オーナー決定、TAG-UX-9 見た目刷新）追記: ノード
	 * 種別アイコン・グループの設定周期表示・空グループ案内行を追加する。
	 * banto-hub 固有の意匠（アイコン絵文字・periodMs の表示フォーマット・
	 * 「収集グループ未登録」の文言）はすべてこのファイルに閉じ込め、
	 * TreeView.svelte はジェネリックな構造（階層別背景・枠線コンテナ・
	 * 空状態スロット）だけを提供する。階層は「接続→収集グループ」の
	 * 2階層のまま（タグはツリーに出さない）で変更しない。
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

	/**
	 * T18-6c: ノード種別アイコン（行頭）。実 PLC 接続は 🔌、`calc`/`mem`
	 * 予約接続はそれぞれ 🧮/💾（末尾バッジ側の絵文字と重複させないため、
	 * `connectionBadge` からは絵文字を外しテキストのみにした - 下記参照）。
	 */
	function connectionIcon(connection: PlcConnection): string {
		if (isVirtualConnection(connection)) {
			return connection.name === CALC_CONNECTION_NAME ? '🧮' : '💾';
		}
		return '🔌';
	}

	/**
	 * T18-6c: `calc`/`mem` は行頭アイコンで種別を示すようになったため、末尾
	 * バッジは廃止する（2026-08-27 オーナー決定「バッジは消すか calc/mem の
	 * テキストのみにする」の前者を採る - `connection.name` 自体が既に
	 * `'calc'`/`'mem'` なので、テキストのみのバッジにしても行の表示が
	 * 「calc calc」/「mem mem」のように名前と二重になってしまうと実機
	 * 確認で判明したため）。`⚠ SIM` バッジは種別ではなく状態（実機では
	 * ない）を表すので、アイコン化の対象外のまま従来どおり維持する。
	 */
	function connectionBadge(connection: PlcConnection): string | null {
		if (isVirtualConnection(connection)) return null;
		return connection.simulation ? '⚠ SIM' : null;
	}

	/** T18-6c: グループ行に併記する設定周期。`periodMs` をそのまま `100ms` 形式で表示する（実測周期は対象外 - オーナー決定）。 */
	function formatPeriod(periodMs: number): string {
		return `${periodMs}ms`;
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
			<span class="icon" aria-hidden="true">📁</span>
			<span class="label">すべて</span>
		{:else if node.data.kind === 'connection'}
			<span class="icon" aria-hidden="true">{connectionIcon(node.data.connection)}</span>
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
			<span class="icon" aria-hidden="true">⏱</span>
			<span class="label">{node.data.group.name}</span>
			<span class="count">({tagCountForGroup(node.data.group.id)})</span>
			<span class="period">{formatPeriod(node.data.group.periodMs)}</span>
		{/if}
	{/snippet}
	{#snippet emptyState(node)}
		{#if node.data.kind === 'connection'}
			収集グループ未登録
		{/if}
	{/snippet}
</TreeView>

<style>
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

	.count {
		margin-left: 0.3rem;
		color: var(--banto-text-muted);
		font-size: 0.75rem;
	}

	.period {
		margin-left: 0.3rem;
		color: var(--banto-text-muted);
		font-size: 0.75rem;
	}
</style>
