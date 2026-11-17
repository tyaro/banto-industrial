<script lang="ts">
	/**
	 * T13-1（docs/ux-plan.md §4b）: 接続（plc_connections）→ 収集グループの
	 * 2階層を、banto の `@banto/tree-svelte` の `BantoTree` に流し込む、
	 * アプリ側コンポーネント。banto-hub の型（PlcConnection/CollectionGroup/
	 * Tag）に依存する部分はすべてここに閉じ込める。
	 *
	 * #1179 PR2a（2026-11-18）: 以前は自前の汎用部品 `TreeView.svelte` に
	 * 流し込んでいたが、「重複実装は banto に寄せる」（オーナー方針）に従い
	 * banto v7.3.0 の `BantoTree` に置き換えて `TreeView.svelte` を消した。
	 * 置き換えでの対応:
	 * - 選択はホスト主導（`selection` prop）。このコンポーネントは選択状態を
	 *   所有しない（tags/monitor ページが `selectedId` を渡し、選択の通知を
	 *   受けるだけ）のは以前と同じ。
	 * - 新しいルートの自動展開と開閉状態の保持は `autoExpandNewRoots`。
	 * - 右クリック・`Shift+F10`・メニューキーは `onContextMenu` に集約される。
	 * - 行のアクセシブル名は `TreeNode.label`（`label` スニペットを使うと
	 *   BantoTree は行の `aria-label` に `node.label` を出す）。以前の「ボタンの
	 *   中身の文字」と同じ文字列（アイコンを除く名前＋バッジ・件数・周期）を
	 *   `label` に入れて、E2E・支援技術から見える名前を変えない。
	 * - 子グループが 0 件の接続の「収集グループ未登録」の案内は、BantoTree に
	 *   空状態のスロットが無いため、選べない（`disabled`）子ノードとして出す。
	 *   選択・右クリックはこのコンポーネントで捨てる。
	 *
	 * T18-6c（2026-08-27 オーナー決定、TAG-UX-9 見た目刷新）: ノード種別
	 * アイコン・グループの設定周期表示・空グループ案内行。banto-hub 固有の
	 * 意匠（アイコン絵文字・periodMs の表示フォーマット・「収集グループ
	 * 未登録」の文言）はすべてこのファイルに閉じ込める。階層は「接続→収集
	 * グループ」の 2 階層のまま（タグはツリーに出さない）。
	 */
	import { BantoTree, type TreeNode } from '@banto/tree-svelte';
	import type { ConnectionTreeNodeData } from './connectionTreeTypes';
	import { isNavigationKey } from './connectionTreeNav';
	import { buildTagCountsByGroup, buildGroupsByConnection } from './connectionTreeBuild';
	import {
		isVirtualConnection,
		isDbSourceConnection,
		CALC_CONNECTION_NAME,
		type PlcConnection,
		type CollectionGroup,
		type Tag
	} from '#lib/banto/tagRegistryAdmin.js';

	interface Props {
		connections: PlcConnection[];
		groups: CollectionGroup[];
		tags: Tag[];
		selectedId?: string | null;
		/**
		 * ノードの選択の通知。`navigation: true` は、上下・Home・End・左右の
		 * キーでフォーカスが動いたことに選択が付いてきた場合（BantoTree は単一
		 * 選択でフォーカスに選択が付く）。クリック・Space・Enter（子の無い行の
		 * 確定）は `false`。狭幅で退避パネルを閉じるのは `false` のときだけ。
		 */
		onselect?: (data: ConnectionTreeNodeData, info: { navigation: boolean }) => void;
		/**
		 * T13-3 / T18-2e: ノードのコンテキストメニューの要求（マウス右クリック、
		 * `Shift+F10`、メニューキー）。キー操作では行の左下を位置として渡す
		 * （BantoTree の `onContextMenu`）。#1179 PR2a からは `onselect` と同じく
		 * ノードの中身（data）だけを渡す。未指定ならブラウザ標準のメニューを出す。
		 */
		oncontextmenu?: (data: ConnectionTreeNodeData, position: { x: number; y: number }) => void;
	}

	let { connections, groups, tags, selectedId = null, onselect, oncontextmenu }: Props = $props();

	/** ツリーの中だけで使う、「収集グループ未登録」の案内行の data。 */
	type NodeData = ConnectionTreeNodeData | { kind: 'empty' };

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

	/**
	 * 行のアクセシブル名（= `TreeNode.label`）。以前の `TreeView` のボタンの
	 * 中身から計算されていた名前と同じ文字列にする（行頭アイコンは
	 * `aria-hidden` だったので含めない）。
	 */
	function connectionAccessibleName(connection: PlcConnection): string {
		const badge = connectionBadge(connection);
		return badge ? `${connection.name} ${badge}` : connection.name;
	}

	function groupAccessibleName(group: CollectionGroup): string {
		const base = `${group.name} (${tagCountForGroup(group.id)}) ${formatPeriod(group.periodMs)}`;
		return group.querySql !== null ? `${base} SQL` : base;
	}

	const nodes = $derived.by((): TreeNode<NodeData>[] => {
		const allNode: TreeNode<NodeData> = { id: 'all', label: 'すべて', data: { kind: 'all' } };
		const connectionNodes = connections.map((connection): TreeNode<NodeData> => {
			const childGroups = groupsByConnection.get(connection.id) ?? [];
			const children: TreeNode<NodeData>[] =
				childGroups.length > 0
					? childGroups.map((group): TreeNode<NodeData> => ({
							id: `group:${group.id}`,
							label: groupAccessibleName(group),
							data: { kind: 'group', group, connection }
						}))
					: [
							{
								id: `empty:${connection.id}`,
								label: '収集グループ未登録',
								disabled: true,
								data: { kind: 'empty' }
							}
						];
			return {
				id: `conn:${connection.id}`,
				label: connectionAccessibleName(connection),
				data: { kind: 'connection', connection },
				children
			};
		});
		return [allNode, ...connectionNodes];
	});

	/** id → data（選択の通知は id で来るので、data を引き直す）。 */
	const dataById = $derived.by((): Map<string, NodeData> => {
		const map = new Map<string, NodeData>();
		const walk = (list: TreeNode<NodeData>[]): void => {
			for (const node of list) {
				if (node.data) map.set(node.id, node.data);
				if (node.children) walk(node.children);
			}
		};
		walk(nodes);
		return map;
	});

	/** 案内行（と data の無いノード）を外して、ページへ渡す data にする。 */
	function toPageData(data: NodeData | undefined): ConnectionTreeNodeData | null {
		if (!data || data.kind === 'empty') return null;
		return data;
	}

	/**
	 * T18-6c: ノード種別アイコン（行頭）。実 PLC 接続は 🔌、`calc`/`mem`
	 * 予約接続はそれぞれ 🧮/💾（末尾バッジ側の絵文字と重複させないため、
	 * `connectionBadge` からは絵文字を外しテキストのみにした - 下記参照）。
	 *
	 * S1（docs/banto-hub-external-db-design.md §4.1、実装指示4）追記:
	 * `protocol === 'postgres'`（DB Source 接続）は 🗄 - PLC とは全く別の
	 * 接続先（RDB）であることを行頭で即座に区別できるようにする。
	 */
	function connectionIcon(connection: PlcConnection): string {
		if (isVirtualConnection(connection)) {
			return connection.name === CALC_CONNECTION_NAME ? '🧮' : '💾';
		}
		if (isDbSourceConnection(connection)) return '🗄';
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
	 *
	 * S1（実装指示4「postgres 接続は distinct な label/badge（例: 'DB'）を
	 * 出す」）追記: `postgres` 接続は行頭アイコン（🗄）に加え、末尾に
	 * テキストバッジ「DB」も出す - `calc`/`mem`と違い接続名が「DB」を含む
	 * とは限らないため、アイコンだけでは種別の見落としが起きうる
	 * （calc/mem は名前自体が種別を語るので二重表示を避けたが、postgres は
	 * 任意の名前を付けられるため二重にはならない）。
	 */
	function connectionBadge(connection: PlcConnection): string | null {
		if (isVirtualConnection(connection)) return null;
		if (isDbSourceConnection(connection)) return 'DB';
		return connection.simulation ? '⚠ SIM' : null;
	}

	/** T18-6c: グループ行に併記する設定周期。`periodMs` をそのまま `100ms` 形式で表示する（実測周期は対象外 - オーナー決定）。 */
	function formatPeriod(periodMs: number): string {
		return `${periodMs}ms`;
	}

	// 選択の通知は id の配列で来る（単一選択なので先頭だけ）。既に選択中の
	// ノードをもう一度選んでも通知する（以前の `TreeView` と同じ。狭幅で
	// 退避パネルを閉じる契機になる）。案内行（disabled）は選べない。
	function handleSelectionChange(ids: string[]): void {
		const id = ids[0];
		if (id === undefined) return;
		const data = toPageData(dataById.get(id));
		if (!data) return;
		// 直前の入力がナビゲーションのキーなら、フォーカスに付いてきた選択。
		const navigation = lastInputWasNavigation;
		lastInputWasNavigation = false;
		onselect?.(data, { navigation });
	}

	/**
	 * 直前の入力が、フォーカスを動かすキーだったか。ツリーを包む要素で
	 * `keydown`・`pointerdown` を capture で受けて記録する（BantoTree の
	 * ハンドラより先に走るので、その中で呼ばれる `onSelectionChange` から読める）。
	 * BantoTree の DOM やクラスには依存しない。
	 */
	let lastInputWasNavigation = false;

	function handleKeydownCapture(event: KeyboardEvent): void {
		lastInputWasNavigation = isNavigationKey(event.key);
	}

	function handlePointerdownCapture(): void {
		lastInputWasNavigation = false;
	}

	/**
	 * 子の無い行の Enter・ダブルクリック（BantoTree の `onActivate`）。選択済みの
	 * 行でも「確定した」として通知する（狭幅で退避パネルを閉じる契機）。
	 */
	function handleActivate(node: TreeNode<NodeData>): void {
		const data = toPageData(node.data);
		if (data) onselect?.(data, { navigation: false });
	}

	function handleContextMenu(node: TreeNode<NodeData>, position: { x: number; y: number }): void {
		const data = toPageData(node.data);
		if (data) oncontextmenu?.(data, position);
	}
</script>

{#snippet nodeLabel(node: TreeNode<NodeData>)}
	{#if node.data?.kind === 'all'}
		<span class="icon" aria-hidden="true">📁</span>
		<span class="label">すべて</span>
	{:else if node.data?.kind === 'connection'}
		{@const connection = node.data.connection}
		<span class="icon" aria-hidden="true">{connectionIcon(connection)}</span>
		<span class="label">{connection.name}</span>
		{#if connectionBadge(connection)}
			<span
				class="badge"
				class:sim={connection.simulation &&
					!isVirtualConnection(connection) &&
					!isDbSourceConnection(connection)}
				class:db={isDbSourceConnection(connection)}
				title={isDbSourceConnection(connection)
					? 'PostgreSQL 接続（DB Source、S1）'
					: connection.simulation && !isVirtualConnection(connection)
						? 'シミュレーション接続（実機ではありません）'
						: undefined}>{connectionBadge(connection)}</span
			>
		{/if}
	{:else if node.data?.kind === 'group'}
		<span class="icon" aria-hidden="true">⏱</span>
		<span class="label">{node.data.group.name}</span>
		<span class="count">({tagCountForGroup(node.data.group.id)})</span>
		<span class="period">{formatPeriod(node.data.group.periodMs)}</span>
		{#if node.data.group.querySql !== null}
			<!--
				S3（docs/banto-hub-external-db-design.md §7 row S3、実装指示5
				「postgres グループは小さな『SQL』バッジを出す」）:
				`querySql` が非 `null`（= postgres 接続配下）のグループにだけ
				出す。接続ノードの「DB」バッジ（上）と同じ位置づけ - グループ
				一覧の中でどれが DB Source かを一目で区別できるようにする。
			-->
			<span class="badge db" title="SQL（query_sql）を持つグループ（DB Source、S2）">SQL</span>
		{/if}
	{:else if node.data?.kind === 'empty'}
		<!-- T18-6c: 子グループが1つも無い接続の下の淡色の案内行。 -->
		<span class="empty-label">{node.label}</span>
	{/if}
{/snippet}

<!--
	T18-6c: ツリー全体を枠線＋角丸のコンテナに収め、内側でスクロールさせる
	（以前は TreeView 側の `.tree-container` が持っていた）。
-->
<div
	class="tree-container"
	onkeydowncapture={handleKeydownCapture}
	onpointerdowncapture={handlePointerdownCapture}
>
	<BantoTree
		{nodes}
		label={nodeLabel}
		autoExpandNewRoots
		selection={selectedId ? [selectedId] : []}
		onSelectionChange={handleSelectionChange}
		onActivate={handleActivate}
		onContextMenu={oncontextmenu ? handleContextMenu : undefined}
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

	.badge.db {
		color: var(--banto-primary);
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

	/*
	 * 「収集グループ未登録」の案内行。BantoTree は disabled の行を半透明に
	 * するので、色は既存の --banto-text-muted に落とすだけにする。
	 */
	.empty-label {
		color: var(--banto-text-muted);
		font-style: italic;
	}
</style>
