/**
 * T19 S2-b（UX-38、docs/banto-hub-t19-design.md §3.4「親の削除は『定義の
 * み・履歴は残す』」、2026-09-02 オーナー決定）: 接続・収集グループの削除
 * 確認ダイアログ用ヘルパー。バックエンドが `banto_tags::plc_connection::
 * PlcConnectionService::cascade_delete_tx`/`banto_tags::collection_group::
 * CollectionGroupService::cascade_delete_tx` で配下のグループ・タグごと
 * まとめて削除するようになった（子が居ても拒否しない）ため、「何件消える
 * か分からないまま削除させない」（実装指示）が UI 側の役目になる。
 *
 * カウントはこのページが既に読み込み済みの `connections`/`groups`/`tags`
 * （タグ画面はこの3つを常に一括 `reload()` している）からクライアント側で
 * 計算するだけで足りる - `tagTreeContextMenu.ts`/`tagDeleteImpact.ts` と
 * 同じ「サーバーに問い合わせずローカルの一覧から算出する」考え方。件数の
 * 正しさの最終バックストップは常にサーバー側（削除は実際に
 * `cascade_delete_tx` が数える）であり、ここでの計算がずれても実際に消える
 * 件数自体には影響しない（確認文言だけの話）。
 */
import type { CollectionGroup, Tag } from './tagRegistryAdmin';

/** 接続を削除したときに一緒に消える定義の件数。 */
export interface ConnectionCascadeImpact {
	groups: number;
	tags: number;
}

/** 収集グループを削除したときに一緒に消える定義の件数。 */
export interface GroupCascadeImpact {
	tags: number;
}

/** `connectionId` 配下の収集グループ・タグの件数を数える。 */
export function countConnectionCascadeImpact(
	connectionId: number,
	groups: CollectionGroup[],
	tags: Tag[]
): ConnectionCascadeImpact {
	const groupIds = new Set(
		groups.filter((g) => g.plcConnectionId === connectionId).map((g) => g.id)
	);
	const tagCount = tags.filter((t) => groupIds.has(t.collectionGroupId)).length;
	return { groups: groupIds.size, tags: tagCount };
}

/** `groupId` に属するタグの件数を数える。 */
export function countGroupCascadeImpact(groupId: number, tags: Tag[]): GroupCascadeImpact {
	return { tags: tags.filter((t) => t.collectionGroupId === groupId).length };
}

/**
 * 接続削除の `window.confirm` 文言。件数が0でも「削除しますか？」だけに
 * 戻さない（`tagDeleteImpact.ts::formatDeleteConfirmMessage` と同じ方針 -
 * 空でも対象そのものは明示する）。件数が1件以上あるときだけ影響行を足し、
 * **常に**「履歴（記録済みの値）は残ります」を明示する（実装指示「履歴が
 * 残ることも明示すること」）。
 */
export function formatConnectionDeleteConfirmMessage(
	connectionName: string,
	impact: ConnectionCascadeImpact
): string {
	const lines = [`${connectionName} を削除しますか？`];
	if (impact.groups > 0 || impact.tags > 0) {
		lines.push('');
		lines.push(
			`この接続の収集グループ ${impact.groups}件とタグ ${impact.tags}件も一緒に削除されます。`
		);
	}
	lines.push('');
	lines.push('記録済みの履歴（収集データ）は削除されません。');
	return lines.join('\n');
}

/** 収集グループ削除の `window.confirm` 文言。上記の同型。 */
export function formatGroupDeleteConfirmMessage(
	groupName: string,
	impact: GroupCascadeImpact
): string {
	const lines = [`${groupName} を削除しますか？`];
	if (impact.tags > 0) {
		lines.push('');
		lines.push(`このグループのタグ ${impact.tags}件も一緒に削除されます。`);
	}
	lines.push('');
	lines.push('記録済みの履歴（収集データ）は削除されません。');
	return lines.join('\n');
}
