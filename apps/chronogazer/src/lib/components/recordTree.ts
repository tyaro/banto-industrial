/**
 * #1179 PR4b: 「記録」ページの左ツリー（`RecordTree.svelte`）の木の組み立て。
 * HUB の `connectionTreeBuild.ts`・`ConnectionTree.svelte` と同じく、
 * 「すべて / 接続先 / 収集グループ」の 2 階層を作る（記録項目はツリーに出さない）。
 * DOM に依存しない純関数にして unit test で確かめる。
 *
 * - ノードの id は `all` / `conn:<id>` / `group:<id>`（{@link recordTargetNodeId}）。
 * - 行のアクセシブル名（`TreeNode.label`）は、見た目の行頭アイコンを除いた名前と
 *   印（「Hub 経由」「無効」「シミュレーション」・件数・周期）。E2E と支援技術は
 *   この名前で行を探す。
 * - 収集グループが 0 件の接続先にも `children: []` を渡す（BantoTree の
 *   `emptyChildren` で「収集グループ未登録」が出る）。
 */
import type { TreeNode } from '@banto/tree-svelte';
import type { CollectionGroup, PlcConnection } from '#lib/banto/tagRegistryAdmin.js';
import type { RecordTarget } from '#lib/banto/recordsView.js';

export type RecordTreeNodeData =
	| { kind: 'all' }
	| { kind: 'connection'; connection: PlcConnection }
	| { kind: 'group'; group: CollectionGroup; connection: PlcConnection };

/** Hub 経由の接続か（`protocol: 'hub'`、「Hub から追加」が作る接続）。 */
export function isHubConnection(connection: Pick<PlcConnection, 'protocol'>): boolean {
	return connection.protocol === 'hub';
}

/** 周期の表示（`1s` / `100ms`）。グループのフォームの選択肢と同じ書き方。 */
export function formatPeriodMs(periodMs: number): string {
	return periodMs >= 1000 ? `${periodMs / 1000}s` : `${periodMs}ms`;
}

/** 接続先の行に付ける印（表示順）。 */
export function connectionMarks(connection: PlcConnection): string[] {
	const marks = [isHubConnection(connection) ? 'Hub 経由' : 'PLC 直結'];
	if (!connection.enabled) marks.push('無効');
	if (connection.simulation && !isHubConnection(connection)) marks.push('シミュレーション');
	return marks;
}

/** 収集グループの行に付ける印。 */
export function groupMarks(group: CollectionGroup): string[] {
	return group.enabled ? [] : ['無効'];
}

export function connectionAccessibleName(connection: PlcConnection): string {
	return [connection.name, ...connectionMarks(connection)].join(' ');
}

export function groupAccessibleName(group: CollectionGroup, recordCount: number): string {
	return [
		group.name,
		`(${recordCount})`,
		formatPeriodMs(group.periodMs),
		...groupMarks(group)
	].join(' ');
}

/** 収集グループごとの記録項目の件数。 */
export function countRecordsByGroup(
	records: readonly { collectionGroupId: number }[]
): Map<number, number> {
	const counts = new Map<number, number>();
	for (const record of records) {
		counts.set(record.collectionGroupId, (counts.get(record.collectionGroupId) ?? 0) + 1);
	}
	return counts;
}

/** 「すべて / 接続先 / 収集グループ」の木。接続先・グループの並びは一覧の順のまま。 */
export function buildRecordTreeNodes(
	connections: readonly PlcConnection[],
	groups: readonly CollectionGroup[],
	records: readonly { collectionGroupId: number }[]
): TreeNode<RecordTreeNodeData>[] {
	const counts = countRecordsByGroup(records);
	const groupsByConnection = new Map<number, CollectionGroup[]>();
	for (const group of groups) {
		const list = groupsByConnection.get(group.plcConnectionId);
		if (list) list.push(group);
		else groupsByConnection.set(group.plcConnectionId, [group]);
	}
	const allNode: TreeNode<RecordTreeNodeData> = {
		id: 'all',
		label: 'すべて',
		data: { kind: 'all' }
	};
	const connectionNodes = connections.map((connection): TreeNode<RecordTreeNodeData> => ({
		id: `conn:${connection.id}`,
		label: connectionAccessibleName(connection),
		data: { kind: 'connection', connection },
		children: (groupsByConnection.get(connection.id) ?? []).map(
			(group): TreeNode<RecordTreeNodeData> => ({
				id: `group:${group.id}`,
				label: groupAccessibleName(group, counts.get(group.id) ?? 0),
				data: { kind: 'group', group, connection }
			})
		)
	}));
	return [allNode, ...connectionNodes];
}

/** ツリーの対象からノードの id。 */
export function recordTargetNodeId(target: RecordTarget): string {
	if (target.type === 'all') return 'all';
	return target.type === 'connection' ? `conn:${target.id}` : `group:${target.id}`;
}

/** ノードの中身から対象。 */
export function recordTargetOf(data: RecordTreeNodeData): RecordTarget {
	if (data.kind === 'all') return { type: 'all' };
	if (data.kind === 'connection') return { type: 'connection', id: data.connection.id };
	return { type: 'group', id: data.group.id };
}

/**
 * フォーカスを動かすキー（広幅では BantoTree の選択がフォーカスに付いてくる）。
 * これで起きた選択は、ページが履歴を積まず置き換える（HUB の
 * `connectionTreeNav.ts` と同じ）。
 */
const NAVIGATION_KEYS: ReadonlySet<string> = new Set([
	'ArrowUp',
	'ArrowDown',
	'ArrowLeft',
	'ArrowRight',
	'Home',
	'End',
	'PageUp',
	'PageDown'
]);

export function isNavigationKey(key: string): boolean {
	return NAVIGATION_KEYS.has(key);
}
