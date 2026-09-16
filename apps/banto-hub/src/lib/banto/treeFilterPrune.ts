/**
 * #381 レビュー対応3回目（2026-09-16）: ツリー選択（接続 / 収集グループ /
 * すべて）が**消えた対象を指したままにならない**ようにする純関数。
 *
 * タグ登録（`(app)/tags/+page.svelte` の `TreeFilter`）とタグモニタ
 * （`$lib/banto/monitorFilter.ts` の `MonitorTreeFilter`）は同じ3値の判別共用体で、
 * どちらもカタログ再取得（`reload()`）のあとも選択 id をそのまま持ち続けていた。
 * 選択中の接続・収集グループが削除されると、
 *
 * - グリッドは**存在しない id で絞られたまま**＝常に空になり、
 * - #378 で足した「現在の選択」の表示は名前を引けずに「すべて」と出る
 *   （表示と実際の絞り込みが食い違う）
 *
 * という状態になる。フィルタ側を「すべて」へ戻すのが正しい直し方
 * （表示だけ「選択が無効」にしてもグリッドが空のまま残るので直らない）。
 *
 * 2画面で同じ判断をするので、UI に依存しない純関数としてここに置く
 * （`monitorFilter.ts` はモニタ専用の絞り込みなので、共用のこれは別ファイル）。
 */

/** タグ登録の `TreeFilter` / モニタの `MonitorTreeFilter` と同じ3値。 */
export type TreeFilterSelection =
	{ type: 'all' } | { type: 'connection'; id: number } | { type: 'group'; id: number };

/** 存在確認に使う最小限の形（`PlcConnection`/`CollectionGroup` はこれを満たす）。 */
interface HasId {
	id: number;
}

/**
 * `filter` が指す接続・収集グループが `connections`/`groups` に無ければ
 * `{ type: 'all' }` を返す。それ以外は `filter` を**同一参照のまま**返す
 * （呼び出し側が `!==` で「変わったときだけ書き戻す」と書けるようにするため -
 * 毎回新しいオブジェクトを返すと Svelte の `$effect` が自分で自分を起こし続ける）。
 *
 * **両方の一覧が空のときは何もしない**: 「まだ読み込めていない」のか「全部
 * 消された」のかを区別できないためで、前者で選択を消すと、ディープリンク
 * （`?group=`）や作成直後のプリセット選択が読み込み完了の前に失われる。後者の
 * 状態ではそもそもグリッドも空・ツリーも「すべて」しか無いので実害が無い。
 */
export function pruneTreeFilter(
	filter: TreeFilterSelection,
	connections: readonly HasId[],
	groups: readonly HasId[]
): TreeFilterSelection {
	if (filter.type === 'all') return filter;
	if (connections.length === 0 && groups.length === 0) return filter;
	if (filter.type === 'connection') {
		return connections.some((c) => c.id === filter.id) ? filter : { type: 'all' };
	}
	return groups.some((g) => g.id === filter.id) ? filter : { type: 'all' };
}
