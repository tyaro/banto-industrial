/**
 * T18-4b（docs/banto-hub-t18-design.md「T18-4b 選択購読と再接続堅牢化」、
 * TAG-UX-H の一部）: ライブタグモニタ（`(app)/monitor/+page.svelte`）の
 * ツリー選択（`MonitorTreeFilter`、`monitorFilter.ts`）から、WS 購読
 * （`connectTagStream` が送る `subscribe.tags`）に渡すパターン配列を組み立てる
 * 依存ゼロの純関数。T18-4a まではツリー選択は表示絞り込み（`filterMonitorRows`）
 * にしか使われておらず、購読は常に `["*"]` 固定だった - このファイルはそこに
 * 「選択した範囲だけ購読する」を足す。
 *
 * サーバー側の購読解決（`apps/banto-hub/core/src/subscribe_core.rs::TagPattern`）
 * が正本。このファイルはそのワイヤフォーマットに合わせてクライアント側で
 * 文字列を組み立てるだけで、パターンの意味づけ（マッチ規則）は一切持たない:
 *
 * - `"*"` → 全タグ（`TagPattern::All`）。
 * - `"{connection}.{group}.*"` → その接続名・グループ名の表示名文字列と
 *   完全一致するタグ（`TagPattern::GroupWildcard`）。**表示名（`connection`/
 *   `group` の name 文字列）での比較であって、接続 ID・グループ ID による
 *   ワイルドカードは存在しない** - サーバーに ID ベースの購読は無いため、
 *   このファイルは常に `PlcConnection.name`/`CollectionGroup.name` から
 *   パターン文字列を組み立てる。
 * - それ以外の非空文字列 → 具体名の完全一致（`TagPattern::Exact`）。
 *
 * サーバーは空の `tags` 配列を拒否する（`subscribe_core.rs` 冒頭 doc comment
 * 参照）ため、**`subscriptionPatternsFor` は絶対に空配列を返さない** -
 * 呼び出し側がそのまま `subscribe` の `tags` に使っても壊れないようにする
 * のが最優先の契約。
 *
 * フォールバック方針（`treeFilter` が指す接続/グループが `connections`/
 * `groups` に見つからない場合）: 常に `["*"]`（全件購読）にフォールバックする。
 * 理由は「解決できない = 一覧（`connections`/`groups`）がまだロード中/
 * 直後の CRUD で古い/競合状態の可能性がある」ため、絞り込みを諦めて全件
 * 購読にフォールバックし、取りこぼし（本来届くべき値が届かない）を避ける -
 * 過剰購読（不要な値まで届く）は表示側の `filterMonitorRows` が黙って弾く
 * だけなので実害が無いのに対し、過小購読（届くべき値が届かない）は
 * 「値が更新されない」というユーザーに気づかれにくい不具合になるため、
 * 安全側に倒すなら過剰購読を選ぶ、という判断。
 *
 * `'connection'` で接続自体は解決できたがその接続配下のグループが0件の
 * 場合も、同じ理由でこの実装は `["*"]` にフォールバックする（他の選択肢と
 * して「該当なしを表す空でない番兵パターン」も検討したが、`filterMonitorRows`
 * が表示側で結局その接続の行しか残さないため実害が無く、番兵パターンを
 * 導入する複雑さに見合わないと判断した）。
 */
import type { CollectionGroup, PlcConnection } from './tagRegistryAdmin';
import type { MonitorTreeFilter } from './monitorFilter';

/** 絞り込みが解決できない場合の安全側フォールバック（このファイル冒頭の
 * doc comment 参照）。 */
const SUBSCRIBE_ALL: string[] = ['*'];

function groupWildcardPattern(connection: PlcConnection, group: CollectionGroup): string {
	return `${connection.name}.${group.name}.*`;
}

/**
 * `treeFilter` から WS `subscribe.tags` に渡すパターン配列を組み立てる。
 * ファイル冒頭の doc comment に規則とフォールバック方針を記載。
 */
export function subscriptionPatternsFor(
	treeFilter: MonitorTreeFilter,
	connections: PlcConnection[],
	groups: CollectionGroup[]
): string[] {
	if (treeFilter.type === 'all') return SUBSCRIBE_ALL;

	if (treeFilter.type === 'group') {
		const group = groups.find((g) => g.id === treeFilter.id);
		if (!group) return SUBSCRIBE_ALL;
		const connection = connections.find((c) => c.id === group.plcConnectionId);
		if (!connection) return SUBSCRIBE_ALL;
		return [groupWildcardPattern(connection, group)];
	}

	// treeFilter.type === 'connection'
	const connection = connections.find((c) => c.id === treeFilter.id);
	if (!connection) return SUBSCRIBE_ALL;
	const connectionGroups = groups.filter((g) => g.plcConnectionId === connection.id);
	if (connectionGroups.length === 0) return SUBSCRIBE_ALL;
	return connectionGroups.map((group) => groupWildcardPattern(connection, group));
}
