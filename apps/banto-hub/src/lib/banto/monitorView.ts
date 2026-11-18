/**
 * #1179 PR3b（docs/hub/banto-hub-ui-improvement-proposal.md §HUB への当てはめ、段階 ①
 * 「2 つのページでツリーの選択を共有する」）: タグモニタ（`(app)/monitor/+page.svelte`）
 * の URL の形。対象はタグ登録と同じ `?target=connection:<id>` / `group:<id>`
 * （`#lib/banto/tagsView.ts` の `parseTreeTarget`・`treeTargetSearch`）。
 * タブは無いので `tab` は持たない。
 *
 * `focus`（T18-4c の「確認対象」、external_name のカンマ区切り）は URL に残す
 * （再読み込みでも強調が戻る）。ツリーの選択を変えても消さない。
 *
 * 以前の `?group=<id>` / `?connection=<id>`（T18-4c）は受け付けない（オーナー方針
 * 「破壊的変更してよい」、2026-10-17）。アプリ内のリンク（`monitorHref`）は
 * `target` の形に移した。読み込み後の正規化で URL から消える。
 */
import { treeTargetSearch } from './tagsView.js';
import type { TreeFilterSelection } from './treeFilterPrune.js';

export const MONITOR_FOCUS_PARAM = 'focus';

/**
 * `focus` の external_name の一覧（空の要素は捨てる）。`URLSearchParams.get()` が
 * 1 回デコード済みなので、ここで `decodeURIComponent` は掛けない（二重デコードで
 * `%` を含む名前が `URIError` になる - T18-4c の注意）。
 */
export function parseMonitorFocus(params: Pick<URLSearchParams, 'get'>): string[] {
	const raw = params.get(MONITOR_FOCUS_PARAM);
	if (!raw) return [];
	return raw.split(',').filter((name) => name !== '');
}

/**
 * タグモニタの URL のクエリ（先頭の `?` 付き、何も無ければ空文字）。区切りの
 * カンマは生のまま、要素の中身だけ `encodeURIComponent` する（`URLSearchParams`
 * は `,` まで encode するので使わない）。
 */
export function monitorViewSearch(
	target: TreeFilterSelection,
	focus: readonly string[] = []
): string {
	const params: string[] = [];
	const targetSearch = treeTargetSearch(target);
	if (targetSearch !== '') params.push(targetSearch.slice(1));
	if (focus.length > 0) {
		params.push(
			`${MONITOR_FOCUS_PARAM}=${focus.map((name) => encodeURIComponent(name)).join(',')}`
		);
	}
	return params.length === 0 ? '' : `?${params.join('&')}`;
}
