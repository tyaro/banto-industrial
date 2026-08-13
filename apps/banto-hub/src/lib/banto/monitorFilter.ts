/**
 * T18-4a（docs/banto-hub-t18-design.md「T18-4a モニタの Tree/検索統合」、
 * T13-2 移管）: ライブタグモニタ（`(app)/monitor/+page.svelte`）向けの
 * クライアント側絞り込み純関数。
 *
 * タグ登録ページ（`(app)/tags/+page.svelte`）の `TreeFilter`/`filteredTags`
 * と同じ「ツリー選択 + 検索ボックスの2条件を両方満たす行だけを残す」設計を
 * そのまま踏襲する — 登録と同じ操作感でモニタ対象を絞り込めるようにする、
 * というこのスライスの目的そのもの。tags 側の `TreeFilter` は
 * `{ type: 'all' | 'connection' | 'group'; id? }` 判別共用体だが、モニタの
 * 行（`CatalogTagEntry`）は接続・グループを **`ids: [connection_id,
 * group_id, tag_id]`（numeric）** でしか持たない（`connection`/`group` は
 * 表示用の名前文字列 - 同名の別接続/別グループを誤って一致させないため、
 * 絞り込みは常に ids の数値比較で行う）。tags 側の型をそのまま import
 * すると `group.plcConnectionId` 由来の判定になってしまい、モニタの
 * データ形状（catalog 行はグループ経由で接続を辿れない・接続一覧は
 * `ConnectionTree` に渡すためだけの補助データ）に合わないため、モニタ専用
 * にこの薄い型を定義する。
 *
 * `tags` ページの検索は `name`/`address` の部分一致だが、モニタの検索は
 * これに `external_name` を加える（TAG-UX-E「検索は外部名も対象」）—
 * モニタの一覧が主に外部名（`{接続}.{グループ}.{タグ}`）で識別されるため、
 * タグ単体の `name` だけでなく外部名でも探せた方が自然という判断
 * （実装指示 T18-4a 明記）。
 */

/** ツリー側の選択状態。tags ページの `TreeFilter` と同じ3値。 */
export type MonitorTreeFilter =
	{ type: 'all' } | { type: 'connection'; id: number } | { type: 'group'; id: number };

/**
 * `filterMonitorRows` が絞り込みに必要とする最小限のフィールド。
 * `CatalogTagEntry`（tagMonitorAdmin.ts）はこれを満たすスーパーセット
 * なので、呼び出し側は `Row extends CatalogTagEntry` をそのまま渡せる。
 */
export interface MonitorFilterableRow {
	ids: [number, number, number];
	external_name: string;
	name: string;
	address: string;
}

/**
 * ツリー選択（接続 or グループ）と検索ボックス（外部名・名前・アドレスの
 * 部分一致、大小文字無視）を両方満たす行だけを返す。サーバーへの再取得は
 * 発生しない（`rows` は呼び出し側が既にロード済みの全件）。
 *
 * - `treeFilter.type === 'all'`: ツリー側の絞り込みなし。
 * - `'connection'`: `row.ids[0] === treeFilter.id`（接続 ID 一致）。
 * - `'group'`: `row.ids[1] === treeFilter.id`（グループ ID 一致）。
 * - `searchQuery` が空/空白のみ: 検索条件は素通し（絞り込まない）。
 * - 非空: トリム＋小文字化した上で `external_name`/`name`/`address` の
 *   いずれかに部分一致すれば残す。
 */
export function filterMonitorRows<T extends MonitorFilterableRow>(
	rows: T[],
	treeFilter: MonitorTreeFilter,
	searchQuery: string
): T[] {
	let list = rows;
	if (treeFilter.type === 'connection') {
		const id = treeFilter.id;
		list = list.filter((r) => r.ids[0] === id);
	} else if (treeFilter.type === 'group') {
		const id = treeFilter.id;
		list = list.filter((r) => r.ids[1] === id);
	}

	const q = searchQuery.trim().toLowerCase();
	if (q === '') return list;
	return list.filter(
		(r) =>
			r.external_name.toLowerCase().includes(q) ||
			r.name.toLowerCase().includes(q) ||
			r.address.toLowerCase().includes(q)
	);
}
