/**
 * T18-5a（docs/banto-hub-t18-design.md「T18-5a 大量タグ性能」第1段）:
 * ConnectionTree.svelte のツリー構築を O(T+G+C) 化するための集計ヘルパー。
 *
 * 改修前は「接続ごとに groups を全走査してフィルタする」
 * （O(接続数 × グループ数)）「グループごとに tags を全走査して件数を数える」
 * （`tagCountForGroup`、O(グループ数 × タグ数)）という実装になっており、
 * 基準機（10,000タグ・500グループ）規模では groups.filter が最大
 * 接続数×500件、tagCountForGroup がツリー描画のたびに最大500グループ×
 * 10,000タグ＝500万反復に達しうる。
 *
 * ここでは tags/groups をそれぞれ1パスして Map に集計し、ConnectionTree
 * 側では Map 参照（O(1)）だけで済むようにする。呼び出し側（.svelte）の
 * $derived で1回だけ構築し、ラベル描画のたびに再走査しないのが前提。
 */

/** `groupId` ごとのタグ件数を1パスで集計する（O(T)）。 */
export function buildTagCountsByGroup(tags: { collectionGroupId: number }[]): Map<number, number> {
	const counts = new Map<number, number>();
	for (const tag of tags) {
		counts.set(tag.collectionGroupId, (counts.get(tag.collectionGroupId) ?? 0) + 1);
	}
	return counts;
}

/**
 * `plcConnectionId` ごとのグループ配列を1パスで集計する（O(G)）。
 * 各配列内の順序は入力 `groups` 配列の順序を保つ。グループが0件の接続は
 * Map に存在しないので、呼び出し側は `groupsByConnection.get(id) ?? []`
 * のようにフォールバックすること。
 */
export function buildGroupsByConnection<G extends { id: number; plcConnectionId: number }>(
	groups: G[]
): Map<number, G[]> {
	const byConnection = new Map<number, G[]>();
	for (const group of groups) {
		const existing = byConnection.get(group.plcConnectionId);
		if (existing) {
			existing.push(group);
		} else {
			byConnection.set(group.plcConnectionId, [group]);
		}
	}
	return byConnection;
}
