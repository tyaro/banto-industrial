/**
 * 収集イベント・接続ごとの状態に出る内部キー（`conn:<id>` / `tag:<id>`）を、
 * 利用者が付けた名前に直す（#383 段階3 P5）。
 *
 * キーは収集エンジンの識別子（`banto-collect` の `ConnectionPlan::key` など）で、
 * 利用者には何のことか分からない。レジストリ（接続・収集グループ・タグの一覧）から
 * 名前を引く。**判断は純関数**（`buildKeyNames` / `resolveKeyLabel`）に出し、
 * `collectNames.test.ts` で固定する。
 *
 * - 名前が見つからないキー（削除された接続・タグのイベントなど）は**キーをそのまま
 *   出す**（落とさない・空にしない）。
 * - 同じ名前のタグが別のグループにあるときだけ、`名前（グループ名）` と区別する。
 * - 一覧を読めなかったときは、その部分だけ名前が無い（キーのまま）。名前の
 *   解決に失敗しても、イベントや状態の表示自体は止めない。
 */
import {
	listCollectionGroups,
	listPlcConnections,
	listTags,
	isTagRegistryAvailable
} from './tagRegistryAdmin';

/** キー（`conn:<id>` / `tag:<id>`）→ 表示名。 */
export type KeyNames = ReadonlyMap<string, string>;

interface Named {
	id: number;
	name: string;
}

export function buildKeyNames(input: {
	connections: readonly Named[];
	groups: readonly Named[];
	tags: readonly (Named & { collectionGroupId: number })[];
}): Map<string, string> {
	const names = new Map<string, string>();
	for (const connection of input.connections) {
		names.set(`conn:${connection.id}`, connection.name);
	}
	const groupNames = new Map(input.groups.map((group) => [group.id, group.name]));
	const tagNameCount = new Map<string, number>();
	for (const tag of input.tags) {
		tagNameCount.set(tag.name, (tagNameCount.get(tag.name) ?? 0) + 1);
	}
	for (const tag of input.tags) {
		const groupName = groupNames.get(tag.collectionGroupId);
		const ambiguous = (tagNameCount.get(tag.name) ?? 0) > 1;
		names.set(`tag:${tag.id}`, ambiguous && groupName ? `${tag.name}（${groupName}）` : tag.name);
	}
	return names;
}

/** キーの表示（純関数）。`null`（そのイベントに無い）は `-`、名前が無ければキーのまま。 */
export function resolveKeyLabel(key: string | null | undefined, names: KeyNames): string {
	if (key === null || key === undefined) return '-';
	return names.get(key) ?? key;
}

/**
 * レジストリから名前を読む。一覧ごとに失敗を吸収する（読めなかった部分は空）。
 * デモモード（backend 無し）では何も読まない。
 */
export async function loadKeyNames(): Promise<Map<string, string>> {
	if (!isTagRegistryAvailable()) return new Map();
	const [connections, groups, tags] = await Promise.all([
		listPlcConnections().catch(() => []),
		listCollectionGroups().catch(() => []),
		listTags().catch(() => [])
	]);
	return buildKeyNames({ connections, groups, tags });
}
