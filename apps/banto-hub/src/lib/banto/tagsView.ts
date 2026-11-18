/**
 * #1179 PR3a（docs/hub/banto-hub-ui-improvement-proposal.md §HUB への当てはめ、
 * 2026-11-17 オーナー決定「選択中の対象とタブを URL に持ち、再読み込みや
 * 戻る操作で同じ画面に戻る」）: タグ登録ページ（`(app)/tags/+page.svelte`）の
 * 「左ツリーで選んだ対象」と「中央のタブ」を URL のクエリと行き来させる純関数。
 *
 * URL の形（既定の値は書かない）:
 *
 * | 画面                         | クエリ                             |
 * | ---------------------------- | ---------------------------------- |
 * | すべて・タグ一覧             | （無し）                           |
 * | 接続先・タグ一覧             | `?target=connection:<id>`          |
 * | 収集グループ・設定           | `?target=group:<id>&tab=settings`  |
 *
 * - 「すべて」には「設定」タブが無いので、`tab` は対象があるときだけ意味を持つ。
 * - 以前の `?groupId=<id>`（T18-2d）は受け付けない（オーナー方針「破壊的変更して
 *   よい」、2026-10-17）。アプリ内にこの形へのリンクは無い。ほかのクエリと一緒に、
 *   読み込み後の正規化（{@link tagsViewSearch} との差）で URL から消える。
 * - 読めない値（`target=foo`・`tab=bar`）と、読み込み済みの一覧に無い id は
 *   「すべて」・「タグ一覧」へ倒す（{@link resolveTagsView} の `missingTarget`）。
 *   一覧を読み終える前は存在を判定しない（`pruneTreeFilter` と同じ理由 -
 *   読み込み前に消すと、正しい id のディープリンクも「すべて」になる）。
 */
import { pruneTreeFilter, type TreeFilterSelection } from './treeFilterPrune.js';

/** 中央のタブ。`list` = 「タグ一覧」（既定）、`settings` = 「設定」。 */
export type TagsTab = 'list' | 'settings';

/** 画面の状態のうち URL に持つもの。 */
export interface TagsView {
	target: TreeFilterSelection;
	tab: TagsTab;
}

export const TAGS_VIEW_TARGET_PARAM = 'target';
export const TAGS_VIEW_TAB_PARAM = 'tab';

/**
 * クエリをそのまま読んだ結果（存在の確認の前）。`null` は「値はあるが読めない」。
 * `target` が無いときは「すべて」、`tab` が無いときは「タグ一覧」。
 */
export interface ParsedTagsView {
	target: TreeFilterSelection | null;
	tab: TagsTab | null;
}

const TARGET_PATTERN = /^(connection|group):([1-9][0-9]*)$/;

/**
 * #1179 PR3b: `target` だけを読む（タグモニタ `(app)/monitor/+page.svelte` と共有）。
 * 無ければ「すべて」、値はあるが読めなければ `null`。
 */
export function parseTreeTarget(params: Pick<URLSearchParams, 'get'>): TreeFilterSelection | null {
	const rawTarget = params.get(TAGS_VIEW_TARGET_PARAM);
	if (rawTarget === null) return { type: 'all' };
	const match = TARGET_PATTERN.exec(rawTarget);
	const id = match ? Number(match[2]) : NaN;
	return match && Number.isSafeInteger(id)
		? { type: match[1] as 'connection' | 'group', id }
		: null;
}

export function parseTagsView(params: URLSearchParams): ParsedTagsView {
	const target = parseTreeTarget(params);
	const rawTab = params.get(TAGS_VIEW_TAB_PARAM);
	const tab: TagsTab | null =
		rawTab === null || rawTab === 'list' ? 'list' : rawTab === 'settings' ? 'settings' : null;
	return { target, tab };
}

/**
 * #1179 PR3b: URL の `target` を、読み込み済みの一覧に照らして決める（両ページで
 * 共有）。`missingTarget` は {@link ResolvedTagsView} と同じ意味。
 */
export function resolveTreeTarget(
	parsed: TreeFilterSelection | null,
	connections: readonly { id: number }[],
	groups: readonly { id: number }[],
	options: { loaded: boolean }
): { target: TreeFilterSelection; missingTarget: boolean } {
	const target: TreeFilterSelection = parsed ?? { type: 'all' };
	const pruned = pruneTreeFilter(target, connections, groups, options);
	return { target: pruned, missingTarget: parsed === null || pruned !== target };
}

/** URL の対象が見つからないときの案内（両ページで同じ文言）。 */
export const MISSING_TREE_TARGET_MESSAGE =
	'指定された接続先・収集グループが見つからないため、すべてのタグを表示します';

export interface ResolvedTagsView {
	view: TagsView;
	/**
	 * URL の `target` が読めない、または読み込み済みの一覧に無かった（「すべて」へ
	 * 倒した）。削除などで今の対象が消えた場合も立つ - 案内を出すかは呼び出し側が
	 * 決める。
	 */
	missingTarget: boolean;
}

/**
 * URL の値を、読み込み済みの一覧に照らして画面の状態にする。対象が「すべて」なら
 * タブは常に「タグ一覧」（「設定」タブが無い）。読めない `tab` も「タグ一覧」。
 */
export function resolveTagsView(
	parsed: ParsedTagsView,
	connections: readonly { id: number }[],
	groups: readonly { id: number }[],
	options: { loaded: boolean }
): ResolvedTagsView {
	const { target, missingTarget } = resolveTreeTarget(parsed.target, connections, groups, options);
	const tab: TagsTab = target.type === 'all' ? 'list' : (parsed.tab ?? 'list');
	return { view: { target, tab }, missingTarget };
}

/**
 * #1179 PR3b: 対象だけのクエリ（先頭の `?` 付き、「すべて」なら空文字）。
 * タグモニタの URL と、ナビのリンクで対象を引き継ぐときに使う。
 */
export function treeTargetSearch(target: TreeFilterSelection): string {
	if (target.type === 'all') return '';
	// `URLSearchParams` は `:` を `%3A` にするので手で組む（値は英字・`:`・数字だけ）。
	return `?${TAGS_VIEW_TARGET_PARAM}=${target.type}:${target.id}`;
}

/** 画面の状態を URL のクエリ（先頭の `?` 付き、既定だけなら空文字）にする。 */
export function tagsViewSearch(view: TagsView): string {
	const target = treeTargetSearch(view.target);
	if (target === '') return '';
	return view.tab === 'settings' ? `${target}&${TAGS_VIEW_TAB_PARAM}=settings` : target;
}

export function sameTreeTarget(a: TreeFilterSelection, b: TreeFilterSelection): boolean {
	if (a.type === 'all' || b.type === 'all') return a.type === b.type;
	return a.type === b.type && a.id === b.id;
}

export function sameTagsView(a: TagsView, b: TagsView): boolean {
	return a.tab === b.tab && sameTreeTarget(a.target, b.target);
}
